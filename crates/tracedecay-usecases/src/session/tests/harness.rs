use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{Value, json};
use sha2::Digest;
use tempfile::TempDir;
use tracedecay_domain::{
    CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1, CanonicalObservationEvidenceV1,
    CanonicalObservationFactV1, CanonicalObservationRelationsV1, DurableObservationV1,
    ObservationId, ObservationIdentityMaterialV1, ObservationOrderingDomainV1, ObservationScopeV1,
    ObservationSourceGenerationV1, ObservationSourceIdentityV1, ObservationSourceRangeV1,
    PayloadReferenceV1, ProjectId, ProjectionGenerationId, ProviderId, RetentionClass,
    RetrievalAnchorRecord, SanitizationReceiptId, SanitizationReceiptRefV1, SanitizationReceiptV1,
    SanitizerDispositionV1, SensitivityV1, SessionId, UtcMicros,
};
use tracedecay_store::{
    build_observation_resolution_authorization_v1, build_observation_retrieval_anchor_v2,
};

use crate::host_admission::{HostAdmissionScope, HostAdmissionTestRuntimeV1};
use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_runtime_core::db::engine::params;
use tracedecay_sessions::runtime::lcm::payload::{upsert_payload_metadata, write_external_payload};

pub(super) const PROJECT_ID: &str = "project.tracedecay";
pub(super) const INLINE_PAYLOAD: &str = "non-empty inline occurrence payload";
pub(super) const EXTERNAL_PAYLOAD: &str = "non-empty external occurrence payload";
pub(super) const PRIVACY_CANARY: &str = "sk-proj-private-canary";
pub(super) const SAFE_PRIVACY_PAYLOAD: &str = "The billing pipeline regression is fixed.";

pub(super) struct RegisteredTemporalHarness {
    pub(super) registered: Arc<RegisteredGlobalDb>,
    _directory: TempDir,
    _runtime: HostAdmissionTestRuntimeV1,
}

impl RegisteredTemporalHarness {
    pub(super) async fn open(label: &str) -> Self {
        let directory = tempfile::tempdir().expect("temporary registered session store");
        let profile_root = directory.path().join("profile");
        let runtime = HostAdmissionTestRuntimeV1::profile(&profile_root)
            .await
            .unwrap_or_else(|error| panic!("{label}: registered profile runtime: {error}"));
        let registered = runtime
            .registered_database_arc(HostAdmissionScope::Profile)
            .expect("registered profile database");
        Self {
            registered,
            _directory: directory,
            _runtime: runtime,
        }
    }

    pub(super) async fn seed_application_fixture(&self) -> [u8; 32] {
        self.seed_cursor_key("application-key", 1, 0x44).await;
        let inline = fixture_observation(
            1,
            "session.temporal.application",
            "provider.application",
            "message-1",
            "record-1",
            "receipt-1",
            INLINE_PAYLOAD,
            false,
        );
        let inline_anchor = self.persist_observation(&inline).await;
        let external = fixture_observation(
            2,
            "session.temporal.application",
            "provider.application",
            "message-2",
            "record-2",
            "receipt-2",
            EXTERNAL_PAYLOAD,
            true,
        );
        let external_anchor = self.persist_observation(&external).await;
        assert_eq!(
            policy_digest_bytes(&inline_anchor),
            policy_digest_bytes(&external_anchor),
            "one registered authority namespace must produce one access policy"
        );
        let authority = fixture_observation(
            3,
            "session.temporal.application",
            "provider.application",
            "message-3",
            "record-3",
            "receipt-3",
            "payload authority",
            false,
        );
        let authority_anchor = self.persist_observation(&authority).await;
        self.seed_session(
            "session.temporal.application",
            "provider.application",
            "application-key",
            1,
        )
        .await;
        self.seed_occurrence(&inline, &inline_anchor, "message-1", INLINE_PAYLOAD, 1)
            .await;
        self.seed_occurrence(
            &external,
            &external_anchor,
            "message-2",
            EXTERNAL_PAYLOAD,
            2,
        )
        .await;
        self.seed_external_payload(&authority_anchor).await;
        policy_digest_bytes(&inline_anchor)
    }

    pub(super) fn application_external_payload_path(&self) -> PathBuf {
        let payload_dir = self
            .registered
            .db_path()
            .parent()
            .expect("registered profile storage root")
            .join("lcm-payloads");
        let mut payloads = fs::read_dir(payload_dir)
            .expect("application payload directory")
            .map(|entry| entry.expect("application payload entry").path())
            .collect::<Vec<_>>();
        assert_eq!(payloads.len(), 1, "application fixture has one payload");
        payloads.pop().expect("application payload path")
    }

    pub(super) async fn seed_empty_fixture(&self) -> [u8; 32] {
        self.seed_cursor_key("application-empty-key", 1, 0x47).await;
        self.seed_session(
            "session.temporal.application",
            "provider.application",
            "application-empty-key",
            1,
        )
        .await;
        [0x5a; 32]
    }

    pub(super) async fn seed_root_fixture(&self) -> [u8; 32] {
        self.seed_cursor_key("application-root-key", 1, 0x45).await;
        let mut digest = None;
        for (session_id, provider, message_id, record_id, receipt_id, payload) in [
            (
                "session.root.a",
                "provider.application",
                "message-root-a",
                "record-root-a",
                "receipt-root-a",
                "root-wide payload from session alpha",
            ),
            (
                "session.root.b",
                "provider.other",
                "message-root-b",
                "record-root-b",
                "receipt-root-b",
                "root-wide payload from session beta",
            ),
        ] {
            let observation = fixture_observation(
                1, session_id, provider, message_id, record_id, receipt_id, payload, false,
            );
            let anchor = self.persist_observation(&observation).await;
            let actual = policy_digest_bytes(&anchor);
            if let Some(expected) = digest {
                assert_eq!(actual, expected);
            } else {
                digest = Some(actual);
            }
            self.seed_session(session_id, provider, "application-root-key", 1)
                .await;
            self.seed_occurrence(&observation, &anchor, message_id, payload, 1)
                .await;
        }
        let foreign = fixture_observation(
            1,
            "session.root.foreign",
            "provider.application",
            "message-root-foreign",
            "record-root-foreign",
            "receipt-root-foreign",
            "root-wide payload from another project",
            false,
        );
        let foreign_anchor = self.persist_observation(&foreign).await;
        self.seed_session_in_project(
            "session.root.foreign",
            "provider.application",
            "project.foreign",
            "application-root-key",
            1,
        )
        .await;
        self.seed_occurrence(
            &foreign,
            &foreign_anchor,
            "message-root-foreign",
            "root-wide payload from another project",
            1,
        )
        .await;
        digest.expect("root fixture policy digest")
    }

    pub(super) async fn seed_privacy_fixture(&self) -> [u8; 32] {
        self.seed_cursor_key("privacy-key", 1, 0x46).await;
        let observation = fixture_observation(
            1,
            "session.temporal.privacy",
            "codex",
            "message-privacy",
            "record-privacy",
            "receipt-privacy",
            SAFE_PRIVACY_PAYLOAD,
            false,
        );
        let anchor = self.persist_observation(&observation).await;
        self.seed_session("session.temporal.privacy", "codex", "privacy-key", 1)
            .await;
        self.seed_occurrence(
            &observation,
            &anchor,
            "message-privacy",
            SAFE_PRIVACY_PAYLOAD,
            1,
        )
        .await;
        policy_digest_bytes(&anchor)
    }

    pub(super) async fn seed_quarantined_legacy_fixture(&self) {
        self.registered
            .writer_connection()
            .expect("registered writer")
            .execute_batch(
                "INSERT INTO sessions (
                    provider, session_id, project_key, project_path
                 ) VALUES (
                    'claude', 'session.temporal.legacy', 'project.tracedecay', '/fixture'
                 );
                 INSERT INTO lcm_raw_messages (
                    provider, message_id, session_id, role, ordinal, timestamp,
                    content, content_hash, storage_kind, payload_ref,
                    snippet_text, index_text, metadata_json, legacy_source, legacy_truncated
                 ) VALUES (
                    'claude', 'message.temporal.legacy', 'session.temporal.legacy',
                    'user', 1, 1, 'sk-proj-private-canary',
                    'sha256:quarantined', 'inline', NULL,
                    'quarantined legacy record', 'quarantined legacy record',
                    '{\"payload_access\":\"quarantined\",\"migration\":\"legacy-unsanitized\"}',
                    1, 0
                 );",
            )
            .await
            .expect("seed quarantined legacy source");
    }

    /// Reopens the profile sessions store through the registry, so a test can
    /// assert a property survives losing and re-acquiring the handle rather
    /// than only rebuilding the objects layered over one.
    pub(super) async fn remount(&self) -> Arc<RegisteredGlobalDb> {
        self._runtime
            .remount_profile_database_for_test()
            .await
            .expect("remounted profile sessions")
    }

    /// Every full-text sink the schema currently defines. Enumerated rather
    /// than listed so a sink added later is swept without editing the test.
    pub(super) async fn full_text_sinks(&self) -> Vec<String> {
        let snapshot = self
            .registered
            .read_snapshot()
            .await
            .expect("registered read snapshot");
        let mut rows = snapshot
            .query(
                "SELECT name FROM sqlite_master
                 WHERE type = 'table' AND sql LIKE '%USING fts5%'
                 ORDER BY name",
                (),
            )
            .await
            .expect("query full-text sinks");
        let mut sinks = Vec::new();
        while let Some(row) = rows.next().await.expect("full-text sink row") {
            sinks.push(row.get::<String>(0).expect("sink name column"));
        }
        sinks
    }

    pub(super) async fn count(&self, sql: &str) -> i64 {
        let snapshot = self
            .registered
            .read_snapshot()
            .await
            .expect("registered read snapshot");
        let mut rows = snapshot.query(sql, ()).await.expect("count query");
        rows.next()
            .await
            .expect("count row")
            .expect("count result")
            .get(0)
            .expect("count value")
    }

    pub(super) async fn raw_store_id(&self, message_id: &str) -> i64 {
        let snapshot = self
            .registered
            .read_snapshot()
            .await
            .expect("registered read snapshot");
        let mut rows = snapshot
            .query(
                "SELECT store_id
                 FROM lcm_raw_messages
                 WHERE provider = 'provider.application'
                   AND session_id = 'session.temporal.application'
                   AND message_id = ?1",
                [message_id],
            )
            .await
            .expect("raw message lookup");
        let store_id = rows
            .next()
            .await
            .expect("raw message row")
            .expect("raw message result")
            .get(0)
            .expect("raw message store id");
        assert!(
            rows.next().await.expect("raw message uniqueness").is_none(),
            "raw message fixture must be unique"
        );
        store_id
    }

    async fn seed_cursor_key(&self, key_id: &str, version: i64, material: u8) {
        self.registered
            .writer_connection()
            .expect("registered writer")
            .execute(
                "INSERT INTO session_query_cursor_keys (
                    key_id, key_version, key_material, created_at, retired_at
                 ) VALUES (?1, ?2, ?3, 1, NULL)",
                params![key_id, version, vec![material; 32]],
            )
            .await
            .expect("seed cursor key");
    }

    async fn seed_session(&self, session_id: &str, provider: &str, key_id: &str, version: i64) {
        self.seed_session_in_project(session_id, provider, PROJECT_ID, key_id, version)
            .await;
    }

    async fn seed_session_in_project(
        &self,
        session_id: &str,
        provider: &str,
        project_key: &str,
        key_id: &str,
        version: i64,
    ) {
        let transaction = self
            .registered
            .begin_write_transaction()
            .await
            .expect("registered writer transaction");
        transaction
            .execute(
                "INSERT INTO sessions (provider, session_id, project_key, project_path)
                 VALUES (?1, ?2, ?3, '/fixture')",
                params![provider, session_id, project_key],
            )
            .await
            .expect("seed session");
        let frozen = json!({
            "active_generation": 1,
            "cursor_key": {"key_id": key_id, "version": version},
            "projection_frontier": 0,
            "source_frontier": 0,
            "summary_frontier": 0
        })
        .to_string();
        transaction
            .execute(
                "INSERT INTO session_temporal_generations (
                    session_id, generation, state, frozen_watermarks_json, created_at,
                    ready_at, activated_at, completed_at
                 ) VALUES (?1, 1, 'building', ?2, 1, NULL, NULL, NULL)",
                params![session_id, frozen],
            )
            .await
            .expect("seed building generation");
        transaction
            .execute(
                "UPDATE session_temporal_generations
                 SET state = 'ready', ready_at = 1
                 WHERE session_id = ?1 AND generation = 1",
                params![session_id],
            )
            .await
            .expect("ready generation");
        transaction
            .execute(
                "UPDATE session_temporal_generations
                 SET state = 'active', activated_at = 1
                 WHERE session_id = ?1 AND generation = 1",
                params![session_id],
            )
            .await
            .expect("activate generation");
        transaction.commit().await.expect("commit session fixture");
    }

    async fn persist_observation(
        &self,
        observation: &DurableObservationV1,
    ) -> RetrievalAnchorRecord {
        let projection = ProjectionGenerationId::new("projection.application-fixture.v1").unwrap();
        let authorization =
            build_observation_resolution_authorization_v1(observation, "application-fixture")
                .unwrap();
        let anchor = build_observation_retrieval_anchor_v2(
            observation,
            projection,
            UtcMicros(1),
            authorization,
        )
        .unwrap();
        let receipt = observation.receipt();
        let receipt_json = serde_json::to_string(receipt).unwrap();
        let observation_json = serde_json::to_string(observation).unwrap();
        let anchor_json = serde_json::to_string(&anchor).unwrap();
        let owner_json = serde_json::to_string(anchor.owner()).unwrap();
        let transaction = self
            .registered
            .begin_write_transaction()
            .await
            .expect("registered writer transaction");
        transaction
            .execute(
                "INSERT INTO sanitization_receipts (
                    receipt_id, sanitizer_version, payload_digest, receipt_json
                 ) VALUES (?1, ?2, ?3, ?4)",
                params![
                    receipt.receipt().receipt_id().as_str(),
                    receipt.receipt().sanitizer_version().as_str(),
                    observation.payload_reference().digest().as_str(),
                    receipt_json
                ],
            )
            .await
            .expect("seed receipt");
        transaction
            .execute(
                "INSERT INTO observations (
                    observation_id, payload_digest, receipt_id,
                    observation_json, committed_cursor_json
                 ) VALUES (?1, ?2, ?3, ?4, '{}')",
                params![
                    observation.observation_id().as_str(),
                    observation.payload_reference().digest().as_str(),
                    receipt.receipt().receipt_id().as_str(),
                    observation_json
                ],
            )
            .await
            .expect("seed observation");
        transaction
            .execute(
                "INSERT INTO retrieval_anchors (
                    anchor_id, anchor_json, owner_json, projection_generation
                 ) VALUES (?1, ?2, ?3, ?4)",
                params![
                    anchor.anchor_id().as_str(),
                    anchor_json,
                    owner_json,
                    anchor.projection_generation().as_str()
                ],
            )
            .await
            .expect("seed retrieval anchor");
        transaction
            .execute(
                "INSERT INTO observation_retrieval_anchors (observation_id, anchor_id)
                 VALUES (?1, ?2)",
                params![
                    observation.observation_id().as_str(),
                    anchor.anchor_id().as_str()
                ],
            )
            .await
            .expect("bind observation anchor");
        transaction
            .commit()
            .await
            .expect("commit observation fixture");
        anchor
    }

    async fn seed_occurrence(
        &self,
        observation: &DurableObservationV1,
        anchor: &RetrievalAnchorRecord,
        message_id: &str,
        payload: &str,
        ordinal: i64,
    ) {
        let occurrence_id = tracedecay_domain::MessageOccurrenceIdV1::derive(
            observation.observation_id(),
            tracedecay_domain::ProjectionOutputOrdinalV1::new(0),
        );
        let evidence = json!({
            "authority": "provider_native",
            "evidence_class": "provider_declared",
            "source_anchor_id": anchor.anchor_id(),
            "sanitization_receipt": {
                "receipt_id": observation.receipt().receipt().receipt_id(),
                "sanitizer_version": "sanitizer.application-fixture.v1"
            }
        })
        .to_string();
        let writer = self
            .registered
            .writer_connection()
            .expect("registered writer");
        writer
            .execute(
                "INSERT INTO session_occurrences (
                    session_id, generation, occurrence_id, source_observation_id,
                    projection_output_ordinal, retrieval_anchor_id, message_id,
                    role, knowledge_at, valid_time_json, evidence_json,
                    snippet_text, index_text
                 ) VALUES (?1, 1, ?2, ?3, 0, ?4, ?5, 'assistant', ?6, ?7, ?8, ?9, ?9)",
                params![
                    observation.source().session_id().as_str(),
                    occurrence_id.as_str(),
                    observation.observation_id().as_str(),
                    anchor.anchor_id().as_str(),
                    message_id,
                    ordinal,
                    json!({"kind": "known", "valid_at": ordinal}).to_string(),
                    evidence,
                    payload
                ],
            )
            .await
            .expect("seed temporal occurrence");
        writer
            .execute(
                "INSERT INTO session_current_entities (
                    session_id, generation, entity_kind, entity_id,
                    current_assertion_id, current_occurrence_id, coverage_json
                 ) VALUES (?1, 1, 'occurrence_anchor', ?2, NULL, ?3,
                           '{\"occurrence_count\":1}')",
                params![
                    observation.source().session_id().as_str(),
                    anchor.anchor_id().as_str(),
                    occurrence_id.as_str()
                ],
            )
            .await
            .expect("seed current occurrence");
        if message_id != "message-2" {
            writer
                .execute(
                    "INSERT INTO lcm_raw_messages (
                        provider, message_id, session_id, role, ordinal, timestamp,
                        content, content_hash, storage_kind, payload_ref,
                        snippet_text, index_text, legacy_source, legacy_truncated
                     ) VALUES (
                        ?1, ?2, ?3, 'assistant', ?4, ?4, ?5, ?6,
                        'inline', NULL, ?5, ?5, 0, 0
                     )",
                    params![
                        observation.source().provider().as_str(),
                        message_id,
                        observation.source().session_id().as_str(),
                        ordinal,
                        payload,
                        payload_digest(payload)
                    ],
                )
                .await
                .expect("seed inline raw message");
        }
    }

    async fn seed_external_payload(&self, authority_anchor: &RetrievalAnchorRecord) {
        let db_path = self.registered.db_path();
        let payload = write_external_payload(
            db_path.parent().unwrap(),
            "provider.application",
            "session.temporal.application",
            "message-2",
            "message",
            EXTERNAL_PAYLOAD,
            None,
        )
        .expect("write external payload through production filesystem authority");
        let transaction = self
            .registered
            .begin_write_transaction()
            .await
            .expect("registered writer transaction");
        transaction
            .execute(
                "INSERT INTO lcm_raw_messages (
                    provider, message_id, session_id, role, ordinal, timestamp,
                    content, content_hash, storage_kind, payload_ref,
                    snippet_text, index_text, legacy_source, legacy_truncated
                 ) VALUES (
                    'provider.application', 'message-2', 'session.temporal.application',
                    'assistant', 2, 2, NULL, ?1, 'external', ?2, ?3, ?3, 0, 0
                 )",
                params![
                    payload.content_hash.as_str(),
                    payload.payload_ref.as_str(),
                    EXTERNAL_PAYLOAD
                ],
            )
            .await
            .expect("seed external raw message");
        upsert_payload_metadata(&transaction, &payload)
            .await
            .expect("seed external payload");
        let manifest = json!({
            "provider": payload.provider.as_str(),
            "session_id": payload.session_id.as_str(),
            "message_id": payload.message_id.as_str(),
            "byte_count": payload.byte_count,
            "char_count": payload.char_count
        })
        .to_string();
        let publication = json!({
            "receipt_id": "receipt-3",
            "payloads": [{
                "payload_ref": payload.payload_ref.as_str(),
                "digest": payload.content_hash.as_str(),
                "manifest_json": manifest.as_str()
            }]
        })
        .to_string();
        transaction
            .execute(
                "INSERT INTO session_summary_nodes (
                    summary_id, session_id, summary_anchor_id, summary_text,
                    index_text, source_horizon_json, publication_json, created_at
                 ) VALUES (
                    'summary-external-authority', 'session.temporal.application', ?1,
                    'payload authority', 'payload authority', '{}', ?2, 1
                 )",
                params![authority_anchor.anchor_id().as_str(), publication],
            )
            .await
            .expect("seed external payload authority");
        transaction
            .execute(
                "INSERT INTO session_external_payload_manifests (
                    payload_ref, session_id, payload_digest, manifest_json, receipt_id, created_at
                 ) VALUES (?1, 'session.temporal.application', ?2, ?3, 'receipt-3', 1)",
                params![
                    payload.payload_ref.as_str(),
                    payload.content_hash.as_str(),
                    manifest
                ],
            )
            .await
            .expect("seed external payload manifest");
        transaction
            .commit()
            .await
            .expect("commit external payload fixture");
    }
}

fn fixture_receipt(receipt_id: &str, payload: &Value) -> SanitizationReceiptV1 {
    SanitizationReceiptV1::new(
        SanitizationReceiptRefV1::new(
            SanitizationReceiptId::new(receipt_id).unwrap(),
            tracedecay_domain::ComponentVersion::new("sanitizer.application-fixture.v1").unwrap(),
        )
        .unwrap(),
        SanitizerDispositionV1::Accepted,
        SensitivityV1::NonSensitive,
        Some(PayloadReferenceV1::for_payload(payload).unwrap()),
    )
    .unwrap()
}

#[allow(clippy::too_many_arguments)]
fn fixture_observation(
    ordinal: u64,
    session_id: &str,
    provider: &str,
    message_id: &str,
    record_id: &str,
    receipt_id: &str,
    content: &str,
    without_payload: bool,
) -> DurableObservationV1 {
    let session_id = SessionId::new(session_id).unwrap();
    let provider = ProviderId::new(provider).unwrap();
    let source =
        ObservationSourceIdentityV1::for_provider(provider.clone(), session_id.clone()).unwrap();
    let range = ObservationSourceRangeV1::new(ordinal, ordinal + 1).unwrap();
    let message_id = ObservationId::new(message_id).unwrap();
    let record_id = ObservationId::new(record_id).unwrap();
    let facts = if without_payload {
        vec![CanonicalObservationFactV1::Usage {
            input_tokens: None,
            output_tokens: None,
            cache_read_tokens: None,
            cache_write_tokens: None,
            reasoning_tokens: None,
        }]
    } else {
        vec![CanonicalObservationFactV1::Message {
            role: CanonicalMessageRoleV1::Assistant,
            content: json!({"text": content}),
            model: None,
            timestamp: Some(ordinal as i64),
        }]
    };
    let kind = if without_payload { "usage" } else { "message" };
    let relations = CanonicalObservationRelationsV1::new(session_id).with_message_id(message_id);
    let envelope = CanonicalObservationEnvelopeV1::new(
        provider,
        kind,
        record_id.clone(),
        relations,
        facts,
        CanonicalObservationEvidenceV1::new(ObservationOrderingDomainV1::SnapshotOrder, range),
    )
    .unwrap();
    let payload = serde_json::to_value(envelope).unwrap();
    let identity = ObservationIdentityMaterialV1::for_native_record(
        source,
        ObservationScopeV1::Project {
            project_id: ProjectId::new(PROJECT_ID).unwrap(),
        },
        ObservationSourceGenerationV1::new(1).unwrap(),
        range,
        ObservationOrderingDomainV1::SnapshotOrder,
        record_id,
    )
    .unwrap();
    DurableObservationV1::new(
        identity,
        fixture_receipt(receipt_id, &payload),
        RetentionClass::new("retention.application-fixture").unwrap(),
        payload,
    )
    .unwrap()
}

fn policy_digest_bytes(anchor: &RetrievalAnchorRecord) -> [u8; 32] {
    let encoded = anchor
        .authorization()
        .access_policy_digest
        .as_str()
        .strip_prefix("sha256:")
        .unwrap();
    hex::decode(encoded).unwrap().try_into().unwrap()
}

fn payload_digest(payload: &str) -> String {
    format!(
        "sha256:{}",
        hex::encode(sha2::Sha256::digest(payload.as_bytes()))
    )
}
