use std::path::{Path, PathBuf};
use std::sync::{Arc, atomic::Ordering};

use tempfile::TempDir;

use super::{
    CodeIndexReconcileOutcomeV1, CodeIndexSchedulerErrorV1, CodeIndexWorktreeSchedulerV1,
    SharedCodeIndexBytePoolV1,
};
use crate::code_index::production::{CodeIndexInterruptionV1, CodeIndexProductionErrorV1};
use tracedecay_lsp::{
    AdmittedRoot, ClientCapabilities, DaemonLspProtocolSession, FeedbackCyclePort,
    FeedbackCycleRequest, FeedbackCycleResponse, GatewayCapabilities, SemanticProviderPort,
    UnavailableDiagnosticSnapshotProvider, UpstreamCapabilities, negotiate_capabilities,
};

struct Feedback;

impl FeedbackCyclePort for Feedback {
    fn request_feedback_cycle(&self, _request: FeedbackCycleRequest) -> FeedbackCycleResponse {
        panic!("an unsaved overlay must not request a durable feedback cycle")
    }
}

struct Semantics;

impl SemanticProviderPort for Semantics {}

#[test]
fn unsaved_lsp_overlay_leaves_durable_code_index_generation_and_storage_unchanged() {
    let worktree = TempDir::new().expect("worktree");
    git(worktree.path(), &["init", "-q", "-b", "main"]);
    git(worktree.path(), &["config", "user.name", "TraceDecay Test"]);
    git(
        worktree.path(),
        &["config", "user.email", "tracedecay@example.invalid"],
    );
    std::fs::create_dir_all(worktree.path().join("src")).expect("source directory");
    std::fs::write(
        worktree.path().join("src/lib.rs"),
        "pub fn durable() -> u32 { 1 }\n",
    )
    .expect("source");
    git(worktree.path(), &["add", "."]);
    git(worktree.path(), &["commit", "-qm", "fixture"]);

    let store = TempDir::new().expect("code-index store");
    let mut scheduler = CodeIndexWorktreeSchedulerV1::open(
        worktree.path(),
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    )
    .expect("scheduler");
    assert!(matches!(
        scheduler.reconcile_now().expect("initial reconcile"),
        CodeIndexReconcileOutcomeV1::Published(_)
    ));
    let generation_before = scheduler
        .latest_complete()
        .expect("initial generation")
        .generation
        .manifest()
        .generation_id
        .clone();
    let storage_before = storage_image(store.path());

    let root_uri = url::Url::from_directory_path(worktree.path())
        .expect("root URI")
        .to_string();
    let document_uri = url::Url::from_file_path(worktree.path().join("src/lib.rs"))
        .expect("document URI")
        .to_string();
    let gateway_capabilities = GatewayCapabilities::default();
    let upstream_capabilities = UpstreamCapabilities::default();
    let effective = negotiate_capabilities(
        &ClientCapabilities::default(),
        &gateway_capabilities,
        &upstream_capabilities,
    );
    let mut session = DaemonLspProtocolSession::from_ports(
        AdmittedRoot::new(root_uri.clone()),
        effective,
        gateway_capabilities,
        upstream_capabilities,
        Feedback,
        Semantics,
        UnavailableDiagnosticSnapshotProvider,
    );
    send(
        &mut session,
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "rootUri": root_uri,
                "capabilities": {
                    "general": { "positionEncodings": ["utf-16"] }
                }
            }
        }),
        1,
    );
    session.drain_outbound();
    send(
        &mut session,
        serde_json::json!({
            "jsonrpc": "2.0",
            "method": "initialized",
            "params": {}
        }),
        2,
    );
    send(
        &mut session,
        serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": document_uri,
                    "languageId": "rust",
                    "version": 1,
                    "text": "pub fn overlay() -> u32 { 2 }\n"
                }
            }
        }),
        3,
    );
    send(
        &mut session,
        serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didChange",
            "params": {
                "textDocument": {
                    "uri": document_uri,
                    "version": 2
                },
                "contentChanges": [{
                    "text": "pub fn unsaved_overlay() -> u32 { 3 }\n"
                }]
            }
        }),
        4,
    );
    let _ = session.flush_due(100);
    session.drain_outbound();

    assert_eq!(
        scheduler
            .latest_complete()
            .expect("generation remains mounted")
            .generation
            .manifest()
            .generation_id,
        generation_before
    );
    assert_eq!(storage_image(store.path()), storage_before);
    assert_eq!(
        std::fs::read_to_string(worktree.path().join("src/lib.rs")).expect("disk source"),
        "pub fn durable() -> u32 { 1 }\n"
    );
}

#[test]
fn cancelled_superseding_code_index_work_retains_prior_active_generation() {
    let worktree = TempDir::new().expect("worktree");
    git(worktree.path(), &["init", "-q", "-b", "main"]);
    git(worktree.path(), &["config", "user.name", "TraceDecay Test"]);
    git(
        worktree.path(),
        &["config", "user.email", "tracedecay@example.invalid"],
    );
    std::fs::create_dir_all(worktree.path().join("src")).expect("source directory");
    std::fs::write(
        worktree.path().join("src/lib.rs"),
        "pub fn prior() -> u32 { 1 }\n",
    )
    .expect("source");
    git(worktree.path(), &["add", "."]);
    git(worktree.path(), &["commit", "-qm", "fixture"]);

    let store = TempDir::new().expect("code-index store");
    let mut scheduler = CodeIndexWorktreeSchedulerV1::open(
        worktree.path(),
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    )
    .expect("scheduler");
    assert!(matches!(
        scheduler.reconcile_now().expect("initial reconcile"),
        CodeIndexReconcileOutcomeV1::Published(_)
    ));
    let prior = scheduler
        .latest_complete()
        .expect("prior active generation")
        .generation;
    let storage_before = storage_image(store.path());

    std::fs::write(
        worktree.path().join("src/lib.rs"),
        "pub fn superseding() -> u32 { 2 }\n",
    )
    .expect("superseding source");
    scheduler.notify_path(worktree.path().join("src/lib.rs"));
    Arc::clone(&scheduler.shutting_down).store(true, Ordering::Release);

    assert!(matches!(
        scheduler.reconcile_now(),
        Err(CodeIndexSchedulerErrorV1::Production(
            CodeIndexProductionErrorV1::Interrupted(CodeIndexInterruptionV1::Cancelled)
        ))
    ));
    let still_active = scheduler
        .latest_complete()
        .expect("prior generation remains active")
        .generation;
    assert_eq!(
        still_active.manifest().generation_id,
        prior.manifest().generation_id
    );
    assert_eq!(
        still_active.snapshot().content_identity,
        prior.snapshot().content_identity
    );
    assert_eq!(storage_image(store.path()), storage_before);

    let reopened = CodeIndexWorktreeSchedulerV1::open(
        worktree.path(),
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    )
    .expect("reopen scheduler");
    assert_eq!(
        reopened
            .latest_complete()
            .expect("reopened prior active generation")
            .generation
            .manifest()
            .generation_id,
        prior.manifest().generation_id
    );
}

fn send(
    session: &mut DaemonLspProtocolSession<
        Feedback,
        Semantics,
        UnavailableDiagnosticSnapshotProvider,
    >,
    message: serde_json::Value,
    now_ms: u64,
) {
    let payload = serde_json::to_vec(&message).expect("LSP payload");
    let dispatch = session.handle_payload(&payload, now_ms);
    assert!(
        !dispatch.closed,
        "LSP session closed while handling {message}"
    );
}

fn storage_image(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    fn collect(root: &Path, path: &Path, files: &mut Vec<(PathBuf, Vec<u8>)>) {
        let mut entries = std::fs::read_dir(path)
            .expect("read storage directory")
            .map(|entry| entry.expect("storage entry").path())
            .collect::<Vec<_>>();
        entries.sort();
        for entry in entries {
            if entry.is_dir() {
                collect(root, &entry, files);
            } else {
                files.push((
                    entry
                        .strip_prefix(root)
                        .expect("relative storage path")
                        .to_path_buf(),
                    std::fs::read(&entry).expect("storage bytes"),
                ));
            }
        }
    }

    let mut files = Vec::new();
    collect(root, root, &mut files);
    files
}

fn git(root: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .current_dir(root)
        .args(args)
        .status()
        .expect("git fixture command");
    assert!(status.success(), "git fixture command failed: {args:?}");
}
