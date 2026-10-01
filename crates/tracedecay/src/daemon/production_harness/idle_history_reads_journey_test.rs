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
            names.insert(wd, path.file_name().unwrap().to_string_lossy().into_owned());
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
