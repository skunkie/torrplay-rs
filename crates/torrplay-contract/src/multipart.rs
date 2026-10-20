// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! A minimal reader of `multipart/form-data` bodies (RFC 7578), enough to
//! check the parts of a request against the contract.

/// One part of a form.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Part {
    pub(crate) name: String,
    pub(crate) content_type: Option<String>,
    pub(crate) data: Vec<u8>,
}

/// Splits `body` into its parts, or describes why it is not a form.
pub(crate) fn parse(body: &[u8], boundary: &str) -> Result<Vec<Part>, String> {
    let delimiter = format!("--{boundary}");
    let mut parts = Vec::new();
    let mut rest = after(body, delimiter.as_bytes()).ok_or("the body has no opening boundary")?;
    loop {
        if rest.starts_with(b"--") {
            return Ok(parts);
        }
        rest = rest
            .strip_prefix(b"\r\n")
            .ok_or("a boundary is not followed by CRLF")?;
        let end = find(rest, format!("\r\n{delimiter}").as_bytes())
            .ok_or("a part has no closing boundary")?;
        parts.push(part(&rest[..end])?);
        rest = &rest[end + 2 + delimiter.len()..];
    }
}

fn part(bytes: &[u8]) -> Result<Part, String> {
    let split = find(bytes, b"\r\n\r\n").ok_or("a part has no blank line after its headers")?;
    let head = std::str::from_utf8(&bytes[..split]).map_err(|_| "part headers are not UTF-8")?;
    let mut name = None;
    let mut content_type = None;
    for line in head.split("\r\n") {
        let (field, value) = line
            .split_once(':')
            .ok_or_else(|| format!("malformed part header {line:?}"))?;
        match field.trim().to_ascii_lowercase().as_str() {
            "content-disposition" => {
                name = value
                    .split(';')
                    .filter_map(|parameter| parameter.trim().strip_prefix("name="))
                    .map(|quoted| quoted.trim_matches('"').to_owned())
                    .next();
            }
            "content-type" => content_type = Some(value.trim().to_ascii_lowercase()),
            _ => {}
        }
    }
    Ok(Part {
        name: name.ok_or("a part has no name")?,
        content_type,
        data: bytes[split + 4..].to_vec(),
    })
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn after<'a>(haystack: &'a [u8], needle: &[u8]) -> Option<&'a [u8]> {
    find(haystack, needle).map(|start| &haystack[start + needle.len()..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_named_parts_with_their_types() {
        let body = b"--b\r\nContent-Disposition: form-data; name=\"torrent_file\"; filename=\"a.torrent\"\r\n\
Content-Type: application/x-bittorrent\r\n\r\nd4:infoe\r\n--b\r\n\
Content-Disposition: form-data; name=\"desired_state\"\r\n\r\npaused\r\n--b--\r\n";
        let parts = parse(body, "b").unwrap();
        assert_eq!(
            parts,
            [
                Part {
                    name: "torrent_file".into(),
                    content_type: Some("application/x-bittorrent".into()),
                    data: b"d4:infoe".to_vec(),
                },
                Part {
                    name: "desired_state".into(),
                    content_type: None,
                    data: b"paused".to_vec(),
                },
            ]
        );
    }

    #[test]
    fn rejects_a_body_without_its_boundary() {
        assert!(parse(b"name=value", "b").is_err());
    }
}
