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

use crate::common::{
    git_program, spawn_tracedecay_daemon, spawn_tracedecay_daemon_logged,
    tracedecay_command_with_home,
};

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
    let mut published = std::fs::read_dir(receipts).map_or_default(|entries| {
        entries
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.ends_with(".delivery.v1.json"))
            })
            .collect::<Vec<_>>()
    });
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

/// The project's `hook_replay` status section, read through the daemon as
/// an agent reads it.
fn hook_replay_status(home: &Path, project: &Path) -> serde_json::Value {
    let output = tracedecay_command_with_home(home)
        .args(["tool", "tracedecay_status", "--json"])
        .current_dir(project)
        .output()
        .unwrap();
    let envelope: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|_| {
        panic!(
            "tracedecay_status must answer JSON\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    envelope["structuredContent"]["hook_replay"].clone()
}

/// The `hook_replay` status once it reads `expected`, or the last reading
/// at `limit`.
fn await_hook_replay_status(
    home: &Path,
    project: &Path,
    expected: &serde_json::Value,
    limit: Duration,
) -> serde_json::Value {
    let started = Instant::now();
    loop {
        let status = hook_replay_status(home, project);
        if &status == expected || started.elapsed() >= limit {
            return status;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
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
fn a_failed_receipt_drain_is_reported_on_status_and_clears_when_the_spool_drains() {
    let home = tempfile::TempDir::new().unwrap();
    let home = home.path().canonicalize().unwrap();
    let project = init_git_project(&home);
    let _daemon = spawn_tracedecay_daemon(&home);
    tracedecay(&home, &project, &["init"]);
    let layout = default_profile_sharded_layout(&project, &home.join(".tracedecay")).unwrap();
    let receipts =
        hook_delivery_receipt_spool_root(&layout.data_root, NativeHostIdentityV1::CursorDesktop);
    drop(HookDeliveryReceiptWriterV1::open_within(&receipts, HOOK_SYNCHRONOUS_BUDGET).unwrap());
    let corrupt = receipts.join(format!("{:032x}.delivery.v1.json", 7));
    std::fs::write(&corrupt, b"not a delivery receipt").unwrap();

    let failed = json!({
        "status": "failed",
        "failures": [{
            "host": "cursor_desktop",
            "spool": "delivery_receipts",
            "cause": "hook delivery receipt spool is corrupt",
        }],
    });
    assert_eq!(
        await_hook_replay_status(&home, &project, &failed, PROMPT_DRAIN),
        failed
    );
    assert!(
        corrupt.is_file(),
        "a receipt the drain could not settle stays durable"
    );

    // Once the spool is readable again, the next publication drains it and
    // the failure clears.
    std::fs::remove_file(&corrupt).unwrap();
    capture_stop(&home, &project, "after-repair");
    let drained = json!({ "status": "drained" });
    assert_eq!(
        await_hook_replay_status(&home, &project, &drained, PROMPT_DRAIN),
        drained
    );
    assert_eq!(published_receipts(&receipts), Vec::<PathBuf>::new());
}

#[test]
fn a_receipt_spool_root_that_is_not_a_directory_is_reported_on_status() {
    let home = tempfile::TempDir::new().unwrap();
    let home = home.path().canonicalize().unwrap();
    let project = init_git_project(&home);
    let _daemon = spawn_tracedecay_daemon(&home);
    tracedecay(&home, &project, &["init"]);
    let layout = default_profile_sharded_layout(&project, &home.join(".tracedecay")).unwrap();
    let receipts =
        hook_delivery_receipt_spool_root(&layout.data_root, NativeHostIdentityV1::CursorDesktop);
    if receipts.exists() {
        std::fs::remove_dir_all(&receipts).unwrap();
    }
    std::fs::create_dir_all(receipts.parent().unwrap()).unwrap();
    std::fs::write(&receipts, b"not a receipt spool").unwrap();
    capture_edit(&home, &project, "after-unsafe-root");

    let failed = json!({
        "status": "failed",
        "failures": [{
            "host": "cursor_desktop",
            "spool": "delivery_receipts",
            "cause": "hook delivery receipt spool path is unsafe",
        }],
    });
    assert_eq!(
        await_hook_replay_status(&home, &project, &failed, PROMPT_DRAIN),
        failed
    );
    assert!(
        receipts.is_file(),
        "the drain must not replace the unusable root"
    );
}

/// A receipt backlog larger than one drain batch settles in back-to-back
/// sweeps, and status reads `drained` only once none is left.
#[test]
fn a_receipt_backlog_beyond_one_batch_drains_without_waiting_for_the_sweep() {
    const BACKLOG: usize = 65;
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

    drop(daemon);
    for index in 0..BACKLOG {
        capture_stop(&home, &project, &format!("backlog-{index}"));
    }
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
    assert_eq!(
        held.len(),
        BACKLOG,
        "each stop publishes one delivery receipt"
    );

    // Drain the stops' events first so only the receipt backlog remains.
    let daemon = spawn_tracedecay_daemon(&home);
    assert!(
        drain_latency(&records, PROMPT_DRAIN).is_some(),
        "the stops' events must drain within {PROMPT_DRAIN:?}"
    );
    drop(daemon);
    for receipt in &held {
        publish(&receipts, receipt);
    }

    let _daemon = spawn_tracedecay_daemon(&home);
    assert!(
        receipt_drain_latency(&receipts, PROMPT_DRAIN).is_some(),
        "a {BACKLOG}-receipt backlog must drain within {PROMPT_DRAIN:?}, not one batch per 30 s sweep; {} left",
        published_receipts(&receipts).len()
    );
    let drained = json!({ "status": "drained" });
    assert_eq!(
        await_hook_replay_status(&home, &project, &drained, PROMPT_DRAIN),
        drained
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

    // The native output writer can see the same event after its receipt was
    // acknowledged. Its logical key survives, while the new callback time
    // must not replace the durable settlement's first timestamps.
    let mut retry = held[0].settlement.clone();
    retry.attempt.valid_at.0 += 1;
    retry.attempt.attempted_at.0 += 1;
    retry.settled_at.0 += 1;
    let retry = HookDeliverySourceReceiptV1::new(retry).unwrap();
    assert_eq!(retry.receipt_id, held[0].receipt_id);
    publish(&receipts, &retry);
    assert!(
        receipt_drain_latency(&receipts, PROMPT_DRAIN).is_some(),
        "an acknowledged exact output retry must replay its durable settlement"
    );
    let drained = json!({ "status": "drained" });
    assert_eq!(
        await_hook_replay_status(&home, &project, &drained, PROMPT_DRAIN),
        drained
    );
}

/// One Claude `PostToolUse` Write of `src/lib.rs` in `checkout`, as the host
/// invokes it, and the `hook_completed` disposition reason it recorded.
fn claude_edit(home: &Path, checkout: &Path, data_root: &Path, tool_use_id: &str) -> String {
    let mut event: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../crates/tracedecay-hooks/fixtures/host_events/claude/post_tool_use_write.json"
    ))
    .unwrap();
    let file = checkout.join("src/lib.rs");
    event["cwd"] = json!(checkout);
    event["tool_use_id"] = json!(tool_use_id);
    event["prompt_id"] = json!(tool_use_id);
    event["tool_input"]["file_path"] = json!(file);
    event["tool_response"]["filePath"] = json!(file);
    run_hook(home, checkout, "hook-claude-post-tool-use", &event);
    let rows = std::fs::read_to_string(data_root.join("hook_analytics.jsonl")).unwrap();
    let completed = rows
        .lines()
        .rev()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .find(|row| row["event"] == "hook_completed" && row["hook_name"] == "PostToolUse")
        .expect("the edit hook records its completion");
    completed["disposition"]["reason_code"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn bound_worktrees(data_root: &Path) -> usize {
    std::fs::read_dir(data_root.join("hook-configurations")).map_or(0, Iterator::count)
}

#[test]
fn an_edit_in_a_never_opened_linked_worktree_binds_it_and_reaches_the_daemon() {
    let home = tempfile::TempDir::new().unwrap();
    let home = home.path().canonicalize().unwrap();
    let project = init_git_project(&home);
    let daemon = spawn_tracedecay_daemon(&home);
    tracedecay(&home, &project, &["init"]);
    let data_root = default_profile_sharded_layout(&project, &home.join(".tracedecay"))
        .unwrap()
        .data_root;
    let worktree = project.join(".claude/worktrees/agent-x");
    let other_worktree = project.join(".claude/worktrees/agent-y");
    for (root, branch) in [(&worktree, "agent-x"), (&other_worktree, "agent-y")] {
        assert!(
            Command::new(git_program())
                .args(["worktree", "add", "-q", "-b", branch])
                .arg(root)
                .current_dir(&project)
                .status()
                .unwrap()
                .success()
        );
        std::fs::write(root.join("src/lib.rs"), "pub fn edited_in_worktree() {}\n").unwrap();
    }
    assert_eq!(
        bound_worktrees(&data_root),
        1,
        "only the main checkout is bound"
    );

    drop(daemon);
    let hold = home.join("hold-after-core");
    let entered = PathBuf::from(format!("{}.entered", hold.display()));
    std::fs::write(&hold, b"hold").unwrap();
    let log = home.join("daemon.log");
    let _daemon = spawn_tracedecay_daemon_logged(&home, &log, |command| {
        command.env("TRACEDECAY_TEST_HOLD_AFTER_CORE_PUBLISH", &hold);
    });
    assert_eq!(
        claude_edit(&home, &worktree, &data_root, "toolu_unbound_first"),
        "hook_v2_accepted",
        "the first edit in an unbound worktree must reach the daemon, not be dropped"
    );
    assert!(
        latency_until(PROMPT_DRAIN, || entered.is_file()).is_some(),
        "the ordinary project open must reach the existing core publication hold: {}",
        std::fs::read_to_string(&log).unwrap()
    );
    assert!(
        hold.is_file(),
        "the first edit was acknowledged while full publication is held"
    );
    assert_eq!(
        claude_edit(
            &home,
            &other_worktree,
            &data_root,
            "toolu_other_unbound_first"
        ),
        "hook_v2_accepted",
        "another linked checkout's same relative file must commit while full publication is held"
    );
    // Graph admission publishes hook bindings before the core hold. Full
    // publication is the capability boundary this fixture actually holds.
    let full_markers = [&worktree, &other_worktree]
        .map(|root| format!("project={} phase=full_published", root.display()));
    let held_log = std::fs::read_to_string(&log).unwrap();
    for marker in &full_markers {
        assert!(
            !held_log.contains(marker),
            "full publication escaped its hold: {held_log}"
        );
    }
    std::fs::remove_file(&hold).unwrap();
    assert!(
        latency_until(PROMPT_DRAIN, || {
            let published = std::fs::read_to_string(&log).unwrap();
            bound_worktrees(&data_root) == 3
                && full_markers.iter().all(|marker| published.contains(marker))
        })
        .is_some(),
        "delivering that edit opens the worktree and publishes its binding: {}",
        std::fs::read_to_string(&log).unwrap()
    );
    assert_eq!(bound_worktrees(&data_root), 3);
    let next = claude_edit(&home, &worktree, &data_root, "toolu_unbound_next");
    assert!(
        matches!(next.as_str(), "hook_v2_accepted" | "hook_v2_spooled"),
        "later edits admit through the worktree's own binding, got {next}"
    );
}
