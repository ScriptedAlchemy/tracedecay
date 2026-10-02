use std::collections::{BTreeMap, BTreeSet};
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::journey_test_support::{git, resolved, tool_answer};
use super::*;

const CONVERGENCE_DEADLINE: Duration = Duration::from_secs(90);
/// One idle discovery recheck (60 s) plus slack for its pass to publish.
const RECHECK_DEADLINE: Duration = Duration::from_secs(90);
const STATUS_POLL_INTERVAL: Duration = Duration::from_millis(250);
/// Longer than a scheduler pass, so a window without reads means no pass
/// is still working through the corpus.
const QUIET_WINDOW: Duration = Duration::from_secs(5);

fn write_claude(home: &Path, cwd: &Path, session: &str) -> PathBuf {
    let dir = home.join(".claude/projects/idle-history");
    std::fs::create_dir_all(&dir).unwrap();
    let mut body = String::new();
    for turn in 0..4 {
        let mut record: Value = serde_json::from_str(include_str!(
            "../../../../../tests/fixtures/provider_normalization/claude/assistant_tool_use.input.json"
        ))
        .unwrap();
        record["sessionId"] = json!(session);
        record["cwd"] = json!(cwd);
        record["uuid"] = json!(format!("{session}-{turn}"));
        record["message"]["id"] = json!(format!("msg-{session}-{turn}"));
        record["message"]["content"][0]["text"] = json!(format!("turn {turn} of {session}"));
        record["timestamp"] = json!(format!("2026-09-30T10:00:{turn:02}.000Z"));
        body.push_str(&serde_json::to_string(&record).unwrap());
        body.push('\n');
    }
    let path = dir.join(format!("{session}.jsonl"));
    std::fs::write(&path, body).unwrap();
    path
}

fn codex_record(fixture: &str, stamp: u32) -> Value {
    let mut record: Value = serde_json::from_str(fixture).unwrap();
    record["timestamp"] = json!(format!(
        "2026-09-30T10:{:02}:{:02}.000Z",
        stamp / 60,
        stamp % 60
    ));
    record
}

fn append_jsonl(path: &Path, records: &[Value]) {
    let mut body = String::new();
    for record in records {
        body.push_str(&serde_json::to_string(record).unwrap());
        body.push('\n');
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    std::io::Write::write_all(&mut file, body.as_bytes()).unwrap();
}

fn write_codex(home: &Path, cwd: &Path, index: u32) -> PathBuf {
    let dir = home.join(".codex/sessions/2026/09/30");
    std::fs::create_dir_all(&dir).unwrap();
    let mut meta = codex_record(CODEX_META, 2 * index);
    meta["payload"]["id"] = json!(format!("idle-codex-{index:04}"));
    meta["payload"]["cwd"] = json!(cwd);
    let mut message = codex_record(CODEX_MESSAGE, 2 * index + 1);
    message["payload"]["message"] = json!(format!("codex turn of session {index}"));
    let path = dir.join(format!(
        "rollout-2026-09-30T10-00-00-idle-codex-{index:04}.jsonl"
    ));
    append_jsonl(&path, &[meta, message]);
    path
}

/// A Vibe session: `meta.json` binds it to `cwd`, `messages.jsonl` holds one
/// user turn. Returns both files.
fn write_vibe(home: &Path, cwd: &Path, session: &str, text: &str) -> Vec<PathBuf> {
    let dir = home.join(format!(
        ".vibe/logs/session/session_20260930_100000_{session}"
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let meta = dir.join("meta.json");
    std::fs::write(
        &meta,
        json!({"session_id": session, "environment": {"working_directory": cwd}}).to_string(),
    )
    .unwrap();
    let messages = dir.join("messages.jsonl");
    append_jsonl(
        &messages,
        &[json!({"role": "user", "content": text, "timestamp": 1_790_359_201})],
    );
    vec![meta, messages]
}

/// One session per host whose adapter reads something besides the
/// transcript's new bytes before its cursor can answer: a Pi session header,
/// a Kimi `state.json`, a Vibe `meta.json`, and a Cursor transcript. Returns
/// every file such a read touches.
fn write_peer_hosts(home: &Path, cwd: &Path, index: u32) -> Vec<PathBuf> {
    let stamp = format!("2026-09-30T10:00:{index:02}.000Z");
    let pi = home.join(format!(
        ".pi/agent/sessions/--idle--/2026-09-30T10-00-{index:02}-000Z_idle-pi-{index}.jsonl"
    ));
    std::fs::create_dir_all(pi.parent().unwrap()).unwrap();
    append_jsonl(
        &pi,
        &[
            json!({"type": "session", "version": 3, "id": format!("idle-pi-{index}"), "timestamp": stamp, "cwd": cwd}),
            json!({"type": "message", "id": format!("{index:08}"), "parentId": null, "timestamp": stamp, "message": {"role": "user", "content": format!("pi turn {index}"), "timestamp": 1_790_359_201_000_u64}}),
        ],
    );

    let kimi = home.join(format!(".kimi-code/sessions/wd_project/idle-kimi-{index}"));
    let kimi_wire = kimi.join("agents/main/wire.jsonl");
    std::fs::create_dir_all(kimi_wire.parent().unwrap()).unwrap();
    let kimi_state = kimi.join("state.json");
    std::fs::write(
        &kimi_state,
        json!({"id": format!("idle-kimi-{index}"), "version": 2, "cwd": cwd, "agents": {"main": {"type": "main"}}}).to_string(),
    )
    .unwrap();
    append_jsonl(
        &kimi_wire,
        &[
            json!({"type": "context.append_message", "agentId": "main", "message": {"role": "user", "content": [{"type": "text", "text": format!("kimi turn {index}")}], "toolCalls": []}, "time": 1_790_359_201 + u64::from(index)}),
        ],
    );

    let slug = cwd
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(part) => part.to_str(),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("-");
    let cursor = home.join(format!(
        ".cursor/projects/{slug}/agent-transcripts/idle-cursor-{index}.jsonl"
    ));
    std::fs::create_dir_all(cursor.parent().unwrap()).unwrap();
    let mut cursor_record: Value = serde_json::from_str(CURSOR_ASSISTANT).unwrap();
    cursor_record["id"] = json!(format!("idle-cursor-{index}"));
    cursor_record["timestamp"] = json!(stamp);
    append_jsonl(&cursor, &[cursor_record]);

    let mut written = vec![pi, kimi_state, kimi_wire, cursor];
    written.extend(write_vibe(
        home,
        cwd,
        &format!("idle-vibe-{index}"),
        &format!("vibe turn {index}"),
    ));
    written
}

const CURSOR_ASSISTANT: &str =
    include_str!("../../../../../tests/fixtures/provider_normalization/cursor/tool_use.input.json");
const CODEX_META: &str = include_str!(
    "../../../../../tests/fixtures/provider_normalization/codex/session_meta.input.json"
);
const CODEX_MESSAGE: &str = include_str!(
    "../../../../../tests/fixtures/provider_normalization/codex/agent_message.input.json"
);

/// Records every read of the watched files without touching their metadata,
/// which a transcript's change token would otherwise observe.
struct TranscriptReads {
    fd: libc::c_int,
    names: BTreeMap<libc::c_int, String>,
}

impl TranscriptReads {
    fn watch(paths: &[PathBuf]) -> Self {
        // SAFETY: plain syscall with constant flags; the result is checked.
        let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        assert!(
            fd >= 0,
            "inotify_init1: {}",
            std::io::Error::last_os_error()
        );
        let mut names = BTreeMap::new();
        for path in paths {
            let c_path = CString::new(path.as_os_str().as_bytes()).unwrap();
            // SAFETY: `fd` is a live inotify descriptor and `c_path` is
            // NUL-terminated for the duration of the call.
            let wd = unsafe { libc::inotify_add_watch(fd, c_path.as_ptr(), libc::IN_ACCESS) };
            assert!(
                wd >= 0,
                "inotify_add_watch: {}",
                std::io::Error::last_os_error()
            );
            names.insert(wd, path.to_string_lossy().into_owned());
        }
        Self { fd, names }
    }

    /// Files read since the previous call.
    fn take(&self) -> BTreeSet<String> {
        let mut read = BTreeSet::new();
        let mut buffer = vec![0_u8; 64 * 1024];
        loop {
            // SAFETY: the buffer outlives the call and its length is passed.
            let filled = unsafe { libc::read(self.fd, buffer.as_mut_ptr().cast(), buffer.len()) };
            if filled < 0 {
                let error = std::io::Error::last_os_error();
                assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock, "{error}");
                return read;
            }
            let filled = usize::try_from(filled).unwrap();
            let mut offset = 0;
            while offset < filled {
                // SAFETY: the kernel writes whole events, each a header
                // followed by `len` name bytes; the header may be unaligned.
                let event = unsafe {
                    std::ptr::read_unaligned(
                        buffer.as_ptr().add(offset).cast::<libc::inotify_event>(),
                    )
                };
                assert_eq!(
                    event.mask & libc::IN_Q_OVERFLOW,
                    0,
                    "inotify queue overflowed"
                );
                if event.mask & libc::IN_ACCESS != 0 {
                    read.insert(self.names[&event.wd].clone());
                }
                offset += std::mem::size_of::<libc::inotify_event>() + event.len as usize;
            }
        }
    }
}

impl Drop for TranscriptReads {
    fn drop(&mut self) {
        // SAFETY: `fd` was opened by `watch` and is closed exactly once.
        unsafe { libc::close(self.fd) };
    }
}

async fn lcm_status(harness: &ProductionProjectCompositionHarnessV1, project: &Path) -> Value {
    let response = harness
        .call_tool(project, "tracedecay_lcm_status", json!({"format": "json"}))
        .await
        .unwrap();
    let (_, payload) = tool_answer(&response);
    let payload = resolved(harness, project, "tracedecay_lcm_status", payload).await;
    if payload["outcome"]["outcome"] == json!("evidence") {
        payload["outcome"]["value"]["payload"].clone()
    } else {
        payload
    }
}

fn convergence_state(status: &Value) -> &str {
    status["projection"]["convergence"]["state"]
        .as_str()
        .unwrap_or_else(|| panic!("lcm_status must report convergence state: {status}"))
}

async fn converged_messages(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
    expected: u64,
) -> Value {
    loop {
        let status = lcm_status(harness, project).await;
        match convergence_state(&status) {
            "converging" => tokio::time::sleep(STATUS_POLL_INTERVAL).await,
            "converged" if status["lcm"]["store"]["messages"] == json!(expected) => {
                return status;
            }
            "converged" => {
                panic!(
                    "history converged on the wrong message count (expected {expected}): {status}"
                )
            }
            other => panic!("history settled as {other} before converging: {status}"),
        }
    }
}

/// A message streamed into one Codex rollout used to make every history pass
/// re-read the head of every other rollout to recover its session identity.
/// The corpus is larger than every per-process metadata cache, so only a
/// skip that never opens an unchanged rollout keeps the cost to the one that
/// changed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streamed_codex_message_reads_only_its_rollout() {
    let isolation = tempfile::TempDir::new().unwrap();
    let project = isolation.path().join("project");
    std::fs::create_dir_all(project.join("src")).unwrap();
    git(&project, &["init", "--quiet", "-b", "main"]);
    std::fs::write(project.join("src/lib.rs"), "pub fn idle() {}\n").unwrap();
    let cwd = std::fs::canonicalize(&project).unwrap();
    let home =
        ProductionProjectCompositionHarnessV1::transcript_source_home(isolation.path()).unwrap();
    let unchanged =
        u32::try_from(std::thread::available_parallelism().unwrap().get().max(32) * 2).unwrap();
    let rollouts = (0..=unchanged)
        .map(|index| write_codex(&home, &cwd, index))
        .collect::<Vec<_>>();
    let live = rollouts.last().unwrap().clone();
    let reads = TranscriptReads::watch(&rollouts);

    let harness = ProductionProjectCompositionHarnessV1::open_for_session_retrieval(
        isolation.path(),
        [project.clone()],
    )
    .await
    .unwrap();
    let converged = converged_messages(&harness, &project, u64::from(unchanged) + 1).await;
    assert_eq!(converged["projection"]["state"], json!("current"));
    // The profile scope catches up on the same rollouts independently of the
    // project's convergence, so the corpus is quiet only once a whole window
    // passes without a read.
    let mut caught_up = reads.take();
    let deadline = Instant::now() + CONVERGENCE_DEADLINE;
    loop {
        tokio::time::sleep(QUIET_WINDOW).await;
        let read = reads.take();
        if read.is_empty() {
            break;
        }
        caught_up.extend(read);
        assert!(
            Instant::now() < deadline,
            "rollouts never stopped being read"
        );
    }
    assert_eq!(
        caught_up.len(),
        rollouts.len(),
        "catch-up reads every rollout"
    );

    let mut streamed = codex_record(CODEX_MESSAGE, 3_000);
    streamed["payload"]["message"] = json!("streamed codex turn");
    append_jsonl(&live, &[streamed]);
    converged_messages(&harness, &project, u64::from(unchanged) + 2).await;
    assert_eq!(
        reads.take(),
        BTreeSet::from([live.to_string_lossy().into_owned()])
    );

    harness.shutdown().await;
}

async fn quiet_converged(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
    reads: &TranscriptReads,
) -> (Value, BTreeSet<String>) {
    let mut read = BTreeSet::new();
    loop {
        tokio::time::sleep(QUIET_WINDOW).await;
        let window = reads.take();
        let status = lcm_status(harness, project).await;
        let state = convergence_state(&status);
        if window.is_empty() && state == "converged" {
            return (status, read);
        }
        assert!(
            state == "converging" || state == "converged",
            "history settled as {state} before converging: {status}"
        );
        read.extend(window);
    }
}

/// A message streamed into one Codex rollout runs a history pass for every
/// host. Each peer adapter used to read its unchanged sessions' headers or
/// sidecars before its cursor could prove them unchanged.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streamed_message_reads_no_peer_host_session() {
    let isolation = tempfile::TempDir::new().unwrap();
    let project = isolation.path().join("project");
    std::fs::create_dir_all(project.join("src")).unwrap();
    git(&project, &["init", "--quiet", "-b", "main"]);
    std::fs::write(project.join("src/lib.rs"), "pub fn idle() {}\n").unwrap();
    let cwd = std::fs::canonicalize(&project).unwrap();
    let home =
        ProductionProjectCompositionHarnessV1::transcript_source_home(isolation.path()).unwrap();
    let live = write_codex(&home, &cwd, 0);
    let mut watched = vec![live.clone()];
    for index in 0..3 {
        watched.extend(write_peer_hosts(&home, &cwd, index));
    }
    let reads = TranscriptReads::watch(&watched);

    let harness = ProductionProjectCompositionHarnessV1::open_for_session_retrieval(
        isolation.path(),
        [project.clone()],
    )
    .await
    .unwrap();
    let (converged, caught_up) = quiet_converged(&harness, &project, &reads).await;
    assert_eq!(
        caught_up.len(),
        watched.len(),
        "catch-up reads {caught_up:?}"
    );
    let messages = converged["lcm"]["store"]["messages"].as_u64().unwrap();

    let mut streamed = codex_record(CODEX_MESSAGE, 3_000);
    streamed["payload"]["message"] = json!("streamed codex turn");
    append_jsonl(&live, &[streamed]);
    converged_messages(&harness, &project, messages + 1).await;
    let (_, after) = quiet_converged(&harness, &project, &reads).await;
    assert_eq!(after, BTreeSet::from([live.to_string_lossy().into_owned()]));

    harness.shutdown().await;
}

/// Vibe has no direct host surface, yet its sessions are captured by
/// provider ingestion. Its catch-up used to be refused at the direct
/// host-admission gate, so history never converged for any project that had
/// a Vibe session.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vibe_session_converges_beside_a_peer_host_and_is_searchable() {
    let isolation = tempfile::TempDir::new().unwrap();
    let project = isolation.path().join("project");
    std::fs::create_dir_all(project.join("src")).unwrap();
    git(&project, &["init", "--quiet", "-b", "main"]);
    std::fs::write(project.join("src/lib.rs"), "pub fn idle() {}\n").unwrap();
    let cwd = std::fs::canonicalize(&project).unwrap();
    let home =
        ProductionProjectCompositionHarnessV1::transcript_source_home(isolation.path()).unwrap();
    write_codex(&home, &cwd, 0);
    write_vibe(&home, &cwd, "vibe-search", "marmalade vibe handoff");

    let harness = ProductionProjectCompositionHarnessV1::open_for_session_retrieval(
        isolation.path(),
        [project.clone()],
    )
    .await
    .unwrap();
    converged_messages(&harness, &project, 2).await;

    let search = |provider: &'static str| {
        let harness = &harness;
        let project = &project;
        async move {
            let response = harness
                .call_tool(
                    project,
                    "tracedecay_message_search",
                    json!({"query": "marmalade", "provider": provider, "format": "json"}),
                )
                .await
                .unwrap();
            let (_, payload) = tool_answer(&response);
            let payload = resolved(harness, project, "tracedecay_message_search", payload).await;
            payload["outcome"]["value"]["payload"]["results"]
                .as_array()
                .unwrap_or_else(|| panic!("message search returned no results: {payload}"))
                .iter()
                .map(|hit| {
                    (
                        hit["message"]["provider"].clone(),
                        hit["message"]["text"].clone(),
                    )
                })
                .collect::<Vec<_>>()
        }
    };
    assert_eq!(
        search("vibe").await,
        vec![(json!("vibe"), json!("marmalade vibe handoff"))]
    );
    assert_eq!(search("codex").await, Vec::new());

    harness.shutdown().await;
}

/// A transcript whose working directory was deleted, inside the project or
/// inside another repository, used to stay undecidable: every history pass
/// re-read it whole, deferred it, and retried 250 ms later, so an idle daemon
/// streamed its transcripts forever. Once converged, the idle discovery
/// recheck must find every transcript unchanged without reading one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn converged_daemon_reads_no_transcript_while_idle() {
    let isolation = tempfile::TempDir::new().unwrap();
    let project = isolation.path().join("project");
    let other = isolation.path().join("other");
    std::fs::create_dir_all(project.join("src")).unwrap();
    std::fs::create_dir_all(&other).unwrap();
    git(&project, &["init", "--quiet", "-b", "main"]);
    git(&other, &["init", "--quiet", "-b", "main"]);
    std::fs::write(project.join("src/lib.rs"), "pub fn idle() {}\n").unwrap();
    let project_cwd = std::fs::canonicalize(&project).unwrap();
    let other_cwd = std::fs::canonicalize(&other).unwrap();
    let home =
        ProductionProjectCompositionHarnessV1::transcript_source_home(isolation.path()).unwrap();
    let mut transcripts = Vec::new();
    for index in 0..3 {
        transcripts.push(write_claude(
            &home,
            &project_cwd,
            &format!("idle-in-project-{index}"),
        ));
        transcripts.push(write_claude(
            &home,
            &project_cwd.join("deleted-worktree"),
            &format!("idle-deleted-in-project-{index}"),
        ));
        transcripts.push(write_claude(
            &home,
            &other_cwd.join("deleted-worktree"),
            &format!("idle-deleted-in-other-{index}"),
        ));
    }
    let reads = TranscriptReads::watch(&transcripts);

    let harness = ProductionProjectCompositionHarnessV1::open_for_session_retrieval(
        isolation.path(),
        [project.clone()],
    )
    .await
    .unwrap();
    let deadline = Instant::now() + CONVERGENCE_DEADLINE;
    let converged = loop {
        let status = lcm_status(&harness, &project).await;
        if status["projection"]["convergence"]["state"] == json!("converged") {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "history never converged over undecidable transcripts: {status}"
        );
        tokio::time::sleep(STATUS_POLL_INTERVAL).await;
    };
    assert_eq!(converged["projection"]["state"], json!("current"));
    assert_eq!(
        reads.take().len(),
        transcripts.len(),
        "catch-up reads every transcript"
    );

    let epoch = converged["projection"]["convergence"]["epoch"]
        .as_u64()
        .unwrap();
    let deadline = Instant::now() + RECHECK_DEADLINE;
    let rechecked = loop {
        tokio::time::sleep(STATUS_POLL_INTERVAL).await;
        let status = lcm_status(&harness, &project).await;
        if status["projection"]["convergence"]["epoch"]
            .as_u64()
            .is_some_and(|current| current > epoch)
        {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "no idle history recheck published: {status}"
        );
    };
    assert_eq!(
        rechecked["projection"]["convergence"]["state"],
        json!("converged")
    );
    assert_eq!(reads.take(), BTreeSet::new());

    harness.shutdown().await;
}
