// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! DIDL-Lite, the XML documents `Browse` returns, and XML escaping.

use crate::dlna;

/// A container or an item, ready to render.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Object {
    Container {
        id: String,
        parent: String,
        title: String,
        child_count: usize,
        album_art: Option<String>,
    },
    Item {
        id: String,
        parent: String,
        title: String,
        class: &'static str,
        media_type: &'static str,
        size_bytes: u64,
        url: String,
        album_art: Option<String>,
    },
}

/// Escapes text for XML content and attribute values.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

fn album_art(out: &mut String, url: Option<&String>) {
    if let Some(url) = url {
        out.push_str(&format!(
            "<upnp:albumArtURI>{}</upnp:albumArtURI>",
            escape(url)
        ));
    }
}

/// Renders `objects` as one DIDL-Lite document.
pub(crate) fn render(objects: &[Object]) -> String {
    let mut out = String::from(
        r#"<DIDL-Lite xmlns="urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:upnp="urn:schemas-upnp-org:metadata-1-0/upnp/" xmlns:dlna="urn:schemas-dlna-org:metadata-1-0/">"#,
    );
    for object in objects {
        match object {
            Object::Container {
                id,
                parent,
                title,
                child_count,
                album_art: art,
            } => {
                out.push_str(&format!(
                    r#"<container id="{}" parentID="{}" restricted="1" childCount="{child_count}" searchable="0"><dc:title>{}</dc:title><upnp:class>object.container.storageFolder</upnp:class>"#,
                    escape(id),
                    escape(parent),
                    escape(title),
                ));
                album_art(&mut out, art.as_ref());
                out.push_str("</container>");
            }
            Object::Item {
                id,
                parent,
                title,
                class,
                media_type,
                size_bytes,
                url,
                album_art: art,
            } => {
                out.push_str(&format!(
                    r#"<item id="{}" parentID="{}" restricted="1"><dc:title>{}</dc:title><upnp:class>{class}</upnp:class>"#,
                    escape(id),
                    escape(parent),
                    escape(title),
                ));
                album_art(&mut out, art.as_ref());
                out.push_str(&format!(
                    r#"<res protocolInfo="{}" size="{size_bytes}">{}</res></item>"#,
                    escape(&dlna::protocol_info(media_type)),
                    escape(url),
                ));
            }
        }
    }
    out.push_str("</DIDL-Lite>");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markup_characters_are_escaped() {
        assert_eq!(
            escape(r#"<a & "b" 'c'>"#),
            "&lt;a &amp; &quot;b&quot; &apos;c&apos;&gt;"
        );
        assert_eq!(escape("Кино"), "Кино");
    }
}
