// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! SOAP over HTTP, as UPnP control uses it: reading an action request and
//! writing its response or a UPnP error.

use std::collections::HashMap;

use quick_xml::Reader;
use quick_xml::events::Event;

use crate::didl::escape;

/// The most bytes a SOAP request body may have.
pub const BODY_MAX_BYTES: usize = 64 * 1024;

/// The media type of SOAP and description documents.
pub const XML: &str = r#"text/xml; charset="utf-8""#;

/// An action request: its service type, its name, and its arguments by
/// name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Action {
    /// The service type, such as
    /// `urn:schemas-upnp-org:service:ContentDirectory:1`.
    pub service: String,
    /// The action's name, such as `Browse`.
    pub name: String,
    /// Its arguments' values.
    pub arguments: HashMap<String, String>,
}

impl Action {
    /// Reads an action from its `SOAPACTION` header and body. The body's
    /// action element must be the one the header names.
    pub fn parse(soap_action: &str, body: &[u8]) -> Result<Self, Fault> {
        let (service, name) = soap_action
            .trim()
            .trim_matches('"')
            .split_once('#')
            .ok_or(Fault::InvalidAction)?;
        let mut reader = Reader::from_reader(body);
        let mut buffer = Vec::new();
        // The element path below the envelope's body.
        let mut depth_in_body: Option<usize> = None;
        let mut depth = 0;
        let mut action_found = false;
        let mut argument: Option<String> = None;
        let mut arguments = HashMap::new();
        loop {
            let event = reader
                .read_event_into(&mut buffer)
                .map_err(|_| Fault::InvalidArgs)?;
            match event {
                Event::Start(start) => {
                    depth += 1;
                    let local = String::from_utf8_lossy(start.local_name().as_ref()).into_owned();
                    match depth_in_body {
                        None if local == "Body" => depth_in_body = Some(depth),
                        Some(body) if depth == body + 1 => {
                            if local != name {
                                return Err(Fault::InvalidAction);
                            }
                            action_found = true;
                        }
                        Some(body) if depth == body + 2 => {
                            arguments.insert(local.clone(), String::new());
                            argument = Some(local);
                        }
                        _ => {}
                    }
                }
                Event::Empty(empty) => {
                    let local = String::from_utf8_lossy(empty.local_name().as_ref()).into_owned();
                    match depth_in_body {
                        Some(body) if depth == body => {
                            if local != name {
                                return Err(Fault::InvalidAction);
                            }
                            action_found = true;
                        }
                        Some(body) if depth == body + 1 => {
                            arguments.insert(local, String::new());
                        }
                        _ => {}
                    }
                }
                Event::Text(text) => {
                    if let Some(argument) = &argument {
                        let value = text.decode().map_err(|_| Fault::InvalidArgs)?;
                        let value =
                            quick_xml::escape::unescape(&value).map_err(|_| Fault::InvalidArgs)?;
                        if let Some(slot) = arguments.get_mut(argument) {
                            slot.push_str(&value);
                        }
                    }
                }
                Event::GeneralRef(reference) => {
                    if let Some(argument) = &argument {
                        let name = reference.decode().map_err(|_| Fault::InvalidArgs)?;
                        let reference = format!("&{name};");
                        let value = quick_xml::escape::unescape(&reference)
                            .map_err(|_| Fault::InvalidArgs)?;
                        if let Some(slot) = arguments.get_mut(argument) {
                            slot.push_str(&value);
                        }
                    }
                }
                Event::End(_) => {
                    if depth_in_body.is_some_and(|body| depth == body + 2) {
                        argument = None;
                    }
                    depth -= 1;
                }
                Event::Eof => break,
                _ => {}
            }
            buffer.clear();
        }
        if !action_found {
            return Err(Fault::InvalidAction);
        }
        Ok(Self {
            service: service.to_owned(),
            name: name.to_owned(),
            arguments,
        })
    }

    /// An argument's value, or [`Fault::InvalidArgs`] if it is missing.
    pub fn argument(&self, name: &str) -> Result<&str, Fault> {
        self.arguments
            .get(name)
            .map(String::as_str)
            .ok_or(Fault::InvalidArgs)
    }

    /// An unsigned integer argument.
    pub fn number(&self, name: &str) -> Result<u32, Fault> {
        self.argument(name)?
            .trim()
            .parse()
            .map_err(|_| Fault::InvalidArgs)
    }
}

/// A UPnP error a SOAP fault carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Fault {
    /// `401`: the service has no such action.
    #[error("Invalid Action")]
    InvalidAction,
    /// `402`: an argument is missing or invalid.
    #[error("Invalid Args")]
    InvalidArgs,
    /// `701`: the object ID names nothing.
    #[error("No such object")]
    NoSuchObject,
    /// `708`: the search criteria cannot be read.
    #[error("Unsupported or invalid search criteria")]
    InvalidSearch,
    /// `709`: the sort criteria cannot be read.
    #[error("Unsupported or invalid sort criteria")]
    InvalidSort,
    /// `720`: the request cannot be processed now.
    #[error("Cannot process the request")]
    CannotProcess,
}

impl Fault {
    /// The UPnP error code.
    pub fn code(self) -> u16 {
        match self {
            Self::InvalidAction => 401,
            Self::InvalidArgs => 402,
            Self::NoSuchObject => 701,
            Self::InvalidSearch => 708,
            Self::InvalidSort => 709,
            Self::CannotProcess => 720,
        }
    }

    /// The fault's envelope, sent with HTTP `500`.
    pub fn envelope(self) -> String {
        envelope(&format!(
            r#"<s:Fault><faultcode>s:Client</faultcode><faultstring>UPnPError</faultstring><detail><UPnPError xmlns="urn:schemas-upnp-org:control-1-0"><errorCode>{}</errorCode><errorDescription>{self}</errorDescription></UPnPError></detail></s:Fault>"#,
            self.code()
        ))
    }
}

fn envelope(body: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?><s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/"><s:Body>{body}</s:Body></s:Envelope>"#
    )
}

/// The response envelope of `action` with output arguments `outputs`, in
/// order; their values are escaped here.
pub fn response(action: &Action, outputs: &[(&str, &str)]) -> String {
    let mut body = format!(
        r#"<u:{}Response xmlns:u="{}">"#,
        action.name,
        escape(&action.service)
    );
    for (name, value) in outputs {
        body.push_str(&format!("<{name}>{}</{name}>", escape(value)));
    }
    body.push_str(&format!("</u:{}Response>", action.name));
    envelope(&body)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BROWSE: &str = r#"<?xml version="1.0"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/">
  <s:Body>
    <u:Browse xmlns:u="urn:schemas-upnp-org:service:ContentDirectory:1">
      <ObjectID>category:A%26B</ObjectID>
      <BrowseFlag>BrowseDirectChildren</BrowseFlag>
      <Filter>*</Filter>
      <StartingIndex>0</StartingIndex>
      <RequestedCount>16</RequestedCount>
      <SortCriteria/>
      <Title>Tom &amp; Jerry</Title>
    </u:Browse>
  </s:Body>
</s:Envelope>"#;

    const HEADER: &str = r#""urn:schemas-upnp-org:service:ContentDirectory:1#Browse""#;

    #[test]
    fn an_action_reads_with_its_arguments() {
        let action = Action::parse(HEADER, BROWSE.as_bytes()).unwrap();
        assert_eq!(
            action.service,
            "urn:schemas-upnp-org:service:ContentDirectory:1"
        );
        assert_eq!(action.name, "Browse");
        assert_eq!(action.argument("ObjectID"), Ok("category:A%26B"));
        assert_eq!(action.argument("SortCriteria"), Ok(""));
        assert_eq!(action.argument("Title"), Ok("Tom & Jerry"));
        assert_eq!(action.number("RequestedCount"), Ok(16));
        assert_eq!(action.argument("Missing"), Err(Fault::InvalidArgs));
        assert_eq!(action.number("BrowseFlag"), Err(Fault::InvalidArgs));
    }

    #[test]
    fn a_header_and_body_that_disagree_are_an_invalid_action() {
        let other = r#""urn:schemas-upnp-org:service:ContentDirectory:1#Search""#;
        assert_eq!(
            Action::parse(other, BROWSE.as_bytes()),
            Err(Fault::InvalidAction)
        );
        assert_eq!(
            Action::parse("Browse", BROWSE.as_bytes()),
            Err(Fault::InvalidAction)
        );
        assert_eq!(
            Action::parse(HEADER, b"<s:Envelope/>"),
            Err(Fault::InvalidAction)
        );
        assert_eq!(
            Action::parse(HEADER, b"<a><b></a>"),
            Err(Fault::InvalidArgs)
        );
    }

    #[test]
    fn responses_and_faults_are_envelopes() {
        let action = Action::parse(HEADER, BROWSE.as_bytes()).unwrap();
        let response = response(
            &action,
            &[("Result", "<DIDL-Lite/>"), ("NumberReturned", "0")],
        );
        assert!(response.contains(
            r#"<u:BrowseResponse xmlns:u="urn:schemas-upnp-org:service:ContentDirectory:1"><Result>&lt;DIDL-Lite/&gt;</Result><NumberReturned>0</NumberReturned></u:BrowseResponse>"#
        ), "{response}");
        let fault = Fault::NoSuchObject.envelope();
        assert!(fault.contains(
            "<errorCode>701</errorCode><errorDescription>No such object</errorDescription>"
        ));
    }
}
