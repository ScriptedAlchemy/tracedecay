//! The daemon-owned Context Scout intent pass through a spawned daemon, real
//! Claude Code hook commands, and a loopback decision engine.

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
    tool_json_structured_content, tracedecay_bin, tracedecay_command_with_home,
};

const YES_INTENT: ContextScoutIntentKindV1 = ContextScoutIntentKindV1::SessionRecall;
const PROJECT_ID: &str = "project.intent-pass-journey";
const ABSENT_SESSION: &str = "00000000-0000-4000-8000-00000000a001";
const ENABLED_SESSION: &str = "00000000-0000-4000-8000-00000000b001";
const WAIT: Duration = Duration::from_secs(180);

/// A loopback `/v1/systemone` server answering yes for [`YES_INTENT`] and no
/// for every other question, counting the requests it answered.
struct FakeDecisionEngine {
    url: String,
    requests: Arc<AtomicUsize>,
}

impl FakeDecisionEngine {
    fn start() -> Self {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&requests);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { return };
                answer(stream, &seen);
            }
        });
        Self { url, requests }
    }

    fn requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }
}

fn answer(mut stream: std::net::TcpStream, seen: &AtomicUsize) {
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
        .into_iter()
        .flat_map(|questions| questions.keys())
        .map(|key| {
            let probability = if key == YES_INTENT.as_key() {
                0.95
            } else {
                0.02
            };
            (key.clone(), json!({"type": "noul", "noul": probability}))
        })
        .collect::<serde_json::Map<_, _>>();
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

/// RFC 3339 UTC with millisecond precision, the timestamp Claude Code writes.
fn rfc3339(at: SystemTime) -> String {
    let millis = at.duration_since(UNIX_EPOCH).unwrap().as_millis();
    let secs = i64::try_from(millis / 1000).unwrap();
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
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
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60,
        millis % 1000
    )
}

/// A Claude Code transcript whose prior turns ask for an earlier session.
fn write_claude_transcript(home: &Path, project: &Path, session: &str) -> PathBuf {
    let dir = home.join(".claude/projects/-intent-pass-journey");
    std::fs::create_dir_all(&dir).unwrap();
    let now = SystemTime::now();
    let turns = [
        (
            "user",
            json!("We settled the retry budget in another session last week. What did we decide?"),
        ),
        (
            "assistant",
            json!([{"type": "text", "text": "That earlier conversation is not loaded here."}]),
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
            let age = Duration::from_secs(30 * u64::try_from(turns.len() - index).unwrap());
            json!({
                "type": role,
                "cwd": project.to_string_lossy(),
                "sessionId": session,
                "uuid": format!("{session}-{index}"),
                "parentUuid": index.checked_sub(1).map(|parent| format!("{session}-{parent}")),
                "timestamp": rfc3339(now - age),
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
    let mut command = Command::new(tracedecay_bin());
    apply_tracedecay_home_env(&mut command, home);
    let mut child = command
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
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "`tracedecay {hook}`: {output:?}");
    output
}

/// The `additionalContext` a Claude Code hook hands its host.
fn hook_context(output: &Output) -> String {
    let response: Value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("hook stdout is JSON ({error}): {output:?}"));
    response["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
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
    tool_json_structured_content(&output.stdout)
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

/// Every `context_scout_intent_work` outcome the daemon logged, in order.
fn intent_outcomes(log: &Path) -> Vec<String> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter(|line| line.contains("event=context_scout_intent_work "))
        .filter_map(|line| line.split(" outcome=").nth(1))
        .map(|outcome| outcome.split_whitespace().next().unwrap_or("").to_owned())
        .collect()
}

fn wait_for(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !ready() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn wait_for_hints(home: &Path, project: &Path, log: &Path, ready: impl Fn((i64, i64)) -> bool) {
    let deadline = Instant::now() + WAIT;
    loop {
        let counts = hint_counts(home, project, YES_INTENT);
        if ready(counts) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out at (emitted, suppressed) {counts:?} after intent outcomes {:?}",
            intent_outcomes(log)
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// `(emitted, suppressed)` for one hint category, read through the analytics
/// tool, which reads the profile `analytics_events` hint settlement reads.
fn hint_counts(home: &Path, project: &Path, intent: ContextScoutIntentKindV1) -> (i64, i64) {
    let hints = run_tool(
        home,
        project,
        "tracedecay_analytics",
        &json!({"section": "hints", "format": "json"}),
    );
    hints["hints"]["by_category"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|row| row["category"] == intent.as_key())
        .map_or((0, 0), |row| {
            (
                row["emitted"].as_i64().unwrap_or(0),
                row["suppressed"].as_i64().unwrap_or(0),
            )
        })
}

fn ingested_messages(home: &Path, project: &Path, session: &str) -> i64 {
    let status = run_tool(
        home,
        project,
        "tracedecay_lcm_status",
        &json!({"provider": "claude", "session_id": session, "format": "json"}),
    );
    status
        .pointer("/outcome/value/payload/lcm/raw_message_count")
        .and_then(Value::as_i64)
        .unwrap_or(0)
}

fn git_fixture(project: &Path) {
    std::fs::create_dir_all(project.join("src")).unwrap();
    std::fs::write(
        project.join("src/lib.rs"),
        "pub fn retry_budget() -> u32 { 3 }\n",
    )
    .unwrap();
    for args in [
        &["init", "-q", "-b", "main"][..],
        &["config", "user.email", "test@tracedecay.dev"][..],
        &["config", "user.name", "TraceDecay Test"][..],
        &["add", "."][..],
        &["commit", "-q", "-m", "fixture"][..],
    ] {
        assert!(
            Command::new(git_program())
                .args(args)
                .current_dir(project)
                .status()
                .unwrap()
                .success()
        );
    }
    assert!(
        tracedecay_runtime_core::storage::write_repository_identity_marker(project, PROJECT_ID)
            .unwrap()
    );
}

#[test]
fn claude_stop_delivers_the_intent_suggestion_the_daemon_judged_at_the_tool_boundary() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path();
    let project = home.join("project");
    git_fixture(&project);
    let absent_transcript = write_claude_transcript(home, &project, ABSENT_SESSION);
    let enabled_transcript = write_claude_transcript(home, &project, ENABLED_SESSION);
    let engine = FakeDecisionEngine::start();
    let log = home.join("daemon.log");
    let _daemon = spawn_tracedecay_daemon_logged(home, &log, |_| {});
    assert!(
        tracedecay_command_with_home(home)
            .arg("init")
            .current_dir(&project)
            .status()
            .unwrap()
            .success(),
        "tracedecay init enrolls the project"
    );
    run_tool(
        home,
        &project,
        "tracedecay_status",
        &json!({"wait_for": {"state": "ready", "timeout_ms": 90_000}}),
    );
    wait_for("the advisory owner that runs hook work to mount", || {
        std::fs::read_to_string(&log)
            .unwrap_or_default()
            .lines()
            .any(|line| {
                line.contains("event=advisory_deferred_attempt ")
                    && line.contains(" phase=mounted ")
            })
    });
    wait_for("both Claude transcripts to be ingested", || {
        ingested_messages(home, &project, ABSENT_SESSION) == 3
            && ingested_messages(home, &project, ENABLED_SESSION) == 3
    });

    run_hook(
        home,
        &project,
        "hook-claude-post-tool-use",
        &post_tool_use(&project, ABSENT_SESSION, &absent_transcript),
    );
    wait_for("the absent-block tool boundary's intent pass", || {
        !intent_outcomes(&log).is_empty()
    });
    let absent_stop = hook_context(&run_hook(
        home,
        &project,
        "hook-stop",
        &stop(&project, ABSENT_SESSION, &absent_transcript),
    ));
    wait_for("the absent-block stop boundary's intent pass", || {
        intent_outcomes(&log).len() >= 2
    });
    assert!(
        intent_outcomes(&log)
            .iter()
            .all(|outcome| outcome == "disabled"),
        "without an intent block every intent pass ends disabled: {:?}",
        intent_outcomes(&log)
    );
    assert_eq!(
        engine.requests(),
        0,
        "without an intent block the decision engine is never asked"
    );
    assert!(
        !absent_stop.contains(&suggestion_text(YES_INTENT)),
        "without an intent block the stop boundary delivers no intent suggestion: {absent_stop}"
    );
    assert_eq!(
        hint_counts(home, &project, YES_INTENT),
        (0, 0),
        "without an intent block no intent hint is recorded"
    );

    let current = run_tool(
        home,
        &project,
        "tracedecay_configuration_get",
        &json!({"key": CONTEXT_SCOUT_SETTINGS_SETTING_KEY}),
    );
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

    let before = intent_outcomes(&log).len();
    run_hook(
        home,
        &project,
        "hook-claude-post-tool-use",
        &post_tool_use(&project, ENABLED_SESSION, &enabled_transcript),
    );
    wait_for("the enabled tool boundary's intent pass", || {
        intent_outcomes(&log).len() > before
    });
    assert_eq!(
        intent_outcomes(&log)[before..],
        ["mounted"],
        "the tool boundary's intent pass enqueues one suggestion and mounts its claim"
    );
    assert_eq!(
        engine.requests(),
        1,
        "the tool boundary asks the decision engine exactly once"
    );

    let stop_event = stop(&project, ENABLED_SESSION, &enabled_transcript);
    let delivers = |context: &String| context.contains(&suggestion_text(YES_INTENT));
    let mut stop_contexts = Vec::new();
    while stop_contexts.len() < 5 && !stop_contexts.last().is_some_and(delivers) {
        stop_contexts.push(hook_context(&run_hook(
            home,
            &project,
            "hook-stop",
            &stop_event,
        )));
    }
    assert!(
        stop_contexts.last().is_some_and(delivers),
        "the stop boundary, or the host's retry of it past the admission ack budget, delivers \
         the session-recall suggestion: {stop_contexts:?}"
    );
    wait_for_hints(home, &project, &log, |(emitted, suppressed)| {
        emitted == 1 && suppressed >= 1
    });
    let after_mount = intent_outcomes(&log)[before + 1..].to_vec();
    assert!(
        !after_mount.is_empty() && after_mount.iter().all(|outcome| outcome == "suppressed"),
        "the stop boundary's own intent pass withholds the suggestion it just delivered: \
         {after_mount:?}"
    );
    assert_eq!(
        hint_counts(home, &project, YES_INTENT).0,
        1,
        "the session receives the suggestion exactly once"
    );
}
