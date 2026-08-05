use std::collections::BTreeSet;

use tracedecay_api::{
    OpenApiDocumentError, OpenApiRouteDocumentV1, OpenApiRouterBuildError,
    bind_http_route_documents, http_route_documents, multi_root_openapi_route_documents,
    openapi_routes_router, operation_openapi_route_documents, work_openapi_route_documents,
    workflow_openapi_route_documents,
};
use tracedecay_application::APPLICATION_DEFAULT_PROFILE_ID;
use tracedecay_tool_catalog::{ProfileId, SchemaBodyAuthorityV1, SchemaId, SchemaRef};

use super::{
    APPLICATION_PROTOCOL_REVISION, HttpOperationCancelResponse, HttpOperationEventQuery,
    application_surface_catalog_ref, wire_schema::build_application_wire_schema_registry,
};

/// Build the complete relative OpenAPI endpoint from the same authorities used
/// to construct every executable application route family.
pub(crate) fn router() -> Result<axum::Router, OpenApiRouterBuildError> {
    openapi_routes_router(&route_documents()?)
}

fn route_documents() -> Result<Vec<OpenApiRouteDocumentV1>, OpenApiDocumentError> {
    let catalog = application_surface_catalog_ref().map_err(|error| schema_error(error))?;
    let profile =
        ProfileId::new(APPLICATION_DEFAULT_PROFILE_ID).map_err(|error| schema_error(error))?;
    let authorized_capabilities = catalog
        .capabilities()
        .map(|capability| capability.capability_id().clone())
        .collect::<BTreeSet<_>>();
    let available_scope = catalog
        .capabilities()
        .flat_map(|capability| capability.scope().dimensions().iter().copied())
        .collect::<BTreeSet<_>>();
    let mut negotiated_features = BTreeSet::new();
    for capability in catalog.capabilities() {
        negotiated_features.extend(capability.required_features().iter().cloned());
        for binding_id in capability.binding_ids() {
            if let Some(binding) = catalog.binding(binding_id) {
                negotiated_features.extend(binding.required_features().iter().cloned());
            }
        }
    }
    let main_routes = http_route_documents(
        catalog,
        &profile,
        &authorized_capabilities,
        &available_scope,
        &negotiated_features,
        APPLICATION_PROTOCOL_REVISION,
    );
    let wire_schemas =
        build_application_wire_schema_registry(catalog).map_err(|error| schema_error(error))?;
    let mut routes = bind_http_route_documents(&main_routes, &wire_schemas)?;
    routes.extend(work_openapi_route_documents()?);
    routes.extend(workflow_openapi_route_documents()?);
    routes.extend(multi_root_openapi_route_documents()?);
    routes.extend(operation_openapi_route_documents(
        schema_authority::<HttpOperationEventQuery>("schema.operation.events.query")?,
        schema_authority::<HttpOperationCancelResponse>("schema.operation.cancel.result")?,
    )?);
    Ok(routes)
}

fn schema_authority<T: schemars::JsonSchema>(
    schema_id: &str,
) -> Result<SchemaBodyAuthorityV1, OpenApiDocumentError> {
    let schema_id = SchemaId::new(schema_id.to_owned()).map_err(|error| schema_error(error))?;
    let schema_ref = SchemaRef::new(schema_id, 1).map_err(|error| schema_error(error))?;
    SchemaBodyAuthorityV1::for_type::<T>(schema_ref).map_err(|error| schema_error(error))
}

fn schema_error(error: impl std::fmt::Display) -> OpenApiDocumentError {
    OpenApiDocumentError::SchemaAuthority {
        family: "application",
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use serde_json::Value;
    use tracedecay_api::{
        MultiRootHttpOperation, OpenApiRequestV1, OpenApiSuccessV1, WorkOperation,
        WorkflowOperation, openapi_routes_document,
    };

    use super::{
        HttpOperationCancelResponse, HttpOperationEventQuery, route_documents, schema_authority,
    };

    #[test]
    fn complete_document_tracks_every_mounted_descriptor_family() {
        let routes = route_documents().unwrap();
        let actual = routes
            .iter()
            .map(|route| (route.operation_id().to_owned(), route.path().to_owned()))
            .collect::<BTreeSet<_>>();

        for operation in WorkOperation::ALL {
            assert!(actual.contains(&(
                operation.operation_id_str().to_owned(),
                operation.route_path().to_owned(),
            )));
        }
        for operation in WorkflowOperation::ALL {
            assert!(actual.contains(&(
                operation.operation_id_str().to_owned(),
                operation.route_path().to_owned(),
            )));
        }
        for operation in MultiRootHttpOperation::ALL {
            assert!(actual.contains(&(
                operation.operation_id().to_owned(),
                operation.route_path().to_owned(),
            )));
        }
        assert!(actual.contains(&(
            "operation.events.subscribe".to_owned(),
            tracedecay_api::operation::OPERATION_EVENTS_ROUTE_PATH.to_owned(),
        )));
        assert!(actual.contains(&(
            "operation.events.cancel".to_owned(),
            tracedecay_api::operation::OPERATION_CANCEL_ROUTE_PATH.to_owned(),
        )));
    }

    #[test]
    fn every_json_success_models_its_result_component_directly() {
        let routes = route_documents().unwrap();
        let document = serde_json::to_value(openapi_routes_document(&routes).unwrap()).unwrap();
        for route in &routes {
            if !matches!(route.success(), OpenApiSuccessV1::ApplicationJson(_)) {
                continue;
            }
            let method = route.method().to_ascii_lowercase();
            let schema = &document["paths"][route.path()][method]["responses"]
                [route.success_statuses()[0]]["content"]["application/json"]["schema"];
            let result_ref = &schema["properties"]["value"]["properties"]["outcome"]["oneOf"][0]["properties"]
                ["value"]["properties"]["payload"]["oneOf"][0]["$ref"];
            assert!(
                matches!(result_ref, Value::String(reference) if reference.starts_with("#/components/schemas/")),
                "{} has no directly modeled result",
                route.operation_id()
            );
        }
    }

    #[test]
    fn complete_document_is_stable_and_preserves_event_stream_media() {
        let mut routes = route_documents().unwrap();
        let forward = openapi_routes_document(&routes)
            .unwrap()
            .to_json_bytes()
            .unwrap();
        routes.reverse();
        let reverse = openapi_routes_document(&routes)
            .unwrap()
            .to_json_bytes()
            .unwrap();
        assert_eq!(forward, reverse);

        let document: Value = serde_json::from_slice(&forward).unwrap();
        assert_eq!(
            document["paths"][tracedecay_api::operation::OPERATION_EVENTS_ROUTE_PATH]["get"]["responses"]
                ["200"]["content"]["text/event-stream"]["schema"]["$ref"],
            "#/components/schemas/operation.events.subscribe.result"
        );
    }

    #[test]
    fn event_documents_bind_the_mounted_paths_and_wire_types() {
        let routes = route_documents().unwrap();
        let events = routes
            .iter()
            .find(|route| route.operation_id() == "operation.events.subscribe")
            .unwrap();
        assert_eq!(
            events.path(),
            tracedecay_api::operation::OPERATION_EVENTS_ROUTE_PATH
        );
        assert_eq!(
            events.request(),
            &OpenApiRequestV1::Query(
                schema_authority::<HttpOperationEventQuery>("schema.operation.events.query")
                    .unwrap()
            )
        );

        let cancel = routes
            .iter()
            .find(|route| route.operation_id() == "operation.events.cancel")
            .unwrap();
        assert_eq!(
            cancel.path(),
            tracedecay_api::operation::OPERATION_CANCEL_ROUTE_PATH
        );
        assert_eq!(cancel.request(), &OpenApiRequestV1::Empty);
        assert_eq!(
            cancel.success(),
            &OpenApiSuccessV1::Json(
                schema_authority::<HttpOperationCancelResponse>("schema.operation.cancel.result")
                    .unwrap()
            )
        );
    }
}
