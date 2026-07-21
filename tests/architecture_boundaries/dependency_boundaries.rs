//! Module dependency boundary guards.
//!
//! Scans first-party session/application/domain/store sources for forbidden
//! import path prefixes so layering rules (ports vs adapters, runtime-free
//! contracts) hold outside the query kernel as well.

use crate::manifest::filesystem_rust_sources;
use crate::module_scanner::tokenize;
use crate::query_kernel::{scan_extern_crate_bindings, scan_qualified_paths, scan_use_bindings};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

fn path_matches_forbidden_prefix(path: &[String], prefixes: &[&[&str]]) -> Option<String> {
    for prefix in prefixes {
        if path.len() >= prefix.len()
            && path
                .iter()
                .zip(prefix.iter())
                .all(|(segment, expected)| segment == *expected)
        {
            return Some(prefix.join("::"));
        }
    }
    None
}

fn forbidden_path_violations(source: &str, path: &Path, prefixes: &[&[&str]]) -> BTreeSet<String> {
    let tokens = tokenize(source);
    let mut violations = BTreeSet::new();
    for binding in scan_use_bindings(&tokens) {
        if let Some(forbidden) = path_matches_forbidden_prefix(&binding.path, prefixes) {
            violations.insert(format!(
                "{}: imports forbidden path {forbidden}",
                path.display()
            ));
        }
    }
    for binding in scan_extern_crate_bindings(&tokens) {
        if let Some(forbidden) = path_matches_forbidden_prefix(&binding.path, prefixes) {
            violations.insert(format!(
                "{}: extern crate forbidden path {forbidden}",
                path.display()
            ));
        }
    }
    for (_, qualified) in scan_qualified_paths(&tokens) {
        if let Some(forbidden) = path_matches_forbidden_prefix(&qualified, prefixes) {
            violations.insert(format!(
                "{}: references forbidden path {forbidden}",
                path.display()
            ));
        }
    }
    violations
}

fn scan_sources_for_forbidden_paths(
    repository: &Path,
    sources: &BTreeSet<PathBuf>,
    prefixes: &[&[&str]],
) -> Result<BTreeSet<String>, String> {
    let mut violations = BTreeSet::new();
    for path in sources {
        let absolute = repository.join(path);
        let source = fs::read_to_string(&absolute)
            .map_err(|error| format!("cannot read {}: {error}", absolute.display()))?;
        violations.extend(forbidden_path_violations(&source, path, prefixes));
    }
    Ok(violations)
}

#[test]
fn application_session_depends_on_ports_not_adapters() {
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let roots = [PathBuf::from("src/application/session")]
        .into_iter()
        .collect::<BTreeSet<_>>();
    let sources =
        filesystem_rust_sources(&repository, &roots).expect("resolve application session sources");
    assert!(
        !sources.is_empty(),
        "application session sources must exist"
    );

    let forbidden: &[&[&str]] = &[
        &["crate", "global_db"],
        &["crate", "store"],
        &["crate", "daemon"],
        &["crate", "mcp"],
        &["crate", "sessions"],
        &["libsql"],
        &["rusqlite"],
        &["sqlx"],
        &["tokio"],
        &["async_std"],
    ];
    let violations = scan_sources_for_forbidden_paths(&repository, &sources, forbidden)
        .expect("inspect application session sources");
    assert!(
        violations.is_empty(),
        "application/session must depend on ports/contracts, not adapters/runtimes:\n{}",
        violations.into_iter().collect::<Vec<_>>().join("\n")
    );
}

#[test]
fn pr8_temporal_read_surfaces_cannot_import_refresh_or_writer_authorities() {
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let sources = [
        PathBuf::from("src/application/session/retrieval.rs"),
        PathBuf::from("src/application/session/ports.rs"),
        PathBuf::from("src/application/session/types.rs"),
    ]
    .into_iter()
    .collect::<BTreeSet<_>>();

    let forbidden: &[&[&str]] = &[
        &["crate", "application", "session", "refresh"],
        &["super", "refresh"],
        &["crate", "global_db"],
        &["crate", "store"],
        &["crate", "daemon"],
        &["crate", "mcp"],
        &["crate", "sessions", "ingest"],
        &["libsql"],
        &["rusqlite"],
        &["sqlx"],
    ];
    let violations = scan_sources_for_forbidden_paths(&repository, &sources, forbidden)
        .expect("inspect PR8 temporal read surfaces");
    assert!(
        violations.is_empty(),
        "PR8 temporal read surfaces must stay free of refresh/writer authorities:\n{}",
        violations.into_iter().collect::<Vec<_>>().join("\n")
    );

    // Retrieval may race deadlines with tokio, but must not own refresh ports.
    let refresh_tokens = [
        "SessionRefreshStore",
        "begin_or_join_refresh",
        "wake_refresh",
    ];
    for path in &sources {
        if path.file_name().and_then(|name| name.to_str()) == Some("retrieval.rs") {
            let source = fs::read_to_string(repository.join(path)).expect("read retrieval.rs");
            for token in refresh_tokens {
                assert!(
                    !source.contains(token),
                    "retrieval.rs must not reference refresh authority token {token}"
                );
            }
        }
    }
}

#[test]
fn domain_session_contracts_are_runtime_and_store_free() {
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let sources = [PathBuf::from("crates/tracedecay-domain/src/session.rs")]
        .into_iter()
        .collect::<BTreeSet<_>>();
    let forbidden: &[&[&str]] = &[
        &["tracedecay_store"],
        &["tracedecay"],
        &["libsql"],
        &["rusqlite"],
        &["sqlx"],
        &["tokio"],
        &["async_std"],
        &["std", "fs"],
        &["std", "net"],
        &["std", "process"],
        &["std", "thread"],
        &["std", "time", "Instant"],
        &["std", "time", "SystemTime"],
    ];
    let violations = scan_sources_for_forbidden_paths(&repository, &sources, forbidden)
        .expect("inspect domain session contracts");
    assert!(
        violations.is_empty(),
        "domain session contracts must stay runtime/store free:\n{}",
        violations.into_iter().collect::<Vec<_>>().join("\n")
    );
}

#[test]
fn tool_catalog_is_application_transport_runtime_and_store_free() {
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let roots = [PathBuf::from("crates/tracedecay-tool-catalog/src")]
        .into_iter()
        .collect::<BTreeSet<_>>();
    let sources =
        filesystem_rust_sources(&repository, &roots).expect("resolve tool catalog sources");
    assert!(!sources.is_empty(), "tool catalog sources must exist");

    let forbidden: &[&[&str]] = &[
        &["tracedecay"],
        &["tracedecay_domain"],
        &["tracedecay_store"],
        &["libsql"],
        &["rusqlite"],
        &["sqlx"],
        &["tokio"],
        &["async_std"],
        &["axum"],
        &["tower"],
        &["ureq"],
        &["std", "fs"],
        &["std", "net"],
        &["std", "process"],
        &["std", "thread"],
        &["std", "time", "Instant"],
        &["std", "time", "SystemTime"],
    ];
    let violations = scan_sources_for_forbidden_paths(&repository, &sources, forbidden)
        .expect("inspect tool catalog sources");
    assert!(
        violations.is_empty(),
        "tool catalog must remain application/transport/runtime/store free:\n{}",
        violations.into_iter().collect::<Vec<_>>().join("\n")
    );
}

#[test]
fn store_session_contracts_are_adapter_free() {
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let roots = [PathBuf::from("crates/tracedecay-store/src/session")]
        .into_iter()
        .collect::<BTreeSet<_>>();
    let sources =
        filesystem_rust_sources(&repository, &roots).expect("resolve store session sources");
    assert!(!sources.is_empty(), "store session sources must exist");

    let forbidden: &[&[&str]] = &[
        &["tracedecay"],
        &["libsql"],
        &["rusqlite"],
        &["sqlx"],
        &["tokio"],
        &["async_std"],
        &["std", "fs"],
        &["std", "net"],
        &["std", "process"],
        &["std", "thread"],
    ];
    let violations = scan_sources_for_forbidden_paths(&repository, &sources, forbidden)
        .expect("inspect store session contracts");
    assert!(
        violations.is_empty(),
        "store session contracts must stay adapter/runtime free:\n{}",
        violations.into_iter().collect::<Vec<_>>().join("\n")
    );
}

#[test]
fn git_index_transaction_store_contracts_are_adapter_and_runtime_free() {
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let sources = [PathBuf::from(
        "crates/tracedecay-store/src/git_index_transactions.rs",
    )]
    .into_iter()
    .collect::<BTreeSet<_>>();
    let forbidden: &[&[&str]] = &[
        &["tracedecay"],
        &["libsql"],
        &["rusqlite"],
        &["sqlx"],
        &["tokio"],
        &["async_std"],
        &["std", "fs"],
        &["std", "net"],
        &["std", "process"],
        &["std", "thread"],
    ];
    let violations = scan_sources_for_forbidden_paths(&repository, &sources, forbidden)
        .expect("inspect git index transaction store contract");
    assert!(
        violations.is_empty(),
        "git index transaction store must remain DTO/contract-only:\n{}",
        violations.into_iter().collect::<Vec<_>>().join("\n")
    );
}

#[test]
fn git_index_transaction_daemon_adapter_has_no_side_file_authority() {
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let path = repository.join("src/daemon/git_transactions/store.rs");
    let source = fs::read_to_string(&path).expect("read git transaction daemon adapter");
    for forbidden in [
        "std::fs",
        "std::path",
        "serde_json",
        "journal.json",
        "OpenOptions",
        "File::open",
    ] {
        assert!(
            !source.contains(forbidden),
            "{} must not retain JSON side-file authority token {forbidden:?}",
            path.display()
        );
    }
    assert!(
        source.contains("GlobalDb"),
        "{} must bridge the canonical GlobalDb adapter",
        path.display()
    );
}

#[test]
fn code_index_is_filesystem_store_model_and_transport_free() {
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let roots = [PathBuf::from("src/code_index")]
        .into_iter()
        .collect::<BTreeSet<_>>();
    let sources = filesystem_rust_sources(&repository, &roots).expect("resolve code-index sources");
    assert!(!sources.is_empty(), "code-index sources must exist");

    let forbidden: &[&[&str]] = &[
        &["crate", "daemon"],
        &["crate", "db"],
        &["crate", "global_db"],
        &["crate", "mcp"],
        &["crate", "semantic_code"],
        &["crate", "store"],
        &["tracedecay_store"],
        &["fastembed"],
        &["libsql"],
        &["rusqlite"],
        &["sqlx"],
        &["axum"],
        &["ureq"],
        &["tokio"],
        &["async_std"],
        &["std", "fs"],
        &["std", "net"],
        &["std", "process"],
    ];
    let violations = scan_sources_for_forbidden_paths(&repository, &sources, forbidden)
        .expect("inspect code-index sources");
    assert!(
        violations.is_empty(),
        "code index must accept only captured inputs and publish only through projector ports:\n{}",
        violations.into_iter().collect::<Vec<_>>().join("\n")
    );
}
