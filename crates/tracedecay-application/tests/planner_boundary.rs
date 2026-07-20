const MANIFEST: &str = include_str!("../Cargo.toml");
const PORTS: &str = include_str!("../src/retrieval/ports.rs");
const SERVICES: &str = include_str!("../src/retrieval/service.rs");

#[test]
fn application_dependencies_exclude_transports_stores_and_model_runtimes() {
    for forbidden in [
        "axum",
        "hyper",
        "libsql",
        "reqwest",
        "tokio",
        "tower",
        "tracedecay-store",
    ] {
        assert!(
            !MANIFEST.contains(forbidden),
            "application manifest must not depend on {forbidden}"
        );
    }
}

#[test]
fn retrieval_ports_and_services_have_no_planner_or_catalog_dispatch_path() {
    for forbidden in [
        "CatalogSnapshot",
        "Dispatcher",
        "ModelRuntime",
        "Planner",
        "UniversalRetrieval",
    ] {
        assert!(
            !PORTS.contains(forbidden) && !SERVICES.contains(forbidden),
            "application retrieval must not contain {forbidden}"
        );
    }
}
