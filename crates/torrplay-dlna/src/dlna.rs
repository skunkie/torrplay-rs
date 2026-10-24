// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The DLNA extensions: the protocol info of resources and the
//! `contentFeatures.dlna.org` and `transferMode.dlna.org` headers.

/// The request header asking for content features.
pub const GET_CONTENT_FEATURES: &str = "getcontentfeatures.dlna.org";

/// The response header with content features.
pub const CONTENT_FEATURES: &str = "contentfeatures.dlna.org";

/// The request and response header naming the transfer mode.
pub const TRANSFER_MODE: &str = "transfermode.dlna.org";

/// Every resource's DLNA parameters: byte seeks (`OP=01`), no conversion
/// (`CI=0`), and flags for streaming and background transfer, connection
/// stalling, and DLNA 1.5.
pub const FEATURES: &str =
    "DLNA.ORG_OP=01;DLNA.ORG_CI=0;DLNA.ORG_FLAGS=01700000000000000000000000000000";

/// The protocol info of a resource served with `media_type` over HTTP.
pub fn protocol_info(media_type: &str) -> String {
    format!("http-get:*:{media_type}:{FEATURES}")
}

/// A DLNA transfer mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransferMode {
    /// Real-time playback.
    Streaming,
    /// Whole objects, such as images, shown at once.
    Interactive,
    /// Downloads without real-time constraints.
    Background,
}

impl TransferMode {
    /// The mode's header value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Streaming => "Streaming",
            Self::Interactive => "Interactive",
            Self::Background => "Background",
        }
    }
}

/// The request named a transfer mode the server does not support.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("the transfer mode is not Streaming, Interactive, or Background")]
pub struct UnsupportedMode;

/// What a media response says about DLNA, from its request's headers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Negotiated {
    /// Whether to send `contentFeatures.dlna.org`.
    pub content_features: bool,
    /// The `transferMode.dlna.org` to send, if any.
    pub transfer_mode: Option<TransferMode>,
}

/// Negotiates a media response from its request's
/// `getcontentFeatures.dlna.org` and `transferMode.dlna.org` values: the
/// named mode is echoed, and a request for content features without one
/// streams.
pub fn negotiate(
    get_content_features: Option<&str>,
    transfer_mode: Option<&str>,
) -> Result<Negotiated, UnsupportedMode> {
    let content_features = get_content_features.is_some_and(|value| value.trim() == "1");
    let transfer_mode = match transfer_mode.map(str::trim) {
        Some(mode) => Some(
            [
                TransferMode::Streaming,
                TransferMode::Interactive,
                TransferMode::Background,
            ]
            .into_iter()
            .find(|known| known.as_str().eq_ignore_ascii_case(mode))
            .ok_or(UnsupportedMode)?,
        ),
        None => content_features.then_some(TransferMode::Streaming),
    };
    Ok(Negotiated {
        content_features,
        transfer_mode,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transfer_modes_are_echoed_or_default_to_streaming() {
        assert_eq!(negotiate(None, None), Ok(Negotiated::default()));
        assert_eq!(
            negotiate(Some("1"), None),
            Ok(Negotiated {
                content_features: true,
                transfer_mode: Some(TransferMode::Streaming),
            })
        );
        assert_eq!(
            negotiate(None, Some("interactive")),
            Ok(Negotiated {
                content_features: false,
                transfer_mode: Some(TransferMode::Interactive),
            })
        );
        assert_eq!(
            negotiate(Some("1"), Some("Background")).map(|found| found.transfer_mode),
            Ok(Some(TransferMode::Background))
        );
        assert_eq!(negotiate(Some("1"), Some("Push")), Err(UnsupportedMode));
        assert_eq!(negotiate(Some("0"), None), Ok(Negotiated::default()));
    }
}
