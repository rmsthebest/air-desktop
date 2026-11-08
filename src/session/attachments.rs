//! Attachments: what a message's attachment part says, the image preview
//! the view draws, and the downloads. Port of upstream's
//! `attachments_repository` (downloads, thumbnails) and the attachment half
//! of `message_content.rs`.
//!
//! Received files are only ever stored by the core and, on request, copied
//! to the downloads directory: never opened or handed to another program.
//! Images are drawn only from a preview the core made (or checked to be a
//! small WebP), decoded here under limits.

use std::collections::HashSet;
use std::io::{Cursor, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, PoisonError};
use std::time::Duration;

use aircoreclient::clients::CoreUser;
use aircoreclient::db::notification::{DbEntityId, DbNotification, DbOperation};
use aircoreclient::{
    AttachmentContent, AttachmentId, AttachmentStatus, AttachmentThumbnail, AttachmentUrl, ChatId,
    MarkChatAsRead, MessageId, ProvisionAttachmentError, UploadTaskError,
};
use futures_util::StreamExt as _;
use gpui::SharedString;
use image::AnimationDecoder as _;
use image::ImageDecoder as _;
use image::codecs::gif::GifDecoder;
use image::codecs::png::PngDecoder;
use image::codecs::webp::WebPDecoder;
use mimi_content::MimiContent;
use mimi_content::content_container::{Disposition, NestedPart};
use tracing::{debug, error, info, warn};

use crate::model::{self, Animation, Attachment, AttachmentState, Picture};
use crate::session::Session;
use crate::session::contacts::{Failure, on_tokio};
use crate::session::messages::core_chat_id;

impl Session {
    /// Sends the file at `path` to `chat`. Resolves once the message is
    /// stored; the upload goes on detached (see [`send`]).
    pub fn send_attachment(
        &self,
        chat: &model::ChatId,
        path: PathBuf,
        cx: &gpui_kit::App,
    ) -> impl Future<Output = Result<(), String>> + use<> {
        let (user, chat) = (self.user.clone(), core_chat_id(chat));
        on_tokio(cx, "send the file", send(user, chat, path))
    }
}

/// Downloads running at once, as upstream.
const CONCURRENT_DOWNLOADS: usize = 5;

/// Longest edge of a preview drawn: the core's thumbnail edge.
pub(crate) const MAX_PREVIEW_EDGE: u32 = 1024;

/// An original drawn as its own preview (the core found it small enough)
/// is read whole: beyond this, it isn't drawn.
const MAX_PREVIEW_BYTES: usize = 8 * 1024 * 1024;

/// Memory a preview's decoder may take: a 1024² RGBA image is 4 MiB.
const MAX_DECODE_ALLOC: u64 = 16 * 1024 * 1024;

/// A decoded preview is kept at most this large: the box it is drawn in,
/// at twice the pixels for high-density screens (1.5 MiB as BGRA).
const MAX_KEPT: (u32, u32) = (model::PREVIEW_BOX.0 * 2, model::PREVIEW_BOX.1 * 2);

/// Previews decoded per window at most (each at most [`MAX_KEPT`]); past
/// that, images show as plain file cards. Peers choose how many images a
/// chat has.
pub(super) const MAX_PICTURES: usize = 64;

/// Attachments being downloaded: one download per attachment at a time,
/// whoever asks (the core doesn't refuse a second).
static IN_FLIGHT: LazyLock<Mutex<HashSet<AttachmentId>>> = LazyLock::new(Default::default);

/// Holds an attachment in [`IN_FLIGHT`] until dropped.
struct Claim(AttachmentId);

impl Claim {
    fn take(id: AttachmentId) -> Option<Self> {
        let mut in_flight = IN_FLIGHT.lock().unwrap_or_else(PoisonError::into_inner);
        // Not `then_some`: that would make (and drop, relocking) a claim
        // for a taken id while the lock is held.
        if in_flight.insert(id) {
            Some(Self(id))
        } else {
            None
        }
    }
}

/// Holds `id` as a transfer would, until dropped (seam tests).
#[cfg(all(test, feature = "seam"))]
pub(super) fn hold(id: AttachmentId) -> Option<impl Drop> {
    Claim::take(id)
}

impl Drop for Claim {
    fn drop(&mut self) {
        let mut in_flight = IN_FLIGHT.lock().unwrap_or_else(PoisonError::into_inner);
        in_flight.remove(&self.0);
    }
}

/// What a message's attachment part declares. All of it is the sender's
/// word.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Declared {
    pub(super) name: SharedString,
    pub(super) description: Option<SharedString>,
    pub(super) size: u64,
    /// Declared as an image (it has dimensions), as upstream decides.
    pub(super) image: bool,
}

/// The attachment declared in `content`, if it is an attachment message.
/// Port of upstream's `convert_attachment`: the first well-formed
/// attachment part counts.
pub(super) fn declared(content: &MimiContent) -> Option<Declared> {
    let NestedPart::MultiPart {
        disposition: Disposition::Attachment,
        parts,
        ..
    } = &content.nested_part
    else {
        return None;
    };
    parts
        .iter()
        .find_map(|part| match part {
            NestedPart::ExternalPart {
                disposition: Disposition::Attachment,
                url,
                description,
                filename,
                size,
                ..
            } => Some((
                url.parse::<AttachmentUrl>().ok()?,
                filename,
                description,
                size,
            )),
            _ => None,
        })
        .map(|(url, filename, description, size)| Declared {
            name: match model::preview_line(filename) {
                name if name.is_empty() => "file".into(),
                name => name,
            },
            description: Some(model::preview_line(description)).filter(|d| !d.is_empty()),
            size: *size,
            image: url.dimensions().is_some(),
        })
}

/// The message's attachment as the view shows it. `None` when the core has
/// no attachment for the message.
pub(super) async fn load(
    user: &CoreUser,
    message: MessageId,
    declared: Declared,
    cached: impl Fn(AttachmentId) -> Option<Picture>,
    may_decode: bool,
) -> Option<(AttachmentId, Attachment)> {
    let id = user
        .attachment_infos_for_message(message)
        .await
        .first()?
        .attachment_id;
    let state = match user.attachment_status(id).await {
        Ok(Some(status)) => state_of(status),
        Ok(None) => return None,
        Err(err) => {
            error!("failed to load an attachment's status: {err:#}");
            AttachmentState::DownloadFailed
        }
    };
    let image = if state.is_local() {
        match cached(id) {
            Some(picture) => Some(picture),
            None if may_decode => preview(user, id, declared.image).await,
            None => None,
        }
    } else {
        None
    };
    let attachment = Attachment {
        id: model::AttachmentId(id.uuid),
        name: declared.name,
        description: declared.description,
        size: declared.size,
        state,
        image,
        animation: None,
    };
    Some((id, attachment))
}

fn state_of(status: AttachmentStatus) -> AttachmentState {
    match status {
        AttachmentStatus::Pending => AttachmentState::Pending,
        AttachmentStatus::Downloading => AttachmentState::Downloading,
        AttachmentStatus::Uploading => AttachmentState::Uploading,
        AttachmentStatus::Ready => AttachmentState::Ready,
        AttachmentStatus::UploadFailed => AttachmentState::UploadFailed,
        AttachmentStatus::NotFound => AttachmentState::Gone,
        AttachmentStatus::DownloadFailed | AttachmentStatus::Unknown => {
            AttachmentState::DownloadFailed
        }
    }
}

/// The preview to draw: the core's thumbnail, or the original where the
/// core found it small enough to serve as one. Never the original of a
/// failed (or missing) thumbnail, as upstream would: that is exactly the
/// file the core's decoder refused.
async fn preview(user: &CoreUser, id: AttachmentId, image: bool) -> Option<Picture> {
    let mut thumbnail = user.attachment_thumbnail(id).await;
    if image && matches!(thumbnail, Ok(None)) {
        // Downloaded, but the preview wasn't made (the app quit in
        // between, say): make it now, as upstream does on demand. Tried
        // once: whatever it gives (a failure too) is stored.
        make_preview(user, id, true).await;
        thumbnail = user.attachment_thumbnail(id).await;
    }
    let bytes = match thumbnail {
        Ok(Some(AttachmentThumbnail::Ready { bytes })) => bytes,
        Ok(Some(AttachmentThumbnail::OriginalFits)) => match user.load_attachment(id).await {
            Ok(
                AttachmentContent::Ready(bytes)
                | AttachmentContent::Uploading(bytes)
                | AttachmentContent::UploadFailed(bytes),
            ) if bytes.len() <= MAX_PREVIEW_BYTES => bytes,
            Ok(_) => return None,
            Err(err) => {
                error!("failed to load an attachment: {err:#}");
                return None;
            }
        },
        Ok(Some(AttachmentThumbnail::Failed) | None) => return None,
        Err(err) => {
            error!("failed to load a thumbnail: {err:#}");
            return None;
        }
    };
    match tokio::task::spawn_blocking(move || decode(&bytes)).await {
        Ok(picture) => picture,
        Err(err) => {
            error!("preview decode panicked: {err}");
            None
        }
    }
}

/// Decodes a WebP preview into the BGRA frame gpui draws, refusing
/// anything larger than a preview can be, and keeps it no larger than it
/// is drawn. Our own decoder, with its own limits: the core's checks were
/// made by another decoder (libwebp), which could read a crafted file
/// differently.
pub(crate) fn decode(bytes: &[u8]) -> Option<Picture> {
    decode_within(bytes, MAX_PREVIEW_EDGE, MAX_DECODE_ALLOC, MAX_KEPT)
}

/// Decodes a WebP image no larger than `edge` a side (the decoder taking
/// at most `alloc`), kept at most `kept`.
fn decode_within(bytes: &[u8], edge: u32, alloc: u64, kept: (u32, u32)) -> Option<Picture> {
    let mut reader = image::ImageReader::with_format(Cursor::new(bytes), image::ImageFormat::WebP);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(edge);
    limits.max_image_height = Some(edge);
    limits.max_alloc = Some(alloc);
    reader.limits(limits);
    let image = match reader.decode() {
        Ok(image) => image,
        Err(err) => {
            debug!("not drawing a preview: {err}");
            return None;
        }
    };
    let image = if image.width() > kept.0 || image.height() > kept.1 {
        image.thumbnail(kept.0, kept.1)
    } else {
        image
    };
    let mut pixels = image.into_rgba8();
    for pixel in pixels.pixels_mut() {
        pixel.0.swap(0, 2);
    }
    let frame = image::Frame::new(pixels);
    Some(Picture(Arc::new(gpui::RenderImage::new(vec![frame]))))
}

/// Frames of an animation at most.
const MAX_ANIMATION_FRAMES: usize = 240;

/// Memory an animation's frames may take, as BGRA: past it, the image stays
/// still. A preview-box GIF of about 80 frames.
const MAX_ANIMATION_BYTES: u64 = 32 * 1024 * 1024;

/// Animations decoded per window at most (bounds them to 192 MiB, with
/// their copies on the GPU); past that, images stay still.
pub(super) const MAX_ANIMATIONS: usize = 6;

/// The shortest frame delay played. Shorter ones, 0 above all, are played
/// slower, as browsers do: a peer's file must not make every frame a redraw.
const MIN_FRAME_DELAY: Duration = Duration::from_millis(20);
const SLOWED_FRAME_DELAY: Duration = Duration::from_millis(100);

/// The frames of a stored image the core found animated, for one already
/// drawn as a preview (so never the original of a failed thumbnail). `None`
/// when it is still, too large, or any of it can't be read.
pub(super) async fn animation(user: &CoreUser, id: AttachmentId) -> Option<Animation> {
    if !matches!(user.attachment_is_animated(id).await, Ok(Some(true))) {
        return None;
    }
    let bytes = match user.load_attachment(id).await {
        Ok(
            AttachmentContent::Ready(bytes)
            | AttachmentContent::Uploading(bytes)
            | AttachmentContent::UploadFailed(bytes),
        ) if bytes.len() <= MAX_PREVIEW_BYTES => bytes,
        Ok(_) => return None,
        Err(err) => {
            error!("failed to load an attachment: {err:#}");
            return None;
        }
    };
    match tokio::task::spawn_blocking(move || decode_animation(&bytes)).await {
        Ok(animation) => animation,
        Err(err) => {
            error!("animation decode panicked: {err}");
            None
        }
    }
}

/// Decodes an animated GIF, WebP or PNG under limits, by our own decoder.
fn decode_animation(bytes: &[u8]) -> Option<Animation> {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_PREVIEW_EDGE);
    limits.max_image_height = Some(MAX_PREVIEW_EDGE);
    limits.max_alloc = Some(MAX_DECODE_ALLOC);
    let reader = Cursor::new(bytes);
    let frames = match image::guess_format(bytes).ok()? {
        image::ImageFormat::Gif => {
            let mut decoder = GifDecoder::new(reader).ok()?;
            decoder.set_limits(limits).ok()?;
            decoder.into_frames()
        }
        image::ImageFormat::WebP => {
            let mut decoder = WebPDecoder::new(reader).ok()?;
            decoder.set_limits(limits).ok()?;
            if !decoder.has_animation() {
                return None;
            }
            decoder.into_frames()
        }
        image::ImageFormat::Png => {
            let decoder = PngDecoder::with_limits(reader, limits).ok()?;
            if !decoder.is_apng().ok()? {
                return None;
            }
            decoder.apng().ok()?.into_frames()
        }
        _ => return None,
    };
    collect_frames(frames)
}

/// The frames as gpui draws them, kept no larger than a preview. Stops as
/// soon as a limit is passed.
fn collect_frames(frames: image::Frames<'_>) -> Option<Animation> {
    let mut kept = Vec::new();
    let mut bytes = 0;
    let mut loop_time = Duration::ZERO;
    for frame in frames {
        if kept.len() == MAX_ANIMATION_FRAMES {
            return None;
        }
        let frame = frame.ok()?;
        let delay = match Duration::from(frame.delay()) {
            delay if delay < MIN_FRAME_DELAY => SLOWED_FRAME_DELAY,
            delay => delay,
        };
        let image = image::DynamicImage::ImageRgba8(frame.into_buffer());
        let (width, height) = MAX_KEPT;
        let image = if image.width() > width || image.height() > height {
            image.thumbnail(width, height)
        } else {
            image
        };
        let mut pixels = image.into_rgba8();
        bytes += u64::from(pixels.width()) * u64::from(pixels.height()) * 4;
        if bytes > MAX_ANIMATION_BYTES {
            return None;
        }
        for pixel in pixels.pixels_mut() {
            pixel.0.swap(0, 2);
        }
        loop_time += delay;
        kept.push(image::Frame::from_parts(
            pixels,
            0,
            0,
            image::Delay::from_saturating_duration(delay),
        ));
    }
    (kept.len() > 1).then(|| Animation {
        picture: Picture(Arc::new(gpui::RenderImage::new(kept))),
        loop_time,
    })
}

/// Longest edge of an image the viewer decodes: Air's clients send images
/// at most 4096 a side.
const MAX_FULL_EDGE: u32 = 4096;

/// Memory the viewer's decoder may take: 4096² RGBA is 64 MiB.
const MAX_FULL_ALLOC: u64 = 80 * 1024 * 1024;

/// The largest original the viewer reads.
const MAX_FULL_BYTES: usize = 32 * 1024 * 1024;

/// The viewer keeps an image at most this large (16 MiB as BGRA).
pub(crate) const MAX_FULL_KEPT: u32 = 2048;

/// Set while the viewer loads an image: one at a time, so repeated
/// requests (a held key) can't load and decode many large files at once.
static VIEWING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Holds [`VIEWING`] until dropped.
struct Viewing;

impl Viewing {
    fn take() -> Option<Self> {
        use std::sync::atomic::Ordering;
        VIEWING
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
            .then_some(Self)
    }
}

impl Drop for Viewing {
    fn drop(&mut self) {
        VIEWING.store(false, std::sync::atomic::Ordering::Release);
    }
}

/// The image of `message` (in `chat`) as large as the viewer shows it:
/// decoded from the downloaded original, under limits, one at a time.
/// Only an image the core made a preview of (or found small enough to be
/// its own): never the original of a failed preview, which the core's
/// decoder refused.
pub(super) async fn full_picture(
    user: CoreUser,
    chat: ChatId,
    message: MessageId,
) -> Result<Picture, Failure> {
    let viewing = Viewing::take().ok_or_else(|| {
        Failure::Shown("Another image is still opening; try again in a moment".into())
    })?;
    let id = attachment_in(&user, chat, message).await?;
    match user.attachment_thumbnail(id).await? {
        Some(AttachmentThumbnail::Ready { .. } | AttachmentThumbnail::OriginalFits) => {}
        Some(AttachmentThumbnail::Failed) | None => {
            return Err(Failure::Shown(
                "That file isn't an image Air can show".into(),
            ));
        }
    }
    let bytes = match user.load_attachment(id).await? {
        AttachmentContent::Ready(bytes)
        | AttachmentContent::Uploading(bytes)
        | AttachmentContent::UploadFailed(bytes) => bytes,
        AttachmentContent::Pending | AttachmentContent::Downloading => {
            return Err(Failure::Shown("Still downloading".into()));
        }
        _ => return Err(Failure::Shown("That image isn't on this device".into())),
    };
    if bytes.len() > MAX_FULL_BYTES {
        return Err(Failure::Shown("That image is too large to show".into()));
    }
    // The claim goes with the decode: the load may be aborted (the view
    // dropped it), a blocking decode can't be, and two may not run at once.
    let decoded = tokio::task::spawn_blocking(move || {
        let _viewing = viewing;
        decode_within(
            &bytes,
            MAX_FULL_EDGE,
            MAX_FULL_ALLOC,
            (MAX_FULL_KEPT, MAX_FULL_KEPT),
        )
    })
    .await
    .map_err(anyhow::Error::from)?;
    decoded.ok_or_else(|| Failure::Shown("That image can't be shown".into()))
}

/// The change a finished download is reported as: the attachment
/// updated. The core reports the download starting, not its content
/// landing (nor the preview made after).
pub(super) fn previewed(id: AttachmentId) -> Arc<DbNotification> {
    let mut notification = DbNotification::default();
    notification
        .ops
        .insert(DbEntityId::Attachment(id), DbOperation::Update.into());
    Arc::new(notification)
}

/// Downloads every pending attachment, as upstream: those left from before
/// (interrupted ones first reset) and each one that arrives, a few at a
/// time. After a download, makes the preview of an image and reports the
/// attachment through `changed` (the core notifies neither).
/// Runs until dropped; dropping it stops the downloads, which the next
/// start resumes.
pub(super) async fn download_all(user: CoreUser, changed: impl Fn(AttachmentId) + Send + Sync) {
    // Subscribe before listing, so no arrival is missed.
    let arrivals = user.db_notifications().flat_map(|notification| {
        let added: Vec<_> = notification
            .ops
            .iter()
            .filter_map(|(entity, ops)| match entity {
                DbEntityId::Attachment(id) if ops.contains(DbOperation::Add) => Some(*id),
                _ => None,
            })
            .collect();
        futures_util::stream::iter(added)
    });
    if let Err(err) = user.reset_interrupted_attachment_downloads().await {
        error!("failed to reset interrupted downloads: {err:#}");
    }
    let pending = user.pending_attachments().await.unwrap_or_else(|err| {
        error!("failed to list pending attachments: {err:#}");
        Vec::new()
    });
    info!("{} attachments to download", pending.len());
    let changed = &changed;
    let user = &user;
    futures_util::stream::iter(pending)
        .chain(arrivals)
        .for_each_concurrent(CONCURRENT_DOWNLOADS, |id| async move {
            if download(user, id, false).await == Downloaded::Yes {
                changed(id);
            }
        })
        .await;
}

#[derive(Debug, PartialEq)]
enum Downloaded {
    Yes,
    /// Already downloading.
    Busy,
    /// Not pending (or failed, when retrying): downloaded already, say.
    NotNeeded,
    Failed,
}

/// Downloads `id` if it is pending (or, `retry`, failed), unless it is
/// already downloading, then makes its preview.
async fn download(user: &CoreUser, id: AttachmentId, retry: bool) -> Downloaded {
    let Some(_claim) = Claim::take(id) else {
        return Downloaded::Busy;
    };
    match user.attachment_status(id).await {
        Ok(Some(AttachmentStatus::Pending)) => {}
        Ok(Some(AttachmentStatus::DownloadFailed)) if retry => {}
        // Stale while we hold the claim (a quit mid-download whose reset
        // didn't happen): nothing is fetching it, so fetch it.
        Ok(Some(AttachmentStatus::Downloading)) if retry => {}
        Ok(_) => return Downloaded::NotNeeded,
        Err(err) => {
            error!("failed to read an attachment's status: {err:#}");
            return Downloaded::Failed;
        }
    }
    let (_progress, task) = user.download_attachment(id);
    if let Err(err) = task.await {
        warn!("failed to download an attachment: {err:#}");
        return Downloaded::Failed;
    }
    make_preview(user, id, false).await;
    Downloaded::Yes
}

/// Makes the core's thumbnail for a downloaded image that has none yet:
/// a WebP one (the only image format Air sends), or, `declared_image`,
/// whatever the sender declared an image. The core stores a failure
/// too, so a file is never read for this twice.
async fn make_preview(user: &CoreUser, id: AttachmentId, declared_image: bool) {
    if !matches!(user.attachment_thumbnail(id).await, Ok(None)) {
        return;
    }
    let bytes = match user.load_attachment(id).await {
        Ok(AttachmentContent::Ready(bytes)) if declared_image || is_webp(&bytes) => bytes,
        Ok(_) => return,
        Err(err) => {
            error!("failed to load a downloaded attachment: {err:#}");
            return;
        }
    };
    if let Err(err) = user
        .generate_attachment_thumbnail(id, Arc::new(bytes))
        .await
    {
        error!("failed to make a thumbnail: {err:#}");
    }
}

fn is_webp(bytes: &[u8]) -> bool {
    bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP"
}

/// The attachment of `message`, if the message is in `chat`: the core
/// finds messages and attachments by id alone.
async fn attachment_in(
    user: &CoreUser,
    chat: ChatId,
    message: MessageId,
) -> Result<AttachmentId, Failure> {
    let found = user.message(message).await?;
    if found.is_none_or(|found| found.chat_id() != chat) {
        return Err(Failure::Shown("That message is gone".into()));
    }
    let infos = user.attachment_infos_for_message(message).await;
    match infos.first() {
        Some(info) => Ok(info.attachment_id),
        None => Err(Failure::Shown("That message has no file".into())),
    }
}

/// Downloads `message`'s attachment now, at the user's request: a failed
/// download again, or a pending one the download loop hasn't reached (it
/// can miss an arrival when changes pile up). Resolves to the attachment,
/// for the caller to report (see [`previewed`]).
pub(super) async fn retry(
    user: CoreUser,
    chat: ChatId,
    message: MessageId,
) -> Result<AttachmentId, Failure> {
    let id = attachment_in(&user, chat, message).await?;
    match download(&user, id, true).await {
        Downloaded::Yes => Ok(id),
        // Done meanwhile: reported all the same, so the view catches up.
        Downloaded::NotNeeded => match user.attachment_status(id).await? {
            Some(AttachmentStatus::Ready) => Ok(id),
            _ => Err(Failure::Shown("That file can't be downloaded".into())),
        },
        Downloaded::Busy => Err(Failure::Shown("Already downloading".into())),
        Downloaded::Failed => Err(Failure::Shown("The download failed".into())),
    }
}

/// Sends the file at `path` to `chat`. Returns once the message is stored
/// (shown as sending); the upload carries on detached, and a failed one
/// marks the message as not sent.
pub(super) async fn send(user: CoreUser, chat: ChatId, path: PathBuf) -> Result<(), Failure> {
    match std::fs::metadata(&path) {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => return Err(Failure::Shown(format!("{} isn't a file", path.display()))),
        Err(err) => {
            return Err(Failure::Shown(format!(
                "Can't read {}: {err}",
                path.display()
            )));
        }
    }
    let started = user
        .upload_chat_attachment(chat, &path, MarkChatAsRead::Yes)
        .await?;
    let (_, _progress, task) = started.map_err(|err| Failure::Shown(refused(err)))?;
    tokio::spawn(finish_upload(user, task, None));
    Ok(())
}

/// Sends again the file of `message` (in `chat`), whose upload failed, as
/// upstream's `retry_upload_attachment`. Resolves once the upload restarts;
/// it carries on detached, as a first one does.
pub(super) async fn retry_upload(
    user: CoreUser,
    chat: ChatId,
    message: MessageId,
) -> Result<(), Failure> {
    let id = attachment_in(&user, chat, message).await?;
    // One upload per attachment: a second retry would start it twice.
    let claim = Claim::take(id).ok_or_else(|| Failure::Shown("Already sending".into()))?;
    match user.attachment_status(id).await? {
        Some(AttachmentStatus::UploadFailed) => {}
        // Uploaded, only not sent yet: the outbound service's recovery
        // hands it over (as upstream).
        Some(AttachmentStatus::Ready) => return Ok(()),
        _ => return Err(Failure::Shown("That file isn't waiting to be sent".into())),
    }
    let Some((_progress, task)) = user.retry_upload_chat_attachment(id).await? else {
        // Nothing to send from: the core removed the message.
        return Err(Failure::Shown(
            "That file is no longer here; the message was removed".into(),
        ));
    };
    tokio::spawn(finish_upload(user, task, Some(claim)));
    Ok(())
}

/// Hands an upload's message to the outbound service once uploaded, as
/// upstream; marks it failed if the upload fails. `claim` is held until
/// then.
async fn finish_upload(
    user: CoreUser,
    task: impl Future<Output = Result<aircoreclient::ChatMessage, UploadTaskError>>,
    claim: Option<Claim>,
) {
    let _claim = claim;
    {
        let outbound = user.outbound_service();
        match task.await {
            Ok(message) => {
                if let Err(err) = outbound.enqueue_chat_message(message.id()).await {
                    error!("failed to queue an attachment message: {err:#}");
                }
            }
            Err(UploadTaskError::Provision(err)) => warn!("upload refused: {}", refused(err)),
            Err(UploadTaskError::Failed { message_id, error }) => {
                warn!("failed to send an attachment: {error:#}");
                if let Err(err) = outbound.fail_enqueued_chat_message(message_id).await {
                    error!("failed to mark an attachment message failed: {err:#}");
                }
            }
        }
    }
}

fn refused(err: ProvisionAttachmentError) -> String {
    match err {
        ProvisionAttachmentError::DecodingError => "That image can't be read".into(),
        ProvisionAttachmentError::TooLarge(detail) => format!(
            "That file is too large (at most {})",
            model::format_size(detail.max_size_bytes)
        ),
    }
}

/// Saves a copy of the attachment of `message` (in `chat`) in `dir` under
/// the sender's name made safe, never replacing a file. Returns where.
pub(super) async fn save(
    user: CoreUser,
    chat: ChatId,
    message: MessageId,
    name: String,
    dir: PathBuf,
) -> Result<PathBuf, Failure> {
    let id = attachment_in(&user, chat, message).await?;
    let bytes = match user.load_attachment(id).await? {
        AttachmentContent::Ready(bytes)
        | AttachmentContent::Uploading(bytes)
        | AttachmentContent::UploadFailed(bytes) => bytes,
        AttachmentContent::Pending | AttachmentContent::Downloading => {
            return Err(Failure::Shown("Still downloading".into()));
        }
        _ => return Err(Failure::Shown("That file isn't on this device".into())),
    };
    tokio::task::spawn_blocking(move || write_new(&dir, &model::file_name(&name), &bytes))
        .await
        .map_err(anyhow::Error::from)?
        .map_err(|err| Failure::Shown(format!("Couldn't save the file: {err}")))
}

/// Most numbered alternatives tried for a taken name.
const MAX_ALTERNATIVES: usize = 999;

/// Writes `bytes` to a new file in `dir` named `name`, or the first free
/// numbered alternative. Never follows or replaces what is there.
fn write_new(dir: &Path, name: &str, bytes: &[u8]) -> std::io::Result<PathBuf> {
    for n in 1..=MAX_ALTERNATIVES {
        let candidate = match n {
            1 => name.to_string(),
            n => model::numbered_file_name(name, n),
        };
        let path = dir.join(&candidate);
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        match options.open(&path) {
            Ok(mut file) => {
                let written = file.write_all(bytes).and_then(|()| file.sync_all());
                if let Err(err) = written {
                    // Don't leave half a file under the sender's name.
                    let _ = std::fs::remove_file(&path);
                    return Err(err);
                }
                return Ok(path);
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    }
    Err(std::io::Error::other("too many files with that name"))
}

/// Where saved files go: the user's downloads directory, else home.
pub(super) fn downloads_dir() -> Option<PathBuf> {
    let dirs = directories::UserDirs::new()?;
    Some(
        dirs.download_dir()
            .unwrap_or_else(|| dirs.home_dir())
            .to_path_buf(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn webp(width: u32, height: u32) -> Vec<u8> {
        let image = image::RgbaImage::from_pixel(width, height, image::Rgba([10, 20, 30, 255]));
        let mut bytes = Vec::new();
        image
            .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::WebP)
            .unwrap();
        bytes
    }

    /// A GIF of `frames` frames, each `delay_ms` long.
    fn gif(width: u32, height: u32, frames: usize, delay_ms: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut encoder = image::codecs::gif::GifEncoder::new(&mut bytes);
        let frames = (0..frames).map(|n| {
            let pixel = image::Rgba([(n % 256) as u8, 20, 30, 255]);
            image::Frame::from_parts(
                image::RgbaImage::from_pixel(width, height, pixel),
                0,
                0,
                image::Delay::from_numer_denom_ms(delay_ms, 1),
            )
        });
        encoder.encode_frames(frames).unwrap();
        drop(encoder);
        bytes
    }

    #[test]
    fn animations_decode_with_their_loop_time() {
        let animation = decode_animation(&gif(4, 4, 3, 50)).unwrap();
        assert_eq!(animation.picture.0.frame_count(), 3);
        assert_eq!(animation.loop_time, Duration::from_millis(150));
    }

    #[test]
    fn a_peers_animation_is_bounded() {
        // The canvas, the frame count, and the total all have a limit:
        // past any, the image stays still.
        assert!(decode_animation(&gif(MAX_PREVIEW_EDGE + 1, 4, 2, 50)).is_none());
        assert!(decode_animation(&gif(1, 1, MAX_ANIMATION_FRAMES, 50)).is_some());
        assert!(decode_animation(&gif(1, 1, MAX_ANIMATION_FRAMES + 1, 50)).is_none());
        let (width, height) = MAX_KEPT;
        let frame = u64::from(width * height * 4);
        let too_many = (MAX_ANIMATION_BYTES / frame + 1) as usize;
        assert!(too_many < MAX_ANIMATION_FRAMES);
        assert!(decode_animation(&gif(width, height, too_many, 50)).is_none());
        // Kept no larger than a preview.
        let large = decode_animation(&gif(1000, 1000, 2, 50)).unwrap();
        assert!(large.picture.size().0 <= width && large.picture.size().1 <= height);
    }

    #[test]
    fn a_zero_delay_never_redraws_every_frame() {
        let animation = decode_animation(&gif(1, 1, 2, 0)).unwrap();
        assert_eq!(animation.loop_time, SLOWED_FRAME_DELAY * 2);
    }

    #[test]
    fn still_or_broken_images_are_no_animation() {
        assert!(decode_animation(&gif(4, 4, 1, 50)).is_none());
        assert!(decode_animation(&webp(4, 4)).is_none());
        let mut truncated = gif(4, 4, 5, 50);
        truncated.truncate(truncated.len() / 2);
        assert!(decode_animation(&truncated).is_none());
        assert!(decode_animation(b"not an image").is_none());
    }

    fn part(disposition: Disposition, url: &str, filename: &str) -> NestedPart {
        NestedPart::ExternalPart {
            disposition,
            language: String::new(),
            content_type: "application/octet-stream".into(),
            url: url.into(),
            expires: 0,
            size: 8,
            enc_alg: mimi_content::content_container::EncryptionAlgorithm::None,
            key: Vec::new(),
            nonce: Vec::new(),
            aad: Vec::new(),
            hash_alg: Default::default(),
            content_hash: Vec::new(),
            description: String::new(),
            filename: filename.into(),
        }
    }

    fn content(disposition: Disposition, parts: Vec<NestedPart>) -> MimiContent {
        MimiContent {
            nested_part: NestedPart::MultiPart {
                disposition,
                language: String::new(),
                part_semantics: mimi_content::content_container::PartSemantics::ProcessAll,
                parts,
            },
            ..Default::default()
        }
    }

    #[test]
    fn only_a_well_formed_attachment_part_is_declared() {
        let url = "air:///attachment/9b2d2b6e-3f6c-4d2a-8a4e-0c1d2e3f4a5b";
        let image_url = format!("{url}?width=40&height=30");
        let declared = declared(&content(
            Disposition::Attachment,
            vec![
                part(Disposition::Attachment, "https://evil.example/x", "first"),
                part(Disposition::Attachment, &image_url, "\u{202E}\u{200B}"),
            ],
        ))
        .expect("the valid part counts");
        // The malformed part is skipped; a blank name is still a name.
        assert_eq!(declared.name, "file");
        assert!(declared.image);

        let plain = declared_of(Disposition::Attachment, url);
        assert!(!plain.unwrap().image);
        // Not an attachment message, or no attachment part in it.
        assert!(declared_of(Disposition::Render, url).is_none());
        let inline = content(
            Disposition::Attachment,
            vec![part(Disposition::Inline, url, "x")],
        );
        assert!(super::declared(&inline).is_none());
    }

    fn declared_of(disposition: Disposition, url: &str) -> Option<Declared> {
        declared(&content(
            disposition,
            vec![part(Disposition::Attachment, url, "a.txt")],
        ))
    }

    #[test]
    fn one_download_per_attachment_at_a_time() {
        let id = AttachmentId {
            uuid: uuid::Uuid::new_v4(),
        };
        let claim = Claim::take(id).expect("free");
        // On a thread, so a deadlock fails the test instead of hanging it.
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || tx.send(Claim::take(id).is_none()));
        let refused = rx.recv_timeout(std::time::Duration::from_secs(5));
        assert_eq!(refused, Ok(true), "a taken attachment is refused, promptly");
        drop(claim);
        assert!(Claim::take(id).is_some());
    }

    #[test]
    fn previews_are_decoded_only_within_bounds() {
        let picture = decode(&webp(3, 2)).expect("a small preview is drawn");
        assert_eq!(picture.size(), (3, 2));
        // gpui draws BGRA.
        let bytes = picture.0.as_bytes(0).unwrap();
        assert_eq!(&bytes[..4], &[30, 20, 10, 255]);

        // What the core's decoder passed, ours still bounds; what it
        // keeps is no larger than drawn.
        assert!(decode(&webp(MAX_PREVIEW_EDGE + 1, 1)).is_none());
        let kept = decode(&webp(MAX_PREVIEW_EDGE, MAX_PREVIEW_EDGE)).unwrap();
        assert_eq!(kept.size(), (MAX_KEPT.1, MAX_KEPT.1));
        // Not WebP, or not an image at all.
        let mut png = Vec::new();
        image::RgbaImage::new(2, 2)
            .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        assert!(decode(&png).is_none());
        assert!(decode(b"RIFF\0\0\0\0WEBPjunk").is_none());
    }

    #[test]
    fn the_viewer_opens_one_image_at_a_time() {
        let first = Viewing::take().expect("free");
        assert!(Viewing::take().is_none(), "a second load waits its turn");
        drop(first);
        assert!(Viewing::take().is_some());
    }

    #[test]
    fn the_viewer_decodes_larger_but_still_bounded() {
        let full = |bytes: &[u8]| {
            decode_within(
                bytes,
                MAX_FULL_EDGE,
                MAX_FULL_ALLOC,
                (MAX_FULL_KEPT, MAX_FULL_KEPT),
            )
        };
        // Past a preview's bounds, within the viewer's: shown, kept smaller.
        let wide = webp(MAX_FULL_KEPT + 100, 10);
        assert!(decode(&wide).is_none());
        assert_eq!(full(&wide).unwrap().size().0, MAX_FULL_KEPT);
        assert!(full(&webp(MAX_FULL_EDGE + 1, 1)).is_none());
    }

    #[test]
    fn saving_never_replaces_a_file_nor_leaves_the_directory() {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside.txt");
        std::fs::write(&outside, "mine").unwrap();
        let inner = dir.path().join("downloads");
        std::fs::create_dir(&inner).unwrap();
        std::fs::write(inner.join("a.txt"), "mine").unwrap();

        let saved = write_new(&inner, &model::file_name("a.txt"), b"new").unwrap();
        assert_eq!(saved, inner.join("a (2).txt"));
        assert_eq!(std::fs::read(inner.join("a.txt")).unwrap(), b"mine");

        for hostile in [
            "../outside.txt",
            "/etc/passwd",
            "..",
            ".bashrc",
            "a\u{202E}txt.exe",
        ] {
            let saved = write_new(&inner, &model::file_name(hostile), b"x").unwrap();
            assert_eq!(saved.parent(), Some(inner.as_path()), "{hostile}");
            let name = saved.file_name().unwrap().to_str().unwrap();
            assert!(
                !name.starts_with('.') && !name.contains('\u{202E}'),
                "{name}"
            );
        }
        assert_eq!(std::fs::read(&outside).unwrap(), b"mine");

        // A symlink planted under the name isn't followed.
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside, inner.join("link.txt")).unwrap();
            let saved = write_new(&inner, "link.txt", b"x").unwrap();
            assert_eq!(saved, inner.join("link (2).txt"));
            assert_eq!(std::fs::read(&outside).unwrap(), b"mine");
        }
    }
}
