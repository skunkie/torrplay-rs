// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! Redaction of text bound for logs.
//!
//! Private trackers put passkeys in announce URLs, as a path segment or a
//! query parameter, and magnet URIs carry those URLs in `tr` parameters.
//! Engine errors may quote either. [`redact`] keeps what identifies a
//! torrent or a host and drops the rest.

/// Characters that end a URI inside free text.
fn ends_uri(character: char) -> bool {
    character.is_whitespace() || matches!(character, '"' | '\'' | '<' | '>' | '`')
}

/// Returns `text` with every URL reduced to its scheme and host, and every
/// magnet URI reduced to its `xt` parameters.
pub fn redact(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = next_uri(rest) {
        out.push_str(&rest[..start]);
        let uri_text = &rest[start..];
        let end = uri_text.find(ends_uri).unwrap_or(uri_text.len());
        let uri = &uri_text[..end];
        if let Some(query) = uri.strip_prefix("magnet:?") {
            out.push_str("magnet:?");
            let exact: Vec<&str> = query
                .split('&')
                .filter(|parameter| parameter.starts_with("xt=") || parameter.starts_with("xt."))
                .collect();
            out.push_str(&exact.join("&"));
        } else if let Some((scheme, after)) = uri.split_once("://") {
            let authority = after.split(['/', '?', '#']).next().unwrap_or("");
            // Credentials before the host are dropped as well.
            let host = authority.rsplit('@').next().unwrap_or("");
            out.push_str(scheme);
            out.push_str("://");
            out.push_str(host);
            if host.len() < after.len() {
                out.push_str("/…");
            }
        }
        rest = &uri_text[end..];
    }
    out.push_str(rest);
    out
}

/// Returns where the next URL or magnet URI starts: its scheme, for a URL.
fn next_uri(text: &str) -> Option<usize> {
    let magnet = text.find("magnet:?");
    let url = text.find("://").map(|separator| {
        text[..separator]
            .rfind(|character: char| {
                !(character.is_ascii_alphanumeric() || "+-.".contains(character))
            })
            .map_or(0, |before| before + 1)
    });
    match (magnet, url) {
        (Some(magnet), Some(url)) => Some(magnet.min(url)),
        (magnet, url) => magnet.or(url),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_keep_only_their_scheme_and_host() {
        let cases = [
            (
                "announce to http://tracker.test/s3cr3t/announce failed",
                "announce to http://tracker.test/… failed",
            ),
            (
                "udp://user:pass@tracker.test:6969?passkey=s3cr3t",
                "udp://tracker.test:6969/…",
            ),
            ("\"https://tracker.test\"", "\"https://tracker.test\""),
            ("no uri here", "no uri here"),
        ];
        for (text, expected) in cases {
            assert_eq!(redact(text), expected, "{text}");
        }
    }

    #[test]
    fn magnet_uris_keep_only_their_exact_topics() {
        let text = "bad magnet:?xt=urn:btih:00&dn=x&tr=http%3A%2F%2Ft%2Fs3cr3t then";
        assert_eq!(redact(text), "bad magnet:?xt=urn:btih:00 then");
    }
}
