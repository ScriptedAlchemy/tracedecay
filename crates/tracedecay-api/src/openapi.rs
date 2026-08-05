//! OpenAPI 3.1 projection of the authorized HTTP route catalog.
//!
//! Route paths, methods, operation identities, schemas, and lifecycle
//! contracts come only from [`HttpRouteDocumentV1`]. The projection references
//! canonical wire schemas rather than defining a second set of HTTP DTOs.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use serde_json::{Map, Value};
use thiserror::Error;
use tracedecay_application::ApplicationWireSchemaRegistryV1;

use crate::HttpRouteDocumentV1;

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
    let mut schema_bodies = BTreeMap::new();
    for route in route_documents {
        let schema = wire_schemas
            .iter()
            .find(|schema| schema.binding_id().as_str() == route.binding_id)
            .ok_or_else(|| OpenApiDocumentError::MissingWireSchema {
                binding_id: route.binding_id.clone(),
            })?;
        if schema.operation().as_str() != route.operation
            || schema.capability_id().as_str() != route.capability_id
            || schema.request().schema_ref().schema_id().as_str() != route.request_schema
            || schema.request().schema_ref().revision() != route.request_schema_revision
            || schema.result().schema_ref().schema_id().as_str() != route.result_schema
            || schema.result().schema_ref().revision() != route.result_schema_revision
        {
            return Err(OpenApiDocumentError::MismatchedWireSchema {
                binding_id: route.binding_id.clone(),
            });
        }
        schema_bodies.insert(
            route.binding_id.clone(),
            (
                schema.request().body().clone(),
                schema.result().body().clone(),
            ),
        );
    }
    openapi_document_from_schema_bodies(route_documents, &schema_bodies)
}

fn openapi_document_from_schema_bodies(
    route_documents: &[HttpRouteDocumentV1],
    schema_bodies: &BTreeMap<String, (Value, Value)>,
) -> Result<OpenApiDocumentV1, OpenApiDocumentError> {
    let mut ordered_routes = route_documents.iter().collect::<Vec<_>>();
    ordered_routes.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.method.cmp(right.method))
            .then_with(|| left.operation.cmp(&right.operation))
    });

    let mut route_keys = BTreeSet::new();
    let mut operation_ids = BTreeMap::<String, (String, String)>::new();
    let mut paths = BTreeMap::<String, BTreeMap<String, Value>>::new();

    for route in ordered_routes {
        let method = openapi_method(route.method).ok_or_else(|| {
            OpenApiDocumentError::UnsupportedMethod {
                method: route.method.to_owned(),
                path: route.path.clone(),
            }
        })?;
        let route_key = (method.to_owned(), route.path.clone());
        if !route_keys.insert(route_key) {
            return Err(OpenApiDocumentError::DuplicateRoute {
                method: route.method.to_owned(),
                path: route.path.clone(),
            });
        }

        if let Some((first_method, first_path)) = operation_ids.get(&route.operation) {
            return Err(OpenApiDocumentError::ConflictingOperationId {
                operation_id: route.operation.clone(),
                first_method: first_method.clone(),
                first_path: first_path.clone(),
                second_method: route.method.to_owned(),
                second_path: route.path.clone(),
            });
        }
        operation_ids.insert(
            route.operation.clone(),
            (route.method.to_owned(), route.path.clone()),
        );

        let (request_schema, result_schema) =
            schema_bodies.get(&route.binding_id).ok_or_else(|| {
                OpenApiDocumentError::MissingWireSchema {
                    binding_id: route.binding_id.clone(),
                }
            })?;
        let operation = operation_document(route, request_schema, result_schema)?;
        paths
            .entry(route.path.clone())
            .or_default()
            .insert(method.to_owned(), operation);
    }

    Ok(OpenApiDocumentV1 {
        openapi: "3.1.0",
        json_schema_dialect: "https://json-schema.org/draft/2020-12/schema",
        info: OpenApiInfoV1 {
            title: "TraceDecay HTTP API",
            version: "1",
        },
        paths,
        components: components(schema_bodies),
    })
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

fn operation_document(
    route: &HttpRouteDocumentV1,
    _request_schema_body: &Value,
    _result_schema_body: &Value,
) -> Result<Value, OpenApiDocumentError> {
    let lifecycle = object([
        (
            "cancellation",
            contract_value("cancellation contract", &route.cancellation)?,
        ),
        (
            "deadline",
            contract_value("deadline contract", &route.deadline)?,
        ),
        (
            "pagination",
            contract_value("pagination contract", &route.pagination)?,
        ),
        (
            "receipt",
            contract_value("receipt contract", &route.receipt)?,
        ),
        (
            "terminal_states",
            contract_value("terminal state contract", &route.terminal_states)?,
        ),
    ]);

    let request_schema = object([
        (
            "allOf",
            Value::Array(vec![reference(&wire_component_ref(
                &route.binding_id,
                "request",
            ))]),
        ),
        ("x-tracedecay-schema-id", string(&route.request_schema)),
        (
            "x-tracedecay-schema-revision",
            number(route.request_schema_revision),
        ),
    ]);
    let success_schema = object([
        (
            "allOf",
            Value::Array(vec![
                reference(HTTP_JSON_ENVELOPE_REF),
                reference(HTTP_JSON_SUCCESS_REF),
            ]),
        ),
        (
            "x-tracedecay-result-schema",
            object([
                (
                    "allOf",
                    Value::Array(vec![reference(&wire_component_ref(
                        &route.binding_id,
                        "result",
                    ))]),
                ),
                ("schema_id", string(&route.result_schema)),
                ("revision", number(route.result_schema_revision)),
            ]),
        ),
    ]);

    let mut responses = Map::new();
    responses.insert(
        "200".to_owned(),
        object([
            ("description", string("Canonical application outcome")),
            (
                "content",
                object([("application/json", object([("schema", success_schema)]))]),
            ),
        ]),
    );
    for status in HTTP_PROBLEM_STATUSES {
        responses.insert(
            status.to_owned(),
            reference("#/components/responses/HttpProblem"),
        );
    }

    Ok(object([
        ("operationId", string(&route.operation)),
        (
            "requestBody",
            object([
                ("required", Value::Bool(true)),
                (
                    "content",
                    object([("application/json", object([("schema", request_schema)]))]),
                ),
            ]),
        ),
        ("responses", Value::Object(responses)),
        ("x-tracedecay-binding-id", string(route.binding_id.as_str())),
        (
            "x-tracedecay-capability-id",
            string(route.capability_id.as_str()),
        ),
        ("x-tracedecay-http-method", string(route.method)),
        ("x-tracedecay-lifecycle", lifecycle),
        (
            "x-tracedecay-request-id",
            object([
                (
                    "locations",
                    Value::Array(vec![string("success_envelope"), string("problem_envelope")]),
                ),
                ("schema", reference("#/components/schemas/RequestId")),
                ("source", string("server_generated")),
            ]),
        ),
    ]))
}

fn wire_component_name(binding_id: &str, direction: &str) -> String {
    format!("{binding_id}.{direction}")
}

fn wire_component_ref(binding_id: &str, direction: &str) -> String {
    format!(
        "#/components/schemas/{}",
        wire_component_name(binding_id, direction)
    )
}

fn components(schema_bodies: &BTreeMap<String, (Value, Value)>) -> Value {
    let mut schemas = Map::from_iter([
        (
            "HttpJsonEnvelope".to_owned(),
            object([
                (
                    "oneOf",
                    Value::Array(vec![
                        reference(HTTP_JSON_SUCCESS_REF),
                        reference(HTTP_JSON_PROBLEM_REF),
                    ]),
                ),
                (
                    "discriminator",
                    object([
                        ("propertyName", string("kind")),
                        (
                            "mapping",
                            object([
                                ("problem", string(HTTP_JSON_PROBLEM_REF)),
                                ("success", string(HTTP_JSON_SUCCESS_REF)),
                            ]),
                        ),
                    ]),
                ),
            ]),
        ),
        (
            "HttpJsonProblemEnvelope".to_owned(),
            reference("urn:tracedecay:schema:http-json-problem-envelope:revision:1"),
        ),
        (
            "HttpJsonSuccessEnvelope".to_owned(),
            reference("urn:tracedecay:schema:http-json-success-envelope:revision:1"),
        ),
        (
            "RequestId".to_owned(),
            reference("urn:tracedecay:schema:request-id:revision:1"),
        ),
    ]);
    for (binding_id, (request, result)) in schema_bodies {
        schemas.insert(wire_component_name(binding_id, "request"), request.clone());
        schemas.insert(wire_component_name(binding_id, "result"), result.clone());
    }
    object([
        (
            "responses",
            object([(
                "HttpProblem",
                object([
                    ("description", string("Canonical application problem")),
                    (
                        "content",
                        object([(
                            "application/json",
                            object([(
                                "schema",
                                object([(
                                    "allOf",
                                    Value::Array(vec![
                                        reference(HTTP_JSON_ENVELOPE_REF),
                                        reference(HTTP_JSON_PROBLEM_REF),
                                    ]),
                                )]),
                            )]),
                        )]),
                    ),
                ]),
            )]),
        ),
        ("schemas", Value::Object(schemas)),
    ])
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

fn number(value: u32) -> Value {
    Value::Number(value.into())
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use serde_json::{Value, json};
    use tracedecay_tool_catalog::{
        CancellationContract, CancellationPoint, DeadlineBehavior, DeadlineContract,
        PaginationContract, ReceiptContract, TerminalState, TerminalStateContract,
    };

    use super::{OpenApiDocumentError, openapi_document_from_schema_bodies};
    use crate::HttpRouteDocumentV1;

    fn route(
        method: &'static str,
        path: &str,
        operation: &str,
        request_schema: &str,
        request_revision: u32,
        result_schema: &str,
        result_revision: u32,
    ) -> HttpRouteDocumentV1 {
        HttpRouteDocumentV1 {
            method,
            path: path.to_owned(),
            operation: operation.to_owned(),
            capability_id: format!("capability.{operation}"),
            binding_id: format!("binding.{operation}.http"),
            request_schema: request_schema.to_owned(),
            request_schema_revision: request_revision,
            result_schema: result_schema.to_owned(),
            result_schema_revision: result_revision,
            cancellation: CancellationContract::cooperative(vec![
                CancellationPoint::BeforeAdmission,
                CancellationPoint::DuringRead,
            ])
            .expect("valid cancellation fixture"),
            deadline: DeadlineContract::new(2_500, DeadlineBehavior::ReturnOperationReceipt)
                .expect("valid deadline fixture"),
            pagination: Some(
                PaginationContract::new(20, 100, 60_000).expect("valid pagination fixture"),
            ),
            receipt: ReceiptContract::Operation,
            terminal_states: TerminalStateContract::new(vec![
                TerminalState::Completed,
                TerminalState::Cancelled,
                TerminalState::TimedOut,
                TerminalState::Failed,
                TerminalState::Partial,
            ])
            .expect("valid terminal state fixture"),
        }
    }

    fn schema_bodies(routes: &[HttpRouteDocumentV1]) -> BTreeMap<String, (Value, Value)> {
        routes
            .iter()
            .map(|route| {
                (
                    route.binding_id.clone(),
                    (
                        json!({
                            "type": "object",
                            "title": format!("{} request", route.operation),
                        }),
                        json!({
                            "type": "object",
                            "title": format!("{} result", route.operation),
                        }),
                    ),
                )
            })
            .collect()
    }

    fn document_value(routes: &[HttpRouteDocumentV1]) -> Value {
        let schemas = schema_bodies(routes);
        serde_json::to_value(
            openapi_document_from_schema_bodies(routes, &schemas).expect("valid route documents"),
        )
        .expect("serializable OpenAPI document")
    }

    #[test]
    fn paths_and_methods_are_derived_exactly_from_route_documents() {
        let routes = vec![
            route(
                "POST",
                "/context-scout/inspect",
                "inspect",
                "request.inspect",
                3,
                "result.inspect",
                5,
            ),
            route(
                "DELETE",
                "/context-scout/archive",
                "archive",
                "request.archive",
                2,
                "result.archive",
                4,
            ),
        ];

        let document = document_value(&routes);
        let paths = document["paths"].as_object().expect("OpenAPI paths object");
        let actual = paths
            .iter()
            .flat_map(|(path, item)| {
                item.as_object()
                    .expect("OpenAPI path item")
                    .keys()
                    .map(move |method| (path.clone(), method.clone()))
            })
            .collect::<BTreeSet<_>>();

        assert_eq!(
            actual,
            BTreeSet::from([
                ("/context-scout/archive".to_owned(), "delete".to_owned()),
                ("/context-scout/inspect".to_owned(), "post".to_owned()),
            ])
        );
        assert_eq!(
            paths["/context-scout/inspect"]["post"]["x-tracedecay-http-method"],
            "POST"
        );
        assert_eq!(
            paths["/context-scout/inspect"]["post"]["requestBody"]["content"]["application/json"]["schema"],
            json!({
                "allOf": [{
                    "$ref": "#/components/schemas/binding.inspect.http.request"
                }],
                "x-tracedecay-schema-id": "request.inspect",
                "x-tracedecay-schema-revision": 3
            })
        );
        assert_eq!(
            paths["/context-scout/inspect"]["post"]["responses"]["200"]["content"]["application/json"]
                ["schema"]["x-tracedecay-result-schema"],
            json!({
                "allOf": [{
                    "$ref": "#/components/schemas/binding.inspect.http.result"
                }],
                "schema_id": "result.inspect",
                "revision": 5
            })
        );
        assert_eq!(
            document["components"]["schemas"]["binding.inspect.http.request"],
            json!({
                "type": "object",
                "title": "inspect request"
            })
        );
        assert_eq!(
            document["components"]["schemas"]["binding.inspect.http.result"],
            json!({
                "type": "object",
                "title": "inspect result"
            })
        );
    }

    #[test]
    fn lifecycle_extensions_preserve_every_route_contract() {
        let route = route(
            "POST",
            "/context-scout/inspect",
            "inspect",
            "request.inspect",
            3,
            "result.inspect",
            5,
        );
        let expected = json!({
            "cancellation": route.cancellation,
            "deadline": route.deadline,
            "pagination": route.pagination,
            "receipt": route.receipt,
            "terminal_states": route.terminal_states,
        });

        let document = document_value(&[route]);

        assert_eq!(
            document["paths"]["/context-scout/inspect"]["post"]["x-tracedecay-lifecycle"],
            expected
        );
    }

    #[test]
    fn responses_reference_the_canonical_envelope_and_all_problem_statuses() {
        let document = document_value(&[route(
            "POST",
            "/context-scout/inspect",
            "inspect",
            "request.inspect",
            3,
            "result.inspect",
            5,
        )]);
        let operation = &document["paths"]["/context-scout/inspect"]["post"];

        assert_eq!(
            document["components"]["schemas"]["HttpJsonEnvelope"]["oneOf"],
            json!([
                {"$ref": "#/components/schemas/HttpJsonSuccessEnvelope"},
                {"$ref": "#/components/schemas/HttpJsonProblemEnvelope"}
            ])
        );
        assert_eq!(
            operation["responses"]["200"]["content"]["application/json"]["schema"]["allOf"],
            json!([
                {"$ref": "#/components/schemas/HttpJsonEnvelope"},
                {"$ref": "#/components/schemas/HttpJsonSuccessEnvelope"}
            ])
        );
        for status in ["400", "404", "408", "409", "422", "429", "503", "504"] {
            assert_eq!(
                operation["responses"][status]["$ref"], "#/components/responses/HttpProblem",
                "status {status}"
            );
        }
        assert_eq!(
            document["components"]["responses"]["HttpProblem"]["content"]["application/json"]["schema"]
                ["allOf"],
            json!([
                {"$ref": "#/components/schemas/HttpJsonEnvelope"},
                {"$ref": "#/components/schemas/HttpJsonProblemEnvelope"}
            ])
        );
        assert_eq!(
            operation["x-tracedecay-request-id"],
            json!({
                "locations": ["success_envelope", "problem_envelope"],
                "schema": {"$ref": "#/components/schemas/RequestId"},
                "source": "server_generated"
            })
        );
    }

    #[test]
    fn duplicate_routes_and_operation_ids_are_rejected() {
        let first = route(
            "POST",
            "/context-scout/inspect",
            "inspect",
            "request.inspect",
            3,
            "result.inspect",
            5,
        );
        let mut duplicate_route = first.clone();
        duplicate_route.operation = "inspect_duplicate".to_owned();
        let duplicate_schemas = schema_bodies(&[first.clone(), duplicate_route.clone()]);
        assert_eq!(
            openapi_document_from_schema_bodies(
                &[first.clone(), duplicate_route],
                &duplicate_schemas
            ),
            Err(OpenApiDocumentError::DuplicateRoute {
                method: "POST".to_owned(),
                path: "/context-scout/inspect".to_owned(),
            })
        );

        let conflicting_operation = route(
            "POST",
            "/context-scout/inspect-again",
            "inspect",
            "request.inspect",
            3,
            "result.inspect",
            5,
        );
        let conflict_schemas = schema_bodies(&[first.clone(), conflicting_operation.clone()]);
        assert_eq!(
            openapi_document_from_schema_bodies(&[first, conflicting_operation], &conflict_schemas),
            Err(OpenApiDocumentError::ConflictingOperationId {
                operation_id: "inspect".to_owned(),
                first_method: "POST".to_owned(),
                first_path: "/context-scout/inspect".to_owned(),
                second_method: "POST".to_owned(),
                second_path: "/context-scout/inspect-again".to_owned(),
            })
        );
    }

    #[test]
    fn serialization_is_byte_stable_across_input_order() {
        let first = route(
            "POST",
            "/context-scout/zeta",
            "zeta",
            "request.zeta",
            1,
            "result.zeta",
            1,
        );
        let second = route(
            "POST",
            "/context-scout/alpha",
            "alpha",
            "request.alpha",
            2,
            "result.alpha",
            4,
        );

        let schemas = schema_bodies(&[first.clone(), second.clone()]);
        let forward =
            openapi_document_from_schema_bodies(&[first.clone(), second.clone()], &schemas)
                .expect("valid forward document")
                .to_json_bytes()
                .expect("serializable forward document");
        let reverse = openapi_document_from_schema_bodies(&[second, first], &schemas)
            .expect("valid reverse document")
            .to_json_bytes()
            .expect("serializable reverse document");

        assert_eq!(forward, reverse);
    }
}
