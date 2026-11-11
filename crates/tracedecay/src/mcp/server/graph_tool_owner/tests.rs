//! Source cancellation settles the real owner, including ordinary hydration.

use std::fs::{self, OpenOptions};
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tempfile::TempDir;
use tracedecay_contracts::graph_tool::{GraphToolCompletionV1, GraphToolResultV1};
use tracedecay_contracts::retrieval::{
    ContextCallPathOutcomeV1, ContextPathSourceV1, ContextResultV1,
};
use tracedecay_contracts::{CancellationSignal, Deadline, RequestId, now_micros};
use tracedecay_daemon_service::GraphToolInvocationV1;
use tracedecay_domain::UtcMicros;
use tracedecay_tool_catalog::ApplicationSurfaceOperation;

use super::McpServer;
use crate::daemon::ProductionProjectCompositionHarnessV1;
use crate::test_support::git::git;

fn context_invocation(arguments: Value, cancellation: CancellationSignal) -> GraphToolInvocationV1 {
    GraphToolInvocationV1 {
        operation: ApplicationSurfaceOperation::Context,
        arguments: arguments.as_object().unwrap().clone(),
        request_id: RequestId::new("request.context-source-lifetime").unwrap(),
        deadline: Deadline::new(UtcMicros(now_micros().0 + 30_000_000)).unwrap(),
        cancellation,
    }
}

fn context_result(completion: GraphToolCompletionV1) -> Box<ContextResultV1> {
    let GraphToolResultV1::Context(result) = completion.result else {
        panic!("the context owner must return its typed result");
    };
    result
}

async fn symbol_id(server: &McpServer, name: &str) -> String {
    let result = context_result(
        server
            .compute_graph_tool(context_invocation(
                json!({"task": name, "max_nodes": 1, "include_code": true, "include_memory": false}),
                CancellationSignal::active(format!("cancel.context-lookup.{name}")).unwrap(),
            ))
            .await
            .unwrap(),
    );
    result
        .symbols
        .iter()
        .find(|symbol| symbol.name == name)
        .unwrap_or_else(|| panic!("the real index must contain {name}: {result:?}"))
        .node_id
        .clone()
}

/// Opening the write end proves the real source reader reached this FIFO.
/// Cancellation then leaves its read waiting for bytes until the owner has
/// settled. The backstop releases a regressed synchronous fallback, so a
/// failed assertion cannot leave the test runtime hung.
fn cancel_during_source_read(
    path: PathBuf,
    source: String,
    cancellation: CancellationSignal,
    release: mpsc::Receiver<()>,
) -> std::thread::JoinHandle<(bool, bool)> {
    std::thread::spawn(move || {
        let open_until = Instant::now() + Duration::from_secs(10);
        let writer = loop {
            match OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(&path)
            {
                Ok(writer) => break writer,
                Err(error) if error.raw_os_error() == Some(libc::ENXIO) => {
                    if Instant::now() >= open_until {
                        fs::remove_file(&path).unwrap();
                        fs::write(&path, source).unwrap();
                        return (false, true);
                    }
                    match release.recv_timeout(Duration::from_millis(5)) {
                        Err(RecvTimeoutError::Timeout) => {}
                        Ok(()) | Err(RecvTimeoutError::Disconnected) => {
                            fs::remove_file(&path).unwrap();
                            fs::write(&path, source).unwrap();
                            return (false, false);
                        }
                    }
                }
                Err(error) => panic!("open blocked context source: {error}"),
            }
        };
        assert!(cancellation.cancel(now_micros()));
        let forced_release = matches!(
            release.recv_timeout(Duration::from_secs(10)),
            Err(RecvTimeoutError::Timeout)
        );
        // A late fallback opener must see an ordinary file instead of
        // waiting for another FIFO writer after the existing reader exits.
        fs::remove_file(&path).unwrap();
        fs::write(&path, source).unwrap();
        drop(writer);
        (true, forced_release)
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn context_source_cancellation_settles_without_reading_search_fallback() {
    let isolation = TempDir::new().unwrap();
    let project = isolation.path().join("project");
    fs::create_dir_all(&project).unwrap();
    let source = "pub fn submit_invoice() -> u32 { persist_invoice() }\n\
                  pub fn persist_invoice() -> u32 { 7 }\n\
                  pub fn unrelated_invoice() -> u32 { 99 }\n";
    let path = project.join("lib.rs");
    fs::write(&path, source).unwrap();
    git(&project, &["init", "-q", "-b", "main"]);
    git(&project, &["add", "."]);
    git(&project, &["commit", "-qm", "seed context source lifetime"]);
    let harness = ProductionProjectCompositionHarnessV1::open(isolation.path(), [project.clone()])
        .await
        .unwrap();
    let server = harness.server(&project).unwrap();
    let from = symbol_id(&server, "submit_invoice").await;
    let to = symbol_id(&server, "persist_invoice").await;
    let arguments = json!({
        "task": "invoice",
        "lexical_anchors": ["submit_invoice", "unrelated_invoice"],
        "max_nodes": 10,
        "include_code": true,
        "max_code_blocks": 3,
        "include_memory": false,
        "call_path": {"from_node_id": from, "to_node_id": to},
    });
    let ready = context_result(
        server
            .compute_graph_tool(context_invocation(
                arguments.clone(),
                CancellationSignal::active("cancel.context-source-ready").unwrap(),
            ))
            .await
            .unwrap(),
    );
    for name in ["submit_invoice", "unrelated_invoice"] {
        assert!(
            ready
                .search_matches
                .iter()
                .any(|symbol| symbol.name == name),
            "the fixture must match a path node and an ordinary symbol: {ready:?}"
        );
    }
    assert_eq!(ready.code.len(), 3, "{ready:?}");
    assert!(matches!(
        ready.call_path.as_ref().unwrap().outcome,
        ContextCallPathOutcomeV1::Found { .. }
    ));

    fs::remove_file(&path).unwrap();
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&path)
            .status()
            .unwrap()
            .success()
    );
    let cancellation = CancellationSignal::active("cancel.context-source-blocked").unwrap();
    let (release_tx, release_rx) = mpsc::channel();
    let holding =
        cancel_during_source_read(path, source.to_owned(), cancellation.clone(), release_rx);
    // Call the production owner directly: outer MCP and daemon-executor
    // timers may answer before their retained handler has actually settled.
    let completion = server
        .compute_graph_tool(context_invocation(arguments, cancellation))
        .await;
    drop(release_tx);
    let (source_read_started, forced_release) = holding.join().unwrap();
    harness.shutdown().await;

    assert!(
        source_read_started,
        "the real path source read must be blocked"
    );
    assert!(
        !forced_release,
        "context settlement waited for source bytes after cancellation"
    );
    let result = context_result(completion.unwrap());
    assert!(
        result.code.is_empty(),
        "cancelled source must not be backfilled"
    );
    for name in ["submit_invoice", "unrelated_invoice"] {
        assert!(
            result
                .search_matches
                .iter()
                .any(|symbol| symbol.name == name)
        );
        assert!(result.symbols.iter().any(|symbol| symbol.name == name));
    }
    let path = result.call_path.as_ref().unwrap();
    assert_eq!(
        path.code_generation,
        ready.call_path.unwrap().code_generation
    );
    let ContextCallPathOutcomeV1::Found { nodes, .. } = &path.outcome else {
        panic!("the admitted path must survive source cancellation: {path:?}");
    };
    assert_eq!(nodes.len(), 2);
    for node in nodes {
        let ContextPathSourceV1::Unavailable { evidence } = &node.source else {
            panic!("cancelled path source must be unavailable: {node:?}");
        };
        assert_eq!(evidence.reason_code, "context-call-path-source-cancelled");
    }
}
