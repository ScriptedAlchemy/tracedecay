//! Retained-store recovery through the supported observation-authority reset:
//! ingest → refuse → `storage reset-authority observations` → reopen →
//! converge → the same session is describable and searchable again with the
//! LCM content it had before the reset, and the doctor names where the
//! re-derivation stands on the way there and reports complete/current after.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tempfile::TempDir;
use tracedecay_global_db::observation::OBSERVATION_NATIVE_SOURCE_SCHEME_MIGRATION;

use crate::common::{
    canonical_existing_path, git_program, initialize_tracedecay_cli_project,
    spawn_tracedecay_daemon_with, stop_managed_daemon, tracedecay_command_with_home,
};

/// How long the daemon may take to re-derive the reset store from the
/// preserved transcripts (open the project, re-admit every rollout, run the
/// temporal refresh) before the journey is judged broken.
const CONVERGENCE_TIMEOUT: Duration = Duration::from_secs(120);

/// Filler Codex rollouts beside the session under test, so the reopened
/// authority re-derives a corpus rather than one transcript and search has to
/// select the recovered session out of it.
const ROLLOUT_COUNT: usize = 24;

const SESSION_ID: &str = "codex-reset-recovery-session";
const NEEDLE: &str = "orchard billing pipeline regression";

fn git(project: &Path, args: &[&str]) {
    let output = std::process::Command::new(git_program())
        .args(args)
        .current_dir(project)
        .output()
        .unwrap_or_else(|error| panic!("git {args:?} should run: {error}"));
    assert!(
        output.status.success(),
        "git {args:?} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn seed_committed_project(project: &Path) {
    std::fs::create_dir_all(project.join("src")).unwrap();
    std::fs::write(
        project.join("src/lib.rs"),
        "pub fn billing_pipeline() -> u32 { 42 }\n",
    )
    .unwrap();
    git(project, &["init", "-b", "main"]);
    git(project, &["add", "."]);
    git(
        project,
        &[
            "-c",
            "user.name=TraceDecay Tests",
            "-c",
            "user.email=tests@tracedecay.local",
            "commit",
            "-m",
            "seed reset recovery fixture",
        ],
    );
}

fn codex_rollout_path(home: &Path, session: &str) -> PathBuf {
    home.join(".codex/sessions/2026/01/01")
        .join(format!("rollout-2026-01-01T00-00-00-{session}.jsonl"))
}

fn codex_rollout_contents(project: &Path, session: &str, message: &str) -> String {
    format!(
        "{}\n{}\n{}\n",
        json!({
            "timestamp": "2026-01-01T00:00:00.000Z",
            "type": "session_meta",
            "payload": {"id": session, "cwd": project.to_string_lossy(), "model": "gpt-5.5"}
        }),
        json!({
            "timestamp": "2026-01-01T00:00:01.000Z",
            "type": "event_msg",
            "payload": {"type": "user_message", "message": format!("Investigate the {message}")}
        }),
        json!({
            "timestamp": "2026-01-01T00:00:02.000Z",
            "type": "event_msg",
            "payload": {
                "type": "agent_message",
                "message": format!("The {message} is fixed by the retained change.")
            }
        }),
    )
}

fn write_codex_rollouts(home: &Path, project: &Path) {
    let path = codex_rollout_path(home, SESSION_ID);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, codex_rollout_contents(project, SESSION_ID, NEEDLE)).unwrap();
    for index in 0..ROLLOUT_COUNT {
        let session = format!("codex-reset-recovery-filler-{index:02}");
        std::fs::write(
            codex_rollout_path(home, &session),
            codex_rollout_contents(project, &session, &format!("filler topic {index}")),
        )
        .unwrap();
    }
}

/// Replaces the target rollout with byte-identical content under a new file
/// identity — what a restore, a copy, or a host rewrite does to a transcript.
/// Every observation the file yields now carries a new source generation, so
/// its anchors no longer verify against the ones the pre-reset admission wrote.
fn replace_codex_rollout_identity(home: &Path) {
    let path = codex_rollout_path(home, SESSION_ID);
    let contents = std::fs::read(&path).unwrap();
    let staged = path.with_extension("jsonl.replaced");
    std::fs::write(&staged, contents).unwrap();
    std::fs::rename(&staged, &path).unwrap();
}

fn project_sessions_db(home: &Path) -> PathBuf {
    let projects = home.join(".tracedecay/projects");
    let mut stores = std::fs::read_dir(&projects)
        .unwrap_or_else(|error| panic!("{} should list: {error}", projects.display()))
        .map(|entry| entry.unwrap().path().join("sessions.db"))
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();
    assert_eq!(
        stores.len(),
        1,
        "exactly one project sessions store is expected under {}: {stores:?}",
        projects.display()
    );
    stores.remove(0)
}

/// Turns a healthy store into the exact shape admission refuses with the
/// typed `ResetRequired` state: rows written under the superseded Cline-like
/// native-source scheme, with no enrollment marker.
fn make_observation_authority_refused(db: &Path) {
    let connection = rusqlite::Connection::open(db).unwrap();
    connection
        .pragma_update(None, "foreign_keys", false)
        .unwrap();
    connection
        .execute(
            "INSERT INTO source_cursors(source_json, scope_json, cursor_json)
             VALUES ('{\"provider\":\"cline\",\"session_id\":\"cline.refused\"}',
                     '{\"kind\":\"profile\"}', '{}')",
            [],
        )
        .unwrap();
    connection
        .execute(
            "DELETE FROM global_schema_migrations WHERE migration = ?1",
            [OBSERVATION_NATIVE_SOURCE_SCHEME_MIGRATION],
        )
        .unwrap();
}

fn count(db: &Path, table: &str) -> i64 {
    rusqlite::Connection::open(db)
        .unwrap()
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap()
}

/// Native-source scheduling cursors (host coverage verdicts, discovery
/// frontiers, the Codex corpus epoch) that tell the next pass what it may skip.
fn scheduling_cursor_count(db: &Path) -> i64 {
    rusqlite::Connection::open(db)
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM parse_offsets WHERE file_path NOT LIKE 'hook_analytics:%'",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

/// Runs one tool through the shipped CLI and returns the retained evidence or
/// problem envelope the tool answered with.
fn tool_envelope(home: &Path, project: &Path, tool: &str, args: Value) -> Value {
    let mut args = args;
    args["format"] = json!("json");
    let output = tracedecay_command_with_home(home)
        .current_dir(project)
        .args([
            "tool",
            "--project",
            &project.to_string_lossy(),
            tool,
            "--json",
            "--args",
            &args.to_string(),
        ])
        .output()
        .unwrap_or_else(|error| panic!("tracedecay tool {tool} should run: {error}"));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let wire: Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|error| {
        panic!("{tool} must answer JSON: {error}\nstdout:\n{stdout}\nstderr:\n{stderr}")
    });
    wire["content"]
        .as_array()
        .and_then(|items| {
            items
                .iter()
                .find_map(|item| serde_json::from_str::<Value>(item["text"].as_str()?).ok())
        })
        .unwrap_or_else(|| panic!("{tool} must carry a JSON envelope: {wire}"))
}

fn describe(home: &Path, project: &Path) -> Value {
    tool_envelope(
        home,
        project,
        "tracedecay_lcm_describe",
        json!({"provider": "codex", "session_id": SESSION_ID}),
    )
}

fn doctor(home: &Path, project: &Path) -> Value {
    tool_envelope(home, project, "tracedecay_lcm_doctor", json!({}))
}

fn message_search(home: &Path, project: &Path, catch_up: bool) -> Value {
    tool_envelope(
        home,
        project,
        "tracedecay_message_search",
        json!({"query": NEEDLE, "provider": "codex", "limit": 5, "catch_up": catch_up}),
    )
}

fn is_evidence(envelope: &Value) -> bool {
    envelope.pointer("/outcome/outcome").and_then(Value::as_str) == Some("evidence")
}

fn payload(envelope: &Value) -> &Value {
    envelope
        .pointer("/outcome/value/payload")
        .unwrap_or_else(|| panic!("evidence envelope must carry a payload: {envelope}"))
}

fn problem_message(envelope: &Value) -> &str {
    envelope
        .pointer("/problem/message")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("a non-evidence envelope must carry a problem: {envelope}"))
}

fn served_generation(envelope: &Value) -> Option<u64> {
    envelope
        .pointer("/outcome/value/payload/temporal/watermarks/generation")
        .and_then(Value::as_u64)
        .filter(|generation| *generation > 0)
}

/// Polls describe until the session serves from an active temporal
/// generation. A complete description at generation zero is the window
/// between the projection drain and the temporal refresh, not evidence.
fn wait_for_described_session(home: &Path, project: &Path, phase: &str) -> Value {
    let deadline = Instant::now() + CONVERGENCE_TIMEOUT;
    loop {
        let envelope = describe(home, project);
        if is_evidence(&envelope) && served_generation(&envelope).is_some() {
            let payload = payload(&envelope);
            assert_eq!(payload["status"], "ok", "{phase}: {payload}");
            assert_eq!(payload["description"]["session_id"], SESSION_ID, "{phase}");
            assert_eq!(
                payload["description"]["raw_message_count"], 2,
                "{phase}: {payload}"
            );
            return envelope;
        }
        assert!(
            Instant::now() < deadline,
            "{phase}: the session never became describable; last answer: {envelope}"
        );
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// A search without catch-up serves the retained projection and must answer
/// evidence at once. A catch-up search demands fresh history, so while the
/// worker's last pass is still retrying (a first pass after reopen can end
/// retryable) it may refuse — but only with the typed historical state, and
/// it must settle to evidence within the convergence bound.
fn assert_search_hits(home: &Path, project: &Path, catch_up: bool, phase: &str) {
    let deadline = Instant::now() + CONVERGENCE_TIMEOUT;
    let envelope = loop {
        let envelope = message_search(home, project, catch_up);
        if is_evidence(&envelope) {
            break envelope;
        }
        assert!(
            catch_up,
            "{phase}: message_search must answer evidence: {envelope}"
        );
        let message = problem_message(&envelope);
        assert!(
            message.contains("HistoricalRetry") || message.contains("HistoricalConvergence"),
            "{phase}: a refused catch-up must name the historical state: {envelope}"
        );
        assert!(
            Instant::now() < deadline,
            "{phase}: catch-up never reached a terminal state; last answer: {envelope}"
        );
        std::thread::sleep(Duration::from_millis(250));
    };
    let payload = payload(&envelope);
    let hits = payload["results"]
        .as_array()
        .unwrap_or_else(|| panic!("{phase}: message_search must list results: {payload}"));
    assert!(
        hits.iter()
            .any(|hit| hit["session"]["session_id"] == SESSION_ID),
        "{phase}: the recovered session must be searchable: {payload}"
    );
    assert!(
        payload["temporal"]["watermarks"]["generation"]
            .as_u64()
            .is_some_and(|generation| generation > 0),
        "{phase}: a served search page must carry a non-zero generation watermark: {payload}"
    );
}

#[test]
fn observation_authority_reset_recovers_the_retained_temporal_authority() {
    let home = TempDir::new().unwrap();
    let project_dir = TempDir::new().unwrap();
    let home = canonical_existing_path(home.path());
    let project = canonical_existing_path(project_dir.path());
    seed_committed_project(&project);
    write_codex_rollouts(&home, &project);
    initialize_tracedecay_cli_project(&home, &project);

    // Baseline: the retained authority serves the ingested session.
    let baseline = wait_for_described_session(&home, &project, "before reset");
    assert_search_hits(&home, &project, false, "before reset");
    let sessions_db = project_sessions_db(&home);
    assert!(count(&sessions_db, "observations") > 0);
    assert!(count(&sessions_db, "retrieval_anchor_aliases") > 0);
    assert!(
        scheduling_cursor_count(&sessions_db) > 0,
        "a converged store records the swept Codex corpus and provider coverage"
    );

    // Reopening unchanged history must settle without a spurious conflict.
    assert_search_hits(&home, &project, true, "baseline catch_up");
    stop_managed_daemon(&home);
    let reopen_log = home.join("ordinary-reopen.log");
    let daemon = spawn_tracedecay_daemon_with(&home, |command| {
        command.stderr(std::fs::File::create(&reopen_log).unwrap());
    });
    wait_for_described_session(&home, &project, "ordinary reopen");
    assert_search_hits(&home, &project, true, "ordinary reopen catch_up");
    drop(daemon);
    assert_no_replay_conflicts(&reopen_log);

    // Recovery runs offline: the daemon cannot open a refused store.
    make_observation_authority_refused(&sessions_db);
    replace_codex_rollout_identity(&home);

    let reset = tracedecay_command_with_home(&home)
        .current_dir(&project)
        .args([
            "storage",
            "--yes",
            "reset-authority",
            "observations",
            "--db",
            &sessions_db.to_string_lossy(),
        ])
        .output()
        .expect("storage reset-authority should run");
    let reset_stdout = String::from_utf8_lossy(&reset.stdout);
    assert!(
        reset.status.success(),
        "the scoped reset must succeed\nstdout:\n{reset_stdout}\nstderr:\n{}",
        String::from_utf8_lossy(&reset.stderr)
    );
    assert!(
        reset_stdout.contains("recreated observations empty"),
        "the reset must report the refused authority: {reset_stdout}"
    );
    assert_eq!(count(&sessions_db, "observations"), 0);
    assert_eq!(count(&sessions_db, "session_temporal_generations"), 0);
    assert_eq!(
        count(&sessions_db, "retrieval_anchor_aliases"),
        0,
        "native-record aliases bound by the reset stream must not outlive it"
    );
    assert_eq!(
        scheduling_cursor_count(&sessions_db),
        0,
        "a swept-corpus frontier or complete coverage verdict must not tell the rebuilt \
         authority to skip the transcripts it has to re-read"
    );
    assert!(
        count(&sessions_db, "lcm_raw_messages") > 0,
        "the reset preserves LCM content"
    );

    // Reopen: the runtime re-derives the projection from the preserved
    // transcripts. The doctor answers evidence throughout, so it is the
    // surface that names where that stands: a store still converging is
    // partial evidence whose projection carries the historical state, and a
    // store that has already converged is complete and current — and only
    // once its generations are rebuilt. Neither reading may be an unavailable
    // projection or the reset store read as converged.
    let reset_log = home.join("reset-reopen.log");
    let daemon = spawn_tracedecay_daemon_with(&home, |command| {
        command.stderr(std::fs::File::create(&reset_log).unwrap());
    });
    let reopened = doctor(&home, &project);
    assert!(
        is_evidence(&reopened),
        "doctor must answer evidence: {reopened}"
    );
    let report = payload(&reopened);
    match report["projection"]["state"].as_str() {
        Some("stale") => {
            assert_eq!(
                report["status"], "partial",
                "a converging store is partial diagnostic evidence: {report}"
            );
            assert!(
                report["projection"]["reason"]
                    .as_str()
                    .is_some_and(|reason| reason.starts_with("historical_")),
                "the doctor must name the historical convergence state: {report}"
            );
        }
        Some("current") => {
            assert_eq!(report["status"], "complete", "{report}");
            assert!(
                count(&sessions_db, "session_temporal_generations") > 0,
                "a current projection must be a rebuilt one: {report}"
            );
        }
        other => {
            panic!("the reopened store must be converging or current, not {other:?}: {report}")
        }
    }

    // `tracedecay tool` honours the daemon's after-delay retry directive: a
    // describe issued while history converges rides out the typed converging
    // refusal inside the tool deadline and answers once the projection is
    // current, so the journey reads the converged answer here rather than
    // polling for the transient. The recovered session must carry the LCM
    // content the reset preserved, byte for byte.
    let recovered = wait_for_described_session(&home, &project, "after reset");
    assert_eq!(
        payload(&recovered)["description"],
        payload(&baseline)["description"],
        "the rebuilt authority must serve the session's preserved LCM content unchanged"
    );
    assert_search_hits(&home, &project, false, "after reset");
    // Historical catch-up must reach a terminal state: the frontier the pass
    // persists is the whole reason a second pass has nothing left to do.
    assert_search_hits(&home, &project, true, "after reset with catch_up");

    let converged = doctor(&home, &project);
    assert!(is_evidence(&converged), "{converged}");
    let report = payload(&converged);
    assert_eq!(report["status"], "complete", "{report}");
    assert_eq!(report["health"]["status"], "complete", "{report}");
    assert_eq!(report["projection"]["state"], "current", "{report}");

    let import = tracedecay_command_with_home(&home)
        .current_dir(&project)
        .args([
            "sessions",
            "import",
            "--project-path",
            &project.to_string_lossy(),
        ])
        .output()
        .expect("sessions import should run");
    assert!(
        import.status.success(),
        "explicit import must reach a terminal state once history converged\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&import.stdout),
        String::from_utf8_lossy(&import.stderr)
    );

    assert!(count(&sessions_db, "observations") > 0);
    assert!(
        count(&sessions_db, "session_temporal_generations") > 0,
        "the temporal projection must be rebuilt from the preserved transcripts"
    );
    assert!(
        scheduling_cursor_count(&sessions_db) > 0,
        "the rebuilt authority must record the swept Codex corpus and provider coverage again"
    );
    drop(daemon);
    assert_no_replay_conflicts(&reset_log);
}

fn assert_no_replay_conflicts(log: &Path) {
    let output = std::fs::read_to_string(log).unwrap();
    for reason in [
        "authority_write_failed",
        "external_source_commit_failed",
        "observation repository provenance collision",
        "external source idempotency key conflicts",
    ] {
        assert!(
            !output.contains(reason),
            "reopen must settle on its first pass: {output}"
        );
    }
}
