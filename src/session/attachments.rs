//! Attachments: what a message's attachment part says, the image preview
//! the view draws, and the downloads. Port of upstream's
//! `attachments_repository` (downloads, thumbnails) and the attachment half
//! of `message_content.rs`.
//!
//! Received files are only ever stored by the core and, on request, copied
//! to the downloads directory: never opened or handed to another program.
//! Images are drawn only from a preview the core made (or checked to be a
//! small WebP), decoded here under limits.

use std::io::{Cursor, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use aircoreclient::clients::CoreUser;
use aircoreclient::db::notification::{DbEntityId, DbNotification, DbOperation};
use aircoreclient::{
    AttachmentContent, AttachmentId, AttachmentStatus, AttachmentThumbnail, AttachmentUrl, ChatId,
    MarkChatAsRead, MessageId, ProvisionAttachmentError, UploadTaskError,
};
use futures_util::StreamExt as _;
use gpui::SharedString;
use mimi_content::MimiContent;
use mimi_content::content_container::{Disposition, NestedPart};
use tracing::{debug, error, info, warn};

use crate::model::{self, Attachment, AttachmentState, Picture};
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

/// What a message's attachment part declares. All of it is the sender's
/// word.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Declared {
    pub(super) name: SharedString,
    pub(super) description: Option<SharedString>,
    pub(super) size: u64,
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
    parts.iter().find_map(|part| match part {
        NestedPart::ExternalPart {
            disposition: Disposition::Attachment,
            url,
            description,
            filename,
            size,
            ..
        } if url.parse::<AttachmentUrl>().is_ok() => Some(Declared {
            name: match model::preview_line(filename) {
                name if name.is_empty() => "file".into(),
                name => name,
            },
            description: Some(model::preview_line(description)).filter(|d| !d.is_empty()),
            size: *size,
        }),
        _ => None,
    })
}

/// The message's attachment as the view shows it. `None` when the core has
/// no attachment for the message.
pub(super) async fn load(
    user: &CoreUser,
    message: MessageId,
    declared: Declared,
    cached: impl Fn(AttachmentId) -> Option<Picture>,
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
            None => preview(user, id).await,
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
async fn preview(user: &CoreUser, id: AttachmentId) -> Option<Picture> {
    let bytes = match user.attachment_thumbnail(id).await {
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
/// anything larger than a preview can be. Our own decoder, with its own
/// limits: the core's checks were made by another decoder (libwebp), which
/// could read a crafted file differently.
pub(crate) fn decode(bytes: &[u8]) -> Option<Picture> {
    let mut reader = image::ImageReader::with_format(Cursor::new(bytes), image::ImageFormat::WebP);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_PREVIEW_EDGE);
    limits.max_image_height = Some(MAX_PREVIEW_EDGE);
    limits.max_alloc = Some(MAX_DECODE_ALLOC);
    reader.limits(limits);
    let image = match reader.decode() {
        Ok(image) => image,
        Err(err) => {
            debug!("not drawing a preview: {err}");
            return None;
        }
    };
    let mut pixels = image.into_rgba8();
    for pixel in pixels.pixels_mut() {
        pixel.0.swap(0, 2);
    }
    let frame = image::Frame::new(pixels);
    Some(Picture(Arc::new(gpui::RenderImage::new(vec![frame]))))
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
            if download(user, id).await {
                changed(id);
            }
        })
        .await;
}

/// Downloads `id` if it is pending, then makes its preview. Whether it
/// was downloaded.
async fn download(user: &CoreUser, id: AttachmentId) -> bool {
    match user.attachment_status(id).await {
        Ok(Some(AttachmentStatus::Pending)) => {}
        Ok(_) => return false,
        Err(err) => {
            error!("failed to read an attachment's status: {err:#}");
            return false;
        }
    }
    let (_progress, task) = user.download_attachment(id);
    if let Err(err) = task.await {
        warn!("failed to download an attachment: {err:#}");
        return false;
    }
    make_preview(user, id).await;
    true
}

/// Makes the core's thumbnail for a downloaded WebP image (the only image
/// format Air sends) that has none yet.
async fn make_preview(user: &CoreUser, id: AttachmentId) {
    if !matches!(user.attachment_thumbnail(id).await, Ok(None)) {
        return;
    }
    let bytes = match user.load_attachment(id).await {
        Ok(AttachmentContent::Ready(bytes)) if is_webp(&bytes) => bytes,
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

/// Retries the failed download of `message`'s attachment, at the user's
/// request. Resolves to the attachment, for the caller to report (see
/// [`previewed`]).
pub(super) async fn retry(
    user: CoreUser,
    chat: ChatId,
    message: MessageId,
) -> Result<AttachmentId, Failure> {
    let id = attachment_in(&user, chat, message).await?;
    match user.attachment_status(id).await? {
        Some(AttachmentStatus::DownloadFailed) => {}
        _ => return Err(Failure::Shown("Nothing to retry".into())),
    }
    // A failed download keeps what it needs to start over.
    let (_progress, task) = user.download_attachment(id);
    task.await
        .map_err(|_| Failure::Shown("The download failed again".into()))?;
    make_preview(&user, id).await;
    Ok(id)
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
    tokio::spawn(async move {
        match task.await {
            Ok(_) => {}
            Err(UploadTaskError::Provision(err)) => warn!("upload refused: {}", refused(err)),
            Err(UploadTaskError::Failed { error, .. }) => {
                warn!("failed to send an attachment: {error:#}")
            }
        }
    });
    Ok(())
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

    #[test]
    fn previews_are_decoded_only_within_bounds() {
        let picture = decode(&webp(3, 2)).expect("a small preview is drawn");
        assert_eq!(picture.size(), (3, 2));
        // gpui draws BGRA.
        let bytes = picture.0.as_bytes(0).unwrap();
        assert_eq!(&bytes[..4], &[30, 20, 10, 255]);

        // What the core's decoder passed, ours still bounds.
        assert!(decode(&webp(MAX_PREVIEW_EDGE + 1, 1)).is_none());
        // Not WebP, or not an image at all.
        let mut png = Vec::new();
        image::RgbaImage::new(2, 2)
            .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        assert!(decode(&png).is_none());
        assert!(decode(b"RIFF\0\0\0\0WEBPjunk").is_none());
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
