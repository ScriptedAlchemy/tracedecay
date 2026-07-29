//! Compile-graph isolation for packages that do not own code indexing.

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::path::PathBuf;
use std::process::Command;

use serde::Deserialize;

#[derive(Deserialize)]
struct CargoMetadata {
    packages: Vec<CargoPackage>,
    resolve: CargoResolve,
}

#[derive(Deserialize)]
struct CargoPackage {
    id: String,
    name: String,
    dependencies: Vec<CargoDependency>,
}

#[derive(Deserialize)]
struct CargoDependency {
    name: String,
}

#[derive(Deserialize)]
struct CargoResolve {
    nodes: Vec<CargoResolveNode>,
}

#[derive(Deserialize)]
struct CargoResolveNode {
    id: String,
    dependencies: Vec<String>,
}

fn cargo_metadata() -> CargoMetadata {
    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let output = Command::new(cargo)
        .current_dir(repository)
        .args(["metadata", "--format-version=1", "--no-default-features"])
        .output()
        .expect("run stock cargo metadata");
    assert!(
        output.status.success(),
        "cargo metadata failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("parse cargo metadata")
}

fn dependency_closure(metadata: &CargoMetadata, package_name: &str) -> BTreeSet<String> {
    let package_names = metadata
        .packages
        .iter()
        .map(|package| (package.id.as_str(), package.name.as_str()))
        .collect::<BTreeMap<_, _>>();
    let dependencies = metadata
        .resolve
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node.dependencies.as_slice()))
        .collect::<BTreeMap<_, _>>();
    let root = metadata
        .packages
        .iter()
        .find(|package| package.name == package_name)
        .unwrap_or_else(|| panic!("workspace package {package_name}"))
        .id
        .as_str();

    let mut pending = vec![root];
    let mut seen = BTreeSet::new();
    let mut names = BTreeSet::new();
    while let Some(package_id) = pending.pop() {
        if !seen.insert(package_id) {
            continue;
        }
        names.insert(
            package_names
                .get(package_id)
                .unwrap_or_else(|| panic!("resolved package {package_id}"))
                .to_string(),
        );
        if let Some(children) = dependencies.get(package_id) {
            pending.extend(children.iter().map(String::as_str));
        }
    }
    names
}

fn direct_dependencies(metadata: &CargoMetadata, package_name: &str) -> BTreeSet<String> {
    metadata
        .packages
        .iter()
        .find(|package| package.name == package_name)
        .unwrap_or_else(|| panic!("workspace package {package_name}"))
        .dependencies
        .iter()
        .map(|dependency| dependency.name.clone())
        .collect()
}

#[test]
fn non_indexing_packages_exclude_grammars_structural_search_and_root_indexer() {
    let metadata = cargo_metadata();
    let forbidden_exact = BTreeSet::from([
        "ast-grep-core",
        "tokensave-large-treesitters",
        "tokensave-medium-treesitters",
        "tracedecay",
        "tree-sitter",
        "tree-sitter-language",
    ]);

    for package in [
        "tracedecay-application",
        "tracedecay-domain",
        "tracedecay-policy",
        "tracedecay-store",
    ] {
        let closure = dependency_closure(&metadata, package);
        let violations = closure
            .iter()
            .filter(|dependency| {
                forbidden_exact.contains(dependency.as_str())
                    || dependency.starts_with("tree-sitter-")
            })
            .cloned()
            .collect::<Vec<_>>();
        assert!(
            violations.is_empty(),
            "{package} must not compile grammar, structural-search, or root code-index packages: {}",
            violations.join(", ")
        );
    }
}

#[test]
fn code_index_dependencies_are_explicit_and_root_free() {
    let metadata = cargo_metadata();
    let direct = direct_dependencies(&metadata, "tracedecay-code-index");
    let expected = [
        "ast-grep-core",
        "cc",
        "ignore",
        "serde",
        "serde_json",
        "sha2",
        "static_assertions",
        "tempfile",
        "thiserror",
        "tokensave-large-treesitters",
        "tokensave-medium-treesitters",
        "tracedecay-application",
        "tracedecay-domain",
        "tree-sitter",
        "tree-sitter-hlsl",
        "tree-sitter-language",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<BTreeSet<_>>();
    assert_eq!(direct, expected);
    assert!(!direct.contains("tracedecay"));
}

#[test]
fn root_uses_code_index_facades_instead_of_inline_sources() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for path in ["src/code_index", "src/extraction", "src/ast_grep_search.rs"] {
        assert!(
            !root.join(path).exists(),
            "root must not retain extracted code-index source at {path}"
        );
    }
    let lib = std::fs::read_to_string(root.join("src/lib.rs")).expect("read root lib facade");
    assert!(lib.contains("pub use tracedecay_code_index as code_index;"));
    assert!(lib.contains("pub use tracedecay_code_index::extraction;"));
    assert!(lib.contains("pub use tracedecay_code_index::ast_grep_search;"));
}

#[test]
fn query_dependencies_are_explicit_and_root_free() {
    let metadata = cargo_metadata();
    let direct = direct_dependencies(&metadata, "tracedecay-query");
    let expected = [
        "hex",
        "hmac",
        "serde",
        "serde_json",
        "sha2",
        "static_assertions",
        "thiserror",
        "tracedecay-code-index",
        "tracedecay-domain",
        "tracedecay-policy",
        "zeroize",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<BTreeSet<_>>();
    assert_eq!(direct, expected);
    assert!(!direct.contains("tracedecay"));
}

/// `tracedecay-query` depends on `tracedecay-code-index`, which depends on
/// `tracedecay-application`. The decision is that this application edge is
/// accepted transitively — the kernel needs code-index language and historical
/// contracts, and code-index legitimately owns its application ports — but the
/// kernel must never acquire a direct application dependency, and code-index
/// must remain the only hop that carries it. Query source paths are guarded
/// separately by
/// `query_kernel::query_source_guard_refuses_direct_application_layer_paths`.
#[test]
fn query_reaches_application_only_through_code_index() {
    let metadata = cargo_metadata();
    let direct = direct_dependencies(&metadata, "tracedecay-query");
    assert!(
        !direct.contains("tracedecay-application"),
        "tracedecay-query must not depend on tracedecay-application directly"
    );
    assert!(
        direct.contains("tracedecay-code-index"),
        "tracedecay-code-index is the admitted carrier of the application edge"
    );
    assert!(
        dependency_closure(&metadata, "tracedecay-query").contains("tracedecay-application"),
        "the transitive application edge is the condition this guard documents; \
         remove the guard rather than let it pass vacuously"
    );

    let carriers = direct
        .iter()
        .filter(|dependency| {
            dependency_closure(&metadata, dependency.as_str()).contains("tracedecay-application")
        })
        .cloned()
        .collect::<BTreeSet<_>>();
    assert_eq!(
        carriers,
        BTreeSet::from(["tracedecay-code-index".to_owned()]),
        "exactly one query dependency may carry tracedecay-application"
    );
}

#[test]
fn root_uses_query_facade_instead_of_inline_sources() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    assert!(
        !root.join("src/query").exists(),
        "root must not retain extracted query source"
    );
    let lib = std::fs::read_to_string(root.join("src/lib.rs")).expect("read root lib facade");
    assert!(lib.contains("pub use tracedecay_query as query;"));
}

#[test]
fn domain_dependencies_are_exactly_the_pure_value_allowlist() {
    let metadata = cargo_metadata();
    let direct = direct_dependencies(&metadata, "tracedecay-domain");
    let expected_direct = [
        "schemars",
        "serde",
        "serde_json",
        "sha2",
        "thiserror",
        "url",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<BTreeSet<_>>();
    assert_eq!(
        direct, expected_direct,
        "tracedecay-domain must not depend on I/O, stores, transports, providers, settings, credentials, lifecycle, UI, or the root crate"
    );
}

#[test]
fn policy_package_has_only_pure_value_dependencies() {
    let metadata = cargo_metadata();
    let policy = metadata
        .packages
        .iter()
        .find(|package| package.name == "tracedecay-policy")
        .expect("workspace policy package");
    let actual = policy
        .dependencies
        .iter()
        .map(|dependency| dependency.name.as_str())
        .collect::<BTreeSet<_>>();
    let expected = BTreeSet::from(["serde", "serde_json", "tracedecay-domain"]);

    assert_eq!(
        actual, expected,
        "policy must not depend on I/O, stores, transports, models, or configuration resolution"
    );
}

#[test]
fn store_dependencies_are_exactly_the_contract_allowlist() {
    let metadata = cargo_metadata();
    let direct = direct_dependencies(&metadata, "tracedecay-store");
    let expected_direct = ["serde", "serde_json", "thiserror", "tracedecay-domain"]
        .into_iter()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        direct, expected_direct,
        "tracedecay-store dependencies must remain exactly contract-only"
    );
}

#[test]
fn migrate_dependencies_are_exactly_the_planning_allowlist() {
    let metadata = cargo_metadata();
    let direct = direct_dependencies(&metadata, "tracedecay-migrate");
    let expected_direct = [
        "serde",
        "serde_json",
        "tempfile",
        "tracedecay-domain",
        "tracedecay-store",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<BTreeSet<_>>();
    assert_eq!(
        direct, expected_direct,
        "tracedecay-migrate plans migrations; it must not depend on databases, \
         lifecycle leases, daemons, transports, or the root crate"
    );
    assert!(!direct.contains("tracedecay"));
}

/// The migration package decides *what* to migrate and records how far an
/// attempt got. Acquiring the maintenance fence, opening a store, and driving
/// the rusqlite runtime stay in root, so a future edit must not quietly pull
/// that authority across the boundary.
#[test]
fn migrate_package_owns_no_store_or_lifecycle_authority() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let sources = rust_sources(&root.join("crates/tracedecay-migrate/src"));
    assert!(
        !sources.is_empty(),
        "expected tracedecay-migrate sources; a vacuous scan proves nothing"
    );
    // Guard the guard: stripping documentation must not also hide real code.
    assert!(
        strip_line_comments("use rusqlite::Connection;").contains("rusqlite"),
        "comment stripping must leave code that reaches store authority visible"
    );
    assert!(
        !strip_line_comments("//! drives the rusqlite runtime in root").contains("rusqlite"),
        "prose naming the boundary must not trip this guard"
    );
    for (path, source) in &sources {
        // Scan code only: these modules legitimately *name* the authority they
        // must not reach when documenting the boundary.
        let code = strip_line_comments(source);
        for forbidden in [
            "crate::db",
            "crate::global_db",
            "crate::lifecycle_lease",
            "crate::sqlite_read_snapshot",
            "crate::daemon",
            "rusqlite",
            "tracedecay_rusqlite",
        ] {
            assert!(
                !code.contains(forbidden),
                "{}: migration planning must not reach store or lifecycle authority: {forbidden}",
                path.display()
            );
        }
    }
}

fn strip_line_comments(source: &str) -> String {
    source
        .lines()
        .map(|line| line.split_once("//").map_or(line, |(code, _)| code))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn root_uses_migrate_facades_instead_of_inline_sources() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for path in [
        "src/migrate/durability.rs",
        "src/migrate/inventory/model.rs",
    ] {
        assert!(
            !root.join(path).exists(),
            "root must not retain extracted migration source at {path}"
        );
    }
    for (path, expected) in [
        (
            "src/migrate/mod.rs",
            "pub use tracedecay_migrate::durability;",
        ),
        (
            "src/migrate/inventory/mod.rs",
            "pub use tracedecay_migrate::inventory::*;",
        ),
        (
            "src/migrate/manifest.rs",
            "pub use tracedecay_migrate::manifest::{",
        ),
    ] {
        let source = std::fs::read_to_string(root.join(path))
            .unwrap_or_else(|error| panic!("read migration facade {path}: {error}"));
        assert!(
            source.contains(expected),
            "{path} must re-export the extracted module so caller paths stay stable"
        );
    }
}

fn rust_sources(dir: &std::path::Path) -> Vec<(PathBuf, String)> {
    let mut sources = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return sources;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            sources.extend(rust_sources(&path));
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            let source = std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
            sources.push((path, source));
        }
    }
    sources.sort_by(|left, right| left.0.cmp(&right.0));
    sources
}

#[test]
fn canonical_projector_is_pure_and_store_owned() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let source =
        std::fs::read_to_string(root.join("crates/tracedecay-store/src/canonical_projection.rs"))
            .expect("read canonical projector");
    for forbidden in [
        "crate::db",
        "crate::global_db",
        "rusqlite",
        "tracedecay_rusqlite",
        "tokio::",
    ] {
        assert!(
            !source.contains(forbidden),
            "canonical projector must not import DB/runtime authority: {forbidden}"
        );
    }
}

#[test]
fn observation_application_uses_narrow_capture_ports() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let source = std::fs::read_to_string(root.join("src/application/observation.rs"))
        .expect("read observation application");
    for required in [
        "ObservationCaptureSink",
        "ObservationCursorPort",
        "ObservationAdmissionPort",
    ] {
        assert!(
            source.contains(required),
            "observation application must use narrow authority: {required}"
        );
    }
    assert!(
        !source.contains("ObservationStore,"),
        "observation application must not depend on the aggregate store port"
    );
}

#[test]
fn observation_projection_owns_raw_storage_and_uses_store_records() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for path in [
        "src/global_db/observation_projection/apply.rs",
        "src/global_db/observation_projection/rebuild.rs",
        "src/global_db/observation_projection/state.rs",
    ] {
        let source = std::fs::read_to_string(root.join(path)).expect("read projection adapter");
        for forbidden in [
            "crate::sessions::SessionRecord",
            "crate::sessions::SessionMessageRecord",
            "crate::sessions::{SessionMessageRecord, SessionRecord}",
            "crate::sessions::lcm::raw",
        ] {
            assert!(
                !source.contains(forbidden),
                "{path} must not reach through sessions for projection storage: {forbidden}"
            );
        }
    }
}

#[test]
fn session_lcm_publication_and_rendering_do_not_reach_back_into_global_db() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let dag = std::fs::read_to_string(root.join("src/sessions/lcm/dag.rs"))
        .expect("read session LCM DAG");
    assert!(
        dag.contains("LcmSummaryPublicationPort"),
        "session LCM publication must use its narrow publication port"
    );
    assert!(
        !dag.contains("global_db"),
        "session LCM publication must not reach back into global_db"
    );

    let adapter = std::fs::read_to_string(
        root.join("src/global_db/session_temporal/operations/publication.rs"),
    )
    .expect("read global DB LCM publication adapter");
    assert!(
        adapter.contains("impl<E> LcmSummaryPublicationPort for GlobalDbLcmSummaryPublication"),
        "global_db must retain the concrete transaction-backed publication adapter"
    );
}

#[test]
fn lcm_compatibility_contracts_are_application_owned_and_db_free() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    assert!(
        !root.join("src/sessions/lcm/render.rs").exists(),
        "LCM rendering must not be duplicated back under the session store"
    );

    for path in [
        "src/application/session/lcm/contracts.rs",
        "src/application/session/lcm/render.rs",
    ] {
        let source = std::fs::read_to_string(root.join(path)).expect("read LCM contract module");
        for forbidden in [
            "crate::db",
            "crate::global_db",
            "crate::sessions",
            "ReadSnapshot",
            "Executor",
            "rusqlite",
        ] {
            assert!(
                !source.contains(forbidden),
                "{path} must stay DB-free and free of session-store dependencies: {forbidden}"
            );
        }
    }

    let contracts = std::fs::read_to_string(root.join("src/application/session/lcm/contracts.rs"))
        .expect("read LCM contracts");
    for owned in [
        "pub enum LcmError",
        "pub struct LcmContentRange",
        "pub struct LcmContentSlice",
        "pub struct LcmExpandResponse",
        "pub struct LcmDescribeResponse",
        "pub fn validate_payload_ref",
    ] {
        assert!(
            contracts.contains(owned),
            "the application session layer must own the LCM compatibility contract: {owned}"
        );
    }

    let types = std::fs::read_to_string(root.join("src/sessions/lcm/types.rs"))
        .expect("read session LCM types");
    assert!(
        types.contains("pub use crate::application::session::lcm::contracts::"),
        "sessions::lcm::types must re-export the application-owned contracts"
    );
    assert!(
        types.contains("impl From<crate::db::engine::Error> for LcmError"),
        "the SQL error mapping stays with the session store, not the application contract"
    );
}

#[test]
fn session_temporal_rendering_does_not_import_the_session_lcm_tree() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for path in [
        "src/global_db/session_temporal/mod.rs",
        "src/global_db/session_temporal/registered_lcm_render.rs",
        "src/global_db/session_temporal/direct.rs",
        "src/global_db/session_temporal/operations/compatibility.rs",
    ] {
        let source = std::fs::read_to_string(root.join(path)).expect("read temporal LCM adapter");
        let production = source
            .split_once("#[cfg(test)]")
            .map_or(source.as_str(), |(before, _)| before);
        assert!(
            !production.contains("crate::sessions::lcm::"),
            "{path} must reach LCM rendering contracts through the application layer"
        );
    }

    let renderer = std::fs::read_to_string(
        root.join("src/global_db/session_temporal/registered_lcm_render.rs"),
    )
    .expect("read registered LCM renderer");
    assert!(
        renderer.contains("use crate::application::session::lcm::render::apply_canonical_content;"),
        "the registered renderer must apply the application-owned canonical shaping"
    );
    assert!(
        renderer.contains("ReadSnapshot"),
        "the registered renderer keeps snapshot ownership; remove this guard if that changes"
    );
}

#[test]
fn lcm_store_uses_narrow_authorities_with_global_db_adapters() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let payload = std::fs::read_to_string(root.join("src/sessions/lcm/payload.rs"))
        .expect("read session LCM payload store");
    for required in [
        "pub trait LcmRawMessagePort",
        "pub trait LcmPayloadAuthorityPort",
    ] {
        assert!(
            payload.contains(required),
            "the session LCM payload store must declare its narrow authority: {required}"
        );
    }
    assert!(
        !payload.contains("RegisteredGlobalDb"),
        "the session LCM payload store must not name the concrete registered database"
    );

    let adapter = std::fs::read_to_string(root.join("src/global_db/registered_lcm.rs"))
        .expect("read registered LCM adapter");
    for required in [
        "impl payload::LcmRawMessagePort for RegisteredGlobalDb",
        "impl payload::LcmPayloadAuthorityPort for RegisteredGlobalDb",
        "self.read_snapshot()",
        "begin_write_transaction()",
    ] {
        assert!(
            adapter.contains(required),
            "global_db must retain the SQL and filesystem transaction adapter: {required}"
        );
    }
}

#[test]
fn transcript_ingest_core_uses_store_and_admission_ports() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for path in [
        "src/sessions/source.rs",
        "src/sessions/ingest/scheduler.rs",
        "src/sessions/ingest/user_provider.rs",
    ] {
        let source = std::fs::read_to_string(root.join(path)).expect("read transcript ingest core");
        for forbidden in ["RegisteredGlobalDb", "GlobalDbTranscriptStore"] {
            assert!(
                !source.contains(forbidden),
                "{path} must not depend on concrete global database storage: {forbidden}"
            );
        }
    }

    let claude = std::fs::read_to_string(root.join("src/sessions/claude_observation.rs"))
        .expect("read Claude observation coordinator");
    let claude_production = claude
        .split_once("#[cfg(test)]")
        .expect("Claude test module boundary")
        .0;
    for forbidden in ["RegisteredGlobalDb", "HostAdmissionFacade"] {
        assert!(
            !claude_production.contains(forbidden),
            "Claude production ingest must use ports: {forbidden}"
        );
    }
    assert!(claude_production.contains("ObservationCaptureAdmissionPort"));
    assert!(claude_production.contains("TranscriptCursorAdmissionPort"));

    let adapter = std::fs::read_to_string(root.join("src/store/global_db.rs"))
        .expect("read global database transcript adapter");
    assert!(adapter.contains("impl TranscriptIngestStore for GlobalDbTranscriptStore"));
    assert!(adapter.contains("record_session_ingest_activity"));
}

#[test]
fn application_dependencies_are_exactly_the_use_case_allowlist() {
    let metadata = cargo_metadata();
    let direct = direct_dependencies(&metadata, "tracedecay-application");
    let expected_direct = [
        "schemars",
        "serde",
        "serde_json",
        "thiserror",
        "tracedecay-domain",
        "tracedecay-policy",
        "tracedecay-tool-catalog",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<BTreeSet<_>>();
    assert_eq!(
        direct, expected_direct,
        "tracedecay-application must depend only on domain, policy, catalog, and value libraries"
    );
}

#[test]
fn api_dependencies_are_exactly_the_thin_adapter_allowlist() {
    let metadata = cargo_metadata();
    let direct = direct_dependencies(&metadata, "tracedecay-api");
    let expected_direct = [
        "axum",
        "futures-util",
        "schemars",
        "serde",
        "serde_json",
        "thiserror",
        "tracedecay-application",
        "tracedecay-tool-catalog",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<BTreeSet<_>>();
    assert_eq!(
        direct, expected_direct,
        "tracedecay-api must remain a transport adapter over application and catalog contracts"
    );
}
