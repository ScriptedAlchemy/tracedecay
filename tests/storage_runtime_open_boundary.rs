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
    #[serde(default)]
    scope: Option<String>,
    callee: String,
    disposition: String,
}

fn matches_qualified_suffix(value: &str, suffix: &str) -> bool {
    value == suffix
        || value
            .strip_suffix(suffix)
            .is_some_and(|prefix| prefix.ends_with("::"))
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
                .find(|suffix| matches_qualified_suffix(&entry.callee, suffix))
                .cloned()
                .unwrap_or_else(|| entry.callee.clone());
            ((entry.path.clone(), suffix, entry.scope.clone()), entry)
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
                .find(|suffix| matches_qualified_suffix(&call.callee, suffix))
            else {
                continue;
            };
            let exact_key = (path.clone(), suffix.clone(), Some(call.scope.clone()));
            let broad_key = (path.clone(), suffix.clone(), None);
            observed.insert(exact_key.clone());
            observed.insert(broad_key.clone());
            if !allowed.contains_key(&exact_key) && !allowed.contains_key(&broad_key) {
                violations.push(format!(
                    "{}:{} {} in {}",
                    path, call.line, call.callee, call.scope
                ));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "direct SQLite opens escaped the daemon/runtime allowlist:\n{}",
        violations.join("\n")
    );

    let mut stale = Vec::new();
    for (key, entry) in allowed {
        assert!(
            !entry.disposition.trim().is_empty(),
            "allowlisted direct open needs a deletion or permanent-owner disposition: {key:?}"
        );
        if !observed.contains(&key) {
            stale.push(format!("{key:?}"));
        }
    }
    assert!(
        stale.is_empty(),
        "stale direct-open allowlist entries must be removed with their call sites:\n{}",
        stale.join("\n")
    );
}

#[test]
fn files_below_cfg_test_parent_modules_are_not_production_sources() {
    let files = rust_files_below(&["src".to_owned()]);
    assert!(
        !files
            .iter()
            .any(|path| path == "src/sessions/claude_observation_benchmark/runner.rs"),
        "a child of a cfg(test) parent module must not enter the production-open scan"
    );
    assert!(
        files.iter().any(|path| path == "src/sessions/ingest.rs"),
        "the module-aware filter must retain ordinary production siblings"
    );
}

#[test]
fn direct_opens_under_test_items_are_not_production_sources() {
    let attachment = RustAst::parse("crates/tracedecay-rusqlite-runtime/src/graph/attachment.rs");
    assert!(
        !attachment
            .production_calls()
            .iter()
            .any(|call| call.scope == "create_identity_database"),
        "a direct SQLite open in a cfg(test) module must not enter the production-open scan"
    );
}

#[test]
fn direct_open_suffixes_match_complete_qualified_segments() {
    assert!(matches_qualified_suffix(
        "rusqlite::Connection::open_with_flags",
        "Connection::open_with_flags"
    ));
    assert!(!matches_qualified_suffix(
        "SnapshotConnection::open",
        "Connection::open"
    ));
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
