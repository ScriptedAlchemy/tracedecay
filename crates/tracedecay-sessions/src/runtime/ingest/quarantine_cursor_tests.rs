use std::path::PathBuf;

use serde_json::json;
use tracedecay_domain::{ObservationScopeV1, ProjectId};
use tracedecay_runtime_core::config::ProfileRoot;

use crate::admission::test_support::MemoryHostAdmission;
use crate::observation::ObservationCancellation;
use crate::runtime::SessionProvider;
use crate::runtime::hosts::codex::CodexDiscoveryHub;
use crate::runtime::ingest::project_provider::ProjectProviderRun;
use crate::runtime::jsonl_observation_admission::install_test_shared_jsonl_preparation_authority;
use crate::runtime::source::spin_until_jsonl_change_settled;

use super::project::with_transcript_source_profile;
use super::quarantine_contract::{assert_refused_source_contract, refuse_session};

const WORKSPACE: &str = "workspace-hash";

struct Fixture {
    _temp: tempfile::TempDir,
    home: PathBuf,
    project: PathBuf,
    state_db: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        let project = home.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let state_dir = home.join(".config/Cursor/User/globalStorage");
        std::fs::create_dir_all(&state_dir).unwrap();
        let state_db = state_dir.join("state.vscdb");
        rusqlite::Connection::open(&state_db)
            .unwrap()
            .execute_batch(
                "PRAGMA journal_mode=DELETE;
                 CREATE TABLE cursorDiskKV (key TEXT PRIMARY KEY, value TEXT);",
            )
            .unwrap();
        Self {
            _temp: temp,
            home,
            project,
            state_db,
        }
    }

    /// Writes a composer envelope and one user bubble per entry of `texts`.
    fn write_composer(&self, composer: &str, texts: &[&str]) {
        let connection = rusqlite::Connection::open(&self.state_db).unwrap();
        let headers = (0..texts.len())
            .map(|index| json!({ "bubbleId": format!("b{index}") }))
            .collect::<Vec<_>>();
        let envelope = json!({
            "composerId": composer,
            "lastUpdatedAt": 1_700_000_000_000_i64 + texts.len() as i64,
            "workspaceIdentifier": {
                "id": WORKSPACE,
                "uri": { "fsPath": self.project.to_string_lossy() }
            },
            "fullConversationHeadersOnly": headers
        });
        connection
            .execute(
                "INSERT OR REPLACE INTO cursorDiskKV(key, value) VALUES (?1, ?2)",
                rusqlite::params![format!("composerData:{composer}"), envelope.to_string()],
            )
            .unwrap();
        for (index, text) in texts.iter().enumerate() {
            connection
                .execute(
                    "INSERT OR REPLACE INTO cursorDiskKV(key, value) VALUES (?1, ?2)",
                    rusqlite::params![
                        format!("bubbleId:{composer}:b{index}"),
                        json!({ "type": 1, "text": text }).to_string()
                    ],
                )
                .unwrap();
        }
        drop(connection);
        spin_until_jsonl_change_settled(&self.state_db);
    }

    /// Writes `~/.cursor/chats/<ws>/<agent>/store.db` holding one user message.
    fn write_chat_store(&self, agent: &str, content: &str) {
        let dir = self.home.join(".cursor/chats").join(WORKSPACE).join(agent);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("store.db");
        let leaf = "ab".repeat(32);
        let meta = json!({
            "agentId": agent,
            "latestRootBlobId": leaf,
            "createdAt": 1_700_000_000_000_i64,
        })
        .to_string();
        let meta_hex = meta
            .bytes()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "PRAGMA journal_mode=DELETE;
                 CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT);
                 CREATE TABLE IF NOT EXISTS blobs (id TEXT PRIMARY KEY, data BLOB);",
            )
            .unwrap();
        connection
            .execute(
                "INSERT OR REPLACE INTO meta(key, value) VALUES ('0', ?1)",
                rusqlite::params![meta_hex],
            )
            .unwrap();
        connection
            .execute(
                "INSERT OR REPLACE INTO blobs(id, data) VALUES (?1, ?2)",
                rusqlite::params![
                    leaf,
                    json!({ "role": "user", "content": content })
                        .to_string()
                        .into_bytes()
                ],
            )
            .unwrap();
        drop(connection);
        spin_until_jsonl_change_settled(&path);
    }

    async fn assert_contract(
        &self,
        admission: &MemoryHostAdmission,
        sibling: &str,
        change_refused_source: impl FnOnce(),
    ) {
        install_test_shared_jsonl_preparation_authority();
        let hub = CodexDiscoveryHub::default();
        hub.register("project", Some(&self.home));
        let project_id = ProjectId::new("project-cursor-refused-source").unwrap();
        let scope = ObservationScopeV1::Project {
            project_id: project_id.clone(),
        };
        let cancellation = ObservationCancellation::default();
        assert_refused_source_contract(
            admission,
            sibling,
            || {
                let run = ProjectProviderRun {
                    project_root: &self.project,
                    project_id: &project_id,
                    facade: admission,
                    scope: &scope,
                    candidate: SessionProvider::Cursor,
                    max_new_bytes: u64::MAX,
                    cancellation: &cancellation,
                    codex_discovery: Some((&hub, "project")),
                };
                with_transcript_source_profile(
                    ProfileRoot::under_home(self.home.clone()),
                    async move { run.run().await.outcome },
                )
            },
            change_refused_source,
        )
        .await;
    }
}

/// `composerData:refused` sorts before `composerData:sibling`, so a scan that
/// stops at the refused composer never reaches the sibling.
#[tokio::test]
async fn cursor_composer_keeps_the_refused_source_contract() {
    let fixture = Fixture::new();
    fixture.write_composer("refused", &["first turn"]);
    fixture.write_composer("sibling", &["first turn"]);
    let admission = MemoryHostAdmission::default();
    refuse_session(&admission, "refused");

    fixture
        .assert_contract(&admission, "sibling", || {
            fixture.write_composer("refused", &["first turn", "second turn"]);
        })
        .await;
}

#[tokio::test]
async fn cursor_chat_store_keeps_the_refused_source_contract() {
    let fixture = Fixture::new();
    // The chat stores reach the sweep through this composer's workspace.
    fixture.write_composer("workspace-anchor", &[]);
    fixture.write_chat_store("refused", "first turn");
    fixture.write_chat_store("sibling", "first turn");
    let admission = MemoryHostAdmission::default();
    refuse_session(&admission, "cursor-chat:refused");

    fixture
        .assert_contract(&admission, "cursor-chat:sibling", || {
            fixture.write_chat_store("refused", "rewritten turn");
        })
        .await;
}
