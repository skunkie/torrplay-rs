// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The ContentDirectory and ConnectionManager actions the contract lists.

use crate::dlna;
use crate::id::ObjectId;
use crate::search::{Criteria, SEARCH_CAPABILITIES, SORT_CAPABILITIES, SortCriteria};
use crate::soap::{self, Action, Fault};
use crate::tree::{Base, BrowseFlag, Browsed, Catalog, Page};

/// ContentDirectory's service type.
pub const CONTENT_DIRECTORY: &str = "urn:schemas-upnp-org:service:ContentDirectory:1";

/// ConnectionManager's service type.
pub const CONNECTION_MANAGER: &str = "urn:schemas-upnp-org:service:ConnectionManager:1";

/// The media types ConnectionManager reports as sources.
pub const SOURCE_TYPES: &[&str] = &[
    "video/x-matroska",
    "video/mp4",
    "video/webm",
    "video/x-msvideo",
    "video/quicktime",
    "video/mp2t",
    "video/ogg",
    "audio/mpeg",
    "audio/mp4",
    "audio/aac",
    "audio/flac",
    "audio/ogg",
    "audio/opus",
    "audio/wav",
    "audio/x-matroska",
    "audio/webm",
    "image/jpeg",
    "image/png",
    "image/gif",
    "image/webp",
    "image/avif",
    "image/bmp",
];

/// ConnectionManager's `SourceProtocolInfo`: the protocol info of every
/// source media type.
pub fn source_protocol_info() -> String {
    SOURCE_TYPES
        .iter()
        .map(|media_type| dlna::protocol_info(media_type))
        .collect::<Vec<_>>()
        .join(",")
}

/// Samsung's feature list, which points its televisions at the root for
/// every kind of media.
const FEATURE_LIST: &str = r#"<?xml version="1.0" encoding="utf-8"?><Features xmlns="urn:schemas-upnp-org:av:avs" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" xsi:schemaLocation="urn:schemas-upnp-org:av:avs http://www.upnp.org/schemas/av/avs.xsd"><Feature name="samsung.com_BASICVIEW" version="1"><container id="0" type="object.item.audioItem"/><container id="0" type="object.item.videoItem"/><container id="0" type="object.item.imageItem"/></Feature></Features>"#;

/// What a ContentDirectory action reads: the catalog, `None` while the
/// stored torrents are being restored, and the system update ID.
pub struct Directory<'a> {
    /// The library as the media server presents it.
    pub catalog: Option<&'a Catalog>,
    /// The current `SystemUpdateID`.
    pub update_id: u32,
    /// Where resources live.
    pub base: Base<'a>,
}

/// Answers a ContentDirectory action with its response envelope.
pub fn content_directory(action: &Action, directory: &Directory<'_>) -> Result<String, Fault> {
    if action.service != CONTENT_DIRECTORY {
        return Err(Fault::InvalidAction);
    }
    let update_id = directory.update_id.to_string();
    let page = || -> Result<Page, Fault> {
        Ok(Page {
            start: action.number("StartingIndex")? as usize,
            count: action.number("RequestedCount")? as usize,
        })
    };
    // A missing sort is no sort, as some control points omit it.
    let sort = || {
        SortCriteria::parse(
            action
                .arguments
                .get("SortCriteria")
                .map_or("", String::as_str),
        )
        .map_err(|_| Fault::InvalidSort)
    };
    let answer = |browsed: Browsed| {
        soap::response(
            action,
            &[
                ("Result", &browsed.didl),
                ("NumberReturned", &browsed.returned.to_string()),
                ("TotalMatches", &browsed.total.to_string()),
                ("UpdateID", &update_id),
            ],
        )
    };
    match action.name.as_str() {
        "Browse" => {
            let id = action.argument("ObjectID")?;
            let flag = match action.argument("BrowseFlag")? {
                "BrowseMetadata" => BrowseFlag::Metadata,
                "BrowseDirectChildren" => BrowseFlag::DirectChildren,
                _ => return Err(Fault::InvalidArgs),
            };
            let (page, sort) = (page()?, sort()?);
            let catalog = directory.catalog.ok_or(Fault::CannotProcess)?;
            let id = ObjectId::parse(id).ok_or(Fault::NoSuchObject)?;
            let browsed = catalog
                .browse(&id, flag, page, &sort, directory.base)
                .map_err(|_| Fault::NoSuchObject)?;
            Ok(answer(browsed))
        }
        "Search" => {
            let id = action.argument("ContainerID")?;
            let criteria = Criteria::parse(action.argument("SearchCriteria")?)
                .map_err(|_| Fault::InvalidSearch)?;
            let (page, sort) = (page()?, sort()?);
            let catalog = directory.catalog.ok_or(Fault::CannotProcess)?;
            let id = ObjectId::parse(id).ok_or(Fault::NoSuchObject)?;
            let found = catalog
                .search(&id, &criteria, page, &sort, directory.base)
                .map_err(|_| Fault::NoSuchObject)?;
            Ok(answer(found))
        }
        "GetSearchCapabilities" => Ok(soap::response(
            action,
            &[("SearchCaps", SEARCH_CAPABILITIES)],
        )),
        "GetSortCapabilities" => Ok(soap::response(action, &[("SortCaps", SORT_CAPABILITIES)])),
        "GetSystemUpdateID" => Ok(soap::response(action, &[("Id", &update_id)])),
        "X_GetFeatureList" => Ok(soap::response(action, &[("FeatureList", FEATURE_LIST)])),
        _ => Err(Fault::InvalidAction),
    }
}

/// Answers a ConnectionManager action with its response envelope.
pub fn connection_manager(action: &Action) -> Result<String, Fault> {
    if action.service != CONNECTION_MANAGER {
        return Err(Fault::InvalidAction);
    }
    match action.name.as_str() {
        "GetProtocolInfo" => Ok(soap::response(
            action,
            &[("Source", &source_protocol_info()), ("Sink", "")],
        )),
        "GetCurrentConnectionIDs" => Ok(soap::response(action, &[("ConnectionIDs", "0")])),
        "GetCurrentConnectionInfo" => {
            if action.number("ConnectionID")? != 0 {
                return Err(Fault::InvalidArgs);
            }
            Ok(soap::response(
                action,
                &[
                    ("RcsID", "-1"),
                    ("AVTransportID", "-1"),
                    ("ProtocolInfo", ""),
                    ("PeerConnectionManager", ""),
                    ("PeerConnectionID", "-1"),
                    ("Direction", "Output"),
                    ("Status", "OK"),
                ],
            ))
        }
        _ => Err(Fault::InvalidAction),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn action(service: &str, name: &str, arguments: &[(&str, &str)]) -> Action {
        Action {
            service: service.to_owned(),
            name: name.to_owned(),
            arguments: arguments
                .iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                .collect(),
        }
    }

    fn directory(catalog: Option<&Catalog>) -> Directory<'_> {
        Directory {
            catalog,
            update_id: 7,
            base: Base("http://10.0.0.2:8642"),
        }
    }

    #[test]
    fn browse_answers_with_its_counts_and_update_id() {
        let catalog = Catalog::default();
        let browse = action(
            CONTENT_DIRECTORY,
            "Browse",
            &[
                ("ObjectID", "0"),
                ("BrowseFlag", "BrowseDirectChildren"),
                ("Filter", "*"),
                ("StartingIndex", "0"),
                ("RequestedCount", "0"),
                ("SortCriteria", ""),
            ],
        );
        let response = content_directory(&browse, &directory(Some(&catalog))).unwrap();
        assert!(response.contains("<NumberReturned>7</NumberReturned><TotalMatches>7</TotalMatches><UpdateID>7</UpdateID>"), "{response}");
        assert_eq!(
            content_directory(&browse, &directory(None)),
            Err(Fault::CannotProcess),
            "the stored torrents are being restored"
        );
    }

    #[test]
    fn browse_refuses_bad_arguments_and_unknown_objects() {
        let catalog = Catalog::default();
        let browse = |id: &str, flag: &str| {
            action(
                CONTENT_DIRECTORY,
                "Browse",
                &[
                    ("ObjectID", id),
                    ("BrowseFlag", flag),
                    ("StartingIndex", "0"),
                    ("RequestedCount", "0"),
                ],
            )
        };
        let directory = directory(Some(&catalog));
        assert_eq!(
            content_directory(&browse("0", "Sideways"), &directory),
            Err(Fault::InvalidArgs)
        );
        assert_eq!(
            content_directory(&browse("t:trr_none", "BrowseMetadata"), &directory),
            Err(Fault::NoSuchObject)
        );
        assert_eq!(
            content_directory(&browse("nonsense", "BrowseMetadata"), &directory),
            Err(Fault::NoSuchObject)
        );
        assert_eq!(
            content_directory(&action(CONTENT_DIRECTORY, "Browse", &[]), &directory),
            Err(Fault::InvalidArgs)
        );
        assert_eq!(
            content_directory(&action(CONTENT_DIRECTORY, "DestroyObject", &[]), &directory),
            Err(Fault::InvalidAction)
        );
        assert_eq!(
            content_directory(&action(CONNECTION_MANAGER, "Browse", &[]), &directory),
            Err(Fault::InvalidAction),
            "an action of another service"
        );
    }

    #[test]
    fn the_capabilities_and_feature_list_answer() {
        let directory = directory(None);
        let update = content_directory(
            &action(CONTENT_DIRECTORY, "GetSystemUpdateID", &[]),
            &directory,
        )
        .unwrap();
        assert!(update.contains("<Id>7</Id>"));
        let search = content_directory(
            &action(CONTENT_DIRECTORY, "GetSearchCapabilities", &[]),
            &directory,
        )
        .unwrap();
        assert!(search.contains(
            "<SearchCaps>dc:title,dc:date,upnp:class,upnp:genre,@id,@parentID</SearchCaps>"
        ));
        let sort = content_directory(
            &action(CONTENT_DIRECTORY, "GetSortCapabilities", &[]),
            &directory,
        )
        .unwrap();
        assert!(sort.contains("<SortCaps>dc:title,dc:date</SortCaps>"));
        let features = content_directory(
            &action(CONTENT_DIRECTORY, "X_GetFeatureList", &[]),
            &directory,
        )
        .unwrap();
        assert!(features.contains("samsung.com_BASICVIEW"));
    }

    #[test]
    fn connection_manager_reports_its_sources_and_one_connection() {
        let info = connection_manager(&action(CONNECTION_MANAGER, "GetProtocolInfo", &[])).unwrap();
        assert!(
            info.contains("http-get:*:video/x-matroska:DLNA.ORG_OP=01"),
            "{info}"
        );
        let ids = connection_manager(&action(CONNECTION_MANAGER, "GetCurrentConnectionIDs", &[]))
            .unwrap();
        assert!(ids.contains("<ConnectionIDs>0</ConnectionIDs>"));
        assert_eq!(
            connection_manager(&action(
                CONNECTION_MANAGER,
                "GetCurrentConnectionInfo",
                &[("ConnectionID", "3")]
            )),
            Err(Fault::InvalidArgs)
        );
    }
}
