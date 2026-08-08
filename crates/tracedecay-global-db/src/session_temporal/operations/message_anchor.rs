//! Canonical retrieval-anchor resolution for one raw-message summary source.
//!
//! A published summary's source lineage must name the same retrieval anchor the
//! temporal projection binds to that message. Anything else is a second anchor
//! identity space: every generation-bound read resolves such a source to no
//! occurrence, reports it missing, and drops the whole summary from the page.
//!
//! The temporal occurrence is generation-bound, so it only exists once a refresh
//! has materialized the message. A summary published while a refresh is still
//! pending must therefore resolve through the durable observation authority
//! instead — the exact-observation anchor identity is retained when the
//! observation is persisted and does not change when the refresh later
//! materializes the occurrence, so both routes agree on the anchor.

use tracedecay_domain::{
    AnchorDurabilityClass, DurableObservationV1, ObservationScopeV1, PayloadAccessState, ProjectId,
    RetrievalAnchorRecord, derive_exact_observation_anchor_id,
};
use tracedecay_runtime_core::db::engine::{Executor, params};
use tracedecay_sessions::runtime::lcm::types::LcmError;
use tracedecay_store::derive_canonical_projection;

use super::sources::unavailable;

/// Resolved canonical source binding: anchor id, whether the publication still
/// has to write a compatibility anchor row, and the source's knowledge time.
pub(super) type ResolvedMessageAnchor = (String, bool, i64);

/// Resolves the canonical retrieval anchor for one raw LCM message.
///
/// `Ok(None)` means the message has no canonical anchor in this store at all —
/// the only case in which the publication falls back to a legacy compatibility
/// anchor.
pub(super) async fn resolve_message_anchor(
    conn: &impl Executor,
    provider: &str,
    session_id: &str,
    message_id: &str,
    now: i64,
) -> Result<Option<ResolvedMessageAnchor>, LcmError> {
    if let Some(resolved) =
        resolve_materialized_occurrence(conn, provider, session_id, message_id, now).await?
    {
        return Ok(Some(resolved));
    }
    resolve_canonical_observation(conn, provider, session_id, message_id, now).await
}

/// Resolves through the message's occurrence in the active temporal generation.
async fn resolve_materialized_occurrence(
    conn: &impl Executor,
    provider: &str,
    session_id: &str,
    message_id: &str,
    now: i64,
) -> Result<Option<ResolvedMessageAnchor>, LcmError> {
    let Some(generation) = super::generation::active_generation(conn, session_id).await? else {
        return Ok(None);
    };
    let mut rows = conn
        .query(
            "SELECT DISTINCT json_object(
                    'anchor_id', occurrence.retrieval_anchor_id,
                    'anchor_json', anchor.anchor_json,
                    'owner_json', anchor.owner_json,
                    'knowledge_at', occurrence.knowledge_at,
                    'observation_json', observation.observation_json,
                    'receipt_id', observation.receipt_id
                )
             FROM session_occurrences occurrence
             JOIN retrieval_anchors anchor
               ON anchor.anchor_id = occurrence.retrieval_anchor_id
             JOIN observations observation
               ON observation.observation_id = occurrence.source_observation_id
             WHERE occurrence.session_id = ?1
               AND occurrence.generation = ?2
               AND occurrence.message_id = ?3
             ORDER BY occurrence.retrieval_anchor_id",
            params![session_id, generation, message_id],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    let encoded = row.get::<String>(0)?;
    let retained: serde_json::Value =
        serde_json::from_str(&encoded).map_err(|error| LcmError::Db(error.to_string()))?;
    let string = |field: &str| {
        retained[field]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| LcmError::Db(format!("retained source {field} is unavailable")))
    };
    let anchor_id = string("anchor_id")?;
    let anchor_json = string("anchor_json")?;
    let owner_json = string("owner_json")?;
    let knowledge_at = retained["knowledge_at"]
        .as_i64()
        .ok_or_else(|| LcmError::Db("retained source knowledge_at is unavailable".to_string()))?;
    if rows.next().await?.is_some() {
        return Err(LcmError::SummarySourceUnavailable {
            source_id: message_id.to_string(),
            reason: "ambiguous_anchor".to_string(),
        });
    }
    let anchor: RetrievalAnchorRecord = serde_json::from_str(&anchor_json)
        .map_err(|_| unavailable(&anchor_id, "unverifiable_anchor"))?;
    let observation_raw = string("observation_json")?;
    let observation: DurableObservationV1 = serde_json::from_str(&observation_raw)
        .map_err(|_| unavailable(&anchor_id, "unverifiable_observation"))?;
    let expected_scope = publishing_scope(conn, provider, session_id).await?;
    require_session_owned_observation(
        &observation,
        &anchor,
        &owner_json,
        &string("receipt_id")?,
        provider,
        session_id,
        &expected_scope,
    )?;
    require_readable_anchor(&anchor, &anchor_id, now)?;
    Ok(Some((anchor_id, false, knowledge_at)))
}

/// Resolves through the durable observation authority, which retains the
/// exact-observation anchor before any generation materializes the occurrence.
async fn resolve_canonical_observation(
    conn: &impl Executor,
    provider: &str,
    session_id: &str,
    message_id: &str,
    now: i64,
) -> Result<Option<ResolvedMessageAnchor>, LcmError> {
    let expected_scope = publishing_scope(conn, provider, session_id).await?;
    let mut rows = conn
        .query(
            "SELECT observation.observation_json, observation.receipt_id,
                    link.anchor_id, anchor.anchor_json, anchor.owner_json
             FROM session_temporal_observation_effects AS effect
             JOIN observations AS observation
               ON observation.observation_id = effect.observation_id
             JOIN observation_retrieval_anchors AS link
               ON link.observation_id = observation.observation_id
             JOIN retrieval_anchors AS anchor
               ON anchor.anchor_id = link.anchor_id
             WHERE effect.session_id = ?1
               AND effect.output_count > 0
               AND json_extract(
                       observation.observation_json, '$.identity.source.provider'
                   ) = ?2
               AND json_extract(
                       observation.observation_json, '$.identity.source.session_id'
                   ) = ?1
               AND COALESCE(
                       json_extract(
                           observation.observation_json, '$.payload.relations.message_id'
                       ),
                       json_extract(
                           observation.observation_json, '$.payload.stable_record_id'
                       )
                   ) = ?3
             ORDER BY effect.observation_sequence, link.anchor_id",
            params![session_id, provider, message_id],
        )
        .await?;
    let mut resolved: Option<ResolvedMessageAnchor> = None;
    while let Some(row) = rows.next().await? {
        let observation_raw = row.get::<String>(0)?;
        let receipt_id = row.get::<String>(1)?;
        let retained_anchor_id = row.get::<String>(2)?;
        let anchor_json = row.get::<String>(3)?;
        let owner_json = row.get::<String>(4)?;
        // The SQL identity prefilter has established that this is a canonical
        // observation for the requested message. From here on, malformed or
        // inconsistent authority is a typed failure; only no selected row may
        // fall back to the legacy raw-message identity space.
        let observation = serde_json::from_str::<DurableObservationV1>(&observation_raw)
            .map_err(|_| unavailable(message_id, "unverifiable_observation"))?;
        require_projects_message(&observation, message_id)?;
        let anchor = serde_json::from_str::<RetrievalAnchorRecord>(&anchor_json)
            .map_err(|_| unavailable(&retained_anchor_id, "unverifiable_anchor"))?;
        require_session_owned_observation(
            &observation,
            &anchor,
            &owner_json,
            &receipt_id,
            provider,
            session_id,
            &expected_scope,
        )?;
        require_exact_observation_anchor(&observation, &anchor)?;
        let anchor_id = anchor.anchor_id().as_str().to_owned();
        require_readable_anchor(&anchor, &anchor_id, now)?;
        let candidate = (anchor_id, false, anchor.ingested_at().0);
        match &resolved {
            Some(existing) if existing.0 != candidate.0 => {
                return Err(LcmError::SummarySourceUnavailable {
                    source_id: message_id.to_string(),
                    reason: "ambiguous_anchor".to_string(),
                });
            }
            Some(_) => {}
            None => resolved = Some(candidate),
        }
    }
    Ok(resolved)
}

fn require_projects_message(
    observation: &DurableObservationV1,
    message_id: &str,
) -> Result<(), LcmError> {
    let projects_message = derive_canonical_projection(observation)
        .map_err(|_| unavailable(message_id, "unverifiable_observation"))?
        .messages()
        .any(|output| output.message().message_id == message_id);
    if !projects_message {
        return Err(unavailable(message_id, "non_exact_observation"));
    }
    Ok(())
}

fn require_session_owned_observation(
    observation: &DurableObservationV1,
    anchor: &RetrievalAnchorRecord,
    owner_json: &str,
    retained_receipt_id: &str,
    provider: &str,
    session_id: &str,
    expected_scope: &ObservationScopeV1,
) -> Result<(), LcmError> {
    if observation.source().provider().as_str() != provider
        || observation.source().session_id().as_str() != session_id
        || observation.scope() != expected_scope
        || anchor.owner() != observation.scope()
        || serde_json::to_string(anchor.owner()).ok().as_deref() != Some(owner_json)
        || retained_receipt_id != observation.receipt().receipt().receipt_id().as_str()
    {
        return Err(LcmError::SummarySourceNotOwnedBySession);
    }
    Ok(())
}

/// The observation route finds the anchor by derivation, so the retained row has
/// to be exactly the canonical exact-observation anchor for that observation.
fn require_exact_observation_anchor(
    observation: &DurableObservationV1,
    anchor: &RetrievalAnchorRecord,
) -> Result<(), LcmError> {
    let expected_anchor =
        derive_exact_observation_anchor_id(observation.scope(), observation.observation_id())
            .map_err(|error| LcmError::Db(error.to_string()))?;
    if anchor.anchor_id() != &expected_anchor
        || !anchor
            .source_observations()
            .contains(observation.observation_id())
    {
        return Err(LcmError::SummarySourceNotOwnedBySession);
    }
    Ok(())
}

fn require_readable_anchor(
    anchor: &RetrievalAnchorRecord,
    anchor_id: &str,
    now: i64,
) -> Result<(), LcmError> {
    match anchor.payload_access() {
        PayloadAccessState::Eligible => {}
        state => {
            return Err(unavailable(
                anchor_id,
                &format!("{state:?}").to_ascii_lowercase(),
            ));
        }
    }
    if let AnchorDurabilityClass::RetentionBound { expires_at } = anchor.durability()
        && expires_at.0 <= now
    {
        return Err(unavailable(anchor_id, "retention_expired"));
    }
    Ok(())
}

async fn publishing_scope(
    conn: &impl Executor,
    provider: &str,
    session_id: &str,
) -> Result<ObservationScopeV1, LcmError> {
    let mut rows = conn
        .query(
            "SELECT project_key FROM sessions WHERE provider = ?1 AND session_id = ?2",
            params![provider, session_id],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Err(LcmError::SummarySourceNotOwnedBySession);
    };
    let project_key: String = row.get(0)?;
    if project_key == "user" {
        return Ok(ObservationScopeV1::Profile);
    }
    ProjectId::new(project_key)
        .map(|project_id| ObservationScopeV1::Project { project_id })
        .map_err(|_| LcmError::SummarySourceNotOwnedBySession)
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};
    use tempfile::tempdir;
    use tracedecay_domain::{
        CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1, CanonicalObservationEvidenceV1,
        CanonicalObservationFactV1, CanonicalObservationRelationsV1, ComponentVersion,
        DurableObservationV1, ObservationId, ObservationIdentityMaterialV1,
        ObservationOrderingDomainV1, ObservationScopeV1, ObservationSourceGenerationV1,
        ObservationSourceIdentityV1, ObservationSourceRangeV1, PayloadReferenceV1,
        ProjectionGenerationId, ProviderId, RetentionClass, SanitizationReceiptId,
        SanitizationReceiptRefV1, SanitizationReceiptV1, SanitizerDispositionV1, SensitivityV1,
        SessionId, UserProfileId, UtcMicros,
    };
    use tracedecay_runtime_core::db::engine::{Executor, params};
    use tracedecay_sessions::runtime::lcm::types::{
        LcmError, LcmImmutableSummaryPublication, LcmSourceRef, LcmSummaryNodeDraft,
    };

    use crate::session_temporal::relations::{SessionRelationProjection, SessionRelationScope};
    use crate::tests::harness::{HostAdmissionScope, HostAdmissionTestRuntimeV1};

    fn fixture_receipt(receipt_id: &str, payload: &Value) -> SanitizationReceiptV1 {
        SanitizationReceiptV1::new(
            SanitizationReceiptRefV1::new(
                SanitizationReceiptId::new(receipt_id).expect("receipt id"),
                ComponentVersion::new("sanitizer.message-anchor-test.v1")
                    .expect("sanitizer version"),
            )
            .expect("receipt reference"),
            SanitizerDispositionV1::Accepted,
            SensitivityV1::NonSensitive,
            Some(PayloadReferenceV1::for_payload(payload).expect("payload reference")),
        )
        .expect("sanitization receipt")
    }

    fn fixture_observation(
        provider: &str,
        session_id: &str,
        message_id: &str,
        ordinal: u64,
    ) -> DurableObservationV1 {
        let provider_id = ProviderId::new(provider).expect("provider");
        let session_id = SessionId::new(session_id).expect("session");
        let record_id = ObservationId::new(message_id).expect("record id");
        let range = ObservationSourceRangeV1::new(ordinal, ordinal + 1).expect("source range");
        let envelope = CanonicalObservationEnvelopeV1::new(
            provider_id.clone(),
            "message",
            record_id.clone(),
            CanonicalObservationRelationsV1::new(session_id.clone())
                .with_message_id(record_id.clone()),
            vec![CanonicalObservationFactV1::Message {
                role: CanonicalMessageRoleV1::Assistant,
                content: json!({"text": "canonical message-anchor fixture"}),
                model: Some("model.fixture".to_string()),
                timestamp: Some(1_715_000_001),
            }],
            CanonicalObservationEvidenceV1::new(ObservationOrderingDomainV1::SnapshotOrder, range),
        )
        .expect("canonical envelope");
        let payload = serde_json::to_value(envelope).expect("canonical payload");
        let identity = ObservationIdentityMaterialV1::for_native_record(
            ObservationSourceIdentityV1::for_provider(provider_id, session_id)
                .expect("source identity"),
            ObservationScopeV1::Profile,
            ObservationSourceGenerationV1::new(1).expect("source generation"),
            range,
            ObservationOrderingDomainV1::SnapshotOrder,
            record_id,
        )
        .expect("observation identity");
        DurableObservationV1::new(
            identity,
            fixture_receipt(&format!("receipt.message-anchor.{ordinal}"), &payload),
            RetentionClass::new("retention.message-anchor-test").expect("retention class"),
            payload,
        )
        .expect("durable observation")
    }

    fn fixture_anchor(
        observation: &DurableObservationV1,
    ) -> tracedecay_domain::RetrievalAnchorRecordV2 {
        let authorization = tracedecay_store::build_observation_resolution_authorization_v1(
            observation,
            "message-anchor-test",
        )
        .expect("anchor authorization");
        tracedecay_store::build_observation_retrieval_anchor_v2(
            observation,
            ProjectionGenerationId::new("projection.message-anchor-test.v1")
                .expect("projection generation"),
            UtcMicros(1_715_000_002),
            authorization,
        )
        .expect("retrieval anchor")
    }

    async fn seed_raw_source(conn: &impl Executor, timestamp_sql: &str) {
        conn.execute(
            "INSERT INTO sessions (provider, session_id, project_key, project_path)
             VALUES ('codex', 'session.message-anchor', 'user', '/fixture')",
            (),
        )
        .await
        .expect("session owner");
        conn.execute_batch(&format!(
            "INSERT INTO lcm_raw_messages (
                provider, message_id, session_id, store_id, role, ordinal, timestamp,
                content, content_hash, storage_kind, payload_ref, snippet_text,
                index_text, legacy_source, legacy_truncated, metadata_json
             ) VALUES (
                'codex', 'message.source', 'session.message-anchor', 41,
                'assistant', 0, {timestamp_sql}, 'source body',
                'sha256:source-body', 'inline', NULL, 'source body', 'source body',
                0, 0, NULL
             );",
        ))
        .await
        .expect("raw source");
    }

    async fn seed_canonical_binding(
        conn: &impl Executor,
        observation_json: &str,
        observation: &DurableObservationV1,
        anchor: &tracedecay_domain::RetrievalAnchorRecordV2,
        owner_json: &str,
    ) {
        let receipt = observation.receipt();
        conn.execute(
            "INSERT INTO sanitization_receipts (
                receipt_id, sanitizer_version, payload_digest, receipt_json
             ) VALUES (?1, ?2, ?3, ?4)",
            params![
                receipt.receipt().receipt_id().as_str(),
                receipt.receipt().sanitizer_version().as_str(),
                observation.payload_reference().digest().as_str(),
                serde_json::to_string(receipt).expect("receipt json"),
            ],
        )
        .await
        .expect("sanitization receipt");
        conn.execute(
            "INSERT INTO observations (
                observation_id, payload_digest, receipt_id, observation_json,
                committed_cursor_json
             ) VALUES (?1, ?2, ?3, ?4, '{}')",
            params![
                observation.observation_id().as_str(),
                observation.payload_reference().digest().as_str(),
                receipt.receipt().receipt_id().as_str(),
                observation_json,
            ],
        )
        .await
        .expect("observation");
        conn.execute(
            "INSERT INTO retrieval_anchors (
                anchor_id, anchor_json, owner_json, projection_generation
             ) VALUES (?1, ?2, ?3, 'projection.message-anchor-test.v1')",
            params![
                anchor.anchor_id().as_str(),
                serde_json::to_string(anchor).expect("anchor json"),
                owner_json,
            ],
        )
        .await
        .expect("retrieval anchor");
        conn.execute(
            "INSERT INTO observation_retrieval_anchors (observation_id, anchor_id)
             VALUES (?1, ?2)",
            params![
                observation.observation_id().as_str(),
                anchor.anchor_id().as_str(),
            ],
        )
        .await
        .expect("observation anchor binding");
        conn.execute(
            "INSERT INTO session_temporal_observation_effects (
                observation_id, observation_sequence, session_id, receipt_id,
                effect_digest, output_count, recorded_at
             ) VALUES (?1, 1, 'session.message-anchor', ?2, 'effect.fixture', 1, 1)",
            params![
                observation.observation_id().as_str(),
                receipt.receipt().receipt_id().as_str(),
            ],
        )
        .await
        .expect("temporal observation effect");
    }

    fn publication() -> LcmImmutableSummaryPublication {
        LcmImmutableSummaryPublication {
            summary_id: "summary.message-anchor".to_string(),
            predecessor_summary_id: None,
            draft: LcmSummaryNodeDraft {
                provider: "codex".to_string(),
                conversation_id: "conversation.message-anchor".to_string(),
                session_id: "session.message-anchor".to_string(),
                depth: 0,
                summary_text: "summary body".to_string(),
                source_refs: vec![LcmSourceRef::RawMessage { store_id: 41 }],
                source_token_count: 2,
                summary_token_count: 2,
                source_time_start: Some(1_715_000_001),
                source_time_end: Some(1_715_000_001),
                expand_hint: None,
                metadata_json: None,
            },
        }
    }

    fn empty_relation_projection() -> SessionRelationProjection {
        SessionRelationProjection {
            scope: SessionRelationScope::profile_sessions(
                UserProfileId::new("profile.message-anchor").expect("profile"),
            ),
            session_id: SessionId::new("session.message-anchor").expect("session"),
            generation: 1,
            summaries: Vec::new(),
            logical_copies: Vec::new(),
            thread_hierarchy: Vec::new(),
            agent_hierarchy: Vec::new(),
            parent_session_id: None,
            workflow_agents: Vec::new(),
        }
    }

    async fn publish(
        conn: &impl Executor,
    ) -> Result<tracedecay_sessions::runtime::lcm::types::LcmSummaryPublicationReceipt, LcmError>
    {
        super::super::publication::publish_immutable_summary(
            conn,
            publication(),
            &empty_relation_projection(),
        )
        .await
    }

    async fn legacy_anchor_count(conn: &impl Executor) -> i64 {
        let mut rows = conn
            .query(
                "SELECT COUNT(*) FROM retrieval_anchors
                 WHERE json_extract(anchor_json, '$.kind') = 'legacy_lcm_raw_message'",
                (),
            )
            .await
            .expect("legacy anchor count");
        rows.next()
            .await
            .expect("legacy anchor row")
            .expect("legacy anchor count row")
            .get(0)
            .expect("legacy anchor count value")
    }

    #[tokio::test]
    async fn malformed_canonical_observation_never_falls_back_to_a_legacy_anchor() {
        let directory = tempdir().expect("temporary directory");
        let runtime = HostAdmissionTestRuntimeV1::profile(directory.path())
            .await
            .expect("registered profile runtime");
        let conn = runtime
            .registered_database(HostAdmissionScope::Profile)
            .expect("profile database")
            .writer_connection()
            .expect("profile writer");
        seed_raw_source(&conn, "1715000001").await;
        let observation =
            fixture_observation("codex", "session.message-anchor", "message.source", 1);
        let anchor = fixture_anchor(&observation);
        let mut malformed = serde_json::to_value(&observation).expect("observation json");
        malformed["receipt"] = Value::Null;
        seed_canonical_binding(
            &conn,
            &malformed.to_string(),
            &observation,
            &anchor,
            &serde_json::to_string(anchor.owner()).expect("owner json"),
        )
        .await;

        let result = publish(&conn).await;

        assert_eq!(legacy_anchor_count(&conn).await, 0);
        assert!(matches!(
            result,
            Err(LcmError::SummarySourceUnavailable { ref reason, .. })
                if reason == "unverifiable_observation"
        ));
    }

    #[tokio::test]
    async fn ownership_mismatched_canonical_binding_never_falls_back_to_a_legacy_anchor() {
        let directory = tempdir().expect("temporary directory");
        let runtime = HostAdmissionTestRuntimeV1::profile(directory.path())
            .await
            .expect("registered profile runtime");
        let conn = runtime
            .registered_database(HostAdmissionScope::Profile)
            .expect("profile database")
            .writer_connection()
            .expect("profile writer");
        seed_raw_source(&conn, "1715000001").await;
        let observation =
            fixture_observation("codex", "session.message-anchor", "message.source", 1);
        let anchor = fixture_anchor(&observation);
        seed_canonical_binding(
            &conn,
            &serde_json::to_string(&observation).expect("observation json"),
            &observation,
            &anchor,
            r#"{"kind":"project","project_id":"project.foreign"}"#,
        )
        .await;

        let result = publish(&conn).await;

        assert_eq!(legacy_anchor_count(&conn).await, 0);
        assert!(matches!(
            result,
            Err(LcmError::SummarySourceNotOwnedBySession)
        ));
    }

    #[tokio::test]
    async fn non_exact_canonical_binding_never_falls_back_to_a_legacy_anchor() {
        let directory = tempdir().expect("temporary directory");
        let runtime = HostAdmissionTestRuntimeV1::profile(directory.path())
            .await
            .expect("registered profile runtime");
        let conn = runtime
            .registered_database(HostAdmissionScope::Profile)
            .expect("profile database")
            .writer_connection()
            .expect("profile writer");
        seed_raw_source(&conn, "1715000001").await;
        let observation =
            fixture_observation("codex", "session.message-anchor", "message.source", 1);
        let foreign_observation =
            fixture_observation("codex", "session.message-anchor", "message.foreign", 2);
        let foreign_anchor = fixture_anchor(&foreign_observation);
        seed_canonical_binding(
            &conn,
            &serde_json::to_string(&observation).expect("observation json"),
            &observation,
            &foreign_anchor,
            &serde_json::to_string(foreign_anchor.owner()).expect("owner json"),
        )
        .await;

        let result = publish(&conn).await;

        assert_eq!(legacy_anchor_count(&conn).await, 0);
        assert!(matches!(
            result,
            Err(LcmError::SummarySourceNotOwnedBySession)
        ));
    }

    #[tokio::test]
    async fn unavailable_session_owner_never_inserts_a_legacy_anchor() {
        let directory = tempdir().expect("temporary directory");
        let runtime = HostAdmissionTestRuntimeV1::profile(directory.path())
            .await
            .expect("registered profile runtime");
        let conn = runtime
            .registered_database(HostAdmissionScope::Profile)
            .expect("profile database")
            .writer_connection()
            .expect("profile writer");
        seed_raw_source(&conn, "1715000001").await;
        conn.execute(
            "UPDATE sessions SET project_key = ''
             WHERE provider = 'codex' AND session_id = 'session.message-anchor'",
            (),
        )
        .await
        .expect("malformed session owner authority");

        let result = publish(&conn).await;

        assert_eq!(legacy_anchor_count(&conn).await, 0);
        assert!(matches!(
            result,
            Err(LcmError::SummarySourceNotOwnedBySession)
        ));
    }

    #[tokio::test]
    async fn malformed_raw_timestamp_never_inserts_a_zero_time_legacy_anchor() {
        let directory = tempdir().expect("temporary directory");
        let runtime = HostAdmissionTestRuntimeV1::profile(directory.path())
            .await
            .expect("registered profile runtime");
        let conn = runtime
            .registered_database(HostAdmissionScope::Profile)
            .expect("profile database")
            .writer_connection()
            .expect("profile writer");
        seed_raw_source(&conn, "'not-a-timestamp'").await;

        let result = publish(&conn).await;

        assert_eq!(legacy_anchor_count(&conn).await, 0);
        assert!(matches!(
            result,
            Err(LcmError::SummarySourceUnavailable { ref reason, .. })
                if reason == "unverifiable_timestamp"
        ));
    }
}
