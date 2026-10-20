// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! File content: media types, `Content-Disposition`, and byte ranges, as
//! the API conventions' Content resources section defines them.

use torrplay_core::engine::ByteRange;

/// The media type of files served for download.
pub(crate) const OCTET_STREAM: &str = "application/octet-stream";

/// The file extensions served inline, with their media types: audio,
/// video, raster images, and WebVTT. Everything else, including HTML and
/// SVG, is a download, because a browser would run its scripts on the API's
/// origin.
const INLINE_TYPES: &[(&str, &str)] = &[
    ("aac", "audio/aac"),
    ("avi", "video/x-msvideo"),
    ("avif", "image/avif"),
    ("bmp", "image/bmp"),
    ("flac", "audio/flac"),
    ("gif", "image/gif"),
    ("jpeg", "image/jpeg"),
    ("jpg", "image/jpeg"),
    ("m4a", "audio/mp4"),
    ("m4v", "video/mp4"),
    ("mka", "audio/x-matroska"),
    ("mkv", "video/x-matroska"),
    ("mov", "video/quicktime"),
    ("mp3", "audio/mpeg"),
    ("mp4", "video/mp4"),
    ("oga", "audio/ogg"),
    ("ogg", "audio/ogg"),
    ("ogv", "video/ogg"),
    ("opus", "audio/opus"),
    ("png", "image/png"),
    ("ts", "video/mp2t"),
    ("vtt", "text/vtt"),
    ("wav", "audio/wav"),
    ("weba", "audio/webm"),
    ("webm", "video/webm"),
    ("webp", "image/webp"),
];

/// Returns the media type a file is served with, from its name's
/// extension, and whether it is served inline.
pub(crate) fn media_type(file_name: &str) -> (&'static str, bool) {
    let extension = file_name
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase());
    extension
        .and_then(|extension| {
            INLINE_TYPES
                .iter()
                .find(|(known, _)| *known == extension)
                .map(|(_, media_type)| (*media_type, true))
        })
        .unwrap_or((OCTET_STREAM, false))
}

/// Returns the `Content-Disposition` value for a file, with its name
/// encoded as RFC 6266 specifies: a quoted ASCII fallback, plus `filename*`
/// in UTF-8 when the name has characters the fallback cannot carry.
pub(crate) fn disposition(file_name: &str, inline: bool) -> String {
    let kind = if inline { "inline" } else { "attachment" };
    let plain = |character: char| {
        character.is_ascii_graphic() && character != '"' && character != '\\' || character == ' '
    };
    if file_name.chars().all(plain) {
        return format!("{kind}; filename=\"{file_name}\"");
    }
    let fallback: String = file_name
        .chars()
        .map(|character| if plain(character) { character } else { '_' })
        .collect();
    let mut encoded = String::new();
    for byte in file_name.bytes() {
        // RFC 8187's attr-char.
        if byte.is_ascii_alphanumeric() || b"!#$&+-.^_`|~".contains(&byte) {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    format!("{kind}; filename=\"{fallback}\"; filename*=UTF-8''{encoded}")
}

/// What a `Range` header asks of a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RangeRequest {
    /// The whole file: no `Range`, one the server ignores, or one that asks
    /// for more than one range.
    Full,
    /// One satisfiable range.
    Partial(ByteRange),
    /// One range that does not overlap the file.
    Unsatisfiable,
}

/// Interprets a `Range` value for a file of `length` bytes. A value that is
/// not a valid `bytes` range set is ignored, as RFC 9110 permits, and so is
/// one with more than one range, because the server does not produce
/// `multipart/byteranges`.
pub(crate) fn range_request(value: &str, length: u64) -> RangeRequest {
    let Some((unit, set)) = value.split_once('=') else {
        return RangeRequest::Full;
    };
    if !unit.trim().eq_ignore_ascii_case("bytes") {
        return RangeRequest::Full;
    }
    let specs: Vec<&str> = set
        .split(',')
        .map(str::trim)
        .filter(|spec| !spec.is_empty())
        .collect();
    let [spec] = specs.as_slice() else {
        return RangeRequest::Full;
    };
    let Some((first, last)) = spec.split_once('-') else {
        return RangeRequest::Full;
    };
    let number = |text: &str| {
        (!text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()))
            .then(|| text.parse::<u64>().ok())
            .flatten()
    };
    let (first, last) = (first.trim(), last.trim());
    let range = if first.is_empty() {
        // A suffix: the last `suffix` bytes.
        let Some(suffix) = number(last) else {
            return RangeRequest::Full;
        };
        if suffix == 0 || length == 0 {
            return RangeRequest::Unsatisfiable;
        }
        ByteRange::new(length - suffix.min(length), length)
    } else {
        let Some(first) = number(first) else {
            return RangeRequest::Full;
        };
        let last = if last.is_empty() {
            None
        } else {
            match number(last) {
                Some(last) if last >= first => Some(last),
                // Digits that overflow or end before they start make the
                // range invalid, so it is ignored.
                _ => return RangeRequest::Full,
            }
        };
        if first >= length {
            return RangeRequest::Unsatisfiable;
        }
        let end = last.map_or(length, |last| last.min(length - 1) + 1);
        ByteRange::new(first, end)
    };
    range.map_or(RangeRequest::Full, RangeRequest::Partial)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn partial(start: u64, end: u64) -> RangeRequest {
        RangeRequest::Partial(ByteRange::new(start, end).unwrap())
    }

    #[test]
    fn single_ranges_resolve_against_the_length() {
        let cases = [
            ("bytes=0-99", partial(0, 100)),
            ("bytes=10-", partial(10, 1000)),
            ("bytes=-100", partial(900, 1000)),
            ("bytes=-5000", partial(0, 1000)),
            ("bytes=999-999", partial(999, 1000)),
            ("bytes=0-0", partial(0, 1)),
            ("bytes=500-5000", partial(500, 1000)),
            ("Bytes = 1-2", partial(1, 3)),
            ("bytes=1000-", RangeRequest::Unsatisfiable),
            ("bytes=1000-2000", RangeRequest::Unsatisfiable),
            ("bytes=-0", RangeRequest::Unsatisfiable),
        ];
        for (value, expected) in cases {
            assert_eq!(range_request(value, 1000), expected, "{value}");
        }
    }

    #[test]
    fn multiple_and_invalid_ranges_are_ignored() {
        for value in [
            "bytes=0-1,5-6",
            "bytes=0-1, 2000-3000",
            "bytes=5-1",
            "bytes=a-b",
            "bytes=1",
            "items=0-1",
            "bytes=-",
            "bytes=99999999999999999999-",
            "bytes=+1-2",
        ] {
            assert_eq!(range_request(value, 1000), RangeRequest::Full, "{value}");
        }
    }

    #[test]
    fn every_range_of_an_empty_file_is_unsatisfiable() {
        for value in ["bytes=0-", "bytes=-1", "bytes=0-0"] {
            assert_eq!(
                range_request(value, 0),
                RangeRequest::Unsatisfiable,
                "{value}"
            );
        }
    }

    #[test]
    fn only_media_and_subtitles_are_inline() {
        let cases = [
            ("video.mkv", "video/x-matroska", true),
            ("video.MP4", "video/mp4", true),
            ("audio.mp3", "audio/mpeg", true),
            ("subtitles.vtt", "text/vtt", true),
            ("image.png", "image/png", true),
            ("page.html", OCTET_STREAM, false),
            ("image.svg", OCTET_STREAM, false),
            ("program.exe", OCTET_STREAM, false),
            ("README", OCTET_STREAM, false),
        ];
        for (name, media, inline) in cases {
            assert_eq!(media_type(name), (media, inline), "{name}");
        }
    }

    #[test]
    fn dispositions_encode_names_the_quoted_form_cannot_carry() {
        assert_eq!(
            disposition("part-1.mkv", true),
            "inline; filename=\"part-1.mkv\""
        );
        assert_eq!(
            disposition("a \"b\".exe", false),
            "attachment; filename=\"a _b_.exe\"; filename*=UTF-8''a%20%22b%22.exe"
        );
        assert_eq!(
            disposition("café.mp3", true),
            "inline; filename=\"caf_.mp3\"; filename*=UTF-8''caf%C3%A9.mp3"
        );
        assert_eq!(
            disposition("a\r\nb", false),
            "attachment; filename=\"a__b\"; filename*=UTF-8''a%0D%0Ab"
        );
    }
}
