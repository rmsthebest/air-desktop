//! Profile and group pictures ("avatars"). A contact's picture is theirs to
//! choose and arrives as raw bytes in their profile; a group's comes with
//! the group. Air's clients send them as JPEG the core resized to at most
//! 256² (`resize_profile_image`), so only that is drawn, decoded here under
//! limits as attachment previews are: a peer chooses the bytes.
//!
//! Workers keep each chat's (or member's) picture with a keyed hash of its
//! bytes: a reload with the same bytes reuses the decoded picture, so a new
//! message doesn't decode the chat's picture again, nor give gpui a new
//! texture to upload.

use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasher, Hash, RandomState};
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::Arc;

use tracing::{debug, error};

use crate::model::Picture;
use crate::session::contacts::Failure;

/// The largest picture read: a 256² JPEG is a few dozen KiB.
const MAX_BYTES: usize = 512 * 1024;

/// Longest edge decoded: well beyond what the core sends.
const MAX_EDGE: u32 = 1024;

/// Memory the decoder may take: a 1024² RGBA image is 4 MiB.
const MAX_DECODE_ALLOC: u64 = 8 * 1024 * 1024;

/// A decoded picture is kept at most this large: the largest avatar (the
/// details pane's, 72 px) at about twice the pixels (64 KiB as BGRA).
pub(crate) const MAX_KEPT: u32 = 128;

/// Decodes a JPEG avatar into the BGRA frame gpui draws, or `None` (drawn
/// as initials). Off the async threads, and a decoder panic is a `None`.
pub(super) async fn decode(bytes: Vec<u8>) -> Option<Picture> {
    if bytes.len() > MAX_BYTES {
        debug!("not drawing a picture of {} bytes", bytes.len());
        return None;
    }
    match tokio::task::spawn_blocking(move || decode_now(&bytes)).await {
        Ok(picture) => picture,
        Err(err) => {
            error!("picture decode panicked: {err}");
            None
        }
    }
}

fn decode_now(bytes: &[u8]) -> Option<Picture> {
    let mut reader = image::ImageReader::with_format(Cursor::new(bytes), image::ImageFormat::Jpeg);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_EDGE);
    limits.max_image_height = Some(MAX_EDGE);
    limits.max_alloc = Some(MAX_DECODE_ALLOC);
    reader.limits(limits);
    let image = match reader.decode() {
        Ok(image) => image,
        Err(err) => {
            debug!("not drawing a picture: {err}");
            return None;
        }
    };
    let image = if image.width() > MAX_KEPT || image.height() > MAX_KEPT {
        image.thumbnail(MAX_KEPT, MAX_KEPT)
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

/// Drops the GPU textures of `old` pictures that aren't among `new`: a
/// replaced or removed picture's texture would otherwise stay uploaded.
pub(super) fn drop_unused<'a>(
    old: impl Iterator<Item = &'a Picture>,
    new: impl Iterator<Item = &'a Picture>,
    cx: &mut gpui::App,
) {
    let kept: HashSet<_> = new.map(|picture| picture.0.id).collect();
    for picture in old {
        if !kept.contains(&picture.0.id) {
            cx.drop_image(picture.0.clone(), None);
        }
    }
}

/// Pictures decoded so far, by whose they are (a chat, a user).
pub(super) struct Avatars<K> {
    /// Keyed per process: nobody can craft bytes that collide with another
    /// picture's to keep a stale one up.
    hasher: RandomState,
    kept: HashMap<K, (u64, Option<Picture>)>,
}

impl<K> Default for Avatars<K> {
    fn default() -> Self {
        Self {
            hasher: RandomState::new(),
            kept: HashMap::new(),
        }
    }
}

impl<K: Hash + Eq + Clone> Avatars<K> {
    /// `key`'s picture from `bytes`: the one kept if the bytes are the
    /// same, else decoded anew (and kept). No bytes, no picture.
    pub(super) async fn get(&mut self, key: &K, bytes: Option<Vec<u8>>) -> Option<Picture> {
        let Some(bytes) = bytes else {
            self.kept.remove(key);
            return None;
        };
        let hash = self.hasher.hash_one(&bytes);
        if let Some((kept, picture)) = self.kept.get(key)
            && *kept == hash
        {
            return picture.clone();
        }
        let picture = decode(bytes).await;
        self.kept.insert(key.clone(), (hash, picture.clone()));
        picture
    }

    /// Forgets the pictures of all but what `keep` keeps.
    pub(super) fn retain(&mut self, mut keep: impl FnMut(&K) -> bool) {
        self.kept.retain(|key, _| keep(key));
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.kept.len()
    }
}

/// The largest file read as a new picture of the user's: the core decodes
/// and resizes it.
const MAX_SOURCE_BYTES: u64 = 16 * 1024 * 1024;

/// The longest edge of a picture the user picks: the core decodes it whole
/// before resizing, without limits of its own.
const MAX_SOURCE_EDGE: u32 = 4096;

/// Memory a trial decode of the user's picture may take (4096² RGBA is
/// 64 MiB).
const MAX_SOURCE_ALLOC: u64 = 80 * 1024 * 1024;

/// Reads the picture the user picked and decodes it once, under limits:
/// the core resizes it without limits of its own, and for a group drops a
/// picture it can't read, which would remove the group's current one.
/// Errors are for the user.
pub(super) async fn read_source(path: PathBuf) -> Result<Vec<u8>, Failure> {
    let read = tokio::task::spawn_blocking(move || -> Result<Vec<u8>, String> {
        let size = std::fs::metadata(&path)
            .map_err(|_| "Couldn't read that file".to_string())?
            .len();
        if size > MAX_SOURCE_BYTES {
            return Err("That picture is too large (16 MB at most)".into());
        }
        let bytes = std::fs::read(&path).map_err(|_| "Couldn't read that file".to_string())?;
        check_source(&bytes)?;
        Ok(bytes)
    });
    match read.await {
        Ok(read) => read.map_err(Failure::Shown),
        Err(err) => {
            error!("picture check panicked: {err}");
            Err(Failure::Shown("Couldn't read that picture".into()))
        }
    }
}

fn check_source(bytes: &[u8]) -> Result<(), String> {
    let not_a_picture = || "That file isn't a picture (JPEG, PNG, WebP or GIF)".to_string();
    let format = image::guess_format(bytes).map_err(|_| not_a_picture())?;
    if !matches!(
        format,
        image::ImageFormat::Jpeg
            | image::ImageFormat::Png
            | image::ImageFormat::WebP
            | image::ImageFormat::Gif
    ) {
        return Err(not_a_picture());
    }
    let (width, height) = image::ImageReader::with_format(Cursor::new(bytes), format)
        .into_dimensions()
        .map_err(|_| not_a_picture())?;
    if width > MAX_SOURCE_EDGE || height > MAX_SOURCE_EDGE {
        return Err(format!(
            "That picture is too large ({MAX_SOURCE_EDGE} pixels a side at most)"
        ));
    }
    let mut reader = image::ImageReader::with_format(Cursor::new(bytes), format);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_SOURCE_EDGE);
    limits.max_image_height = Some(MAX_SOURCE_EDGE);
    limits.max_alloc = Some(MAX_SOURCE_ALLOC);
    reader.limits(limits);
    reader
        .decode()
        .map_err(|_| "Couldn't read that picture".to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encoded(width: u32, height: u32, format: image::ImageFormat) -> Vec<u8> {
        let image = image::RgbImage::from_pixel(width, height, image::Rgb([200, 100, 10]));
        let mut bytes = Vec::new();
        image
            .write_to(&mut Cursor::new(&mut bytes), format)
            .unwrap();
        bytes
    }

    fn jpeg(width: u32, height: u32) -> Vec<u8> {
        encoded(width, height, image::ImageFormat::Jpeg)
    }

    #[test]
    fn pictures_are_decoded_only_within_bounds() {
        let picture = decode_now(&jpeg(4, 2)).expect("a small JPEG is drawn");
        assert_eq!(picture.size(), (4, 2));
        // gpui draws BGRA (JPEG is lossy: roughly the colour put in).
        let bytes = picture.0.as_bytes(0).unwrap();
        assert!(bytes[0] < 60 && bytes[2] > 150, "{:?}", &bytes[..4]);
        // Kept no larger than drawn.
        let kept = decode_now(&jpeg(256, 256)).unwrap();
        assert_eq!(kept.size(), (MAX_KEPT, MAX_KEPT));
        // Past the edge, not at all.
        assert!(decode_now(&jpeg(MAX_EDGE + 1, 1)).is_none());
        // Only JPEG, and only a whole one.
        assert!(decode_now(&encoded(2, 2, image::ImageFormat::Png)).is_none());
        let mut cut = jpeg(64, 64);
        cut.truncate(cut.len() / 2);
        assert!(decode_now(&cut).is_none());
    }

    #[tokio::test]
    async fn oversized_bytes_are_not_decoded() {
        let mut big = jpeg(2, 2);
        big.resize(MAX_BYTES + 1, 0);
        assert!(decode(big).await.is_none());
    }

    #[tokio::test]
    async fn the_same_bytes_reuse_the_decoded_picture() {
        let mut avatars = Avatars::default();
        let a = avatars.get(&1, Some(jpeg(2, 2))).await.unwrap();
        // Same bytes: the same picture (one texture), not a new decode.
        assert_eq!(avatars.get(&1, Some(jpeg(2, 2))).await, Some(a.clone()));
        // New bytes: a new picture.
        let b = avatars.get(&1, Some(jpeg(3, 3))).await.unwrap();
        assert_ne!(a, b);
        assert_eq!(b.size(), (3, 3));
        // Removed: forgotten.
        assert_eq!(avatars.get(&1, None).await, None);
        assert_eq!(avatars.len(), 0);
        avatars.get(&2, Some(jpeg(2, 2))).await;
        avatars.get(&3, Some(jpeg(2, 2))).await;
        avatars.retain(|key| *key == 3);
        assert_eq!(avatars.len(), 1);
    }

    #[test]
    fn a_new_picture_must_be_a_bounded_image() {
        assert!(check_source(&jpeg(8, 8)).is_ok());
        assert!(check_source(&encoded(8, 8, image::ImageFormat::Png)).is_ok());
        assert!(check_source(b"not a picture at all").is_err());
        assert!(check_source(&encoded(8, 8, image::ImageFormat::Bmp)).is_err());
        let wide = encoded(MAX_SOURCE_EDGE + 1, 1, image::ImageFormat::Png);
        assert_eq!(
            check_source(&wide),
            Err("That picture is too large (4096 pixels a side at most)".into())
        );
        // A good header on a broken body: the core would fail on it (for
        // a group, silently removing its picture), so it stops here.
        let mut cut = encoded(64, 64, image::ImageFormat::Png);
        cut.truncate(cut.len() / 2);
        assert_eq!(check_source(&cut), Err("Couldn't read that picture".into()));
    }
}
