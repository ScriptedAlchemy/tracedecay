use std::future::Future;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::json;
use tracedecay_domain::{ObservationScopeV1, ProjectId};
use tracedecay_runtime_core::config::ProfileRoot;
use tracedecay_store::{ParseOffset, TranscriptStore, TranscriptStoreResult, TranscriptWriteBatch};

use crate::admission::test_support::MemoryHostAdmission;
use crate::observation::ObservationCancellation;
use crate::runtime::SessionProvider;
use crate::runtime::hosts::codex::CodexDiscoveryHub;
use crate::runtime::hosts::cursor::cursor_project_slug;
use crate::runtime::ingest::project_provider::ProjectProviderRun;
use crate::runtime::jsonl_observation_admission::install_test_shared_jsonl_preparation_authority;
use crate::runtime::source::spin_until_jsonl_change_settled;
use crate::runtime::store_port::TranscriptIngestStore;

use super::failure::ProviderRunOutcome;
use super::project::with_transcript_source_profile;
use super::quarantine_contract::{assert_refused_source_contract, refuse_session};
use super::user_provider::UserProviderUnit;

const REFUSED: &str = "refused";
const SIBLING: &str = "sibling";

struct Fixture {
    _temp: tempfile::TempDir,
    home: PathBuf,
    project: PathBuf,
    project_id: ProjectId,
    scope: ObservationScopeV1,
    admission: MemoryHostAdmission,
    hub: CodexDiscoveryHub,
    cancellation: ObservationCancellation,
}

impl Fixture {
    fn new(provider: &str) -> Self {
        install_test_shared_jsonl_preparation_authority();
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        let project = home.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let hub = CodexDiscoveryHub::default();
        hub.register("project", Some(&home));
        let project_id = ProjectId::new(format!("project-{provider}-refused-source")).unwrap();
        let scope = ObservationScopeV1::Project {
            project_id: project_id.clone(),
        };
        let admission = MemoryHostAdmission::default();
        refuse_session(&admission, REFUSED);
        Self {
            _temp: temp,
            home,
            project,
            project_id,
            scope,
            admission,
            hub,
            cancellation: ObservationCancellation::default(),
        }
    }

    fn pass(&self, candidate: SessionProvider) -> impl Future<Output = ProviderRunOutcome> + '_ {
        let run = ProjectProviderRun {
            project_root: &self.project,
            project_id: &self.project_id,
            facade: &self.admission,
            scope: &self.scope,
            candidate,
            max_new_bytes: u64::MAX,
            cancellation: &self.cancellation,
            codex_discovery: Some((&self.hub, "project")),
        };
        with_transcript_source_profile(ProfileRoot::under_home(self.home.clone()), async move {
            run.run().await.outcome
        })
    }

    async fn assert_contract(
        &self,
        candidate: SessionProvider,
        change_refused_source: impl FnOnce(),
    ) {
        assert_refused_source_contract(
            &self.admission,
            SIBLING,
            || self.pass(candidate),
            change_refused_source,
        )
        .await;
    }
}

fn write_settled(path: &Path, lines: &[serde_json::Value]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let body = lines
        .iter()
        .map(|line| format!("{line}\n"))
        .collect::<String>();
    std::fs::write(path, body).unwrap();
    spin_until_jsonl_change_settled(path);
}

fn append_settled(path: &Path, line: &serde_json::Value) {
    let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    writeln!(file, "{line}").unwrap();
    drop(file);
    spin_until_jsonl_change_settled(path);
}

#[tokio::test]
async fn vibe_keeps_the_refused_source_contract() {
    let fixture = Fixture::new("vibe");
    let session = |id: &str| fixture.home.join(".vibe/logs/session").join(id);
    // Vibe sweeps newest first, path ascending on a tie: writing the refused
    // session last puts it ahead of its sibling.
    for id in [SIBLING, REFUSED] {
        write_settled(
            &session(id).join("meta.json"),
            &[json!({"session_id": id, "environment": {"working_directory": fixture.project}})],
        );
        write_settled(
            &session(id).join("messages.jsonl"),
            &[json!({"role": "user", "content": format!("{id} first turn")})],
        );
    }

    fixture
        .assert_contract(SessionProvider::Vibe, || {
            append_settled(
                &session(REFUSED).join("messages.jsonl"),
                &json!({"role": "assistant", "content": "refused second turn"}),
            );
        })
        .await;
}

/// Discovery scopes each file by its header `cwd`, not by its directory name.
fn pi_session_file(fixture: &Fixture, id: &str) -> PathBuf {
    fixture
        .home
        .join(".pi/agent/sessions/--project--")
        .join(format!("2026-09-28T00-00-00-000Z_{id}.jsonl"))
}

fn pi_message(entry: &str, parent: Option<&str>, text: &str) -> serde_json::Value {
    json!({
        "type": "message",
        "id": entry,
        "parentId": parent,
        "timestamp": "2026-09-28T00:00:01.000Z",
        "message": {"role": "user", "content": text, "timestamp": 1_790_553_601_000_u64}
    })
}

#[tokio::test]
async fn pi_keeps_the_refused_source_contract() {
    let fixture = Fixture::new("pi");
    // The durable queue holds discovered files in path order, so the refused
    // session is the first queue entry.
    for id in [REFUSED, SIBLING] {
        write_settled(
            &pi_session_file(&fixture, id),
            &[
                json!({"type": "session", "version": 3, "id": id, "timestamp": "2026-09-28T00:00:00.000Z", "cwd": fixture.project}),
                pi_message("11112222", None, &format!("{id} first turn")),
            ],
        );
    }

    fixture
        .assert_contract(SessionProvider::Pi, || {
            append_settled(
                &pi_session_file(&fixture, REFUSED),
                &pi_message("33334444", Some("11112222"), "refused second turn"),
            );
        })
        .await;
}

fn kimi_wire_message(text: &str) -> serde_json::Value {
    json!({
        "type": "context.append_message",
        "agentId": "main",
        "message": {"role": "user", "content": [{"type": "text", "text": text}], "toolCalls": []},
        "time": 1_790_553_601_u64
    })
}

#[tokio::test]
async fn kimi_keeps_the_refused_source_contract() {
    let fixture = Fixture::new("kimi");
    let session = |id: &str| fixture.home.join(".kimi-code/sessions/wd_project").join(id);
    // Admission receives the state's `id` (which must equal the session
    // directory name) unchanged; the durable queue holds wires in path order,
    // so the refused wire is the first queue entry.
    for id in [REFUSED, SIBLING] {
        write_settled(
            &session(id).join("agents/main/wire.jsonl"),
            &[kimi_wire_message(&format!("{id} first turn"))],
        );
        let state = session(id).join("state.json");
        std::fs::write(
            &state,
            json!({"id": id, "version": 2, "cwd": fixture.project, "agents": {"main": {"type": "main"}}})
                .to_string(),
        )
        .unwrap();
        spin_until_jsonl_change_settled(&state);
    }

    fixture
        .assert_contract(SessionProvider::Kimi, || {
            append_settled(
                &session(REFUSED).join("agents/main/wire.jsonl"),
                &kimi_wire_message("refused second turn"),
            );
        })
        .await;
}

fn cursor_message(text: &str) -> serde_json::Value {
    json!({"role": "user", "message": {"content": [{"type": "text", "text": text}]}})
}

#[tokio::test]
async fn cursor_keeps_the_refused_source_contract() {
    let fixture = Fixture::new("cursor");
    // No `state.vscdb` exists, so only the JSONL sweep runs. It pages
    // sessions in id order, so the refused transcript comes first.
    let transcript = |id: &str| {
        fixture
            .home
            .join(".cursor/projects")
            .join(cursor_project_slug(&fixture.project).unwrap())
            .join("agent-transcripts")
            .join(format!("{id}.jsonl"))
    };
    for id in [REFUSED, SIBLING] {
        write_settled(
            &transcript(id),
            &[cursor_message(&format!("{id} first turn"))],
        );
    }

    fixture
        .assert_contract(SessionProvider::Cursor, || {
            append_settled(&transcript(REFUSED), &cursor_message("refused second turn"));
        })
        .await;
}

/// Only the Codex user run reads the transcript store.
struct UnusedTranscriptStore;

impl TranscriptStore for UnusedTranscriptStore {
    fn get_parse_offset(
        &self,
        _cursor_path: &Path,
    ) -> impl Future<Output = TranscriptStoreResult<ParseOffset>> + Send {
        std::future::ready(Ok(ParseOffset::default()))
    }

    fn persist_transcript_batch(
        &self,
        _batch: TranscriptWriteBatch,
    ) -> impl Future<Output = TranscriptStoreResult<()>> + Send {
        std::future::ready(Ok(()))
    }
}

impl TranscriptIngestStore for UnusedTranscriptStore {}

#[tokio::test]
async fn cursor_keeps_the_refused_source_contract_in_user_scope() {
    let fixture = Fixture::new("cursor-user");
    fixture.hub.register("profile", Some(&fixture.home));
    let transcript = |id: &str| {
        fixture
            .home
            .join(".cursor/projects")
            .join(cursor_project_slug(&fixture.project).unwrap())
            .join("agent-transcripts")
            .join(format!("{id}.jsonl"))
    };
    for id in [REFUSED, SIBLING] {
        write_settled(
            &transcript(id),
            &[cursor_message(&format!("{id} first turn"))],
        );
    }
    // User scope sweeps the transcripts of every project nobody registered.
    let pass = || {
        let unit = UserProviderUnit {
            store: &UnusedTranscriptStore,
            profile_root: &fixture.home,
            roots: &[],
            facade: &fixture.admission,
            candidate: SessionProvider::Cursor,
            max_new_bytes: u64::MAX,
            cancellation: &fixture.cancellation,
            codex_discovery: Some((&fixture.hub, "profile")),
        };
        with_transcript_source_profile(ProfileRoot::under_home(fixture.home.clone()), async move {
            unit.run().await.outcome
        })
    };

    assert_refused_source_contract(&fixture.admission, SIBLING, pass, || {
        append_settled(&transcript(REFUSED), &cursor_message("refused second turn"));
    })
    .await;
}
