// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! Request bodies: reading them within their documented limits, and the
//! `multipart/form-data` forms (RFC 7578) that upload torrent files.

use axum::body::Body;
use axum::http::{HeaderMap, header};
use bytes::{Bytes, BytesMut};
use http_body_util::BodyExt;

use crate::problem::{Problem, ProblemType};

/// The largest JSON body, creation or merge patch.
pub(crate) const JSON_LIMIT_BYTES: usize = 16 * 1024;

/// The largest uploaded torrent file.
pub(crate) const TORRENT_FILE_LIMIT_BYTES: usize = 10 * 1024 * 1024;

/// Room for a form's boundaries, part headers, and other fields beyond the
/// torrent file, so the file's own limit decides what is too large.
const FORM_OVERHEAD_BYTES: usize = 64 * 1024;

/// The largest form body.
pub(crate) const FORM_LIMIT_BYTES: usize = TORRENT_FILE_LIMIT_BYTES + FORM_OVERHEAD_BYTES;

/// Reads a body of at most `limit` bytes. A declared `Content-Length` over
/// the limit is refused before reading, and a body that grows past it stops
/// being read at the frame that crosses it, so it is never fully buffered.
pub(crate) async fn read_limited(
    headers: &HeaderMap,
    body: Body,
    limit: usize,
    limit_text: u64,
) -> Result<Bytes, Problem> {
    let too_large = || {
        Problem::new(
            ProblemType::ContentTooLarge,
            format!("The request body exceeds {limit_text} bytes."),
        )
    };
    let declared = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    if declared.is_some_and(|length| length > limit as u64) {
        return Err(too_large());
    }
    let mut body = body;
    let mut collected = BytesMut::new();
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|_| {
            Problem::new(
                ProblemType::MalformedRequest,
                "The request body could not be read.",
            )
        })?;
        if let Ok(data) = frame.into_data() {
            if collected.len() + data.len() > limit {
                return Err(too_large());
            }
            collected.extend_from_slice(&data);
        }
    }
    Ok(collected.freeze())
}

/// One part of a form.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Part {
    pub(crate) name: String,
    pub(crate) data: Bytes,
}

/// The form cannot be parsed.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct MalformedForm;

/// The longest boundary RFC 2046 allows.
const MAX_BOUNDARY_CHARS: usize = 70;

/// Splits a `multipart/form-data` body into its named parts. Part headers
/// other than `Content-Disposition` are ignored: the form's fields say what
/// each part is.
pub(crate) fn parse_form(body: &Bytes, boundary: &str) -> Result<Vec<Part>, MalformedForm> {
    if boundary.is_empty() || boundary.len() > MAX_BOUNDARY_CHARS {
        return Err(MalformedForm);
    }
    let delimiter = format!("\r\n--{boundary}");
    // The first delimiter may open the body without the CRLF before it.
    let first = format!("--{boundary}");
    let mut position = if body.starts_with(first.as_bytes()) {
        first.len()
    } else {
        memchr::memmem::find(body, delimiter.as_bytes()).ok_or(MalformedForm)? + delimiter.len()
    };
    let finder = memchr::memmem::Finder::new(delimiter.as_bytes());
    let mut parts = Vec::new();
    loop {
        let rest = &body[position..];
        if rest.starts_with(b"--") {
            return Ok(parts);
        }
        // Transport padding may follow a delimiter before its CRLF.
        let padding = rest
            .iter()
            .take_while(|&&byte| byte == b' ' || byte == b'\t')
            .count();
        if !rest[padding..].starts_with(b"\r\n") {
            return Err(MalformedForm);
        }
        let start = position + padding + 2;
        let end = start + finder.find(&body[start..]).ok_or(MalformedForm)?;
        parts.push(part(body.slice(start..end))?);
        position = end + delimiter.len();
    }
}

fn part(bytes: Bytes) -> Result<Part, MalformedForm> {
    let (head, data) = if bytes.starts_with(b"\r\n") {
        (&b""[..], bytes.slice(2..))
    } else {
        let split = memchr::memmem::find(&bytes, b"\r\n\r\n").ok_or(MalformedForm)?;
        (&bytes[..split], bytes.slice(split + 4..))
    };
    let head = std::str::from_utf8(head).map_err(|_| MalformedForm)?;
    let mut name = None;
    for line in head.split("\r\n").filter(|line| !line.is_empty()) {
        let (field, value) = line.split_once(':').ok_or(MalformedForm)?;
        if field.trim().eq_ignore_ascii_case("content-disposition") {
            name = Some(disposition_name(value).ok_or(MalformedForm)?);
        }
    }
    Ok(Part {
        name: name.ok_or(MalformedForm)?,
        data,
    })
}

/// Returns the `name` parameter of a `form-data` disposition.
fn disposition_name(value: &str) -> Option<String> {
    let (kind, mut rest) = value.split_once(';')?;
    if !kind.trim().eq_ignore_ascii_case("form-data") {
        return None;
    }
    loop {
        rest = rest.trim_start_matches([' ', '\t', ';']);
        if rest.is_empty() {
            return None;
        }
        let (parameter, after) = rest.split_once('=')?;
        let (value, after) = parameter_value(after.trim_start())?;
        if parameter.trim().eq_ignore_ascii_case("name") {
            return Some(value);
        }
        rest = after;
    }
}

/// Reads a token or quoted string, returning it and the text after it.
fn parameter_value(text: &str) -> Option<(String, &str)> {
    let Some(quoted) = text.strip_prefix('"') else {
        let end = text.find(';').unwrap_or(text.len());
        return Some((text[..end].trim().to_owned(), &text[end..]));
    };
    let mut value = String::new();
    let mut characters = quoted.char_indices();
    while let Some((index, character)) = characters.next() {
        match character {
            '"' => return Some((value, &quoted[index + 1..])),
            '\\' => value.push(characters.next()?.1),
            _ => value.push(character),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    fn form(text: &'static str) -> Bytes {
        Bytes::from_static(text.as_bytes())
    }

    #[test]
    fn forms_split_into_named_parts() {
        let body = form(
            "--b\r\nContent-Disposition: form-data; name=\"torrent_file\"; filename=\"a.torrent\"\r\n\
Content-Type: application/x-bittorrent\r\n\r\nd4:info\r\n--x\r\n--b\r\n\
Content-Disposition: form-data; name=desired_state\r\n\r\npaused\r\n--b--\r\n",
        );
        let parts = parse_form(&body, "b").unwrap();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].name, "torrent_file");
        assert_eq!(
            &parts[0].data[..],
            b"d4:info\r\n--x",
            "a near-boundary stays data"
        );
        assert_eq!(parts[1].name, "desired_state");
        assert_eq!(&parts[1].data[..], b"paused");
    }

    #[test]
    fn a_preamble_and_padding_are_skipped() {
        let body = form(
            "ignored\r\n--b  \r\nContent-Disposition: form-data; name=\"a\\\"b\"\r\n\r\n1\r\n--b--",
        );
        let parts = parse_form(&body, "b").unwrap();
        assert_eq!(parts[0].name, "a\"b");
        assert_eq!(&parts[0].data[..], b"1");
    }

    #[test]
    fn malformed_forms_are_rejected() {
        let cases = [
            ("name=value", "b"),
            (
                "--b\r\nContent-Disposition: form-data; name=\"a\"\r\n\r\n1",
                "b",
            ),
            ("--b\r\nContent-Type: text/plain\r\n\r\n1\r\n--b--", "b"),
            (
                "--b\r\nContent-Disposition: attachment; name=\"a\"\r\n\r\n1\r\n--b--",
                "b",
            ),
            (
                "--b\r\nContent-Disposition: form-data; name=\"a\"\r\n\r\n1\r\n--b--",
                "",
            ),
        ];
        for (body, boundary) in cases {
            assert_eq!(
                parse_form(&form(body), boundary),
                Err(MalformedForm),
                "{body:?}"
            );
        }
    }

    #[tokio::test]
    async fn bodies_over_their_limit_are_refused() {
        let read = |declared: Option<&'static str>, body: &'static [u8]| async move {
            let mut headers = HeaderMap::new();
            if let Some(declared) = declared {
                headers.insert(header::CONTENT_LENGTH, HeaderValue::from_static(declared));
            }
            read_limited(&headers, Body::from(body), 4, 4).await
        };
        assert_eq!(&read(None, b"1234").await.unwrap()[..], b"1234");
        let too_large = |result: Result<Bytes, Problem>| {
            result.unwrap_err().problem_type() == ProblemType::ContentTooLarge
        };
        assert!(too_large(read(None, b"12345").await));
        assert!(
            too_large(read(Some("5"), b"").await),
            "declared lengths count"
        );
    }
}
