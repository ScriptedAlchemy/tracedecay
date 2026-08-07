use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use super::common::{
    apply_tracedecay_home_env, git_program, spawn_tracedecay_daemon, tracedecay_command_with_home,
};

const LIFECYCLE_GUARDED_STDIN_HOOKS: &[&str] = &["hook-user-session-review"];

fn native_hook_commands() -> Vec<(&'static str, Vec<u8>)> {
    let claude_stop =
        include_bytes!("../../crates/tracedecay-hooks/fixtures/host_events/claude/stop.json")
            .to_vec();
    let cursor_edit = include_bytes!(
        "../../crates/tracedecay-hooks/fixtures/host_events/cursor/after-file-edit.json"
    )
    .to_vec();
    let codex_stop =
        include_bytes!("../../crates/tracedecay-hooks/fixtures/host_events/codex/stop.json")
            .to_vec();
    let hermes_receipt = include_bytes!(
        "../../crates/tracedecay-hooks/fixtures/host_events/hermes/terminal-receipt.json"
    )
    .to_vec();
    let kimi_edit = include_bytes!(
        "../../crates/tracedecay-hooks/fixtures/host_events/kimi/post-tool-use-edit.json"
    )
    .to_vec();
    let kiro_prompt = br#"{
        "hook_event_name": "userPromptSubmit",
        "session_id": "<SESSION_ID>",
        "cwd": "<PROJECT_ROOT>",
        "prompt": "<REDACTED_PROMPT>"
    }"#
    .to_vec();
    let opencode =
        include_str!("../../crates/tracedecay-hooks/fixtures/host_events/opencode/baseline.json");
    let opencode_stop = fixture_request(opencode, "stop");
    let opencode_tool_after = fixture_request(opencode, "post_tool_use");

    vec![
        ("hook-prompt-submit", claude_stop.clone()),
        ("hook-stop", claude_stop.clone()),
        ("hook-claude-session-start", claude_stop.clone()),
        ("hook-claude-post-tool-use", claude_stop.clone()),
        ("hook-claude-subagent-start", claude_stop),
        ("hook-kiro-pre-tool-use", kiro_prompt.clone()),
        ("hook-kiro-prompt-submit", kiro_prompt.clone()),
        ("hook-kiro-post-tool-use", kiro_prompt),
        ("hook-cursor-subagent-start", cursor_edit.clone()),
        ("hook-cursor-post-tool-use", cursor_edit.clone()),
        ("hook-cursor-before-submit-prompt", cursor_edit.clone()),
        ("hook-cursor-pre-compact", cursor_edit.clone()),
        ("hook-cursor-after-file-edit", cursor_edit.clone()),
        ("hook-cursor-session-start", cursor_edit.clone()),
        ("hook-cursor-session-end", cursor_edit.clone()),
        ("hook-cursor-after-shell", cursor_edit.clone()),
        ("hook-cursor-workspace-open", cursor_edit.clone()),
        ("hook-cursor-stop", cursor_edit),
        // `hook-codex-post-compact` is deliberately absent: like Claude's
        // PostCompact it is a daemon-owned pressure probe rather than a
        // native capture source.
        ("hook-codex-session-start", codex_stop.clone()),
        ("hook-codex-user-prompt-submit", codex_stop.clone()),
        ("hook-codex-subagent-start", codex_stop.clone()),
        ("hook-codex-post-tool-use", codex_stop.clone()),
        ("hook-codex-stop", codex_stop),
        ("hook-hermes-terminal-receipt", hermes_receipt),
        ("hook-kimi-event", kimi_edit),
        ("hook-opencode-event", opencode_stop),
        ("hook-opencode-tool-after", opencode_tool_after),
    ]
}

fn fixture_request(document: &str, identity: &str) -> Vec<u8> {
    let document: serde_json::Value = serde_json::from_str(document).unwrap();
    let event = document["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|event| event["identity"] == identity)
        .unwrap();
    serde_json::to_vec(&event["request"]).unwrap()
}

fn hold_external_exclusive_lease(home: &Path) -> File {
    let profile = home.join(".tracedecay");
    std::fs::create_dir_all(&profile).unwrap();
    let mut lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(profile.join("lifecycle.lock"))
        .unwrap();
    fs2::FileExt::try_lock_exclusive(&lock).unwrap();
    writeln!(lock, "external-token\tmigration\t999").unwrap();
    lock.flush().unwrap();
    lock
}

fn run_hook(home: &Path, hook: &str, input: Option<&[u8]>) -> Output {
    run_hook_at(home, home, hook, input)
}

fn run_hook_at(home: &Path, cwd: &Path, hook: &str, input: Option<&[u8]>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_tracedecay"));
    apply_tracedecay_home_env(&mut command, home);
    command
        .arg(hook)
        .current_dir(cwd)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    if let Some(input) = input {
        child.stdin.take().unwrap().write_all(input).unwrap();
    }
    child.wait_with_output().unwrap()
}

fn test_now() -> tracedecay_domain::UtcMicros {
    let elapsed = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    tracedecay_domain::UtcMicros(i64::try_from(elapsed.as_micros()).unwrap())
}

#[test]
fn native_host_hooks_do_not_create_a_missing_profile() {
    let temp = tempfile::tempdir().unwrap();

    for (index, (hook, payload)) in native_hook_commands().into_iter().enumerate() {
        let home = temp.path().join(index.to_string());
        std::fs::create_dir_all(&home).unwrap();
        let output = run_hook(&home, hook, Some(&payload));

        assert!(output.status.success(), "{hook}: {output:?}");
        assert!(
            !home.join(".tracedecay").exists(),
            "{hook} created broad profile state"
        );
        assert_eq!(output.stdout, b"{}\n", "{hook}: {output:?}");
        assert!(output.stderr.is_empty(), "{hook}: {output:?}");
    }

    let home = temp.path().join("no-input");
    std::fs::create_dir_all(&home).unwrap();
    let output = run_hook(&home, "hook-pre-tool-use", None);
    assert!(output.status.success(), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    assert!(!home.join(".tracedecay").exists());
}

#[test]
fn native_hook_captures_only_bound_transport_spool_records() {
    use tracedecay_hooks::{
        HookCapabilityV1, HookConfigurationFileWriterV1, HookConfigurationPublisherV1,
        HookConfigurationSnapshotV1, HookEventFamily, HookHostV1, HookScopeBindingV1,
        HookSpoolConfigV1, HookSpoolV1,
    };

    let temp = tempfile::tempdir().unwrap();
    let opencode =
        include_str!("../../crates/tracedecay-hooks/fixtures/host_events/opencode/baseline.json");
    let cases = [
        (
            "hook-claude-post-tool-use",
            HookHostV1::ClaudeCode,
            HookEventFamily::ToolLifecycle,
            include_bytes!(
                "../../crates/tracedecay-hooks/fixtures/host_events/claude/post_tool_use_write.json"
            )
            .to_vec(),
        ),
        (
            "hook-stop",
            HookHostV1::ClaudeCode,
            HookEventFamily::SessionBoundary,
            include_bytes!("../../crates/tracedecay-hooks/fixtures/host_events/claude/stop.json")
                .to_vec(),
        ),
        (
            "hook-codex-stop",
            HookHostV1::Codex,
            HookEventFamily::SessionBoundary,
            include_bytes!("../../crates/tracedecay-hooks/fixtures/host_events/codex/stop.json")
                .to_vec(),
        ),
        (
            "hook-cursor-after-file-edit",
            HookHostV1::CursorDesktop,
            HookEventFamily::SavedEdit,
            include_bytes!(
                "../../crates/tracedecay-hooks/fixtures/host_events/cursor/after-file-edit.json"
            )
            .to_vec(),
        ),
        (
            "hook-hermes-terminal-receipt",
            HookHostV1::Hermes,
            HookEventFamily::ToolLifecycle,
            include_bytes!(
                "../../crates/tracedecay-hooks/fixtures/host_events/hermes/terminal-receipt.json"
            )
            .to_vec(),
        ),
        (
            "hook-kimi-event",
            HookHostV1::KimiCode,
            HookEventFamily::SavedEdit,
            include_bytes!(
                "../../crates/tracedecay-hooks/fixtures/host_events/kimi/post-tool-use-edit.json"
            )
            .to_vec(),
        ),
        (
            "hook-opencode-event",
            HookHostV1::OpenCode,
            HookEventFamily::SessionBoundary,
            fixture_request(opencode, "stop"),
        ),
        (
            "hook-opencode-tool-after",
            HookHostV1::OpenCode,
            HookEventFamily::SavedEdit,
            fixture_request(opencode, "post_tool_use"),
        ),
    ];

    for (index, (hook, host, family, payload)) in cases.into_iter().enumerate() {
        let home = temp.path().join(format!("home-{index}"));
        let project = temp.path().join(format!("project-{index}"));
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        let project_id = format!("proj_hook_capture_{index}");
        tracedecay_runtime_core::storage::write_enrollment_marker(
            &project,
            &tracedecay_runtime_core::storage::EnrollmentMarker {
                project_id: project_id.clone(),
                storage_mode: tracedecay_runtime_core::storage::StorageMode::ProfileSharded,
            },
        )
        .unwrap();
        let data_root = home.join(".tracedecay/projects").join(&project_id);
        std::fs::create_dir_all(&data_root).unwrap();
        let now = test_now();
        let binding = HookScopeBindingV1 {
            host,
            project_id: [1; 16],
            repository_id: [2; 16],
            worktree_id: [3; 16],
            worktree_epoch: 4,
            binding_token: [5; 32],
            capabilities: vec![HookCapabilityV1 {
                family,
                support: tracedecay_hooks::HookEventSupportV1::Native,
            }],
        };
        HookConfigurationPublisherV1::new(HookConfigurationFileWriterV1::new(
            tracedecay_hooks::hook_configuration_path(&data_root, host),
        ))
        .publish(HookConfigurationSnapshotV1 {
            schema_version: tracedecay_hooks::HOOK_CONFIGURATION_SCHEMA_VERSION,
            revision: 1,
            published_at: tracedecay_domain::UtcMicros(now.0 - 1_000_000),
            expires_at: tracedecay_domain::UtcMicros(now.0 + 60_000_000),
            binding,
        })
        .unwrap();

        let output = run_hook_at(&home, &project, hook, Some(&payload));

        assert!(output.status.success(), "{hook}: {output:?}");
        assert_eq!(output.stdout, b"{}\n", "{hook}: {output:?}");
        assert!(output.stderr.is_empty(), "{hook}: {output:?}");
        assert!(!home.join(".tracedecay/lifecycle.lock").exists());
        assert!(!home.join(".tracedecay/global.db").exists());
        assert!(!data_root.join("tracedecay.db").exists());
        assert!(!data_root.join("sessions.db").exists());
        let spool_root = data_root.join("hook-v2-spool").join(host.hook_key());
        let (mut spool, report) =
            HookSpoolV1::open(&spool_root, HookSpoolConfigV1::stock(host), test_now()).unwrap();
        assert_eq!(report.pending_records, 1, "{hook}");
        let batches = spool.claim_replay_batches(test_now(), 1).unwrap();
        assert_eq!(batches.len(), 1, "{hook}");
        assert_eq!(batches[0].records.len(), 1, "{hook}");
        assert_eq!(batches[0].records[0].envelope.producer, host, "{hook}");
    }
}

#[test]
fn exclusive_lifecycle_owner_quiesces_every_hook_before_startup_or_dispatch() {
    assert_eq!(LIFECYCLE_GUARDED_STDIN_HOOKS.len(), 1);
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path();
    let profile = home.join(".tracedecay");
    std::fs::create_dir_all(&profile).unwrap();
    let config = profile.join("config.toml");
    let config_bytes = b"upload_enabled = false\npending_upload = 41\n";
    std::fs::write(&config, config_bytes).unwrap();
    let _exclusive = hold_external_exclusive_lease(home);

    for hook in LIFECYCLE_GUARDED_STDIN_HOOKS {
        let payload = b"{}".to_vec();
        let output = run_hook(home, hook, Some(&payload));
        assert!(output.status.success(), "{hook}: {output:?}");
        assert!(output.stdout.is_empty(), "{hook} wrote stdout");
        assert!(output.stderr.is_empty(), "{hook} wrote stderr");
    }

    assert_eq!(std::fs::read(&config).unwrap(), config_bytes);
    assert!(!profile.join("global.db").exists());
    assert!(!profile.join("projects").exists());
}

#[test]
fn bound_claude_hook_returns_only_the_transport_response() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    std::fs::create_dir_all(project.join("src")).unwrap();
    std::fs::write(project.join("src/lib.rs"), "pub fn hook_fixture() {}\n").unwrap();
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
    let _daemon = spawn_tracedecay_daemon(temp.path());
    assert!(
        tracedecay_command_with_home(temp.path())
            .arg("init")
            .current_dir(&project)
            .status()
            .unwrap()
            .success()
    );
    let event = serde_json::json!({
        "hook_event_name": "SessionStart",
        "session_id": "00000000-0000-4000-8000-000000000001",
        "transcript_path": "/workspace/.claude/transcripts/session.jsonl",
        "cwd": project.to_string_lossy(),
        "source": "startup",
    });

    let output = run_hook_at(
        temp.path(),
        &project,
        "hook-claude-session-start",
        Some(event.to_string().as_bytes()),
    );

    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    assert_eq!(output.stdout, b"{}\n");
}

#[test]
fn lifecycle_path_error_silently_drains_and_quiesces_the_hook() {
    let temp = tempfile::tempdir().unwrap();
    let profile_file = temp.path().join(".tracedecay");
    std::fs::write(&profile_file, b"not a profile directory").unwrap();
    let payload = vec![b' '; 256 * 1024];

    let output = run_hook(temp.path(), "hook-user-session-review", Some(&payload));

    assert!(output.status.success(), "{output:?}");
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
    assert_eq!(
        std::fs::read(profile_file).unwrap(),
        b"not a profile directory"
    );
}
