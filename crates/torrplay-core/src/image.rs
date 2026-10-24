// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! Images clients upload, a torrent's poster and a profile's avatar: the
//! types they may have and what a record keeps of them.

use std::time::SystemTime;

use sha2::{Digest, Sha256};

/// The largest image, in bytes.
pub const IMAGE_MAX_BYTES: usize = 1 << 20;

/// The types an uploaded image may have.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ImageType {
    /// AVIF.
    Avif,
    /// GIF.
    Gif,
    /// JPEG.
    Jpeg,
    /// PNG.
    Png,
    /// WebP.
    Webp,
}

impl ImageType {
    /// Every image type.
    pub const ALL: [Self; 5] = [Self::Avif, Self::Gif, Self::Jpeg, Self::Png, Self::Webp];

    /// Returns the media type.
    pub const fn media_type(self) -> &'static str {
        match self {
            Self::Avif => "image/avif",
            Self::Gif => "image/gif",
            Self::Jpeg => "image/jpeg",
            Self::Png => "image/png",
            Self::Webp => "image/webp",
        }
    }

    /// Parses a media type's essence, ignoring ASCII case.
    pub fn parse(media_type: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|kind| kind.media_type().eq_ignore_ascii_case(media_type))
    }

    /// Returns whether `bytes` begin with this type's signature.
    pub fn matches(self, bytes: &[u8]) -> bool {
        match self {
            // An ISO base media file whose brand is AVIF.
            Self::Avif => {
                bytes.get(4..8) == Some(b"ftyp")
                    && matches!(bytes.get(8..12), Some(b"avif" | b"avis"))
            }
            Self::Gif => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
            Self::Jpeg => bytes.starts_with(&[0xff, 0xd8, 0xff]),
            Self::Png => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
            Self::Webp => bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP"),
        }
    }
}

/// What a record knows of an uploaded image, without its bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageInfo {
    /// The image type.
    pub image_type: ImageType,
    /// The image's size.
    pub size_bytes: u64,
    /// The SHA-256 digest of the image, which its entity tag derives from.
    pub digest: [u8; 32],
    /// When it was uploaded.
    pub updated_at: SystemTime,
}

impl ImageInfo {
    /// Describes an image of `image_type` uploaded at `updated_at`.
    pub fn of(image_type: ImageType, bytes: &[u8], updated_at: SystemTime) -> Self {
        Self {
            image_type,
            size_bytes: bytes.len() as u64,
            digest: Sha256::digest(bytes).into(),
            updated_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn images_must_match_their_type() {
        let png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";
        let jpeg = [0xff, 0xd8, 0xff, 0xe0];
        let avif = b"\0\0\0\x20ftypavif\0\0\0\0";
        let webp = b"RIFF\0\0\0\0WEBPVP8 ";
        assert!(ImageType::Png.matches(png));
        assert!(!ImageType::Png.matches(&jpeg));
        assert!(ImageType::Jpeg.matches(&jpeg));
        assert!(ImageType::Gif.matches(b"GIF89a\x01\0"));
        assert!(ImageType::Avif.matches(avif));
        assert!(!ImageType::Avif.matches(b"\0\0\0\x20ftypmp42\0\0\0\0"));
        assert!(ImageType::Webp.matches(webp));
        assert!(!ImageType::Webp.matches(b"RIFF\0\0\0\0WAVE"));
        assert_eq!(ImageType::parse("IMAGE/PNG"), Some(ImageType::Png));
        assert_eq!(ImageType::parse("image/svg+xml"), None);
    }
}
