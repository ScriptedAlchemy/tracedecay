use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use super::common::{
    apply_tracedecay_home_env, git_program, spawn_tracedecay_daemon, tracedecay_command_with_home,
};

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
        let expected_stdout: &[u8] = match hook {
            // These providers support immediate context for PostToolUse. With
            // no bound daemon guidance, the canonical response journey emits
            // no JSON instead of fabricating context or the capture lane's
            // transport-only `{}` acknowledgement.
            "hook-claude-post-tool-use"
            | "hook-codex-post-tool-use"
            | "hook-cursor-post-tool-use" => b"",
            // Cursor sessionStart has a host-specific response even when no
            // project is bound: empty context and no session environment.
            "hook-cursor-session-start" => b"{\"additional_context\":\"\",\"env\":{}}\n",
            _ => b"{}\n",
        };
        assert_eq!(output.stdout, expected_stdout, "{hook}: {output:?}");
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
fn cursor_before_submit_prompt_remains_capture_only() {
    let temp = tempfile::tempdir().unwrap();
    let payload = serde_json::json!({
        "hook_event_name": "beforeSubmitPrompt",
        "conversation_id": "cursor-prompt-session",
        "generation_id": "cursor-prompt-generation",
        "prompt": "inspect the current change",
        "workspace_roots": [temp.path()],
    })
    .to_string();

    let output = run_hook(
        temp.path(),
        "hook-cursor-before-submit-prompt",
        Some(payload.as_bytes()),
    );

    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"{}\n", "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    assert!(
        !temp.path().join(".tracedecay").exists(),
        "capture-only denial surface must not create profile state"
    );
}

#[test]
fn response_capable_native_hooks_use_each_hosts_stdout_contract() {
    let temp = tempfile::tempdir().unwrap();
    let cases = [
        (
            "hook-claude-session-start",
            serde_json::json!({
                "hook_event_name": "SessionStart",
                "session_id": "claude-session",
                "cwd": temp.path(),
                "source": "startup",
            }),
            b"{}\n".as_slice(),
        ),
        (
            "hook-claude-post-tool-use",
            serde_json::json!({
                "hook_event_name": "PostToolUse",
                "session_id": "claude-session",
                "cwd": temp.path(),
                "tool_name": "Write",
                "tool_input": { "file_path": temp.path().join("src/lib.rs") },
                "tool_response": { "success": true },
            }),
            b"".as_slice(),
        ),
        (
            "hook-stop",
            serde_json::json!({
                "hook_event_name": "Stop",
                "session_id": "claude-session",
                "cwd": temp.path(),
                "stop_hook_active": false,
            }),
            b"{}\n".as_slice(),
        ),
        (
            "hook-codex-session-start",
            serde_json::json!({
                "hook_event_name": "SessionStart",
                "session_id": "codex-session",
                "cwd": temp.path(),
                "source": "startup",
            }),
            b"{}\n".as_slice(),
        ),
        (
            "hook-codex-post-tool-use",
            serde_json::json!({
                "hook_event_name": "PostToolUse",
                "session_id": "codex-session",
                "cwd": temp.path(),
                "tool_name": "apply_patch",
                "tool_input": { "command": "*** Begin Patch\n*** End Patch" },
                "tool_response": "Done!",
            }),
            b"".as_slice(),
        ),
        (
            "hook-cursor-session-start",
            serde_json::json!({
                "hook_event_name": "sessionStart",
                "conversation_id": "cursor-session",
                "workspace_roots": [],
            }),
            b"{\"additional_context\":\"\",\"env\":{}}\n".as_slice(),
        ),
        (
            "hook-cursor-post-tool-use",
            serde_json::json!({
                "hook_event_name": "postToolUse",
                "conversation_id": "cursor-session",
                "workspace_roots": [temp.path()],
                "tool_name": "Write",
                "tool_input": { "file_path": temp.path().join("src/lib.rs") },
            }),
            b"".as_slice(),
        ),
    ];

    for (hook, payload, expected_stdout) in cases {
        let output = run_hook(temp.path(), hook, Some(payload.to_string().as_bytes()));
        assert!(output.status.success(), "{hook}: {output:?}");
        assert_eq!(output.stdout, expected_stdout, "{hook}: {output:?}");
        assert!(output.stderr.is_empty(), "{hook}: {output:?}");
    }
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
        let expected_stdout: &[u8] = if hook == "hook-claude-post-tool-use" {
            b""
        } else {
            b"{}\n"
        };
        assert_eq!(output.stdout, expected_stdout, "{hook}: {output:?}");
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

// The lifecycle-guarded stdin hook surface is retired: `hook-user-session-review`
// was the only stdin hook that acquired the lifecycle lease, and the native
// daemon cutover (58717f2ac2) deleted the subcommand — session review now runs
// as a bounded in-process daemon action. No stdin hook engages the lifecycle
// lock anymore, so the external-exclusive-lease quiesce and lifecycle-path
// drain journeys have no remaining production surface.

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
