#[path = "fixtures/storage_runtime/source_ast.rs"]
mod source_ast;

use std::collections::BTreeSet;

use serde::Deserialize;

use source_ast::{RustAst, has_call_suffix, has_path_suffix};

const S8_ROUTES: &str = include_str!("fixtures/storage_runtime/s8_cutover_routes.json");

#[derive(Debug, Deserialize)]
struct S8CutoverFixture {
    repository_module: String,
    read_vocabulary_module: String,
    parity_fixture: String,
    families: Vec<FamilyFixture>,
    write_routes: Vec<Route>,
    read_routes: Vec<Route>,
    // Read vocabulary variants that this repository executor explicitly does
    // not own (their execute arm rejects rather than routing to a family
    // executor). Tracked so the vocabulary stays fully accounted for.
    #[serde(default)]
    read_unowned_variants: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct FamilyFixture {
    family: String,
    write_payloads: Vec<String>,
    read_operations: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct Route {
    variant: String,
    callee: String,
}

#[test]
fn profile_project_and_session_writes_share_the_closed_runtime_route() {
    let fixture: S8CutoverFixture =
        serde_json::from_str(S8_ROUTES).expect("decode S8 route fixture");
    let repository = RustAst::parse(&fixture.repository_module);
    let paths = repository.method_paths("ConcreteRepositoryWriteExecutor", "execute");
    let calls = repository.method_calls("ConcreteRepositoryWriteExecutor", "execute");

    for route in fixture.write_routes {
        assert!(
            has_path_suffix(
                &paths,
                &format!("RepositoryWritePayloadV1::{}", route.variant)
            ),
            "S8 write route omitted payload variant {}",
            route.variant
        );
        assert!(
            has_call_suffix(&calls, &route.callee),
            "S8 write route {} omitted executor call {}",
            route.variant,
            route.callee
        );
    }
}

#[test]
fn profile_project_and_session_reads_share_the_closed_runtime_route() {
    let fixture: S8CutoverFixture =
        serde_json::from_str(S8_ROUTES).expect("decode S8 route fixture");
    let repository = RustAst::parse(&fixture.repository_module);
    let mut expected_variants = fixture
        .read_routes
        .iter()
        .map(|route| route.variant.clone())
        .collect::<BTreeSet<_>>();
    expected_variants.extend(fixture.read_unowned_variants.iter().cloned());
    // The read operation vocabulary now lives in the `tracedecay-store` runtime
    // port, re-exported through the repository module; assert against its
    // definition site while the executor routing below stays on the module.
    //
    // Subset, not equality: the fixture pins the routes that must exist, the
    // same way the write side above does. Production may add read operations
    // ahead of the fixture without that being a regression, and the per-route
    // dispatch loops below still prove every declared route is handled.
    let vocabulary = RustAst::parse(&fixture.read_vocabulary_module);
    let published_variants = vocabulary.enum_variants("RepositoryReadOperationV1");
    let missing_variants = expected_variants
        .difference(&published_variants)
        .cloned()
        .collect::<Vec<_>>();
    assert!(
        missing_variants.is_empty(),
        "S8 repository read vocabulary dropped declared route variants: {missing_variants:?}"
    );

    let paths = repository.method_paths("ConcreteRepositoryReadExecutor", "execute");
    let calls = repository.method_calls("ConcreteRepositoryReadExecutor", "execute");
    // Unowned variants must still be handled explicitly by the executor (their
    // arm rejects), never silently fall through the read dispatch.
    for variant in &fixture.read_unowned_variants {
        assert!(
            has_path_suffix(&paths, &format!("RepositoryReadOperationV1::{variant}")),
            "S8 unowned read variant {variant} is not explicitly handled by the executor"
        );
    }
    for route in fixture.read_routes {
        assert!(
            has_path_suffix(
                &paths,
                &format!("RepositoryReadOperationV1::{}", route.variant)
            ),
            "S8 read route omitted family variant {}",
            route.variant
        );
        assert!(
            has_call_suffix(&calls, &route.callee),
            "S8 read route {} omitted executor call {}",
            route.variant,
            route.callee
        );
    }
}

#[test]
fn s8_parity_inventory_covers_every_declared_route() {
    let fixture: S8CutoverFixture =
        serde_json::from_str(S8_ROUTES).expect("decode S8 route fixture");
    let parity = RustAst::parse(&fixture.parity_fixture);
    let literals = parity.const_string_literals("PRE_CUTOVER_ADAPTER_PARITY_FIXTURES_V1");
    let mut expected = BTreeSet::new();
    for family in fixture.families {
        expected.insert(family.family);
        expected.extend(family.write_payloads);
        expected.extend(family.read_operations);
    }

    let missing = expected.difference(&literals).cloned().collect::<Vec<_>>();
    assert!(
        missing.is_empty(),
        "S8 parity inventory omitted declared family routes: {missing:?}"
    );
}

#[test]
fn s8_attachment_bundle_is_inert_until_registry_mount() {
    let fixture: S8CutoverFixture =
        serde_json::from_str(S8_ROUTES).expect("decode S8 route fixture");
    let repository = RustAst::parse(&fixture.repository_module);
    assert!(
        repository
            .item_names("struct_item")
            .contains("PreCutoverRepositoryAttachmentBundle")
    );
    let calls = repository.method_calls("PreCutoverRepositoryAttachmentBundle", "new");
    assert!(
        calls.iter().all(|call| {
            !call.callee.ends_with("Connection::open")
                && !call.callee.ends_with("Connection::open_with_flags")
                && !call.callee.ends_with("Builder::new_local")
        }),
        "constructing the S8 bundle must not create a second storage authority"
    );
}

#[test]
fn s8_production_mount_exposes_repository_and_health_data_ports() {
    let attachment =
        RustAst::parse("crates/tracedecay-rusqlite-runtime/src/repository/attachment.rs");
    let factory_methods = attachment.method_names("RepositoryPhysicalAttachmentFactory");
    assert!(
        factory_methods.contains("attach"),
        "S8 production cutover requires a real repository attachment factory"
    );
    let attachment_methods = attachment.method_names("RepositoryRuntimePhysicalAttachment");
    for required in [
        "dispatch_submit",
        "dispatch_read",
        "drain",
        "close_and_join",
        "snapshot",
    ] {
        assert!(
            attachment_methods.contains(required),
            "S8 production attachment omitted data-port method {required}"
        );
    }
    let health_paths = attachment.method_paths("RepositoryRuntimeReadExecutor", "execute_read");
    assert!(
        has_path_suffix(&health_paths, "RuntimeReadOperationV1::TemporalHealth"),
        "S8 health data port must dispatch TemporalHealth on the reserved reader"
    );

    let ports = RustAst::parse("src/daemon/store_runtime/registry/ports.rs");
    let publish_ids = ports.method_identifiers("LifecycleShardRuntimePublisher", "publish");
    assert!(
        publish_ids.contains("RepositoryPhysicalAttachmentFactory"),
        "live S8 publisher must mount RepositoryPhysicalAttachmentFactory"
    );
    let publish_calls = ports.method_calls("LifecycleShardRuntimePublisher", "publish");
    assert!(
        has_call_suffix(&publish_calls, ".attach"),
        "live S8 publisher must attach a real physical repository runtime"
    );
}
