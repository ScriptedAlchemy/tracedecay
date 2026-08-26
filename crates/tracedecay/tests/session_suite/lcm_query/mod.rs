use tempfile::TempDir;
use tracedecay::global_db::ParseOffset;
use tracedecay::host_admission::HostAdmissionTestRuntimeV1;
use tracedecay_runtime_core::db::engine::{Executor, params};
use tracedecay_sessions::runtime::lcm::{
    LCM_SCHEMA_VERSION, LcmContentSlice, LcmDescribeRequest, LcmDescribeTarget, LcmError,
    LcmExpandQueryRequest, LcmExpandRequest, LcmExpandTarget, LcmGcConfig, LcmGrepRequest,
    LcmGrepSort, LcmLifecycleUpdate, LcmLoadSessionRequest, LcmMaintenanceDebt, LcmScope,
    LcmSessionReplayRequest, LcmSourceRef, LcmStorageKind, LcmSummaryNodeDraft,
    MAX_DERIVED_SNIPPET_CHARS,
};
use tracedecay_sessions::runtime::{SessionMessageRecord, SessionRecord};
use tracedecay_usecases::host_admission::HostAdmissionScope;

use crate::common::{self, lcm_dag_message as raw_message};

async fn registered_lcm_runtime(tmp: &TempDir) -> HostAdmissionTestRuntimeV1 {
    HostAdmissionTestRuntimeV1::profile(tmp.path().join(".tracedecay"))
        .await
        .expect("registered LCM test runtime")
}

trait ProfileLcmFixture {
    async fn upsert_session(&self, session: &SessionRecord) -> bool;

    async fn upsert_session_message(&self, message: &SessionMessageRecord) -> bool;

    async fn upsert_transcript_batch(
        &self,
        session: &SessionRecord,
        messages: &[SessionMessageRecord],
        source: &str,
        offset: ParseOffset,
    ) -> bool;

    async fn lcm_insert_summary_node(
        &self,
        draft: LcmSummaryNodeDraft,
    ) -> Result<tracedecay_sessions::runtime::lcm::LcmSummaryNode, LcmError>;

    async fn lcm_ingest_raw_message(&self, message: &SessionMessageRecord) -> Result<(), LcmError>;

    async fn lcm_update_lifecycle(
        &self,
        update: LcmLifecycleUpdate,
    ) -> Result<tracedecay_sessions::runtime::lcm::LcmLifecycleState, LcmError>;
}

impl ProfileLcmFixture for HostAdmissionTestRuntimeV1 {
    async fn upsert_session(&self, session: &SessionRecord) -> bool {
        self.upsert_session_for_test(HostAdmissionScope::Profile, session)
            .await
            .unwrap_or(false)
    }

    async fn upsert_session_message(&self, message: &SessionMessageRecord) -> bool {
        self.upsert_session_message_for_test(HostAdmissionScope::Profile, message)
            .await
            .unwrap_or(false)
    }

    async fn upsert_transcript_batch(
        &self,
        session: &SessionRecord,
        messages: &[SessionMessageRecord],
        source: &str,
        offset: ParseOffset,
    ) -> bool {
        self.upsert_transcript_batch_for_test(
            HostAdmissionScope::Profile,
            session,
            messages,
            source,
            offset,
        )
        .await
        .is_ok()
    }

    async fn lcm_insert_summary_node(
        &self,
        draft: LcmSummaryNodeDraft,
    ) -> Result<tracedecay_sessions::runtime::lcm::LcmSummaryNode, LcmError> {
        self.lcm_insert_summary_node_for_test(HostAdmissionScope::Profile, draft)
            .await
    }

    async fn lcm_ingest_raw_message(&self, message: &SessionMessageRecord) -> Result<(), LcmError> {
        self.lcm_ingest_raw_message_for_test(HostAdmissionScope::Profile, message)
            .await
    }

    async fn lcm_update_lifecycle(
        &self,
        update: LcmLifecycleUpdate,
    ) -> Result<tracedecay_sessions::runtime::lcm::LcmLifecycleState, LcmError> {
        self.lcm_update_lifecycle_for_test(HostAdmissionScope::Profile, update)
            .await
    }
}

fn sample_session(provider: &str, session_id: &str) -> SessionRecord {
    common::session_record(
        provider,
        session_id,
        "/tmp/project",
        "LCM query test",
        None,
        None,
    )
}

struct RawMessageContext<'a> {
    role: &'a str,
    source: &'a str,
    timestamp: i64,
}

fn raw_message_with_role_source_timestamp(
    provider: &str,
    message_id: &str,
    session_id: &str,
    ordinal: i64,
    text: &str,
    context: RawMessageContext<'_>,
) -> SessionMessageRecord {
    let mut message = raw_message(provider, message_id, session_id, ordinal, text);
    message.role = context.role.to_string();
    message.timestamp = Some(context.timestamp);
    message.metadata_json = Some(serde_json::json!({"source": context.source}).to_string());
    message
}

async fn insert_session(db: &HostAdmissionTestRuntimeV1, provider: &str, session_id: &str) {
    assert!(
        db.upsert_session_for_test(
            HostAdmissionScope::Profile,
            &sample_session(provider, session_id),
        )
        .await
        .expect("session fixture should write")
    );
}

async fn insert_raw_messages(
    db: &HostAdmissionTestRuntimeV1,
    provider: &str,
    session_id: &str,
    contents: &[String],
) -> Vec<i64> {
    let session = sample_session(provider, session_id);
    let messages: Vec<_> = contents
        .iter()
        .enumerate()
        .map(|(idx, content)| {
            let message_id = format!("{session_id}-message-{:03}", idx + 1);
            raw_message(provider, &message_id, session_id, (idx + 1) as i64, content)
        })
        .collect();
    db.upsert_transcript_batch_for_test(
        HostAdmissionScope::Profile,
        &session,
        &messages,
        &format!("session-lcm-query-{provider}-{session_id}.jsonl"),
        ParseOffset::default(),
    )
    .await
    .expect("registered transcript fixture should write")
}

async fn replace_inline_content_without_updating_hash(
    db: &HostAdmissionTestRuntimeV1,
    store_id: i64,
    replacement: &str,
) {
    let database = db
        .registered_database(HostAdmissionScope::Profile)
        .expect("registered profile database");
    let writer = database
        .writer_connection()
        .expect("registered profile writer");
    Executor::execute(
        &writer,
        "UPDATE lcm_raw_messages SET content = ?1 WHERE store_id = ?2",
        params![replacement, store_id],
    )
    .await
    .expect("raw message fixture should be tampered");
}

async fn replace_summary_content_without_updating_hash(
    db: &HostAdmissionTestRuntimeV1,
    node_id: &str,
    replacement: &str,
) {
    let database = db
        .registered_database(HostAdmissionScope::Profile)
        .expect("registered profile database");
    let writer = database
        .writer_connection()
        .expect("registered profile writer");
    Executor::execute(
        &writer,
        "UPDATE lcm_summary_nodes SET summary_text = ?1 WHERE node_id = ?2",
        params![replacement, node_id],
    )
    .await
    .expect("summary fixture should be tampered");
}

fn summary_draft(
    provider: &str,
    session_id: &str,
    summary_text: &str,
    source_refs: Vec<LcmSourceRef>,
) -> LcmSummaryNodeDraft {
    LcmSummaryNodeDraft {
        provider: provider.to_string(),
        conversation_id: "conversation-1".to_string(),
        session_id: session_id.to_string(),
        depth: 0,
        summary_text: summary_text.to_string(),
        source_refs,
        source_token_count: 30,
        summary_token_count: 5,
        source_time_start: Some(1_715_000_000),
        source_time_end: Some(1_715_000_030),
        expand_hint: Some("query test summary".to_string()),
        metadata_json: None,
    }
}

mod describe;
mod expand;
mod expand_query;
mod grep;
mod grep_ranking;
mod load_session;
mod sessions;
mod status;
