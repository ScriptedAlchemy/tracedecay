//! End-to-end hook replay: drives every provider's hook subcommands through
//! the real binary with representative event payloads, then asserts the full
//! telemetry wiring — each invocation records an attributed
//! `hook_analytics.jsonl` row in the project store, and `tracedecay analytics
//! sync` bridges those rows into the durable `analytics_events` table.
//!
//! Uses only child-process env (no process-global mutation), so it does not
//! need `GLOBAL_DB_ENV_LOCK`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

use serde_json::{Value, json};
use tracedecay::global_db::AnalyticsEventQuery;
use tracedecay::host_admission::HostAdmissionTestRuntimeV1;
use tracedecay::storage::{StorageMode, default_profile_sharded_layout};

use crate::common::{git_program, spawn_tracedecay_daemon_with, tracedecay_command_with_home};

/// `.cargo/config.toml` sets `TRACEDECAY_DISABLE_GLOBAL_DB=1` so cargo-launched
/// processes never touch the operator's real accounting store. This test's
/// subject is the bridge from hook JSONL into the durable `analytics_events`
/// table, which lives in the registered profile accounting database, so it must
/// opt back in — against its own hermetic temp profile, never a live one.
fn enable_profile_accounting(command: &mut Command) -> &mut Command {
    command.env("TRACEDECAY_ENABLE_GLOBAL_DB", "1")
}

struct Replay {
    subcommand: &'static str,
    agent: &'static str,
    hook_name: &'static str,
    /// JSON piped to stdin; `None` for the legacy Claude `preToolUse` contract
    /// which reads `TOOL_INPUT` from the environment instead.
    stdin: Option<Value>,
    tool_input_env: Option<Value>,
}

fn replays(root: &str) -> Vec<Replay> {
    vec![
        Replay {
            subcommand: "hook-claude-session-start",
            agent: "claude",
            hook_name: "SessionStart",
            stdin: Some(json!({
                "session_id": "claude-s1",
                "cwd": root,
                "hook_event_name": "SessionStart",
                "source": "startup",
            })),
            tool_input_env: None,
        },
        Replay {
            subcommand: "hook-claude-post-tool-use",
            agent: "claude",
            hook_name: "PostToolUse",
            stdin: Some(json!({
                "session_id": "claude-s1",
                "cwd": root,
                "tool_name": "Edit",
                "tool_input": { "file_path": format!("{root}/src/lib.rs") },
            })),
            tool_input_env: None,
        },
        Replay {
            subcommand: "hook-pre-tool-use",
            agent: "claude",
            hook_name: "preToolUse",
            stdin: None,
            tool_input_env: Some(json!({
                "session_id": "claude-s1",
                "subagent_type": "general-purpose",
                "prompt": "implement the parser",
            })),
        },
        Replay {
            subcommand: "hook-codex-session-start",
            agent: "codex",
            hook_name: "SessionStart",
            stdin: Some(json!({ "session_id": "codex-s1", "cwd": root })),
            tool_input_env: None,
        },
        Replay {
            subcommand: "hook-codex-user-prompt-submit",
            agent: "codex",
            hook_name: "UserPromptSubmit",
            stdin: Some(json!({
                "session_id": "codex-s1",
                "cwd": root,
                "prompt": "fix the failing test",
            })),
            tool_input_env: None,
        },
        Replay {
            subcommand: "hook-codex-post-tool-use",
            agent: "codex",
            hook_name: "PostToolUse",
            stdin: Some(json!({
                "session_id": "codex-s1",
                "cwd": root,
                "tool_name": "shell",
                "command": "cargo build",
            })),
            tool_input_env: None,
        },
        Replay {
            subcommand: "hook-codex-stop",
            agent: "codex",
            hook_name: "Stop",
            stdin: Some(json!({ "session_id": "codex-s1", "cwd": root })),
            tool_input_env: None,
        },
        Replay {
            subcommand: "hook-cursor-session-start",
            agent: "cursor",
            hook_name: "sessionStart",
            stdin: Some(json!({ "conversation_id": "cursor-s1", "cwd": root })),
            tool_input_env: None,
        },
        Replay {
            subcommand: "hook-cursor-post-tool-use",
            agent: "cursor",
            hook_name: "postToolUse",
            stdin: Some(json!({
                "conversation_id": "cursor-s1",
                "cwd": root,
                "hook_event_name": "postToolUse",
                "tool_name": "Read",
            })),
            tool_input_env: None,
        },
        Replay {
            subcommand: "hook-cursor-stop",
            agent: "cursor",
            hook_name: "stop",
            stdin: Some(json!({ "conversation_id": "cursor-s1", "cwd": root })),
            tool_input_env: None,
        },
        Replay {
            subcommand: "hook-kiro-pre-tool-use",
            agent: "kiro",
            hook_name: "preToolUse",
            stdin: Some(json!({
                "session_id": "kiro-s1",
                "cwd": root,
                "tool_name": "fsWrite",
            })),
            tool_input_env: None,
        },
        Replay {
            subcommand: "hook-kiro-prompt-submit",
            agent: "kiro",
            hook_name: "userPromptSubmit",
            stdin: Some(json!({
                "session_id": "kiro-s1",
                "cwd": root,
                "prompt": "add a feature",
            })),
            tool_input_env: None,
        },
        Replay {
            subcommand: "hook-kiro-post-tool-use",
            agent: "kiro",
            hook_name: "postToolUse",
            stdin: Some(json!({
                "session_id": "kiro-s1",
                "cwd": root,
                "tool_name": "fsWrite",
                "file_path": format!("{root}/src/lib.rs"),
            })),
            tool_input_env: None,
        },
    ]
}

fn run_replay(home: &Path, project_root: &Path, replay: &Replay) {
    let mut command: Command = {
        let mut c = tracedecay_command_with_home(home);
        enable_profile_accounting(&mut c);
        c.arg(replay.subcommand)
            .current_dir(project_root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        c
    };
    if let Some(tool_input) = &replay.tool_input_env {
        command.env("TOOL_INPUT", tool_input.to_string());
    }
    let mut child = command
        .spawn()
        .unwrap_or_else(|e| panic!("spawn {}: {e}", replay.subcommand));
    {
        let mut stdin = child.stdin.take().expect("child stdin");
        if let Some(event) = &replay.stdin {
            stdin.write_all(event.to_string().as_bytes()).unwrap();
        }
        // Drop closes stdin so stdin-reading handlers see EOF.
    }
    let output = child.wait_with_output().expect("hook child output");
    assert!(
        output.status.success(),
        "{} exited with {:?}\nstdout: {}\nstderr: {}",
        replay.subcommand,
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

fn read_jsonl_rows(path: &Path) -> Vec<Value> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .collect()
}

fn str_field<'a>(row: &'a Value, key: &str) -> &'a str {
    row.get(key).and_then(Value::as_str).unwrap_or_default()
}

#[tokio::test]
async fn replayed_provider_hooks_record_attributed_rows_and_bridge_to_analytics_events() {
    let home = tempfile::TempDir::new().expect("temp home");
    let home_root = home.path().canonicalize().expect("canonical home");
    let project_root = home_root.join("project");
    std::fs::create_dir_all(project_root.join("src")).unwrap();
    std::fs::write(
        project_root.join("Cargo.toml"),
        "[package]\nname = \"hook-replay-fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    std::fs::write(
        project_root.join("src/lib.rs"),
        "pub fn replay_fixture() {}\n",
    )
    .unwrap();
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
                .current_dir(&project_root)
                .status()
                .unwrap()
                .success()
        );
    }
    let daemon = spawn_tracedecay_daemon_with(&home_root, |command| {
        enable_profile_accounting(command);
    });
    let init = enable_profile_accounting(&mut tracedecay_command_with_home(&home_root))
        .arg("init")
        .current_dir(&project_root)
        .output()
        .expect("initialize replay project");
    assert!(
        init.status.success(),
        "fixture init failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&init.stdout),
        String::from_utf8_lossy(&init.stderr)
    );
    let profile_root = home_root.join(".tracedecay");
    let layout = default_profile_sharded_layout(&project_root, &profile_root)
        .expect("resolve initialized project layout");
    assert_eq!(layout.storage_mode, StorageMode::ProfileSharded);
    let root_str = project_root.display().to_string();

    let replays = replays(&root_str);
    for replay in &replays {
        run_replay(&home_root, &project_root, replay);
    }

    // Every replay resolved a project root, so every row must land in the
    // project store file. Raw project paths stay out of hook timing rows; the
    // store placement supplies project attribution to the durable bridge.
    let store_rows = read_jsonl_rows(&layout.data_root.join("hook_analytics.jsonl"));
    let hook_invoked: Vec<&Value> = store_rows
        .iter()
        .filter(|row| str_field(row, "event") == "hook_invoked")
        .collect();
    for replay in &replays {
        let matched: Vec<&&Value> = hook_invoked
            .iter()
            .filter(|row| {
                str_field(row, "agent") == replay.agent
                    && str_field(row, "hook_name") == replay.hook_name
            })
            .collect();
        assert_eq!(
            matched.len(),
            1,
            "expected exactly one {}/{} row, got {} (all rows: {:?})",
            replay.agent,
            replay.hook_name,
            matched.len(),
            hook_invoked
        );
        assert!(
            matched[0].get("project_root").is_none(),
            "{}/{} row must not persist a raw project path",
            replay.agent,
            replay.hook_name
        );
    }
    let fallback_rows = read_jsonl_rows(&profile_root.join("hook_analytics.jsonl"));
    assert!(
        fallback_rows.is_empty(),
        "attributed hooks must not spill into the user-level fallback file: {fallback_rows:?}"
    );

    // Bridge: `analytics sync` imports the JSONL rows into the durable table.
    let sync = enable_profile_accounting(&mut tracedecay_command_with_home(&home_root))
        .args(["analytics", "sync"])
        .current_dir(&project_root)
        .output()
        .expect("analytics sync output");
    assert!(
        sync.status.success(),
        "analytics sync failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&sync.stdout),
        String::from_utf8_lossy(&sync.stderr)
    );
    let sync_outcome: Value = serde_json::from_slice(&sync.stdout).unwrap_or_else(|error| {
        panic!(
            "analytics sync returned invalid JSON: {error}\nstdout:\n{}",
            String::from_utf8_lossy(&sync.stdout)
        )
    });
    assert_eq!(
        sync_outcome.get("imported").and_then(Value::as_u64),
        Some(store_rows.len() as u64),
        "analytics sync must import every emitted hook analytics row: {sync_outcome:#}"
    );
    drop(daemon);

    let runtime = HostAdmissionTestRuntimeV1::profile(&profile_root)
        .await
        .expect("open registered replay profile runtime");
    let events = runtime
        .query_profile_analytics_events_for_test(&AnalyticsEventQuery {
            provider: None,
            project_id: None,
            session_id: None,
            event_kind: Some("hook_invoked".to_string()),
            since: None,
            until: None,
            before_id: None,
            limit: 1000,
        })
        .await
        .expect("query analytics events");
    assert_eq!(
        events.len(),
        replays.len(),
        "every replayed hook must bridge into analytics_events"
    );
    let canonical_project = HostAdmissionTestRuntimeV1::canonical_project_key(&project_root);
    for replay in &replays {
        let provider = format!("hook_{}", replay.agent);
        let event = events
            .iter()
            .find(|event| {
                event.provider == provider && event.hook_name.as_deref() == Some(replay.hook_name)
            })
            .unwrap_or_else(|| {
                panic!(
                    "no analytics event for {provider}/{}; got {:?}",
                    replay.hook_name,
                    events
                        .iter()
                        .map(|event| (event.provider.clone(), event.hook_name.clone()))
                        .collect::<Vec<_>>()
                )
            });
        assert_eq!(
            event.project_id, canonical_project,
            "{provider}/{} must be attributed to the replay project",
            replay.hook_name
        );
    }
}
