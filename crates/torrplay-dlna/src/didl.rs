// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! DIDL-Lite, the XML documents `Browse` returns, and XML escaping.

use std::time::Duration;

use crate::dlna;

/// The class of every container.
pub(crate) const CONTAINER_CLASS: &str = "object.container.storageFolder";

/// A container or an item, ready to render, search, and sort.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Object {
    pub(crate) id: String,
    pub(crate) parent: String,
    pub(crate) title: String,
    pub(crate) class: &'static str,
    /// `dc:date`, as `YYYY-MM-DD`.
    pub(crate) date: Option<String>,
    /// `upnp:genre`, in order.
    pub(crate) genres: Vec<String>,
    pub(crate) album_art: Option<String>,
    pub(crate) kind: Kind,
}

/// What a container or an item has of its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Container {
        child_count: usize,
    },
    Item {
        media_type: &'static str,
        size_bytes: u64,
        url: String,
        /// Where a renderer may resume playing it.
        resume_position: Option<Duration>,
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

/// Renders `objects` as one DIDL-Lite document.
pub(crate) fn render(objects: &[Object]) -> String {
    let mut out = String::from(
        r#"<DIDL-Lite xmlns="urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:upnp="urn:schemas-upnp-org:metadata-1-0/upnp/" xmlns:dlna="urn:schemas-dlna-org:metadata-1-0/" xmlns:sec="http://www.sec.co.kr/">"#,
    );
    for object in objects {
        let ids = format!(
            r#"id="{}" parentID="{}" restricted="1""#,
            escape(&object.id),
            escape(&object.parent)
        );
        match &object.kind {
            Kind::Container { child_count } => out.push_str(&format!(
                r#"<container {ids} childCount="{child_count}" searchable="1">"#
            )),
            Kind::Item { .. } => out.push_str(&format!("<item {ids}>")),
        }
        out.push_str(&format!(
            "<dc:title>{}</dc:title><upnp:class>{}</upnp:class>",
            escape(&object.title),
            object.class
        ));
        if let Some(date) = &object.date {
            out.push_str(&format!("<dc:date>{date}</dc:date>"));
        }
        for genre in &object.genres {
            out.push_str(&format!("<upnp:genre>{}</upnp:genre>", escape(genre)));
        }
        if let Some(url) = &object.album_art {
            out.push_str(&format!(
                "<upnp:albumArtURI>{}</upnp:albumArtURI>",
                escape(url)
            ));
        }
        match &object.kind {
            Kind::Container { .. } => out.push_str("</container>"),
            Kind::Item {
                media_type,
                size_bytes,
                url,
                resume_position,
            } => {
                if let Some(position) = resume_position {
                    let seconds = position.as_secs();
                    out.push_str(&format!(
                        "<upnp:lastPlaybackPosition>{}</upnp:lastPlaybackPosition><sec:dcmInfo>BM={seconds}</sec:dcmInfo>",
                        playback_duration(*position)
                    ));
                }
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

/// A duration as `H:MM:SS`, hours unpadded, in whole seconds.
fn playback_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();
    format!(
        "{}:{:02}:{:02}",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    )
}

/// A time's UTC date as `YYYY-MM-DD`, from the days since the Unix epoch
/// (Howard Hinnant's civil-from-days algorithm).
pub(crate) fn date(time: std::time::SystemTime) -> String {
    let days = time
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() / 86_400) as i64;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_are_civil_utc_dates() {
        let at = |seconds: u64| {
            date(std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(seconds))
        };
        assert_eq!(at(0), "1970-01-01");
        assert_eq!(at(951_782_400), "2000-02-29");
        assert_eq!(at(1_793_000_000), "2026-10-26");
        assert_eq!(
            at(1_793_000_000 + 86_399 - 1_793_000_000 % 86_400),
            "2026-10-26"
        );
    }

    #[test]
    fn playback_durations_have_unpadded_hours_and_whole_seconds() {
        assert_eq!(playback_duration(Duration::ZERO), "0:00:00");
        assert_eq!(playback_duration(Duration::from_millis(83_999)), "0:01:23");
        assert_eq!(playback_duration(Duration::from_secs(3_600)), "1:00:00");
        assert_eq!(
            playback_duration(Duration::from_secs(125 * 3_600 + 61)),
            "125:01:01"
        );
    }

    #[test]
    fn markup_characters_are_escaped() {
        assert_eq!(
            escape(r#"<a & "b" 'c'>"#),
            "&lt;a &amp; &quot;b&quot; &apos;c&apos;&gt;"
        );
        assert_eq!(escape("Кино"), "Кино");
    }
}
