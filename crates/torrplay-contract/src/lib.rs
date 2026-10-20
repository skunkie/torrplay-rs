// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! Checks HTTP traffic against `api/openapi.yaml`, as
//! `docs/adr/0006-contract-validation.md` decides.
//!
//! The server's types are handwritten, so tests pass every request they send
//! and every response they receive through a [`Contract`]. It finds the
//! request's operation, checks its parameters and body, and checks the
//! response's status, media type, headers, and body against a strict copy
//! of the document, which rejects members the contract does not define. It
//! also records which documented responses tests have produced, so a test can
//! require all of them.
//!
//! Every operation may also answer `405` and `406`, which the document's
//! description assigns to the API conventions rather than to each
//! operation; both must be Problem Details, and `405` must carry `Allow`.

mod multipart;
mod strict;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};

use http::{HeaderMap, Method, Request, Response};
use serde_json::{Value, json};

/// The contract this crate checks traffic against.
pub const DOCUMENT: &str = include_str!("../../../api/openapi.yaml");

/// The tag URI under which every problem type lies.
pub const PROBLEM_TYPE_BASE: &str = "tag:skunkie.github.io,2026:torrplay/problems/";

/// The URI the document is registered under for `$ref` resolution. It is
/// never fetched.
const BASE: &str = "https://torrplay.invalid/openapi.yaml";

static CONTRACT: LazyLock<Result<Contract, ContractError>> =
    LazyLock::new(|| Contract::parse(DOCUMENT));

/// Returns the contract parsed from [`DOCUMENT`], shared by all tests in a
/// process so that its coverage accumulates.
pub fn contract() -> Result<&'static Contract, &'static ContractError> {
    CONTRACT.as_ref()
}

/// A document that cannot serve as the contract.
#[derive(Debug, thiserror::Error)]
pub enum ContractError {
    /// The document is not YAML.
    #[error("the document is not valid YAML: {0}")]
    Yaml(String),
    /// The document lacks something an OpenAPI document must have.
    #[error("the document is malformed at {pointer}: {message}")]
    Malformed {
        /// Where in the document.
        pointer: String,
        /// What is wrong.
        message: String,
    },
    /// A schema cannot be compiled.
    #[error("the schema at {pointer} does not compile: {message}")]
    Schema {
        /// Where in the document.
        pointer: String,
        /// The validator's message.
        message: String,
    },
}

/// One way traffic departs from the contract.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Violation {
    /// No documented path matches the request.
    #[error("no documented path matches {method} {path}")]
    UnknownPath {
        /// The request method.
        method: String,
        /// The request path.
        path: String,
    },
    /// The path is documented, but not for this method.
    #[error("{method} is not documented for {template}")]
    UndocumentedMethod {
        /// The request method.
        method: String,
        /// The matched path template.
        template: String,
    },
    /// A query parameter the operation does not define.
    #[error("query parameter {0} is not documented")]
    UnknownQueryParameter(String),
    /// A required parameter is absent.
    #[error("required parameter {0} is missing")]
    MissingParameter(String),
    /// A parameter value fails its schema.
    #[error("parameter {name}: {message}")]
    InvalidParameter {
        /// The parameter.
        name: String,
        /// What is wrong.
        message: String,
    },
    /// The request body is absent although required.
    #[error("the required request body is missing")]
    MissingRequestBody,
    /// A body whose media type the contract does not list for it.
    #[error("{context} media type {media_type:?} is not documented")]
    UndocumentedMediaType {
        /// `request` or the response status.
        context: String,
        /// The `Content-Type` sent, without parameters.
        media_type: Option<String>,
    },
    /// A body that fails its schema.
    #[error("{context} body: {message}")]
    InvalidBody {
        /// `request` or the response status.
        context: String,
        /// What is wrong.
        message: String,
    },
    /// A response status the operation does not document.
    #[error("status {0} is not documented for this operation")]
    UndocumentedStatus(u16),
    /// A required response header is absent.
    #[error("required header {0} is missing")]
    MissingHeader(String),
    /// A response header value fails its schema.
    #[error("header {name}: {message}")]
    InvalidHeader {
        /// The header.
        name: String,
        /// What is wrong.
        message: String,
    },
    /// A body where the contract documents none.
    #[error("{context} has a body although none is documented")]
    UnexpectedBody {
        /// `request` or the response status.
        context: String,
    },
    /// A problem type outside [`PROBLEM_TYPE_BASE`].
    #[error("problem type {0:?} is not under {PROBLEM_TYPE_BASE}")]
    ProblemTypeOutsideBase(String),
}

/// Every violation found in one check.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error("{}", .0.iter().map(ToString::to_string).collect::<Vec<_>>().join("; "))]
pub struct Violations(pub Vec<Violation>);

impl Violations {
    fn into_result(self) -> Result<(), Self> {
        if self.0.is_empty() { Ok(()) } else { Err(self) }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Location {
    Path,
    Query,
    Header,
}

#[derive(Debug)]
struct Parameter {
    name: String,
    location: Location,
    required: bool,
    // The resolved schema's pointer in the document.
    schema: String,
}

#[derive(Debug)]
struct Operation {
    id: String,
    method: Method,
    template: String,
    // None marks a template parameter.
    segments: Vec<Option<String>>,
    pointer: String,
    parameters: Vec<Parameter>,
}

/// The contract and the coverage of its responses.
pub struct Contract {
    document: Value,
    registry: jsonschema::Registry<'static>,
    operations: Vec<Operation>,
    validators: Mutex<HashMap<String, Arc<jsonschema::Validator>>>,
    observed: Mutex<BTreeSet<(String, u16)>>,
}

impl std::fmt::Debug for Contract {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Contract")
            .field("operations", &self.operations.len())
            .finish_non_exhaustive()
    }
}

fn escape(segment: &str) -> String {
    segment.replace('~', "~0").replace('/', "~1")
}

fn malformed(pointer: &str, message: impl Into<String>) -> ContractError {
    ContractError::Malformed {
        pointer: pointer.to_owned(),
        message: message.into(),
    }
}

const METHODS: [(&str, Method); 7] = [
    ("get", Method::GET),
    ("head", Method::HEAD),
    ("post", Method::POST),
    ("put", Method::PUT),
    ("patch", Method::PATCH),
    ("delete", Method::DELETE),
    ("options", Method::OPTIONS),
];

impl Contract {
    /// Parses an OpenAPI document and prepares its schemas.
    pub fn parse(text: &str) -> Result<Self, ContractError> {
        let document: Value =
            serde_saphyr::from_str(text).map_err(|error| ContractError::Yaml(error.to_string()))?;
        let registry = jsonschema::Registry::new()
            .draft(jsonschema::Draft::Draft202012)
            .add(BASE, strict::close_objects(document.clone()))
            .and_then(|builder| builder.prepare())
            .map_err(|error| ContractError::Schema {
                pointer: String::new(),
                message: error.to_string(),
            })?;
        let mut contract = Self {
            document,
            registry,
            operations: Vec::new(),
            validators: Mutex::new(HashMap::new()),
            observed: Mutex::new(BTreeSet::new()),
        };
        contract.operations = contract.collect_operations()?;
        Ok(contract)
    }

    fn collect_operations(&self) -> Result<Vec<Operation>, ContractError> {
        let paths = self
            .document
            .get("paths")
            .and_then(Value::as_object)
            .ok_or_else(|| malformed("/paths", "missing"))?;
        let mut operations = Vec::new();
        for (template, item) in paths {
            let item_pointer = format!("/paths/{}", escape(template));
            for (key, method) in METHODS {
                let Some(operation) = item.get(key) else {
                    continue;
                };
                let pointer = format!("{item_pointer}/{key}");
                let id = operation
                    .get("operationId")
                    .and_then(Value::as_str)
                    .ok_or_else(|| malformed(&pointer, "no operationId"))?
                    .to_owned();
                let mut parameters = Vec::new();
                for list in [
                    format!("{item_pointer}/parameters"),
                    format!("{pointer}/parameters"),
                ] {
                    let count = self
                        .document
                        .pointer(&list)
                        .and_then(Value::as_array)
                        .map_or(0, Vec::len);
                    for index in 0..count {
                        parameters.push(self.parameter(&format!("{list}/{index}"))?);
                    }
                }
                operations.push(Operation {
                    id,
                    method: method.clone(),
                    template: template.clone(),
                    segments: template
                        .split('/')
                        .map(|segment| (!segment.starts_with('{')).then(|| segment.to_owned()))
                        .collect(),
                    pointer,
                    parameters,
                });
            }
        }
        Ok(operations)
    }

    fn parameter(&self, pointer: &str) -> Result<Parameter, ContractError> {
        let (pointer, node) = self.resolve(pointer)?;
        let name = node
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| malformed(&pointer, "parameter without a name"))?;
        let location = match node.get("in").and_then(Value::as_str) {
            Some("path") => Location::Path,
            Some("query") => Location::Query,
            Some("header") => Location::Header,
            other => {
                return Err(malformed(
                    &pointer,
                    format!("unsupported location {other:?}"),
                ));
            }
        };
        let (schema, _) = self.resolve(&format!("{pointer}/schema"))?;
        Ok(Parameter {
            name: name.to_owned(),
            location,
            required: node
                .get("required")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            schema,
        })
    }

    /// Follows local `$ref`s from `pointer` and returns the target's pointer
    /// and value.
    fn resolve(&self, pointer: &str) -> Result<(String, &Value), ContractError> {
        let mut pointer = pointer.to_owned();
        for _ in 0..16 {
            let node = self
                .document
                .pointer(&pointer)
                .ok_or_else(|| malformed(&pointer, "nothing here"))?;
            match node.get("$ref").and_then(Value::as_str) {
                Some(target) => {
                    pointer = target
                        .strip_prefix('#')
                        .ok_or_else(|| malformed(&pointer, "only local references are supported"))?
                        .to_owned();
                }
                None => return Ok((pointer, node)),
            }
        }
        Err(malformed(&pointer, "a reference cycle"))
    }

    fn lock_validators(&self) -> MutexGuard<'_, HashMap<String, Arc<jsonschema::Validator>>> {
        self.validators
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Validates `instance` against the strict schema at `pointer`.
    fn validate(&self, pointer: &str, instance: &Value) -> Result<(), String> {
        let fragment = pointer.replace('{', "%7B").replace('}', "%7D");
        self.validate_with(
            pointer,
            &json!({"$ref": format!("{BASE}#{fragment}")}),
            instance,
        )
    }

    fn validate_with(&self, key: &str, schema: &Value, instance: &Value) -> Result<(), String> {
        let validator = {
            let mut validators = self.lock_validators();
            match validators.get(key) {
                Some(validator) => validator.clone(),
                None => {
                    let validator = jsonschema::options()
                        .with_draft(jsonschema::Draft::Draft202012)
                        .with_registry(&self.registry)
                        .should_validate_formats(true)
                        .build(schema)
                        .map_err(|error| {
                            format!("the schema at {key} does not compile: {error}")
                        })?;
                    let validator = Arc::new(validator);
                    validators.insert(key.to_owned(), validator.clone());
                    validator
                }
            }
        };
        let errors: Vec<String> = validator
            .iter_errors(instance)
            .map(|error| format!("{error} at {}", error.instance_path()))
            .collect();
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }

    fn path_matches(&self, path: &str) -> Vec<&Operation> {
        let segments: Vec<&str> = path.split('/').collect();
        self.operations
            .iter()
            .filter(|operation| {
                operation.segments.len() == segments.len()
                    && operation
                        .segments
                        .iter()
                        .zip(&segments)
                        .all(|(template, actual)| match template {
                            Some(literal) => literal == actual,
                            None => !actual.is_empty(),
                        })
            })
            .collect()
    }

    fn operation<B>(&self, request: &Request<B>) -> Result<&Operation, Violation> {
        let path = request.uri().path();
        let candidates = self.path_matches(path);
        let template = candidates
            .first()
            .map(|operation| operation.template.clone())
            .ok_or_else(|| Violation::UnknownPath {
                method: request.method().to_string(),
                path: path.to_owned(),
            })?;
        candidates
            .into_iter()
            .find(|operation| operation.method == request.method())
            .ok_or_else(|| Violation::UndocumentedMethod {
                method: request.method().to_string(),
                template,
            })
    }

    /// Checks a request against its operation.
    pub fn check_request<B: AsRef<[u8]>>(&self, request: &Request<B>) -> Result<(), Violations> {
        let operation = match self.operation(request) {
            Ok(operation) => operation,
            Err(violation) => return Err(Violations(vec![violation])),
        };
        let mut violations = Vec::new();
        self.check_parameters(operation, request, &mut violations);
        self.check_request_body(operation, request, &mut violations);
        Violations(violations).into_result()
    }

    fn check_parameters<B>(
        &self,
        operation: &Operation,
        request: &Request<B>,
        violations: &mut Vec<Violation>,
    ) {
        let mut query: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (name, value) in
            form_urlencoded::parse(request.uri().query().unwrap_or_default().as_bytes())
        {
            query
                .entry(name.into_owned())
                .or_default()
                .push(value.into_owned());
        }
        let path_values: Vec<&str> = request.uri().path().split('/').collect();
        for name in query.keys() {
            if !operation
                .parameters
                .iter()
                .any(|parameter| parameter.location == Location::Query && &parameter.name == name)
            {
                violations.push(Violation::UnknownQueryParameter(name.clone()));
            }
        }
        for parameter in &operation.parameters {
            let values: Vec<String> = match parameter.location {
                Location::Query => query.get(&parameter.name).cloned().unwrap_or_default(),
                Location::Header => request
                    .headers()
                    .get_all(parameter.name.as_str())
                    .iter()
                    .map(|value| String::from_utf8_lossy(value.as_bytes()).into_owned())
                    .collect(),
                Location::Path => operation
                    .segments
                    .iter()
                    .zip(&path_values)
                    .zip(operation.template.split('/'))
                    .filter(|((template, _), raw)| {
                        template.is_none() && raw.trim_matches(['{', '}']) == parameter.name
                    })
                    .map(|((_, value), _)| (*value).to_owned())
                    .collect(),
            };
            if values.is_empty() {
                if parameter.required {
                    violations.push(Violation::MissingParameter(parameter.name.clone()));
                }
                continue;
            }
            let instance = match self.parameter_value(parameter, &values) {
                Ok(instance) => instance,
                Err(message) => {
                    violations.push(Violation::InvalidParameter {
                        name: parameter.name.clone(),
                        message,
                    });
                    continue;
                }
            };
            if let Err(message) = self.validate(&parameter.schema, &instance) {
                violations.push(Violation::InvalidParameter {
                    name: parameter.name.clone(),
                    message,
                });
            }
        }
    }

    /// Converts serialized parameter values to the JSON value their schema
    /// describes: an array for an exploded array, a number for an integer.
    fn parameter_value(&self, parameter: &Parameter, values: &[String]) -> Result<Value, String> {
        let schema = self
            .document
            .pointer(&parameter.schema)
            .cloned()
            .unwrap_or(Value::Null);
        if schema.get("type").and_then(Value::as_str) == Some("array") {
            let items = schema.get("items").cloned().unwrap_or(Value::Null);
            let items = match items.get("$ref").and_then(Value::as_str) {
                Some(target) => self
                    .document
                    .pointer(target.trim_start_matches('#'))
                    .cloned()
                    .unwrap_or(Value::Null),
                None => items,
            };
            return Ok(Value::Array(
                values.iter().map(|value| typed(&items, value)).collect(),
            ));
        }
        match values {
            [value] => Ok(typed(&schema, value)),
            _ => Err(format!(
                "{} values where the schema expects one",
                values.len()
            )),
        }
    }

    fn check_request_body<B: AsRef<[u8]>>(
        &self,
        operation: &Operation,
        request: &Request<B>,
        violations: &mut Vec<Violation>,
    ) {
        let body = request.body().as_ref();
        let Ok((pointer, request_body)) =
            self.resolve(&format!("{}/requestBody", operation.pointer))
        else {
            if !body.is_empty() {
                violations.push(Violation::UnexpectedBody {
                    context: "request".into(),
                });
            }
            return;
        };
        if body.is_empty() {
            if request_body
                .get("required")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                violations.push(Violation::MissingRequestBody);
            }
            return;
        }
        let content_type = content_type(request.headers());
        let media_type = content_type.as_ref().map(|(essence, _)| essence.clone());
        let Some(media) = media_type.as_deref().and_then(|essence| {
            request_body
                .get("content")
                .and_then(|content| content.get(essence))
        }) else {
            violations.push(Violation::UndocumentedMediaType {
                context: "request".into(),
                media_type,
            });
            return;
        };
        let Some(essence) = media_type else {
            return;
        };
        let schema = format!("{pointer}/content/{}/schema", escape(&essence));
        let instance = if is_json(&essence) {
            serde_json::from_slice(body).map_err(|error| format!("not JSON: {error}"))
        } else if essence == "multipart/form-data" {
            let boundary = content_type
                .as_ref()
                .and_then(|(_, parameters)| parameter(parameters, "boundary"))
                .unwrap_or_default();
            self.form_value(media, body, &boundary)
        } else {
            return;
        };
        let result = instance.and_then(|instance| self.validate(&schema, &instance));
        if let Err(message) = result {
            violations.push(Violation::InvalidBody {
                context: "request".into(),
                message,
            });
        }
    }

    /// Reads a form into an object of its parts, checking each part's
    /// media type against the media type's `encoding`.
    fn form_value(&self, media: &Value, body: &[u8], boundary: &str) -> Result<Value, String> {
        let mut object = serde_json::Map::new();
        for part in multipart::parse(body, boundary)? {
            let expected = media
                .pointer(&format!("/encoding/{}/contentType", escape(&part.name)))
                .and_then(Value::as_str);
            if let (Some(expected), Some(actual)) = (expected, part.content_type.as_deref())
                && !media_type_matches(expected, actual)
            {
                return Err(format!("part {} is {actual}, not {expected}", part.name));
            }
            let value = match std::str::from_utf8(&part.data) {
                Ok(text) => Value::String(text.to_owned()),
                // Binary parts are strings with a content media type.
                Err(_) => Value::String(format!("<{} bytes>", part.data.len())),
            };
            object.insert(part.name, value);
        }
        Ok(Value::Object(object))
    }

    /// Checks a request and the response to it, and records the response.
    pub fn check<B: AsRef<[u8]>, C: AsRef<[u8]>>(
        &self,
        request: &Request<B>,
        response: &Response<C>,
    ) -> Result<(), Violations> {
        let mut violations = self
            .check_request(request)
            .err()
            .map(|v| v.0)
            .unwrap_or_default();
        violations.extend(
            self.check_response(request, response)
                .err()
                .map(|v| v.0)
                .unwrap_or_default(),
        );
        Violations(violations).into_result()
    }

    /// Checks only the response to `request`, and records it. Tests that
    /// send a request the contract forbids, to check the server's answer,
    /// use this.
    pub fn check_response<B, C: AsRef<[u8]>>(
        &self,
        request: &Request<B>,
        response: &Response<C>,
    ) -> Result<(), Violations> {
        let status = response.status().as_u16();
        let mut violations = Vec::new();
        let operation = match self.operation(request) {
            Ok(operation) => Some(operation),
            Err(Violation::UndocumentedMethod { .. }) if status == 405 => None,
            Err(violation) => return Err(Violations(vec![violation])),
        };
        let documented = operation.and_then(|operation| {
            self.resolve(&format!("{}/responses/{status}", operation.pointer))
                .ok()
        });
        match (documented, status) {
            (Some((pointer, documented)), _) => {
                self.check_documented_response(
                    request.method(),
                    &pointer,
                    documented,
                    response,
                    &mut violations,
                );
                if let Some(operation) = operation {
                    let mut observed = self
                        .observed
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    observed.insert((operation.id.clone(), status));
                }
            }
            (None, 405 | 406) => {
                if status == 405 && !response.headers().contains_key(http::header::ALLOW) {
                    violations.push(Violation::MissingHeader("Allow".into()));
                }
                self.check_problem(status, response, &mut violations);
            }
            (None, _) => violations.push(Violation::UndocumentedStatus(status)),
        }
        Violations(violations).into_result()
    }

    fn check_documented_response<C: AsRef<[u8]>>(
        &self,
        method: &Method,
        pointer: &str,
        documented: &Value,
        response: &Response<C>,
        violations: &mut Vec<Violation>,
    ) {
        let status = response.status().as_u16();
        let context = status.to_string();
        if let Some(headers) = documented.get("headers").and_then(Value::as_object) {
            for name in headers.keys() {
                self.check_header(
                    &format!("{pointer}/headers/{}", escape(name)),
                    name,
                    response.headers(),
                    violations,
                );
            }
        }
        let body = response.body().as_ref();
        let content = documented.get("content").and_then(Value::as_object);
        // HEAD responses carry a GET's headers without its body.
        let Some(content) = content.filter(|_| method != Method::HEAD) else {
            if !body.is_empty() {
                violations.push(Violation::UnexpectedBody { context });
            }
            return;
        };
        let media_type = content_type(response.headers()).map(|(essence, _)| essence);
        let Some(range) = media_type.as_deref().and_then(|essence| {
            content
                .keys()
                .find(|range| media_type_matches(range, essence))
        }) else {
            violations.push(Violation::UndocumentedMediaType {
                context,
                media_type,
            });
            return;
        };
        let Some(essence) = media_type else {
            return;
        };
        if !is_json(&essence) {
            return;
        }
        let instance: Value = match serde_json::from_slice(body) {
            Ok(instance) => instance,
            Err(error) => {
                violations.push(Violation::InvalidBody {
                    context,
                    message: format!("not JSON: {error}"),
                });
                return;
            }
        };
        if let Err(message) = self.validate(
            &format!("{pointer}/content/{}/schema", escape(range)),
            &instance,
        ) {
            violations.push(Violation::InvalidBody {
                context: context.clone(),
                message,
            });
        }
        if essence == "application/problem+json" {
            check_problem_type(&instance, violations);
        }
    }

    fn check_header(
        &self,
        pointer: &str,
        name: &str,
        headers: &HeaderMap,
        violations: &mut Vec<Violation>,
    ) {
        let Ok((pointer, header)) = self.resolve(pointer) else {
            return;
        };
        let values: Vec<_> = headers.get_all(name).iter().collect();
        let [value] = values.as_slice() else {
            if values.is_empty()
                && header
                    .get("required")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            {
                violations.push(Violation::MissingHeader(name.to_owned()));
            } else if values.len() > 1 {
                violations.push(Violation::InvalidHeader {
                    name: name.to_owned(),
                    message: format!("{} values where one is expected", values.len()),
                });
            }
            return;
        };
        let Ok(text) = value.to_str() else {
            violations.push(Violation::InvalidHeader {
                name: name.to_owned(),
                message: "the value is not visible ASCII".into(),
            });
            return;
        };
        let schema = self
            .document
            .pointer(&format!("{pointer}/schema"))
            .cloned()
            .unwrap_or(Value::Null);
        if let Err(message) = self.validate(&format!("{pointer}/schema"), &typed(&schema, text)) {
            violations.push(Violation::InvalidHeader {
                name: name.to_owned(),
                message,
            });
        }
    }

    /// Checks a `405` or `406` body against the problem schema.
    fn check_problem<C: AsRef<[u8]>>(
        &self,
        status: u16,
        response: &Response<C>,
        violations: &mut Vec<Violation>,
    ) {
        let context = status.to_string();
        let media_type = content_type(response.headers()).map(|(essence, _)| essence);
        if media_type.as_deref() != Some("application/problem+json") {
            violations.push(Violation::UndocumentedMediaType {
                context,
                media_type,
            });
            return;
        }
        let schema = json!({
            "$ref": format!("{BASE}#/components/schemas/Problem"),
            "unevaluatedProperties": false,
        });
        let instance = serde_json::from_slice(response.body().as_ref())
            .map_err(|error| format!("not JSON: {error}"))
            .and_then(|instance: Value| {
                self.validate_with("problem", &schema, &instance)?;
                Ok(instance)
            });
        match instance {
            Ok(instance) => check_problem_type(&instance, violations),
            Err(message) => violations.push(Violation::InvalidBody { context, message }),
        }
    }

    /// Every operation ID and status the contract documents.
    pub fn documented_responses(&self) -> BTreeSet<(String, u16)> {
        let mut documented = BTreeSet::new();
        for operation in &self.operations {
            let responses = self
                .document
                .pointer(&format!("{}/responses", operation.pointer))
                .and_then(Value::as_object);
            for status in responses.into_iter().flat_map(|responses| responses.keys()) {
                if let Ok(status) = status.parse() {
                    documented.insert((operation.id.clone(), status));
                }
            }
        }
        documented
    }

    /// Every operation ID and status checked so far in this process.
    pub fn observed_responses(&self) -> BTreeSet<(String, u16)> {
        self.observed
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Documented responses no check has seen, apart from `exempt` ones.
    pub fn unexercised(&self, exempt: &[(&str, u16)]) -> Vec<(String, u16)> {
        let observed = self.observed_responses();
        self.documented_responses()
            .into_iter()
            .filter(|response| !observed.contains(response))
            .filter(|(id, status)| !exempt.contains(&(id.as_str(), *status)))
            .collect()
    }
}

fn check_problem_type(instance: &Value, violations: &mut Vec<Violation>) {
    if let Some(problem_type) = instance.get("type").and_then(Value::as_str)
        && !problem_type.starts_with(PROBLEM_TYPE_BASE)
    {
        violations.push(Violation::ProblemTypeOutsideBase(problem_type.to_owned()));
    }
}

/// The `Content-Type` essence, lowercased, and its parameters.
fn content_type(headers: &HeaderMap) -> Option<(String, String)> {
    let value = headers.get(http::header::CONTENT_TYPE)?.to_str().ok()?;
    let (essence, parameters) = value.split_once(';').unwrap_or((value, ""));
    Some((essence.trim().to_ascii_lowercase(), parameters.to_owned()))
}

fn parameter(parameters: &str, name: &str) -> Option<String> {
    parameters.split(';').find_map(|parameter| {
        let (key, value) = parameter.split_once('=')?;
        (key.trim().eq_ignore_ascii_case(name)).then(|| value.trim().trim_matches('"').to_owned())
    })
}

fn is_json(essence: &str) -> bool {
    essence == "application/json" || essence.ends_with("+json")
}

/// Whether `actual` falls within the media range `range`, such as `video/*`.
fn media_type_matches(range: &str, actual: &str) -> bool {
    match range.strip_suffix("/*") {
        Some(kind) => actual
            .split_once('/')
            .is_some_and(|(actual_kind, _)| actual_kind == kind),
        None => range.eq_ignore_ascii_case(actual),
    }
}

/// Reads a serialized value as the type its schema names, so a schema check
/// can judge it.
fn typed(schema: &Value, value: &str) -> Value {
    match schema.get("type").and_then(Value::as_str) {
        Some("integer") => value
            .parse::<i64>()
            .map(Value::from)
            .unwrap_or_else(|_| Value::String(value.to_owned())),
        Some("boolean") => value
            .parse::<bool>()
            .map(Value::from)
            .unwrap_or_else(|_| Value::String(value.to_owned())),
        _ => Value::String(value.to_owned()),
    }
}

#[cfg(test)]
mod tests;
