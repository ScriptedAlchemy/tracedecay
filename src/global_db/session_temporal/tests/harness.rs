use std::fs;
use std::sync::Arc;

use serde_json::{Value, json};
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

use crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1;
use crate::db::DaemonDatabaseScope;
use crate::db::engine::{Executor, QueryExecutor, params};
use crate::global_db::RegisteredGlobalDb;

pub(super) const PROJECT_ID: &str = "project.tracedecay";
pub(super) const INLINE_PAYLOAD: &str = "non-empty inline occurrence payload";
pub(super) const EXTERNAL_PAYLOAD: &str = "non-empty external occurrence payload";
pub(super) const PRIVACY_CANARY: &str = "sk-proj-private-canary";
pub(super) const SAFE_PRIVACY_PAYLOAD: &str = "The billing pipeline regression is fixed.";

pub(super) struct RegisteredTemporalHarness {
    pub(super) registered: Arc<RegisteredGlobalDb>,
    _directory: TempDir,
    _scope: DaemonDatabaseScope,
    _registry: DaemonSessionRuntimeRegistryV1,
}

impl RegisteredTemporalHarness {
    pub(super) async fn open(label: &str) -> Self {
        let directory = tempfile::tempdir().expect("temporary registered session store");
        let profile_root = directory.path().join("profile");
        let identity = crate::daemon::profile_identity::load_or_create(&profile_root)
            .expect("profile identity");
        let scope = crate::db::enter_daemon_database_scope(&profile_root, 1, label)
            .expect("daemon database scope");
        let registry = DaemonSessionRuntimeRegistryV1::open(identity)
            .await
            .expect("session runtime registry");
        let registered = registry
            .profile_sessions()
            .await
            .expect("registered profile sessions");
        Self {
            registered,
            _directory: directory,
            _scope: scope,
            _registry: registry,
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
        for (session_id, provider, record_id, receipt_id) in [
            (
                "session.root.a",
                "provider.application",
                "record-root-a",
                "receipt-root-a",
            ),
            (
                "session.root.b",
                "provider.other",
                "record-root-b",
                "receipt-root-b",
            ),
        ] {
            let observation = fixture_observation(
                1,
                session_id,
                provider,
                "duplicate-message",
                record_id,
                receipt_id,
                "duplicate root-wide payload",
                false,
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
            self.seed_occurrence(
                &observation,
                &anchor,
                "duplicate-message",
                "duplicate root-wide payload",
                1,
            )
            .await;
        }
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
        let writer = self
            .registered
            .writer_connection()
            .expect("registered writer");
        writer
            .execute(
                "INSERT INTO sessions (provider, session_id, project_key, project_path)
                 VALUES (?1, ?2, ?3, '/fixture')",
                params![provider, session_id, PROJECT_ID],
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
        writer
            .execute(
                "INSERT INTO session_temporal_generations (
                    session_id, generation, state, frozen_watermarks_json, created_at,
                    ready_at, activated_at, completed_at
                 ) VALUES (?1, 1, 'building', ?2, 1, NULL, NULL, NULL)",
                params![session_id, frozen],
            )
            .await
            .expect("seed building generation");
        writer
            .execute(
                "UPDATE session_temporal_generations
                 SET state = 'ready', ready_at = 1
                 WHERE session_id = ?1 AND generation = 1",
                params![session_id],
            )
            .await
            .expect("ready generation");
        writer
            .execute(
                "UPDATE session_temporal_generations
                 SET state = 'active', activated_at = 1
                 WHERE session_id = ?1 AND generation = 1",
                params![session_id],
            )
            .await
            .expect("activate generation");
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
        let writer = self
            .registered
            .writer_connection()
            .expect("registered writer");
        writer
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
        writer
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
        writer
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
        writer
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
        let payload_ref = "application-fixture.bin";
        let payload_dir = db_path.parent().unwrap().join("lcm-payloads");
        fs::create_dir_all(&payload_dir).unwrap();
        fs::write(payload_dir.join(payload_ref), EXTERNAL_PAYLOAD).unwrap();
        let digest = payload_digest(EXTERNAL_PAYLOAD);
        let writer = self
            .registered
            .writer_connection()
            .expect("registered writer");
        writer
            .execute(
                "INSERT INTO lcm_raw_messages (
                    provider, message_id, session_id, role, ordinal, timestamp,
                    content, content_hash, storage_kind, payload_ref,
                    snippet_text, index_text, legacy_source, legacy_truncated
                 ) VALUES (
                    'provider.application', 'message-2', 'session.temporal.application',
                    'assistant', 2, 2, NULL, ?1, 'external', ?2, ?3, ?3, 0, 0
                 )",
                params![digest, payload_ref, EXTERNAL_PAYLOAD],
            )
            .await
            .expect("seed external raw message");
        writer
            .execute(
                "INSERT INTO lcm_external_payloads (
                    payload_ref, provider, session_id, message_id, kind,
                    content_hash, byte_count, char_count, created_at
                 ) VALUES (
                    ?1, 'provider.application', 'session.temporal.application',
                    'message-2', 'message', ?2, ?3, ?4, 1
                 )",
                params![
                    payload_ref,
                    payload_digest(EXTERNAL_PAYLOAD),
                    i64::try_from(EXTERNAL_PAYLOAD.len()).unwrap(),
                    i64::try_from(EXTERNAL_PAYLOAD.chars().count()).unwrap()
                ],
            )
            .await
            .expect("seed external payload");
        let manifest = json!({
            "provider": "provider.application",
            "session_id": "session.temporal.application",
            "message_id": "message-2",
            "byte_count": EXTERNAL_PAYLOAD.len(),
            "char_count": EXTERNAL_PAYLOAD.chars().count()
        })
        .to_string();
        let publication = json!({
            "receipt_id": "receipt-3",
            "payloads": [{
                "payload_ref": payload_ref,
                "digest": payload_digest(EXTERNAL_PAYLOAD),
                "manifest_json": manifest
            }]
        })
        .to_string();
        writer
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
        writer
            .execute(
                "INSERT INTO session_external_payload_manifests (
                    payload_ref, session_id, payload_digest, manifest_json, receipt_id, created_at
                 ) VALUES (?1, 'session.temporal.application', ?2, ?3, 'receipt-3', 1)",
                params![payload_ref, payload_digest(EXTERNAL_PAYLOAD), manifest],
            )
            .await
            .expect("seed external payload manifest");
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
    let relations = CanonicalObservationRelationsV1::new(session_id).with_message_id(message_id);
    let envelope = CanonicalObservationEnvelopeV1::new(
        provider,
        "message",
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
    use sha2::Digest;

    format!(
        "sha256:{}",
        hex::encode(sha2::Sha256::digest(payload.as_bytes()))
    )
}
