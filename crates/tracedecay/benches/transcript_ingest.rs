//! Transcript-ingest throughput benchmark.
//!
//! Measures per-item (message) ingest throughput for the session/transcript
//! pipeline in a fully isolated sandbox: temp `HOME`, temp project, temp
//! profile store. Never touches the operator's real TraceDecay or agent-host
//! data. Fixture shapes mirror `crates/tracedecay/tests/transcript_ingest_suite`.
//!
//! Run (numbers):
//!   cargo bench -p tracedecay --bench transcript_ingest \
//!     --features test-helpers -- --run all
//! Run (per-stage attribution):
//!   cargo bench -p tracedecay --bench transcript_ingest \
//!     --features test-helpers,hotpath -- --run claude
//!
//! Providers: `claude` and `codex` exercise the observation capture +
//! projection pipeline; `kiro` exercises the content-hash full-file reader;
//! `store` drives a synthetic in-memory source so the shared
//! persist-transcript store stack is measured without parse cost.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use tracedecay::host_admission::HostAdmissionTestRuntimeV1;
use tracedecay_domain::ProjectId;
use tracedecay_runtime_core::storage::write_repository_identity_marker;
use tracedecay_sessions::runtime::SessionProvider;
use tracedecay_sessions::runtime::source::{
    ParsedTranscript, SessionDraft, StoredCursor, TranscriptSource,
};
use tracedecay_store::SessionMessageRecord;

#[derive(Clone, Copy)]
struct BenchConfig {
    sessions: usize,
    messages_per_session: usize,
    /// Every Nth assistant message carries `large_bytes` of text.
    large_every: usize,
    large_bytes: usize,
    small_bytes: usize,
}

impl Default for BenchConfig {
    fn default() -> Self {
        Self {
            sessions: 4,
            messages_per_session: 512,
            large_every: 64,
            large_bytes: 256 * 1024,
            small_bytes: 400,
        }
    }
}

impl BenchConfig {
    fn total_messages(&self) -> usize {
        self.sessions * self.messages_per_session
    }
}

/// Deterministic pseudo-random printable ASCII filler.
fn synth_text(seed: u64, bytes: usize) -> String {
    const WORDS: [&str; 16] = [
        "ingest",
        "transcript",
        "session",
        "pipeline",
        "throughput",
        "observation",
        "projection",
        "cursor",
        "frame",
        "batch",
        "sanitize",
        "normalize",
        "dedup",
        "store",
        "graph",
        "evidence",
    ];
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15).max(1);
    let mut out = String::with_capacity(bytes + 16);
    while out.len() < bytes {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out.push_str(WORDS[(state % WORDS.len() as u64) as usize]);
        out.push(' ');
    }
    out.truncate(bytes);
    out
}

fn message_text(config: &BenchConfig, session: usize, index: usize) -> String {
    let seed = (session as u64) << 32 | index as u64;
    if config.large_every > 0 && index % config.large_every == config.large_every - 1 {
        synth_text(seed, config.large_bytes)
    } else {
        synth_text(seed, config.small_bytes)
    }
}

fn timestamp(index: usize) -> String {
    // Fixed date, seconds advance per message; parses as ISO-8601.
    format!(
        "2026-01-01T{:02}:{:02}:{:02}.000Z",
        (index / 3600) % 24,
        (index / 60) % 60,
        index % 60
    )
}

struct Sandbox {
    _tmp: tempfile::TempDir,
    /// The process-wide sandbox `HOME`; provider fixture roots (`.claude`,
    /// `.codex`, Kiro's data dir) are namespaced so providers cannot cross.
    home: PathBuf,
    project: PathBuf,
    profile: PathBuf,
}

fn sandbox(label: &str) -> Sandbox {
    let tmp = tempfile::Builder::new()
        .prefix(&format!("tracedecay-ingest-bench-{label}-"))
        .tempdir()
        .expect("create sandbox tempdir");
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .expect("bench main installed a sandbox HOME");
    let project = tmp.path().join("project");
    let profile = tmp.path().join("profile");
    std::fs::create_dir_all(project.join(".tracedecay")).unwrap();
    std::fs::write(project.join(".tracedecay/tracedecay.db"), "").unwrap();
    let git = std::process::Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(&project)
        .status()
        .expect("git init sandbox project");
    assert!(git.success(), "git init must succeed");
    Sandbox {
        _tmp: tmp,
        home,
        project,
        profile,
    }
}

fn write_claude_fixture(sandbox: &Sandbox, config: &BenchConfig) -> PathBuf {
    let dir = sandbox.home.join(".claude/projects/-bench-slug");
    std::fs::create_dir_all(&dir).unwrap();
    let cwd = sandbox.project.to_string_lossy().into_owned();
    for session in 0..config.sessions {
        let session_id = format!("bench-claude-{session:04}");
        let path = dir.join(format!("{session_id}.jsonl"));
        let mut file = std::io::BufWriter::new(std::fs::File::create(&path).unwrap());
        for index in 0..config.messages_per_session {
            let text = message_text(config, session, index);
            let line = if index % 2 == 0 {
                serde_json::json!({
                    "type": "user",
                    "cwd": cwd,
                    "sessionId": session_id,
                    "uuid": format!("u-{session:04}-{index:06}"),
                    "timestamp": timestamp(index),
                    "message": {"role": "user", "content": text}
                })
            } else {
                serde_json::json!({
                    "type": "assistant",
                    "cwd": cwd,
                    "sessionId": session_id,
                    "uuid": format!("u-{session:04}-{index:06}"),
                    "timestamp": timestamp(index),
                    "message": {
                        "id": format!("msg-{session:04}-{index:06}"),
                        "role": "assistant",
                        "model": "claude-bench-1",
                        "content": [{"type": "text", "text": text}]
                    }
                })
            };
            serde_json::to_writer(&mut file, &line).unwrap();
            file.write_all(b"\n").unwrap();
        }
        file.flush().unwrap();
    }
    dir
}

fn write_codex_fixture(sandbox: &Sandbox, config: &BenchConfig) -> PathBuf {
    let dir = sandbox.home.join(".codex/sessions/2026/01/01");
    std::fs::create_dir_all(&dir).unwrap();
    let cwd = sandbox.project.to_string_lossy().into_owned();
    for session in 0..config.sessions {
        let session_id = format!("bench-codex-{session:04}");
        let path = dir.join(format!(
            "rollout-2026-01-01T00-00-{:02}-{session_id}.jsonl",
            session % 60
        ));
        let mut file = std::io::BufWriter::new(std::fs::File::create(&path).unwrap());
        let meta = serde_json::json!({
            "timestamp": timestamp(0),
            "type": "session_meta",
            "payload": {"id": session_id, "cwd": cwd, "model": "gpt-bench"}
        });
        serde_json::to_writer(&mut file, &meta).unwrap();
        file.write_all(b"\n").unwrap();
        for index in 0..config.messages_per_session {
            let text = message_text(config, session, index);
            let line = if index % 2 == 0 {
                serde_json::json!({
                    "timestamp": timestamp(index),
                    "type": "event_msg",
                    "payload": {"type": "user_message", "message": text}
                })
            } else {
                serde_json::json!({
                    "timestamp": timestamp(index),
                    "type": "event_msg",
                    "payload": {"type": "agent_message", "message": text}
                })
            };
            serde_json::to_writer(&mut file, &line).unwrap();
            file.write_all(b"\n").unwrap();
        }
        file.flush().unwrap();
    }
    dir
}

/// Kiro workspace-sessions directory name: unpadded base64 of the absolute
/// workspace path (mirrors the ingest suite's fixture helper).
fn encode_workspace_path(project: &Path) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let path_str = project.to_string_lossy();
    let bytes = path_str.as_bytes();
    let mut out = String::new();
    let mut buf = 0_u32;
    let mut bits = 0_u32;
    for &byte in bytes {
        buf = (buf << 8) | u32::from(byte);
        bits += 8;
        while bits >= 6 {
            bits -= 6;
            let idx = ((buf >> bits) & 0x3F) as usize;
            out.push(TABLE[idx] as char);
        }
    }
    if bits > 0 {
        buf <<= 6 - bits;
        let idx = (buf & 0x3F) as usize;
        out.push(TABLE[idx] as char);
    }
    out
}

fn write_kiro_fixture(sandbox: &Sandbox, config: &BenchConfig) -> PathBuf {
    let data_dir = tracedecay::agents::kiro_data_dir(&sandbox.home);
    let encoded = encode_workspace_path(&sandbox.project);
    let session_dir = data_dir
        .join("User/globalStorage/kiro.kiroagent/workspace-sessions")
        .join(encoded);
    std::fs::create_dir_all(&session_dir).unwrap();
    for session in 0..config.sessions {
        let session_id = format!("bench-kiro-{session:04}");
        let messages = (0..config.messages_per_session)
            .map(|index| {
                serde_json::json!({
                    "role": if index % 2 == 0 { "user" } else { "assistant" },
                    "content": message_text(config, session, index),
                    "timestamp": 1_800_000_000_000_i64 + (index as i64) * 1_000
                })
            })
            .collect::<Vec<_>>();
        std::fs::write(
            session_dir.join(format!("{session_id}.json")),
            serde_json::to_string(&serde_json::json!({
                "sessionId": session_id,
                "modelId": "bench-model",
                "messages": messages
            }))
            .unwrap(),
        )
        .unwrap();
    }
    session_dir
}

/// Synthetic parse-free source: fabricates provider-neutral messages so the
/// shared persist path (cursor load, session merge, privacy staging, LCM raw
/// + projection writes, cursor advance) is measured without file parsing.
struct SyntheticStoreSource {
    config: BenchConfig,
    root: PathBuf,
}

impl TranscriptSource for SyntheticStoreSource {
    fn provider(&self) -> &'static str {
        "vibe"
    }

    fn transcript_paths(&self, _project_root: &Path) -> Vec<PathBuf> {
        (0..self.config.sessions)
            .map(|session| self.root.join(format!("synthetic-{session:04}.log")))
            .collect()
    }

    fn parse_new(
        &self,
        path: &Path,
        prev: StoredCursor,
        project_root: &Path,
        _max_new_bytes: Option<u64>,
    ) -> Option<ParsedTranscript> {
        if prev.position != 0 {
            return None;
        }
        let session = path
            .file_stem()?
            .to_str()?
            .rsplit('-')
            .next()?
            .parse::<usize>()
            .ok()?;
        let session_id = format!("bench-store-{session:04}");
        let messages = (0..self.config.messages_per_session)
            .map(|index| SessionMessageRecord {
                provider: "vibe".to_owned(),
                message_id: format!("bench-store-{session:04}-{index:06}"),
                session_id: session_id.clone(),
                role: if index % 2 == 0 { "user" } else { "assistant" }.to_owned(),
                timestamp: Some(1_800_000_000_000 + index as i64 * 1_000),
                ordinal: index as i64,
                text: message_text(&self.config, session, index),
                kind: None,
                model: None,
                tool_names: None,
                source_path: Some(path.to_string_lossy().into_owned()),
                source_offset: Some(index as i64),
                metadata_json: None,
            })
            .collect();
        Some(ParsedTranscript {
            draft: SessionDraft {
                session_id,
                project_key: project_root.to_string_lossy().into_owned(),
                project_path: project_root.to_string_lossy().into_owned(),
                title: None,
                metadata_json: None,
                parent_session_id: None,
                is_subagent: false,
                agent_id: None,
                parent_tool_use_id: None,
            },
            messages,
            new_cursor: StoredCursor {
                position: 1,
                mtime: 1,
                file_id: 0,
            },
        })
    }
}

struct BenchOutcome {
    provider: &'static str,
    passes: usize,
    elapsed: Duration,
    messages: u64,
    sessions: u64,
    fixture_bytes: u64,
}

impl BenchOutcome {
    fn report(&self, config: &BenchConfig) {
        let secs = self.elapsed.as_secs_f64().max(f64::EPSILON);
        println!(
            "bench.transcript_ingest provider={} sessions={} messages={} fixture_bytes={} \
             passes={} elapsed_ms={:.1} messages_per_sec={:.1} expected_messages={}",
            self.provider,
            self.sessions,
            self.messages,
            self.fixture_bytes,
            self.passes,
            self.elapsed.as_secs_f64() * 1000.0,
            self.messages as f64 / secs,
            config.total_messages(),
        );
    }
}

fn dir_bytes(dir: &Path) -> u64 {
    let mut total = 0;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            total += dir_bytes(&path);
        } else if let Ok(meta) = entry.metadata() {
            total += meta.len();
        }
    }
    total
}

async fn open_runtime(sandbox: &Sandbox, label: &str) -> HostAdmissionTestRuntimeV1 {
    let project_id =
        ProjectId::new(format!("tracedecay-ingest-bench-{label}")).expect("valid project id");
    assert!(
        write_repository_identity_marker(&sandbox.project, project_id.as_str()).unwrap(),
        "sandbox project must accept an identity marker"
    );
    HostAdmissionTestRuntimeV1::project(&sandbox.profile, &sandbox.project, project_id)
        .await
        .expect("open sandbox project runtime")
}

async fn run_provider_bench(
    provider: SessionProvider,
    label: &'static str,
    config: BenchConfig,
    write_fixture: fn(&Sandbox, &BenchConfig) -> PathBuf,
) -> BenchOutcome {
    let sandbox = sandbox(label);
    let fixture_root = write_fixture(&sandbox, &config);
    let fixture_bytes = dir_bytes(&fixture_root);
    let runtime = open_runtime(&sandbox, label).await;
    let home = sandbox.home.clone();
    let mut messages = 0_u64;
    let mut passes = 0_usize;
    let mut elapsed = Duration::ZERO;
    // Bounded pass budgets defer backlog; loop until quiescent so the number
    // covers the whole fixture at steady state.
    loop {
        let started = Instant::now();
        let stats = tracedecay_sessions::runtime::ingest::with_transcript_source_home(
            home.clone(),
            runtime.ingest_project_provider_for_test(&sandbox.project, Some(provider)),
        )
        .await
        .expect("provider ingest pass");
        elapsed += started.elapsed();
        passes += 1;
        messages += stats.messages_upserted;
        if stats.messages_upserted == 0 || passes >= 512 {
            break;
        }
    }
    // Upsert stats can double-count replays; the stored row count is the
    // dedup-checked truth for throughput.
    let stored = runtime
        .project_session_message_count_for_test()
        .await
        .expect("count stored session messages");
    BenchOutcome {
        provider: label,
        passes,
        elapsed,
        messages: messages.max(u64::try_from(stored).unwrap_or(0)),
        sessions: config.sessions as u64,
        fixture_bytes,
    }
}

async fn run_store_bench(config: BenchConfig) -> BenchOutcome {
    let sandbox = sandbox("store");
    let runtime = open_runtime(&sandbox, "store").await;
    let source = SyntheticStoreSource {
        config,
        root: sandbox.home.join("synthetic"),
    };
    let started = Instant::now();
    let stats = runtime
        .ingest_project_transcript_source_for_test(&source, &sandbox.project, None)
        .await
        .expect("synthetic store ingest");
    let elapsed = started.elapsed();
    BenchOutcome {
        provider: "store",
        passes: 1,
        elapsed,
        messages: stats.messages_upserted,
        sessions: stats.sessions_upserted,
        fixture_bytes: 0,
    }
}

fn parse_usize(arguments: &[String], flag: &str, default: usize) -> usize {
    arguments
        .iter()
        .position(|argument| argument == flag)
        .and_then(|index| arguments.get(index + 1))
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn main() {
    // Process-wide sandbox home: every provider source resolves transcripts
    // under `HOME`, and spawned blocking scans do not inherit the task-local
    // override, so the env var is the reliable process boundary here. This is
    // a single-purpose bench process; nothing else reads the real home.
    let sandbox_home = tempfile::Builder::new()
        .prefix("tracedecay-ingest-bench-home-")
        .tempdir()
        .expect("create bench home");
    // SAFETY: single-threaded startup, before the tokio runtime exists.
    unsafe {
        std::env::set_var("HOME", sandbox_home.path());
        std::env::set_var("USERPROFILE", sandbox_home.path());
    }

    let arguments = std::env::args()
        .skip(1)
        .filter(|argument| argument != "--bench")
        .collect::<Vec<_>>();
    let run_index = arguments.iter().position(|argument| argument == "--run");
    let Some(run_index) = run_index else {
        // `cargo bench` without arguments must stay cheap and green.
        println!("transcript_ingest bench: pass `-- --run <claude|codex|kiro|store|all>`");
        return;
    };
    let target = arguments
        .get(run_index + 1)
        .cloned()
        .unwrap_or_else(|| "all".to_owned());
    let defaults = BenchConfig::default();
    let config = BenchConfig {
        sessions: parse_usize(&arguments, "--sessions", defaults.sessions),
        messages_per_session: parse_usize(&arguments, "--messages", defaults.messages_per_session),
        large_every: parse_usize(&arguments, "--large-every", defaults.large_every),
        large_bytes: parse_usize(&arguments, "--large-bytes", defaults.large_bytes),
        small_bytes: parse_usize(&arguments, "--small-bytes", defaults.small_bytes),
    };

    #[cfg(feature = "hotpath")]
    let _hotpath = hotpath::HotpathGuardBuilder::new("transcript-ingest-bench")
        .sections_exclude(vec![hotpath::Section::FunctionsCpu])
        .build();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime");
    #[cfg(feature = "hotpath")]
    hotpath::tokio_runtime!(runtime.handle());

    runtime.block_on(async {
        let mut outcomes = Vec::new();
        if matches!(target.as_str(), "claude" | "all") {
            outcomes.push(
                run_provider_bench(
                    SessionProvider::Claude,
                    "claude",
                    config,
                    write_claude_fixture,
                )
                .await,
            );
        }
        if matches!(target.as_str(), "codex" | "all") {
            outcomes.push(
                run_provider_bench(SessionProvider::Codex, "codex", config, write_codex_fixture)
                    .await,
            );
        }
        if matches!(target.as_str(), "kiro" | "all") {
            outcomes.push(
                run_provider_bench(SessionProvider::Kiro, "kiro", config, write_kiro_fixture).await,
            );
        }
        if matches!(target.as_str(), "store" | "all") {
            outcomes.push(run_store_bench(config).await);
        }
        assert!(
            !outcomes.is_empty(),
            "unknown bench target {target}; expected claude|codex|kiro|store|all"
        );
        for outcome in &outcomes {
            outcome.report(&config);
        }
    });
}
