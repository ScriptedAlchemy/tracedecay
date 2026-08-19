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
fn invocation_requests_keep_configuration_read_and_cas_inputs_typed() {
    let get = ConfigurationGetRequestV1 {
        key: tracedecay_domain::configuration::SettingKey::new("mcp.tool_timings").unwrap(),
    };
    let set = ConfigurationSetRequestV1 {
        layer: tracedecay_domain::configuration::ConfigurationLayerIdV1::Default,
        key: get.key.clone(),
        value: tracedecay_domain::configuration::ConfigurationValueV1::Boolean(true),
        expected_revision: tracedecay_domain::configuration::ConfigurationRevisionId::new(
            "revision.configuration-test",
        )
        .unwrap(),
        idempotency_key: tracedecay_domain::configuration::ConfigurationIdempotencyKey::new(
            "configuration.idempotency.test",
        )
        .unwrap(),
    };

    assert_eq!(get.key, set.key);
    assert!(matches!(
        set.value,
        tracedecay_domain::configuration::ConfigurationValueV1::Boolean(true)
    ));
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
