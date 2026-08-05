use std::collections::{BTreeMap, BTreeSet};

use axum::Router;
use axum::body::Bytes;
use axum::routing::get;
use serde_json::{Map, Value};
use tracedecay_application::ApplicationWireSchemaRegistryV1;
use tracedecay_tool_catalog::{BindingId, BindingSurface, SchemaBodyAuthorityV1};

use super::{
    HTTP_JSON_ENVELOPE_REF, HTTP_JSON_PROBLEM_REF, HTTP_JSON_SUCCESS_REF, HTTP_PROBLEM_STATUSES,
    OPENAPI_DOCUMENT_ROUTE_PATH, OpenApiDocumentError, OpenApiDocumentV1, OpenApiInfoV1,
    OpenApiRequestV1, OpenApiRouteDocumentV1, OpenApiRouterBuildError, OpenApiSuccessV1,
    contract_value, object, openapi_method, reference, serve_openapi, string, typed_outcome_schema,
};
use crate::HttpRouteDocumentV1;

/// Bind ordinary catalog-derived HTTP documents to their concrete wire
/// authorities before composing them with adapter-owned route families.
pub fn bind_http_route_documents(
    route_documents: &[HttpRouteDocumentV1],
    wire_schemas: &ApplicationWireSchemaRegistryV1,
) -> Result<Vec<OpenApiRouteDocumentV1>, OpenApiDocumentError> {
    route_documents
        .iter()
        .map(|route| {
            let binding_id = BindingId::new(route.binding_id.clone()).map_err(|_| {
                OpenApiDocumentError::InvalidBindingId {
                    binding_id: route.binding_id.clone(),
                }
            })?;
            let schema = wire_schemas.get(&binding_id).ok_or_else(|| {
                OpenApiDocumentError::MissingWireSchema {
                    binding_id: route.binding_id.clone(),
                }
            })?;
            if schema.surface() != BindingSurface::Http
                || schema.operation().as_str() != route.operation
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
            Ok(OpenApiRouteDocumentV1 {
                method: route.method,
                path: route.path.clone(),
                operation_id: route.operation.clone(),
                capability_id: Some(route.capability_id.clone()),
                binding_id: Some(route.binding_id.clone()),
                request: OpenApiRequestV1::Json(schema.request().clone()),
                success: OpenApiSuccessV1::ApplicationJson(schema.result().clone()),
                success_statuses: vec!["200"],
                lifecycle: object([
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
                ]),
            })
        })
        .collect()
}

/// Build the deterministic relative endpoint from already-bound route
/// authorities. Projection and encoding finish before the router is returned.
pub fn openapi_routes_router(
    routes: &[OpenApiRouteDocumentV1],
) -> Result<Router, OpenApiRouterBuildError> {
    let bytes = openapi_routes_document(routes)?
        .to_json_bytes()
        .map_err(OpenApiRouterBuildError::Encoding)?;
    Ok(Router::new()
        .route(OPENAPI_DOCUMENT_ROUTE_PATH, get(serve_openapi))
        .with_state(Bytes::from(bytes)))
}

/// Project all mounted route families into one OpenAPI 3.1 document.
pub fn openapi_routes_document(
    routes: &[OpenApiRouteDocumentV1],
) -> Result<OpenApiDocumentV1, OpenApiDocumentError> {
    let mut routes = routes.iter().collect::<Vec<_>>();
    routes.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.method.cmp(right.method))
            .then_with(|| left.operation_id.cmp(&right.operation_id))
    });
    let mut route_keys = BTreeSet::new();
    let mut operation_ids = BTreeMap::<String, (String, String)>::new();
    let mut paths = BTreeMap::<String, BTreeMap<String, Value>>::new();
    let mut schemas = base_schemas();

    for route in routes {
        let method = openapi_method(route.method).ok_or_else(|| {
            OpenApiDocumentError::UnsupportedMethod {
                method: route.method.to_owned(),
                path: route.path.clone(),
            }
        })?;
        if !route_keys.insert((method.to_owned(), route.path.clone())) {
            return Err(OpenApiDocumentError::DuplicateRoute {
                method: route.method.to_owned(),
                path: route.path.clone(),
            });
        }
        if let Some((first_method, first_path)) = operation_ids.get(&route.operation_id) {
            return Err(OpenApiDocumentError::ConflictingOperationId {
                operation_id: route.operation_id.clone(),
                first_method: first_method.clone(),
                first_path: first_path.clone(),
                second_method: route.method.to_owned(),
                second_path: route.path.clone(),
            });
        }
        operation_ids.insert(
            route.operation_id.clone(),
            (route.method.to_owned(), route.path.clone()),
        );

        install_route_schemas(route, &mut schemas)?;
        paths
            .entry(route.path.clone())
            .or_default()
            .insert(method.to_owned(), operation_document(route)?);
    }

    Ok(OpenApiDocumentV1 {
        openapi: "3.1.0",
        json_schema_dialect: "https://json-schema.org/draft/2020-12/schema",
        info: OpenApiInfoV1 {
            title: "TraceDecay HTTP API",
            version: "1",
        },
        paths,
        components: object([
            (
                "responses",
                object([("HttpProblem", problem_response_component())]),
            ),
            ("schemas", Value::Object(schemas)),
        ]),
    })
}

fn install_route_schemas(
    route: &OpenApiRouteDocumentV1,
    schemas: &mut Map<String, Value>,
) -> Result<(), OpenApiDocumentError> {
    if let OpenApiRequestV1::Json(authority) | OpenApiRequestV1::Query(authority) = &route.request {
        install_schema(schemas, &component_name(route, "request"), authority.body())?;
    }
    let authority = match &route.success {
        OpenApiSuccessV1::ApplicationJson(authority)
        | OpenApiSuccessV1::Json(authority)
        | OpenApiSuccessV1::ServerSentEvents(authority) => authority,
    };
    install_schema(schemas, &component_name(route, "result"), authority.body())
}

fn install_schema(
    schemas: &mut Map<String, Value>,
    component: &str,
    body: &Value,
) -> Result<(), OpenApiDocumentError> {
    let body = rebase_local_definitions(body.clone(), component);
    if let Some(existing) = schemas.get(component) {
        if existing != &body {
            return Err(OpenApiDocumentError::ConflictingSchema {
                component: component.to_owned(),
            });
        }
        return Ok(());
    }
    schemas.insert(component.to_owned(), body);
    Ok(())
}

fn rebase_local_definitions(value: Value, component: &str) -> Value {
    match value {
        Value::Array(values) => Value::Array(
            values
                .into_iter()
                .map(|value| rebase_local_definitions(value, component))
                .collect(),
        ),
        Value::Object(values) => Value::Object(
            values
                .into_iter()
                .map(|(key, value)| {
                    let value = if key == "$ref" {
                        let rebased = value
                            .as_str()
                            .and_then(|reference| reference.strip_prefix('#'))
                            .filter(|suffix| suffix.is_empty() || suffix.starts_with('/'))
                            .map(|suffix| {
                                string(format!(
                                    "#/components/schemas/{}{suffix}",
                                    json_pointer_segment(component),
                                ))
                            });
                        rebased.unwrap_or_else(|| rebase_local_definitions(value, component))
                    } else {
                        rebase_local_definitions(value, component)
                    };
                    (key, value)
                })
                .collect(),
        ),
        scalar => scalar,
    }
}

fn operation_document(route: &OpenApiRouteDocumentV1) -> Result<Value, OpenApiDocumentError> {
    let mut operation = Map::new();
    operation.insert("operationId".to_owned(), string(&route.operation_id));
    operation.insert("parameters".to_owned(), Value::Array(parameters(route)?));
    match &route.request {
        OpenApiRequestV1::Json(authority) => {
            operation.insert(
                "requestBody".to_owned(),
                object([
                    ("required", Value::Bool(true)),
                    (
                        "content",
                        object([(
                            "application/json",
                            object([(
                                "schema",
                                schema_reference(route, "request", Some(authority)),
                            )]),
                        )]),
                    ),
                ]),
            );
        }
        OpenApiRequestV1::Query(_) | OpenApiRequestV1::Empty => {}
    }

    let (description, media_type, schema) = match &route.success {
        OpenApiSuccessV1::ApplicationJson(authority) => (
            "Canonical application outcome",
            "application/json",
            application_success_schema(route, authority),
        ),
        OpenApiSuccessV1::Json(authority) => (
            "Canonical adapter outcome",
            "application/json",
            schema_reference(route, "result", Some(authority)),
        ),
        OpenApiSuccessV1::ServerSentEvents(authority) => (
            "Canonical operation event stream",
            "text/event-stream",
            schema_reference(route, "result", Some(authority)),
        ),
    };
    let mut responses = Map::new();
    for status in &route.success_statuses {
        responses.insert(
            (*status).to_owned(),
            object([
                ("description", string(description)),
                (
                    "content",
                    object([(media_type, object([("schema", schema.clone())]))]),
                ),
            ]),
        );
    }
    for status in HTTP_PROBLEM_STATUSES {
        responses
            .entry(status.to_owned())
            .or_insert_with(|| reference("#/components/responses/HttpProblem"));
    }
    operation.insert("responses".to_owned(), Value::Object(responses));
    operation.insert("x-tracedecay-http-method".to_owned(), string(route.method));
    operation.insert("x-tracedecay-lifecycle".to_owned(), route.lifecycle.clone());
    if let Some(binding_id) = &route.binding_id {
        operation.insert("x-tracedecay-binding-id".to_owned(), string(binding_id));
    }
    if let Some(capability_id) = &route.capability_id {
        operation.insert(
            "x-tracedecay-capability-id".to_owned(),
            string(capability_id),
        );
    }
    Ok(Value::Object(operation))
}

fn application_success_schema(
    route: &OpenApiRouteDocumentV1,
    authority: &SchemaBodyAuthorityV1,
) -> Value {
    let result_ref = component_ref(route, "result");
    object([
        (
            "allOf",
            Value::Array(vec![
                reference(HTTP_JSON_ENVELOPE_REF),
                reference(HTTP_JSON_SUCCESS_REF),
            ]),
        ),
        (
            "properties",
            object([(
                "value",
                object([(
                    "properties",
                    object([("outcome", typed_outcome_schema(&result_ref))]),
                )]),
            )]),
        ),
        (
            "x-tracedecay-result-schema",
            object([
                ("allOf", Value::Array(vec![reference(&result_ref)])),
                (
                    "schema_id",
                    string(authority.schema_ref().schema_id().as_str()),
                ),
                (
                    "revision",
                    Value::Number(authority.schema_ref().revision().into()),
                ),
            ]),
        ),
    ])
}

fn parameters(route: &OpenApiRouteDocumentV1) -> Result<Vec<Value>, OpenApiDocumentError> {
    let mut parameters = path_parameters(&route.path)?;
    if let OpenApiRequestV1::Query(authority) = &route.request {
        let object = authority.body().as_object().ok_or_else(|| {
            OpenApiDocumentError::InvalidQuerySchema {
                operation_id: route.operation_id.clone(),
            }
        })?;
        let properties = object
            .get("properties")
            .and_then(Value::as_object)
            .ok_or_else(|| OpenApiDocumentError::InvalidQuerySchema {
                operation_id: route.operation_id.clone(),
            })?;
        let required = object
            .get("required")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<BTreeSet<_>>()
            })
            .unwrap_or_default();
        parameters.extend(properties.keys().map(|name| {
            object([
                ("name", string(name)),
                ("in", string("query")),
                ("required", Value::Bool(required.contains(name.as_str()))),
                (
                    "schema",
                    reference(&format!(
                        "#/components/schemas/{}/properties/{}",
                        json_pointer_segment(&component_name(route, "request")),
                        json_pointer_segment(name)
                    )),
                ),
            ])
        }));
    }
    Ok(parameters)
}

fn path_parameters(path: &str) -> Result<Vec<Value>, OpenApiDocumentError> {
    let mut parameters = Vec::new();
    for segment in path.split('/') {
        if !segment.contains(['{', '}']) {
            continue;
        }
        let Some(name) = segment
            .strip_prefix('{')
            .and_then(|value| value.strip_suffix('}'))
            .filter(|value| !value.is_empty() && !value.contains(['{', '}']))
        else {
            return Err(OpenApiDocumentError::InvalidPathParameter {
                path: path.to_owned(),
            });
        };
        parameters.push(object([
            ("name", string(name)),
            ("in", string("path")),
            ("required", Value::Bool(true)),
            ("schema", object([("type", string("string"))])),
        ]));
    }
    Ok(parameters)
}

fn schema_reference(
    route: &OpenApiRouteDocumentV1,
    direction: &str,
    authority: Option<&SchemaBodyAuthorityV1>,
) -> Value {
    let mut schema = Map::new();
    schema.insert("$ref".to_owned(), string(component_ref(route, direction)));
    if let Some(authority) = authority {
        schema.insert(
            "x-tracedecay-schema-id".to_owned(),
            string(authority.schema_ref().schema_id().as_str()),
        );
        schema.insert(
            "x-tracedecay-schema-revision".to_owned(),
            Value::Number(authority.schema_ref().revision().into()),
        );
    }
    Value::Object(schema)
}

fn component_name(route: &OpenApiRouteDocumentV1, direction: &str) -> String {
    format!("{}.{}", route.component_stem(), direction)
}

fn component_ref(route: &OpenApiRouteDocumentV1, direction: &str) -> String {
    format!(
        "#/components/schemas/{}",
        json_pointer_segment(&component_name(route, direction))
    )
}

fn json_pointer_segment(value: &str) -> String {
    value.replace('~', "~0").replace('/', "~1")
}

fn base_schemas() -> Map<String, Value> {
    Map::from_iter([
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
    ])
}

fn problem_response_component() -> Value {
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
    ])
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::rebase_local_definitions;

    #[test]
    fn embedded_schema_references_resolve_inside_their_component() {
        let schema = json!({
            "properties": {
                "item": {"$ref": "#/$defs/Item"}
            },
            "$defs": {
                "Item": {
                    "properties": {
                        "nested": {"$ref": "#/$defs/Nested"}
                    }
                },
                "Nested": {"type": "string"}
            }
        });

        let rebased = rebase_local_definitions(schema, "binding/example.request");

        assert_eq!(
            rebased["properties"]["item"]["$ref"],
            "#/components/schemas/binding~1example.request/$defs/Item"
        );
        assert_eq!(
            rebased["$defs"]["Item"]["properties"]["nested"]["$ref"],
            "#/components/schemas/binding~1example.request/$defs/Nested"
        );
    }
}
