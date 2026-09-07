use super::*;

#[test]
fn configuration_surface_keeps_every_retained_operation_callable() {
    let contribution = configuration_surface_catalog_contribution().expect("contribution");
    assert_eq!(contribution.capabilities().len(), CONFIGURATION_SPECS.len());
    assert_eq!(
        contribution.executable_schemas().len(),
        CONFIGURATION_SPECS.len()
    );
    assert_eq!(
        contribution.bindings().len(),
        CONFIGURATION_SPECS.len() * CONFIGURATION_SURFACES.len()
    );
    assert!(
        contribution
            .capabilities()
            .iter()
            .all(|capability| capability.availability().is_callable())
    );
}

#[test]
fn configuration_executable_registry_binds_every_public_http_schema() {
    let contribution = configuration_surface_catalog_contribution().expect("contribution");
    let registry = configuration_executable_binding_registry().expect("registry");

    assert_eq!(registry.iter().count(), CONFIGURATION_SPECS.len());
    for spec in &CONFIGURATION_SPECS {
        let operation_id =
            OperationId::new(format!("operation.application.{}", spec.name)).unwrap();
        let binding = registry
            .get(&operation_id)
            .and_then(|availability| availability.binding())
            .expect("available configuration binding");
        let manifest = contribution
            .capabilities()
            .iter()
            .find(|manifest| manifest.capability_id() == binding.capability_id())
            .unwrap();
        assert_eq!(
            binding.request_schema().schema_ref(),
            manifest.request_schema()
        );
        assert_eq!(
            binding.result_schema().schema_ref(),
            manifest.result_schema()
        );
        assert_eq!(binding.terminal_states(), manifest.terminal_states());
        let requires_idempotency = binding
            .request_schema()
            .body()
            .get("required")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|required| {
                required
                    .iter()
                    .any(|field| field.as_str() == Some("idempotency_key"))
            });
        assert_eq!(
            requires_idempotency,
            spec.effect.is_effect(),
            "{} must expose caller idempotency exactly when it admits an effect",
            spec.name
        );
        assert!(matches!(
            binding.exposure(),
            RouteExposureV1::Public { binding_id, route_path }
                if binding_id.as_str() == format!("binding.http.{}.v1", spec.name)
                    && route_path == &format!("/application/configuration/{}", spec.name)
        ));
    }
}

#[test]
fn configuration_surface_exposes_the_dashboard_transport() {
    let contribution = configuration_surface_catalog_contribution().expect("contribution");
    let surfaces = contribution
        .bindings()
        .iter()
        .map(|binding| binding.surface())
        .collect::<std::collections::BTreeSet<_>>();

    assert_eq!(
        surfaces,
        std::collections::BTreeSet::from([
            BindingSurface::Cli,
            BindingSurface::Mcp,
            BindingSurface::Http,
            BindingSurface::Dashboard,
        ])
    );
}

#[test]
fn configuration_surface_requires_mounted_project_and_exact_layer_routes() {
    let contribution = configuration_surface_catalog_contribution().expect("contribution");

    for capability in contribution.capabilities() {
        assert!(
            capability.scope().requires(ScopeDimension::Project),
            "{} must not advertise a nonexistent projectless profile route",
            capability.capability_id()
        );
        assert!(
            capability
                .scope()
                .requires(ScopeDimension::ConfigurationLayer),
            "{} must route through an exact configuration-layer authority",
            capability.capability_id()
        );
    }
}

#[test]
fn exported_configuration_operation_names_match_the_catalog_specs() {
    assert_eq!(
        CONFIGURATION_SPECS
            .iter()
            .map(|spec| spec.name)
            .collect::<Vec<_>>(),
        CONFIGURATION_SURFACE_OPERATION_NAMES
    );
}

#[test]
fn empty_configuration_requests_reject_transport_arguments() {
    assert!(
        serde_json::from_value::<ConfigurationListRequestV1>(serde_json::json!({"format": "json"}))
            .is_err()
    );
    assert!(
        serde_json::from_value::<ConfigurationObservedStateRequestV1>(
            serde_json::json!({"page_size": 10})
        )
        .is_err()
    );
}

#[test]
fn configuration_schema_refs_reject_unknown_operations() {
    assert!(configuration_surface_request_schema("configuration_get").is_ok());
    assert!(configuration_surface_result_schema("configuration_get").is_ok());
    assert!(configuration_surface_request_schema("configuration_unknown").is_err());
}

#[test]
fn invocation_payload_decode_wraps_stripped_get_and_set_bodies() {
    let get = configuration_wire_request_from_invocation_payload(
        "configuration_get",
        serde_json::json!({"key": "mcp.tool_timings"}),
    )
    .expect("stripped get payload");
    assert!(matches!(
        get,
        ConfigurationWireRequestV1::Get(request) if request.key.as_str() == "mcp.tool_timings"
    ));

    let set = configuration_wire_request_from_invocation_payload(
        "configuration_set",
        serde_json::json!({
            "layer": {"kind": "default"},
            "key": "mcp.tool_timings",
            "value": {"kind": "boolean", "value": true},
            "expected_revision": "revision.test-configuration-set",
            "idempotency_key": "configuration.idempotency.test-set"
        }),
    )
    .expect("stripped set payload");
    assert!(matches!(set, ConfigurationWireRequestV1::Set(_)));
}

#[test]
fn invocation_payload_decode_rejects_the_tagged_envelope() {
    let error = configuration_wire_request_from_invocation_payload(
        "configuration_get",
        serde_json::json!({
            "operation": "get",
            "request": {"key": "mcp.tool_timings"}
        }),
    )
    .expect_err("envelope must not admit as a get body");
    assert_eq!(
        error,
        ApplicationContractError::Inconsistent {
            field: "configuration surface request",
        }
    );
}

#[test]
fn invocation_payload_decode_covers_every_configuration_operation_name() {
    for name in CONFIGURATION_SURFACE_OPERATION_NAMES {
        match configuration_wire_request_from_invocation_payload(name, serde_json::json!({})) {
            Ok(_) => {}
            Err(ApplicationContractError::Inconsistent {
                field: "configuration surface request",
            }) => {}
            Err(error) => panic!("{name} must be a known configuration operation: {error}"),
        }
    }
    assert!(matches!(
        configuration_wire_request_from_invocation_payload(
            "configuration_unknown",
            serde_json::json!({}),
        ),
        Err(ApplicationContractError::Inconsistent {
            field: "configuration surface operation",
        })
    ));
}
