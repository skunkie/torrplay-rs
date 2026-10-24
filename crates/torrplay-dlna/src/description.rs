// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The device description and the services' descriptions (SCPDs).

use crate::didl::escape;
use crate::services::{CONNECTION_MANAGER, CONTENT_DIRECTORY};

/// The device type the media server announces.
pub const DEVICE_TYPE: &str = "urn:schemas-upnp-org:device:MediaServer:1";

/// The media server's identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    /// The name televisions show.
    pub friendly_name: String,
    /// Its stored UUID, without the `uuid:` prefix.
    pub uuid: String,
    /// The application's version.
    pub version: String,
}

impl Device {
    /// The unique device name, `uuid:` and the UUID.
    pub fn udn(&self) -> String {
        format!("uuid:{}", self.uuid)
    }

    /// The device description document.
    pub fn description(&self) -> String {
        let service = |service_type: &str, path: &str, id: &str| {
            format!(
                "<service><serviceType>{service_type}</serviceType><serviceId>urn:upnp-org:serviceId:{id}</serviceId><SCPDURL>/dlna/{path}.xml</SCPDURL><controlURL>/dlna/{path}/control</controlURL><eventSubURL>/dlna/{path}/events</eventSubURL></service>"
            )
        };
        format!(
            r#"<?xml version="1.0" encoding="utf-8"?><root xmlns="urn:schemas-upnp-org:device-1-0" xmlns:dlna="urn:schemas-dlna-org:device-1-0"><specVersion><major>1</major><minor>0</minor></specVersion><device><deviceType>{DEVICE_TYPE}</deviceType><friendlyName>{}</friendlyName><manufacturer>TorrPlay</manufacturer><manufacturerURL>https://github.com/skunkie/torrplay-rs</manufacturerURL><modelDescription>TorrPlay media server</modelDescription><modelName>TorrPlay</modelName><modelNumber>{}</modelNumber><UDN>{}</UDN><dlna:X_DLNADOC>DMS-1.50</dlna:X_DLNADOC><serviceList>{}{}</serviceList></device></root>"#,
            escape(&self.friendly_name),
            escape(&self.version),
            escape(&self.udn()),
            service(CONTENT_DIRECTORY, "content-directory", "ContentDirectory"),
            service(
                CONNECTION_MANAGER,
                "connection-manager",
                "ConnectionManager"
            ),
        )
    }
}

fn scpd(actions: &str, variables: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?><scpd xmlns="urn:schemas-upnp-org:service-1-0"><specVersion><major>1</major><minor>0</minor></specVersion><actionList>{actions}</actionList><serviceStateTable>{variables}</serviceStateTable></scpd>"#
    )
}

fn action(name: &str, arguments: &[(&str, &str, &str)]) -> String {
    let arguments: String = arguments
        .iter()
        .map(|(argument, direction, variable)| {
            format!(
                "<argument><name>{argument}</name><direction>{direction}</direction><relatedStateVariable>{variable}</relatedStateVariable></argument>"
            )
        })
        .collect();
    format!("<action><name>{name}</name><argumentList>{arguments}</argumentList></action>")
}

fn variable(name: &str, data_type: &str, events: bool, allowed: &[&str]) -> String {
    let allowed = if allowed.is_empty() {
        String::new()
    } else {
        let values: String = allowed
            .iter()
            .map(|value| format!("<allowedValue>{value}</allowedValue>"))
            .collect();
        format!("<allowedValueList>{values}</allowedValueList>")
    };
    format!(
        r#"<stateVariable sendEvents="{}"><name>{name}</name><dataType>{data_type}</dataType>{allowed}</stateVariable>"#,
        if events { "yes" } else { "no" }
    )
}

/// ContentDirectory's service description.
pub fn content_directory() -> String {
    let actions = [
        action(
            "Browse",
            &[
                ("ObjectID", "in", "A_ARG_TYPE_ObjectID"),
                ("BrowseFlag", "in", "A_ARG_TYPE_BrowseFlag"),
                ("Filter", "in", "A_ARG_TYPE_Filter"),
                ("StartingIndex", "in", "A_ARG_TYPE_Index"),
                ("RequestedCount", "in", "A_ARG_TYPE_Count"),
                ("SortCriteria", "in", "A_ARG_TYPE_SortCriteria"),
                ("Result", "out", "A_ARG_TYPE_Result"),
                ("NumberReturned", "out", "A_ARG_TYPE_Count"),
                ("TotalMatches", "out", "A_ARG_TYPE_Count"),
                ("UpdateID", "out", "A_ARG_TYPE_UpdateID"),
            ],
        ),
        action(
            "Search",
            &[
                ("ContainerID", "in", "A_ARG_TYPE_ObjectID"),
                ("SearchCriteria", "in", "A_ARG_TYPE_SearchCriteria"),
                ("Filter", "in", "A_ARG_TYPE_Filter"),
                ("StartingIndex", "in", "A_ARG_TYPE_Index"),
                ("RequestedCount", "in", "A_ARG_TYPE_Count"),
                ("SortCriteria", "in", "A_ARG_TYPE_SortCriteria"),
                ("Result", "out", "A_ARG_TYPE_Result"),
                ("NumberReturned", "out", "A_ARG_TYPE_Count"),
                ("TotalMatches", "out", "A_ARG_TYPE_Count"),
                ("UpdateID", "out", "A_ARG_TYPE_UpdateID"),
            ],
        ),
        action(
            "GetSearchCapabilities",
            &[("SearchCaps", "out", "SearchCapabilities")],
        ),
        action(
            "GetSortCapabilities",
            &[("SortCaps", "out", "SortCapabilities")],
        ),
        action("GetSystemUpdateID", &[("Id", "out", "SystemUpdateID")]),
        action(
            "X_GetFeatureList",
            &[("FeatureList", "out", "A_ARG_TYPE_Featurelist")],
        ),
    ]
    .concat();
    let variables = [
        variable("A_ARG_TYPE_ObjectID", "string", false, &[]),
        variable(
            "A_ARG_TYPE_BrowseFlag",
            "string",
            false,
            &["BrowseMetadata", "BrowseDirectChildren"],
        ),
        variable("A_ARG_TYPE_Filter", "string", false, &[]),
        variable("A_ARG_TYPE_Index", "ui4", false, &[]),
        variable("A_ARG_TYPE_Count", "ui4", false, &[]),
        variable("A_ARG_TYPE_SortCriteria", "string", false, &[]),
        variable("A_ARG_TYPE_SearchCriteria", "string", false, &[]),
        variable("A_ARG_TYPE_Result", "string", false, &[]),
        variable("A_ARG_TYPE_UpdateID", "ui4", false, &[]),
        variable("A_ARG_TYPE_Featurelist", "string", false, &[]),
        variable("SearchCapabilities", "string", false, &[]),
        variable("SortCapabilities", "string", false, &[]),
        variable("SystemUpdateID", "ui4", true, &[]),
    ]
    .concat();
    scpd(&actions, &variables)
}

/// ConnectionManager's service description.
pub fn connection_manager() -> String {
    let actions = [
        action(
            "GetProtocolInfo",
            &[
                ("Source", "out", "SourceProtocolInfo"),
                ("Sink", "out", "SinkProtocolInfo"),
            ],
        ),
        action(
            "GetCurrentConnectionIDs",
            &[("ConnectionIDs", "out", "CurrentConnectionIDs")],
        ),
        action(
            "GetCurrentConnectionInfo",
            &[
                ("ConnectionID", "in", "A_ARG_TYPE_ConnectionID"),
                ("RcsID", "out", "A_ARG_TYPE_RcsID"),
                ("AVTransportID", "out", "A_ARG_TYPE_AVTransportID"),
                ("ProtocolInfo", "out", "A_ARG_TYPE_ProtocolInfo"),
                (
                    "PeerConnectionManager",
                    "out",
                    "A_ARG_TYPE_ConnectionManager",
                ),
                ("PeerConnectionID", "out", "A_ARG_TYPE_ConnectionID"),
                ("Direction", "out", "A_ARG_TYPE_Direction"),
                ("Status", "out", "A_ARG_TYPE_ConnectionStatus"),
            ],
        ),
    ]
    .concat();
    let variables = [
        variable("SourceProtocolInfo", "string", true, &[]),
        variable("SinkProtocolInfo", "string", true, &[]),
        variable("CurrentConnectionIDs", "string", true, &[]),
        variable("A_ARG_TYPE_ConnectionID", "i4", false, &[]),
        variable("A_ARG_TYPE_RcsID", "i4", false, &[]),
        variable("A_ARG_TYPE_AVTransportID", "i4", false, &[]),
        variable("A_ARG_TYPE_ProtocolInfo", "string", false, &[]),
        variable("A_ARG_TYPE_ConnectionManager", "string", false, &[]),
        variable(
            "A_ARG_TYPE_Direction",
            "string",
            false,
            &["Input", "Output"],
        ),
        variable(
            "A_ARG_TYPE_ConnectionStatus",
            "string",
            false,
            &[
                "OK",
                "ContentFormatMismatch",
                "InsufficientBandwidth",
                "UnreliableChannel",
                "Unknown",
            ],
        ),
    ]
    .concat();
    scpd(&actions, &variables)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_description_names_the_device_and_its_services() {
        let device = Device {
            friendly_name: "TorrPlay & friends".into(),
            uuid: "4b1c0b2a-6f5e-4d3c-8b2a-1f0e9d8c7b6a".into(),
            version: "0.1.0".into(),
        };
        let description = device.description();
        for expected in [
            "<friendlyName>TorrPlay &amp; friends</friendlyName>",
            "<UDN>uuid:4b1c0b2a-6f5e-4d3c-8b2a-1f0e9d8c7b6a</UDN>",
            "<dlna:X_DLNADOC>DMS-1.50</dlna:X_DLNADOC>",
            "<controlURL>/dlna/content-directory/control</controlURL>",
            "<SCPDURL>/dlna/connection-manager.xml</SCPDURL>",
        ] {
            assert!(description.contains(expected), "{expected}\n{description}");
        }
    }

    #[test]
    fn the_service_descriptions_list_the_implemented_actions() {
        let directory = content_directory();
        for action in ["Browse", "Search", "GetSystemUpdateID", "X_GetFeatureList"] {
            assert!(
                directory.contains(&format!("<name>{action}</name>")),
                "{action}"
            );
        }
        assert!(connection_manager().contains("<name>GetProtocolInfo</name>"));
    }
}
