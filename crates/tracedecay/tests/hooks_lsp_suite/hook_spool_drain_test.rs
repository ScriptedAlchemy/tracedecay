//! Capture-only hook callbacks append to the per-host spool, publish their
//! delivery receipts, and exit without contacting the daemon. These journeys
//! pin that a spooled event or receipt reaches the daemon promptly: while the
//! project is open, and after a daemon restart that left it behind, without
//! any other client opening the project.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::json;
use tracedecay_domain::NativeHostIdentityV1;
use tracedecay_hooks::{
    HOOK_SYNCHRONOUS_BUDGET, HookDeliveryReceiptWriterV1, HookDeliveryRetentionV1,
    HookDeliverySourceReceiptV1, hook_delivery_receipt_spool_root,
};
use tracedecay_runtime_core::storage::default_profile_sharded_layout;

use crate::common::{git_program, spawn_tracedecay_daemon, tracedecay_command_with_home};

/// Well under the replay consumer's 30 s interval sweep, so a drain inside it
/// can only come from the append wake or the startup open.
const PROMPT_DRAIN: Duration = Duration::from_secs(10);

fn init_git_project(home: &Path) -> PathBuf {
    let project = home.join("project");
    std::fs::create_dir_all(project.join("src")).unwrap();
    std::fs::write(
        project.join("Cargo.toml"),
        "[package]\nname = \"spool-drain-fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    std::fs::write(project.join("src/lib.rs"), "pub fn drained() {}\n").unwrap();
    let git = git_program();
    for args in [
        &["init", "-q", "-b", "main"][..],
        &["config", "user.email", "test@tracedecay.dev"][..],
        &["config", "user.name", "TraceDecay Test"][..],
        &["add", "."][..],
        &["commit", "-q", "-m", "fixture"][..],
    ] {
        assert!(
            Command::new(&git)
                .args(args)
                .current_dir(&project)
                .status()
                .unwrap()
                .success()
        );
    }
    project
}

fn tracedecay(home: &Path, project: &Path, args: &[&str]) {
    let output = tracedecay_command_with_home(home)
        .args(args)
        .current_dir(project)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "tracedecay {args:?} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// One Cursor `afterFileEdit` through the capture fast path, as the host
/// invokes it.
fn capture_edit(home: &Path, project: &Path, generation: &str) {
    let event = json!({
        "hook_event_name": "afterFileEdit",
        "conversation_id": "spool-drain-conversation",
        "generation_id": generation,
        "model": "auto",
        "session_id": "spool-drain-conversation",
        "cursor_version": "1.7.0",
        "transcript_path": project.join(".cursor-transcript.jsonl"),
        "file_path": project.join("src/lib.rs"),
        "edits": [{ "old_string": "pub fn drained() {}", "new_string": generation }],
        "workspace_roots": [project],
    });
    run_hook(home, project, "hook-cursor-after-file-edit", &event);
}

/// Runs `command` as the host invokes it and requires it to succeed.
fn run_hook(home: &Path, project: &Path, command: &str, event: &serde_json::Value) {
    let mut child = tracedecay_command_with_home(home)
        .arg(command)
        .current_dir(project)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(event.to_string().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(0),
        "{command} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// One Cursor `stop` through the capture fast path; it spools the event and
/// publishes the hook's delivery receipt.
fn capture_stop(home: &Path, project: &Path, generation: &str) {
    let event = json!({
        "conversation_id": "spool-drain-conversation",
        "generation_id": generation,
        "hook_event_name": "stop",
        "model": "auto",
        "status": "completed",
        "loop_count": 0,
        "workspace_roots": [project],
    });
    run_hook(home, project, "hook-cursor-stop", &event);
}

fn published_receipts(receipts: &Path) -> Vec<PathBuf> {
    let mut published = std::fs::read_dir(receipts)
        .map(|entries| {
            entries
                .map(|entry| entry.unwrap().path())
                .filter(|path| {
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.ends_with(".delivery.v1.json"))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    published.sort();
    published
}

/// Seconds until the Cursor receipt spool is empty again, or `None` when it
/// still holds a receipt at `limit`.
fn receipt_drain_latency(receipts: &Path, limit: Duration) -> Option<Duration> {
    latency_until(limit, || published_receipts(receipts).is_empty())
}

fn publish(receipts: &Path, receipt: &HookDeliverySourceReceiptV1) {
    assert_eq!(
        HookDeliveryReceiptWriterV1::open_within(receipts, HOOK_SYNCHRONOUS_BUDGET)
            .unwrap()
            .retain(receipt),
        Ok(HookDeliveryRetentionV1::Published)
    );
}

fn spooled_bytes(records: &Path) -> u64 {
    std::fs::metadata(records).map_or(0, |metadata| metadata.len())
}

/// Seconds until the Cursor spool's records file is empty again, or `None`
/// when it is still holding records at `limit`.
fn drain_latency(records: &Path, limit: Duration) -> Option<Duration> {
    latency_until(limit, || spooled_bytes(records) == 0)
}

fn latency_until(limit: Duration, drained: impl Fn() -> bool) -> Option<Duration> {
    let started = Instant::now();
    while started.elapsed() < limit {
        if drained() {
            return Some(started.elapsed());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    None
}

#[test]
fn spooled_capture_events_drain_promptly_while_open_and_after_a_restart() {
    let home = tempfile::TempDir::new().unwrap();
    let home = home.path().canonicalize().unwrap();
    let project = init_git_project(&home);
    let daemon = spawn_tracedecay_daemon(&home);
    tracedecay(&home, &project, &["init"]);
    let layout = default_profile_sharded_layout(&project, &home.join(".tracedecay")).unwrap();
    let records = layout
        .data_root
        .join("hook-v2-spool/cursor-desktop/records.v1.bin");

    // Open project: the consumer drains on the append wake, not the sweep.
    capture_edit(&home, &project, "open-1");
    capture_edit(&home, &project, "open-2");
    assert!(
        drain_latency(&records, PROMPT_DRAIN).is_some(),
        "spooled events on an open project must drain within {PROMPT_DRAIN:?}, not the 30 s sweep"
    );

    // Daemon down: captures still land in the spool.
    drop(daemon);
    for index in 0..5 {
        capture_edit(&home, &project, &format!("restart-{index}"));
    }
    assert!(
        spooled_bytes(&records) > 0,
        "captures made while the daemon was down must spool"
    );

    // Restart with no client opening the project: the daemon opens it itself.
    let _daemon = spawn_tracedecay_daemon(&home);
    assert!(
        drain_latency(&records, PROMPT_DRAIN).is_some(),
        "records spooled across a restart must drain within {PROMPT_DRAIN:?} without another client"
    );

    // The project the daemon opened for its spool keeps draining new appends.
    capture_edit(&home, &project, "after-restart");
    assert!(
        drain_latency(&records, PROMPT_DRAIN).is_some(),
        "an append after the restart must drain within {PROMPT_DRAIN:?}"
    );
}

#[test]
fn delivery_receipts_drain_promptly_after_the_last_append_and_after_a_restart() {
    let home = tempfile::TempDir::new().unwrap();
    let home = home.path().canonicalize().unwrap();
    let project = init_git_project(&home);
    let daemon = spawn_tracedecay_daemon(&home);
    tracedecay(&home, &project, &["init"]);
    let layout = default_profile_sharded_layout(&project, &home.join(".tracedecay")).unwrap();
    let records = layout
        .data_root
        .join("hook-v2-spool/cursor-desktop/records.v1.bin");
    let receipts =
        hook_delivery_receipt_spool_root(&layout.data_root, NativeHostIdentityV1::CursorDesktop);

    // Two real stops while the daemon is down; their receipts are held back
    // so each can be published when no capture append will follow it.
    drop(daemon);
    capture_stop(&home, &project, "receipt-open");
    capture_stop(&home, &project, "receipt-restart");
    let held = published_receipts(&receipts)
        .into_iter()
        .map(|path| {
            let receipt = serde_json::from_slice::<HookDeliverySourceReceiptV1>(
                &std::fs::read(&path).unwrap(),
            )
            .unwrap();
            std::fs::remove_file(&path).unwrap();
            receipt
        })
        .collect::<Vec<_>>();
    assert_eq!(held.len(), 2, "each stop publishes one delivery receipt");

    // The restart drains the stops' events; the consumer then idles.
    let daemon = spawn_tracedecay_daemon(&home);
    assert!(
        drain_latency(&records, PROMPT_DRAIN).is_some(),
        "the stops' events must drain within {PROMPT_DRAIN:?}"
    );
    std::thread::sleep(Duration::from_secs(1));

    // A receipt published after the last append wakes the drain itself.
    publish(&receipts, &held[0]);
    assert!(
        receipt_drain_latency(&receipts, PROMPT_DRAIN).is_some(),
        "a receipt published after the last append must drain within {PROMPT_DRAIN:?}, not the 30 s sweep"
    );

    // A receipt left over a restart, with no spooled event beside it, opens
    // its project at startup.
    drop(daemon);
    publish(&receipts, &held[1]);
    let _daemon = spawn_tracedecay_daemon(&home);
    assert!(
        receipt_drain_latency(&receipts, PROMPT_DRAIN).is_some(),
        "a receipt spooled across a restart must drain within {PROMPT_DRAIN:?} without another client"
    );
}
