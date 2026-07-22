#[path = "fixtures/storage_runtime/source_ast.rs"]
mod source_ast;

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;

use source_ast::{RustAst, has_call_suffix, rust_files_below};

const OPEN_ALLOWLIST: &str = include_str!("fixtures/storage_runtime/direct_open_allowlist.json");

#[derive(Debug, Deserialize)]
struct OpenBoundaryFixture {
    scan_roots: Vec<String>,
    direct_open_suffixes: Vec<String>,
    allowed: Vec<AllowedOpen>,
}

#[derive(Debug, Deserialize)]
struct AllowedOpen {
    path: String,
    callee: String,
    disposition: String,
}

#[test]
fn concrete_sqlite_opens_are_closed_over_an_explicit_deletion_list() {
    let fixture: OpenBoundaryFixture =
        serde_json::from_str(OPEN_ALLOWLIST).expect("decode direct-open allowlist");
    let allowed = fixture
        .allowed
        .iter()
        .map(|entry| {
            let suffix = fixture
                .direct_open_suffixes
                .iter()
                .find(|suffix| entry.callee.ends_with(suffix.as_str()))
                .cloned()
                .unwrap_or_else(|| entry.callee.clone());
            ((entry.path.clone(), suffix), entry)
        })
        .collect::<BTreeMap<_, _>>();
    let mut observed = BTreeSet::new();
    let mut violations = Vec::new();

    for path in rust_files_below(&fixture.scan_roots) {
        let ast = RustAst::parse(&path);
        for call in ast.production_calls() {
            let Some(suffix) = fixture
                .direct_open_suffixes
                .iter()
                .find(|suffix| call.callee.ends_with(suffix.as_str()))
            else {
                continue;
            };
            let key = (path.clone(), suffix.clone());
            observed.insert(key.clone());
            if !allowed.contains_key(&key) {
                violations.push(format!("{}:{} {}", path, call.line, call.callee));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "direct SQLite opens escaped the daemon/runtime allowlist:\n{}",
        violations.join("\n")
    );

    for (key, entry) in allowed {
        assert!(
            !entry.disposition.trim().is_empty(),
            "allowlisted direct open needs a deletion or permanent-owner disposition: {key:?}"
        );
        assert!(
            observed.contains(&key),
            "stale direct-open allowlist entry must be removed with its call site: {key:?}"
        );
    }
}

#[test]
fn registry_publisher_attaches_real_physical_runtime_parts() {
    let ports = RustAst::parse("src/daemon/store_runtime/registry/ports.rs");
    let publisher_calls = ports.method_calls("LifecycleShardRuntimePublisher", "publish");
    assert!(
        has_call_suffix(&publisher_calls, ".attach"),
        "the canonical registry publisher must invoke a real ShardRuntimeAttachment"
    );
    assert!(
        !ports
            .method_identifiers("LifecycleShardRuntimePublisher", "publish")
            .contains("EmptyPhysicalRuntimeAttachment"),
        "the live publisher must not substitute an empty physical attachment"
    );

    let attachment = RustAst::parse("src/daemon/store_runtime/registry/attachment.rs");
    let attachment_methods = attachment.trait_methods("PhysicalRuntimeAttachment");
    for required in ["snapshot", "drain", "close_and_join"] {
        assert!(
            attachment_methods.contains(required),
            "physical attachment contract omitted {required}"
        );
    }

    let registry = RustAst::parse("src/daemon/store_runtime/registry.rs");
    assert!(
        has_call_suffix(
            &registry.method_calls("StoreRuntimeHandle", "physical_snapshot"),
            ".snapshot"
        ),
        "registry handles must sample the attached physical runtime"
    );
    assert!(
        has_call_suffix(
            &registry.method_calls("StoreRuntimeRegistry", "inventory"),
            ".physical_snapshot"
        ),
        "registry inventory must report physical writer/reader/WAL state"
    );
}
