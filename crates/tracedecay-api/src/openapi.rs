//! OpenAPI 3.1 projection of the authorized HTTP route catalog.
//!
//! Route paths, methods, operation identities, schemas, and lifecycle
//! contracts come from mounted route descriptors and binding-keyed schema
//! authorities. The projection never defines a second set of HTTP DTOs.

use std::collections::BTreeMap;

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderValue, header::CONTENT_TYPE};
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use serde_json::Value;
use thiserror::Error;
use tracedecay_application::ApplicationWireSchemaRegistryV1;

use crate::HttpRouteDocumentV1;

mod projection;
mod routes;

pub use projection::{bind_http_route_documents, openapi_routes_document, openapi_routes_router};
pub use routes::{OpenApiRequestV1, OpenApiRouteDocumentV1, OpenApiSuccessV1};

/// Relative endpoint installed by [`openapi_router`].
pub const OPENAPI_DOCUMENT_ROUTE_PATH: &str = "/openapi.json";

const HTTP_PROBLEM_STATUSES: [&str; 8] = ["400", "404", "408", "409", "422", "429", "503", "504"];
const HTTP_JSON_ENVELOPE_REF: &str = "#/components/schemas/HttpJsonEnvelope";
const HTTP_JSON_SUCCESS_REF: &str = "#/components/schemas/HttpJsonSuccessEnvelope";
const HTTP_JSON_PROBLEM_REF: &str = "#/components/schemas/HttpJsonProblemEnvelope";

/// A deterministic OpenAPI 3.1 document projected from authorized routes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct OpenApiDocumentV1 {
    openapi: &'static str,
    #[serde(rename = "jsonSchemaDialect")]
    json_schema_dialect: &'static str,
    info: OpenApiInfoV1,
    paths: BTreeMap<String, BTreeMap<String, Value>>,
    components: Value,
}

impl OpenApiDocumentV1 {
    /// Serialize the canonical document without formatting-dependent
    /// whitespace. Ordered maps make these bytes stable for equivalent input.
    pub fn to_json_bytes(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(self)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct OpenApiInfoV1 {
    title: &'static str,
    version: &'static str,
}

/// A route-catalog conflict that prevents a truthful OpenAPI projection.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum OpenApiDocumentError {
    #[error("HTTP route has invalid binding ID {binding_id}")]
    InvalidBindingId { binding_id: String },
    #[error("duplicate HTTP route {method} {path}")]
    DuplicateRoute { method: String, path: String },
    #[error(
        "operation ID {operation_id} identifies both {first_method} {first_path} and \
         {second_method} {second_path}"
    )]
    ConflictingOperationId {
        operation_id: String,
        first_method: String,
        first_path: String,
        second_method: String,
        second_path: String,
    },
    #[error("HTTP method {method} for {path} cannot be represented in OpenAPI")]
    UnsupportedMethod { method: String, path: String },
    #[error("failed to serialize {field} into OpenAPI: {message}")]
    Serialization {
        field: &'static str,
        message: String,
    },
    #[error("HTTP binding {binding_id} has no canonical wire schema")]
    MissingWireSchema { binding_id: String },
    #[error("HTTP binding {binding_id} does not match its canonical wire schema")]
    MismatchedWireSchema { binding_id: String },
    #[error("OpenAPI route authority for {operation_id} is invalid: {reason}")]
    RouteAuthority {
        operation_id: String,
        reason: &'static str,
    },
    #[error("OpenAPI schema authority for {family} could not be built: {message}")]
    SchemaAuthority {
        family: &'static str,
        message: String,
    },
    #[error("OpenAPI schema component {component} has conflicting authorities")]
    ConflictingSchema { component: String },
    #[error("OpenAPI query schema for {operation_id} is not an object")]
    InvalidQuerySchema { operation_id: String },
    #[error("OpenAPI route {path} has an invalid path parameter")]
    InvalidPathParameter { path: String },
}

/// A failure detected before the OpenAPI endpoint can be mounted.
#[derive(Debug, Error)]
pub enum OpenApiRouterBuildError {
    #[error(transparent)]
    Document(#[from] OpenApiDocumentError),
    #[error("canonical OpenAPI document could not be serialized")]
    Encoding(#[source] serde_json::Error),
}

/// Build a relative OpenAPI endpoint from the authorized HTTP route snapshot
/// and its canonical concrete wire schemas.
///
/// Projection and serialization complete before a router is returned. The
/// composition root can therefore merge this router under its authenticated
/// API prefix without admitting a route that might serve partial or drifted
/// documentation.
pub fn openapi_router(
    route_documents: &[HttpRouteDocumentV1],
    wire_schemas: &ApplicationWireSchemaRegistryV1,
) -> Result<Router, OpenApiRouterBuildError> {
    openapi_routes_router(&bind_http_route_documents(route_documents, wire_schemas)?)
}

async fn serve_openapi(State(document): State<Bytes>) -> Response {
    (
        [(CONTENT_TYPE, HeaderValue::from_static("application/json"))],
        document,
    )
        .into_response()
}

/// Project the supplied authorized route documents into OpenAPI 3.1.
///
/// Input order is immaterial. Duplicate routes and reused operation IDs fail
/// closed because silently dropping either would make the document diverge
/// from the executable catalog.
pub fn openapi_document(
    route_documents: &[HttpRouteDocumentV1],
    wire_schemas: &ApplicationWireSchemaRegistryV1,
) -> Result<OpenApiDocumentV1, OpenApiDocumentError> {
    openapi_routes_document(&bind_http_route_documents(route_documents, wire_schemas)?)
}

fn openapi_method(method: &str) -> Option<&'static str> {
    if method.eq_ignore_ascii_case("GET") {
        Some("get")
    } else if method.eq_ignore_ascii_case("PUT") {
        Some("put")
    } else if method.eq_ignore_ascii_case("POST") {
        Some("post")
    } else if method.eq_ignore_ascii_case("DELETE") {
        Some("delete")
    } else if method.eq_ignore_ascii_case("OPTIONS") {
        Some("options")
    } else if method.eq_ignore_ascii_case("HEAD") {
        Some("head")
    } else if method.eq_ignore_ascii_case("PATCH") {
        Some("patch")
    } else if method.eq_ignore_ascii_case("TRACE") {
        Some("trace")
    } else {
        None
    }
}

fn typed_outcome_schema(result_ref: &str) -> Value {
    let payload = object([(
        "oneOf",
        Value::Array(vec![
            reference(result_ref),
            object([("type", string("null"))]),
        ]),
    )]);
    let variant = |name: &'static str| {
        object([
            ("type", string("object")),
            (
                "required",
                Value::Array(vec![string("outcome"), string("value")]),
            ),
            (
                "properties",
                object([
                    ("outcome", object([("const", string(name))])),
                    (
                        "value",
                        object([
                            ("type", string("object")),
                            ("required", Value::Array(vec![string("payload")])),
                            ("properties", object([("payload", payload.clone())])),
                        ]),
                    ),
                ]),
            ),
        ])
    };
    object([(
        "oneOf",
        Value::Array(vec![
            variant("evidence"),
            variant("preview"),
            variant("effect"),
        ]),
    )])
}

fn contract_value<T: Serialize>(
    field: &'static str,
    value: &T,
) -> Result<Value, OpenApiDocumentError> {
    serde_json::to_value(value).map_err(|error| OpenApiDocumentError::Serialization {
        field,
        message: error.to_string(),
    })
}

fn object<const N: usize>(entries: [(&str, Value); N]) -> Value {
    Value::Object(
        entries
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect(),
    )
}

fn reference(target: &str) -> Value {
    object([("$ref", string(target))])
}

fn string(value: impl Into<String>) -> Value {
    Value::String(value.into())
}

#[cfg(test)]
mod router_tests;
