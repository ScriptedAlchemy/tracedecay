//! Read-only symbol reads on a dirty Git worktree through the shipped daemon.
//!
//! A registered worktree with uncommitted tracked edits seals a generation
//! that names its ref and worktree but no `source_revision`. Exact lookup and
//! the typed symbol-graph lane must both bind to that sealed generation, a
//! further edit must rebind them to the successor, and a page cursor minted
//! against the superseded generation's content identity must be refused typed
//! rather than served across generations.

use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use serde_json::{Value, json};
use tracedecay::daemon::DaemonHandshake;

use crate::code_index_journey::{
    commit_all, deliver_save, exact_identity, exact_symbol, git, initialize_tracedecay,
    stop_daemon_gracefully, tool, wait_for_terminal_generation,
};
use crate::common::{EnvVarGuard, IsolatedEnv, daemon_socket_path, spawn_tracedecay_daemon_with};

const DIRTY_PROBE_COUNT: usize = 12;

fn initialize_repository(project: &Path) -> String {
    fs::create_dir_all(project.join("src")).expect("fixture source directory");
    fs::write(
        project.join("Cargo.toml"),
        "[package]\nname = \"dirty-worktree\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .expect("fixture manifest");
    fs::write(
        project.join("src/lib.rs"),
        "pub fn committed_anchor_symbol() -> &'static str { \"committed\" }\n",
    )
    .expect("fixture source");
    git(project, &["init", "--quiet", "--initial-branch=main"]);
    commit_all(project, "committed fixture")
}

/// The uncommitted tracked edit: one named symbol for exact/typed lookups plus
/// enough probe symbols to overflow a single typed page and mint a cursor.
fn dirty_source(committed: &str, dirty_symbol: &str) -> String {
    let mut source =
        format!("{committed}pub fn {dirty_symbol}() -> &'static str {{ \"dirty\" }}\n");
    for index in 0..DIRTY_PROBE_COUNT {
        writeln!(
            source,
            "pub fn dirty_tracked_probe_{index:02}(input: u32) -> u32 {{ input + {index} }}"
        )
        .expect("format dirty probe");
    }
    source
}

/// Typed symbol search through the application surface. Unlike
/// `tracedecay_search`, this lane binds its page to the sealed generation via
/// the symbol-graph cursor authority, so it observes scope admission directly.
async fn symbol_search_envelope(
    socket: &Path,
    handshake: &DaemonHandshake,
    query: &str,
    cursor: Option<&str>,
) -> Value {
    tool(
        socket,
        handshake,
        "tracedecay_code_symbol_search",
        json!({
            "query": query,
            "scope": { "path_prefix": null },
            "lazy_index_ignored_dependencies": false,
            "meta": { "projection": "summary", "order": "relevance", "cursor": cursor },
            "format": "json",
        }),
    )
    .await
}

fn symbol_search_payload(envelope: Value, query: &str) -> Value {
    assert_eq!(
        envelope["outcome"]["outcome"], "evidence",
        "typed symbol search must publish evidence for {query}: {envelope}"
    );
    assert_eq!(
        envelope["outcome"]["value"]["execution"]["termination"], "completed",
        "typed symbol search must complete for {query}: {envelope}"
    );
    envelope["outcome"]["value"]["payload"].clone()
}

async fn symbol_search(socket: &Path, handshake: &DaemonHandshake, query: &str) -> Value {
    symbol_search_payload(
        symbol_search_envelope(socket, handshake, query, None).await,
        query,
    )
}

fn symbol_search_files(payload: &Value) -> Vec<&str> {
    payload["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| item["file"].as_str())
        .collect()
}

fn symbol_search_names(payload: &Value) -> Vec<&str> {
    payload["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| item["name"].as_str())
        .collect()
}

#[tokio::test]
async fn dirty_worktree_serves_exact_and_typed_symbol_reads_without_a_source_revision() {
    let (environment, project) = IsolatedEnv::acquire().await;
    let project = project.canonicalize().expect("canonical fixture project");
    let revision = initialize_repository(&project);
    let socket = daemon_socket_path(environment.home());
    let log_path = environment.scratch().join("dirty-worktree-daemon.log");
    let _daemon_log = EnvVarGuard::set("TRACEDECAY_TEST_DAEMON_LOG", &log_path);
    let mut daemon = spawn_tracedecay_daemon_with(environment.home(), |_| {});
    let project_id = initialize_tracedecay(environment.home(), &project);
    let identity = exact_identity(&project, project_id);
    tracedecay::product_runtime::register_fixture_product_runtime();
    let handshake =
        tracedecay::daemon::handshake_for_current_client(Some(project.clone()), None, false, false)
            .expect("production daemon handshake");

    // Clean HEAD: the sealed generation is exact-commit evidence.
    let committed = wait_for_terminal_generation(
        &socket,
        &handshake,
        &project,
        &identity,
        "refs/heads/main",
        Some(&revision),
        None,
        "committed_anchor_symbol",
        Some("src/lib.rs"),
    )
    .await;

    // Uncommitted edit to a tracked file. The successor generation keeps the
    // ref and worktree identity but must not claim HEAD as source_revision.
    let committed_source =
        fs::read_to_string(project.join("src/lib.rs")).expect("committed source");
    fs::write(
        project.join("src/lib.rs"),
        dirty_source(&committed_source, "dirty_tracked_symbol"),
    )
    .expect("edit tracked source file");
    deliver_save(&project, &["src/lib.rs"]).await;
    let dirty = wait_for_terminal_generation(
        &socket,
        &handshake,
        &project,
        &identity,
        "refs/heads/main",
        None,
        Some(&committed.generation_id),
        "dirty_tracked_symbol",
        Some("src/lib.rs"),
    )
    .await;
    assert_ne!(dirty.generation_id, committed.generation_id);
    assert!(
        dirty.status["code_index_freshness"]["worktree"]["source_revision"].is_null(),
        "a dirty worktree generation must not claim HEAD as its source revision: {}",
        dirty.status
    );
    assert_eq!(
        dirty.search["code_generation"], dirty.generation_id,
        "lexical search must bind to the sealed dirty generation: {}",
        dirty.search
    );

    // Both read-only lanes bind to the sealed dirty generation.
    let dirty_exact = exact_symbol(&socket, &handshake, "dirty_tracked_symbol", false).await;
    assert_eq!(
        dirty_exact["count"], 1,
        "exact lookup must serve the dirty tracked edit: {dirty_exact}"
    );
    let dirty_typed = symbol_search(&socket, &handshake, "dirty_tracked_symbol").await;
    assert_eq!(
        symbol_search_files(&dirty_typed),
        ["src/lib.rs"],
        "typed symbol search must serve the dirty tracked edit: {dirty_typed}"
    );

    // A typed page-set minted on the dirty generation resumes while that
    // generation is still the one being served.
    let first_page = symbol_search(&socket, &handshake, "dirty_tracked_probe").await;
    let first_names = symbol_search_names(&first_page);
    assert!(
        !first_names.is_empty() && first_names.len() < DIRTY_PROBE_COUNT,
        "probe query must overflow one typed page: {first_page}"
    );
    let cursor = first_page["next_cursor"]
        .as_str()
        .unwrap_or_else(|| panic!("overflowing typed page must mint a cursor: {first_page}"))
        .to_owned();
    let second_page = symbol_search_payload(
        symbol_search_envelope(&socket, &handshake, "dirty_tracked_probe", Some(&cursor)).await,
        "dirty_tracked_probe",
    );
    let second_names = symbol_search_names(&second_page);
    assert_eq!(
        first_names.len() + second_names.len(),
        DIRTY_PROBE_COUNT,
        "resumed dirty page-set must complete the probe roster: {first_page} {second_page}"
    );
    assert!(
        second_names.iter().all(|name| !first_names.contains(name)),
        "resumed dirty page must not repeat the first page: {first_page} {second_page}"
    );
    assert!(
        second_page["next_cursor"].is_null(),
        "the final dirty page must terminate the page-set: {second_page}"
    );

    // A further uncommitted edit renames the symbol: the successor generation
    // is still commit-less and both lanes rebind to it.
    fs::write(
        project.join("src/lib.rs"),
        dirty_source(&committed_source, "dirty_rebound_symbol"),
    )
    .expect("rename dirty symbol");
    deliver_save(&project, &["src/lib.rs"]).await;
    let rebound = wait_for_terminal_generation(
        &socket,
        &handshake,
        &project,
        &identity,
        "refs/heads/main",
        None,
        Some(&dirty.generation_id),
        "dirty_rebound_symbol",
        Some("src/lib.rs"),
    )
    .await;
    assert_ne!(rebound.generation_id, dirty.generation_id);
    let stale_exact = exact_symbol(&socket, &handshake, "dirty_tracked_symbol", false).await;
    assert_eq!(
        stale_exact["count"], 0,
        "exact lookup must drop the renamed dirty symbol: {stale_exact}"
    );
    let rebound_exact = exact_symbol(&socket, &handshake, "dirty_rebound_symbol", false).await;
    assert_eq!(
        rebound_exact["count"], 1,
        "exact lookup must serve the renamed dirty symbol: {rebound_exact}"
    );
    let stale_typed = symbol_search(&socket, &handshake, "dirty_tracked_symbol").await;
    assert!(
        symbol_search_files(&stale_typed).is_empty(),
        "typed symbol search must drop the renamed dirty symbol: {stale_typed}"
    );
    let rebound_typed = symbol_search(&socket, &handshake, "dirty_rebound_symbol").await;
    assert_eq!(
        symbol_search_files(&rebound_typed),
        ["src/lib.rs"],
        "typed symbol search must follow the dirty rename: {rebound_typed}"
    );

    // The cursor still names the superseded generation's content identity. The
    // typed lane refuses it as stale instead of serving a page-set that spans
    // two generations.
    let refused =
        symbol_search_envelope(&socket, &handshake, "dirty_tracked_probe", Some(&cursor)).await;
    assert_eq!(
        refused["problem"]["kind"], "stale",
        "a cursor from a superseded dirty generation must be refused typed: {refused}"
    );
    assert_eq!(
        refused["problem"]["code"], "application.symbol-graph.cursor-stale",
        "stale refusal must carry its typed code: {refused}"
    );

    stop_daemon_gracefully(&mut daemon);
}
