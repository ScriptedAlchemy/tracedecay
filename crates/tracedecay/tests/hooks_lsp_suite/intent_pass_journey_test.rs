//! The daemon-owned Context Scout intent pass through a spawned daemon, real
//! Claude hook commands, and a loopback decision engine.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use tracedecay_domain::configuration::{
    CONTEXT_SCOUT_SETTINGS_SETTING_KEY, ContextScoutConfigurationStateV1, ContextScoutIntentKindV1,
    ContextScoutIntentSettingsV1, ContextScoutSettingsV1, DecisionEngineSettingsV1,
    IntentWindowSettingsV1,
};
use tracedecay_policy::intent_catalog::suggestion_text;

use super::common::{
    apply_tracedecay_home_env, git_program, spawn_tracedecay_daemon_logged,
    tracedecay_command_with_home,
};

const YES_INTENT: ContextScoutIntentKindV1 = ContextScoutIntentKindV1::SessionRecall;
const DEBUG_LOG: &str = "warn,tracedecay::context_scout_lifecycle=debug,tracedecay_mcp=debug,tracedecay_daemon_service=debug,tracedecay_agent_hosts=debug,tracedecay=debug";
const PROJECT_ID: &str = "project.intent-pass-journey";
const ABSENT_SESSION: &str = "00000000-0000-4000-8000-00000000a001";
const ENABLED_SESSION: &str = "00000000-0000-4000-8000-00000000b001";

struct FakeDecisionEngine {
    url: String,
    requests: Arc<AtomicUsize>,
}

/// Answers every `noul` question of a `/v1/systemone` request: yes for
/// [`YES_INTENT`], no for the rest.
fn fake_decision_engine() -> FakeDecisionEngine {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&requests);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request_line = String::new();
            let _ = reader.read_line(&mut request_line);
            let mut content_length = 0;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    content_length = value.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0; content_length];
            let _ = reader.read_exact(&mut body);
            let request: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            let answers = request["questions"]
                .as_object()
                .map(|questions| {
                    questions
                        .keys()
                        .map(|key| {
                            let probability = if key == YES_INTENT.as_key() {
                                0.95
                            } else {
                                0.02
                            };
                            (key.clone(), json!({"type": "noul", "noul": probability}))
                        })
                        .collect::<serde_json::Map<_, _>>()
                })
                .unwrap_or_default();
            if request_line.starts_with("POST /v1/systemone ") {
                seen.fetch_add(1, Ordering::SeqCst);
            }
            let response = json!({"model": "intent-journey", "answers": answers}).to_string();
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{response}",
                response.len()
            );
        }
    });
    FakeDecisionEngine { url, requests }
}

fn rfc3339(at: SystemTime) -> String {
    let micros = at.duration_since(UNIX_EPOCH).unwrap().as_millis();
    let (secs, millis) = (micros / 1000, micros % 1000);
    let days = i64::try_from(secs / 86_400).unwrap();
    let rem = secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// A Claude Code transcript whose earlier turns ask for prior-session context.
fn write_claude_transcript(home: &Path, project: &Path, session: &str) -> PathBuf {
    let dir = home.join(".claude/projects/-intent-pass-journey");
    std::fs::create_dir_all(&dir).unwrap();
    let now = SystemTime::now();
    let turns = [
        (
            "user",
            json!(
                "Last week we settled the retry budget in another session. What did we decide back then?"
            ),
        ),
        (
            "assistant",
            json!([{"type": "text", "text": "I do not have that earlier conversation loaded yet."}]),
        ),
        (
            "user",
            json!("Pull it up from that previous conversation before we change anything."),
        ),
    ];
    let lines = turns
        .iter()
        .enumerate()
        .map(|(index, (role, content))| {
            let offset = Duration::from_secs(30 * (turns.len() - index) as u64);
            json!({
                "type": role,
                "cwd": project.to_string_lossy(),
                "sessionId": session,
                "uuid": format!("{session}-{index}"),
                "parentUuid": (index > 0).then(|| format!("{session}-{}", index - 1)),
                "timestamp": rfc3339(now - offset),
                "message": {"role": role, "content": content},
            })
            .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n");
    let path = dir.join(format!("{session}.jsonl"));
    std::fs::write(&path, format!("{lines}\n")).unwrap();
    path
}

fn run_hook(home: &Path, project: &Path, hook: &str, event: &Value) -> Output {
    let mut command = Command::new(super::common::tracedecay_bin());
    apply_tracedecay_home_env(&mut command, home);
    let mut child = command
        .env("RUST_LOG", DEBUG_LOG)
        .arg(hook)
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
    child.wait_with_output().unwrap()
}

fn run_tool(home: &Path, project: &Path, name: &str, args: &Value) -> Value {
    let output = tracedecay_command_with_home(home)
        .current_dir(project)
        .args(["tool", name, "--json", "--args"])
        .arg(args.to_string())
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "`tracedecay tool {name}` failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    super::common::tool_json_structured_content(&output.stdout)
}

fn post_tool_use(project: &Path, session: &str, transcript: &Path) -> Value {
    json!({
        "session_id": session,
        "transcript_path": transcript,
        "cwd": project,
        "prompt_id": format!("prompt-{session}"),
        "permission_mode": "acceptEdits",
        "hook_event_name": "PostToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": "git status"},
        "tool_response": {"stdout": "clean"},
        "tool_use_id": format!("toolu-{session}"),
        "duration_ms": 3,
    })
}

fn stop(project: &Path, session: &str, transcript: &Path) -> Value {
    json!({
        "session_id": session,
        "transcript_path": transcript,
        "cwd": project,
        "prompt_id": format!("prompt-{session}"),
        "permission_mode": "acceptEdits",
        "hook_event_name": "Stop",
        "stop_hook_active": false,
        "last_assistant_message": "Checked the working tree.",
        "background_tasks": [],
        "session_crons": [],
    })
}

fn enabled_settings(engine_url: &str) -> ContextScoutSettingsV1 {
    ContextScoutSettingsV1 {
        state: ContextScoutConfigurationStateV1::Active,
        intent: Some(ContextScoutIntentSettingsV1 {
            enabled: true,
            engine: DecisionEngineSettingsV1 {
                base_url: engine_url.to_owned(),
                model: Some("intent-journey".to_owned()),
                timeout_ms: 2_000,
                max_concurrent: 2,
                max_state_bytes: 65_536,
            },
            window: IntentWindowSettingsV1 {
                max_user_turns: 3,
                max_messages: 12,
                max_bytes: 16_384,
                max_age_secs: 3_600,
                freshen_budget_ms: 200,
            },
            questions: ContextScoutIntentSettingsV1::reference_questions(),
            action_fact_score_permille: 700,
            ttl_secs: 600,
            min_delivery_gap_secs: 0,
            prompt_lookup_deadline_ms: 200,
        }),
        ..ContextScoutSettingsV1::disabled()
    }
}

fn intent_outcomes(log: &Path) -> Vec<String> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter(|line| line.contains("context_scout_intent_work"))
        .filter_map(|line| line.split("outcome=").nth(1))
        .map(|outcome| outcome.split_whitespace().next().unwrap_or("").to_owned())
        .collect()
}

fn wait_for(what: &str, timeout: Duration, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while !ready() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn hook_context(output: &Output) -> String {
    assert!(output.status.success(), "{output:?}");
    let response: Value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("hook stdout is JSON ({error}): {output:?}"));
    response["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

#[test]
fn claude_stop_delivers_the_intent_suggestion_the_daemon_judged_at_the_tool_boundary() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path();
    let project = home.join("project");
    std::fs::create_dir_all(project.join("src")).unwrap();
    std::fs::write(
        project.join("src/lib.rs"),
        "pub fn retry_budget() -> u32 { 3 }\n",
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
                .current_dir(&project)
                .status()
                .unwrap()
                .success()
        );
    }
    assert!(
        tracedecay_runtime_core::storage::write_repository_identity_marker(&project, PROJECT_ID)
            .unwrap()
    );
    let absent_transcript = write_claude_transcript(home, &project, ABSENT_SESSION);
    let enabled_transcript = write_claude_transcript(home, &project, ENABLED_SESSION);
    let engine = fake_decision_engine();
    let log = std::env::var_os("TEST_UNDECLARED_OUTPUTS_DIR").map_or_else(
        || home.join("daemon.log"),
        |dir| PathBuf::from(dir).join("daemon.log"),
    );
    let _daemon = spawn_tracedecay_daemon_logged(home, &log, |command| {
        command.env("RUST_LOG", DEBUG_LOG);
    });
    assert!(
        tracedecay_command_with_home(home)
            .arg("init")
            .current_dir(&project)
            .status()
            .unwrap()
            .success()
    );
    let status = run_tool(
        home,
        &project,
        "tracedecay_status",
        &json!({"wait_for": {"state": "ready", "timeout_ms": 90_000}}),
    );
    eprintln!("status: {status}");
    wait_for("the advisory owner mount", Duration::from_secs(60), || {
        std::fs::read_to_string(&log)
            .unwrap_or_default()
            .contains("event=advisory_deferred_attempt project=")
            && std::fs::read_to_string(&log)
                .unwrap_or_default()
                .contains("phase=mounted attempt=terminal")
    });

    let post = run_hook(
        home,
        &project,
        "hook-claude-post-tool-use",
        &post_tool_use(&project, ABSENT_SESSION, &absent_transcript),
    );
    assert!(post.status.success(), "{post:?}");
    eprintln!(
        "post: {} stderr: {}",
        String::from_utf8_lossy(&post.stdout),
        String::from_utf8_lossy(&post.stderr)
    );
    for entry in walk(&home.join(".tracedecay")) {
        eprintln!("file: {}", entry.display());
    }
    wait_for(
        "the absent-block intent pass",
        Duration::from_secs(30),
        || !intent_outcomes(&log).is_empty(),
    );
    let absent_stop = hook_context(&run_hook(
        home,
        &project,
        "hook-stop",
        &stop(&project, ABSENT_SESSION, &absent_transcript),
    ));
    wait_for(
        "the absent-block stop intent pass",
        Duration::from_secs(30),
        || intent_outcomes(&log).len() >= 2,
    );
    eprintln!("absent outcomes: {:?}", intent_outcomes(&log));
    assert_eq!(
        intent_outcomes(&log),
        ["disabled", "disabled"],
        "without an intent block both boundaries end their intent pass disabled"
    );
    assert_eq!(
        engine.requests.load(Ordering::SeqCst),
        0,
        "without an intent block the decision engine is never asked"
    );
    assert!(
        !absent_stop.contains(&suggestion_text(YES_INTENT)),
        "without an intent block the stop boundary delivers no intent suggestion: {absent_stop}"
    );

    let current = run_tool(
        home,
        &project,
        "tracedecay_configuration_get",
        &json!({"key": CONTEXT_SCOUT_SETTINGS_SETTING_KEY}),
    );
    eprintln!("configuration: {current}");
    let revision = current
        .pointer("/outcome/value/payload/revision_id")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("configuration revision: {current}"))
        .to_owned();
    let receipt = run_tool(
        home,
        &project,
        "tracedecay_configuration_set",
        &json!({
            "layer": {"kind": "project", "project_id": PROJECT_ID},
            "key": CONTEXT_SCOUT_SETTINGS_SETTING_KEY,
            "value": {"kind": "context_scout_settings", "value": enabled_settings(&engine.url)},
            "expected_revision": revision,
            "idempotency_key": "configuration.idempotency.intent-pass-journey",
        }),
    );
    assert_eq!(
        receipt["outcome"]["outcome"], "effect",
        "the enabled intent block commits through the configuration surface: {receipt}"
    );

    let post = run_hook(
        home,
        &project,
        "hook-claude-post-tool-use",
        &post_tool_use(&project, ENABLED_SESSION, &enabled_transcript),
    );
    assert!(post.status.success(), "{post:?}");
    eprintln!("post: {}", String::from_utf8_lossy(&post.stdout));
    wait_for("the enabled intent pass", Duration::from_secs(30), || {
        intent_outcomes(&log).len() >= 3
    });
    eprintln!("enabled outcomes: {:?}", intent_outcomes(&log));
    eprintln!("{}", std::fs::read_to_string(&log).unwrap_or_default());
    assert_eq!(
        intent_outcomes(&log)[2],
        "mounted",
        "the tool boundary's intent pass enqueues and mounts one suggestion"
    );
    assert_eq!(
        engine.requests.load(Ordering::SeqCst),
        1,
        "the tool boundary asks the decision engine exactly once"
    );
    let delivered = hook_context(&run_hook(
        home,
        &project,
        "hook-stop",
        &stop(&project, ENABLED_SESSION, &enabled_transcript),
    ));
    assert!(
        delivered.contains(&suggestion_text(YES_INTENT)),
        "the next boundary delivers the session-recall suggestion: {delivered}"
    );

    wait_for(
        "the delivered hint_emitted row",
        Duration::from_secs(30),
        || {
            let hints = run_tool(
                home,
                &project,
                "tracedecay_analytics",
                &json!({"section": "hints", "format": "json"}),
            );
            eprintln!("hints: {hints}");
            hints.to_string().contains(YES_INTENT.as_key())
        },
    );
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(walk(&path));
        } else {
            out.push(path);
        }
    }
    out
}
