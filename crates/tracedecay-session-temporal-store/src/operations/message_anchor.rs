//! Canonical retrieval-anchor resolution for the raw-message sources of one
//! summary publication.
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
//!
//! A publication's raw sources are resolved together: the materialized
//! occurrences of the whole message set are read in one statement, and the
//! messages that leaves unresolved share one pass over the session's canonical
//! observation effects, each observation decoded and projected once. Per
//! message the outcome is exactly the single-message resolution — same anchor
//! derivation, ownership, receipt agreement, readability and ambiguity
//! refusals — so `K` sources cost one scan of `N` effects instead of `K`.

use std::collections::{BTreeMap, BTreeSet};

use tracedecay_domain::{
    AnchorDurabilityClass, DurableObservationV1, ObservationScopeV1, PayloadAccessState, ProjectId,
    RetrievalAnchorRecord, derive_exact_observation_anchor_id,
};
use tracedecay_lcm::types::LcmError;
use tracedecay_runtime_core::db::engine::params;
use tracedecay_store::derive_canonical_projection;

use super::sources::unavailable;

/// Resolved canonical source binding: anchor id, whether the publication still
/// has to write a compatibility anchor row, and the source's knowledge time.
pub(super) type ResolvedMessageAnchor = (String, bool, i64);

/// One materialized occurrence row of a requested message.
struct MaterializedOccurrence {
    anchor_id: String,
    anchor_json: String,
    owner_json: String,
    knowledge_at: i64,
    observation_json: String,
    receipt_id: String,
}

/// Resolves the canonical retrieval anchors of `message_ids` (distinct, in
/// source order) for one session, reading the shared authorities once.
///
/// A message absent from the returned map has no canonical anchor in this
/// store at all — the only case in which the publication falls back to a
/// legacy compatibility anchor. A refusal raised by one message's own
/// evidence names that message; a refusal the shared observation scan raises
/// before any message matched (missing or undecodable observation authority)
/// names the first still-unresolved message in source order, which is the
/// message whose single-message scan met it before.
#[hotpath::measure(future = true, label = "session_temporal.publication.resolve_anchors")]
pub(super) async fn resolve_message_anchors(
    conn: &impl crate::handle::SessionTemporalExec,
    provider: &str,
    session_id: &str,
    project_key: &str,
    active_generation: Option<i64>,
    message_ids: &[String],
    now: i64,
) -> Result<BTreeMap<String, ResolvedMessageAnchor>, LcmError> {
    if message_ids.is_empty() {
        return Ok(BTreeMap::new());
    }
    let expected_scope = publishing_scope(project_key)?;
    let mut resolved = match active_generation {
        Some(generation) => {
            resolve_materialized_occurrences(
                conn,
                provider,
                session_id,
                generation,
                message_ids,
                &expected_scope,
                now,
            )
            .await?
        }
        None => BTreeMap::new(),
    };
    let unresolved = message_ids
        .iter()
        .filter(|message_id| !resolved.contains_key(*message_id))
        .map(String::as_str)
        .collect::<Vec<_>>();
    if !unresolved.is_empty() {
        resolve_canonical_observations(
            conn,
            provider,
            session_id,
            &unresolved,
            &expected_scope,
            now,
            &mut resolved,
        )
        .await?;
    }
    Ok(resolved)
}

/// Resolves every requested message that has an occurrence in the active
/// temporal generation. Messages without one are simply absent from the map.
async fn resolve_materialized_occurrences(
    conn: &impl crate::handle::SessionTemporalExec,
    provider: &str,
    session_id: &str,
    generation: i64,
    message_ids: &[String],
    expected_scope: &ObservationScopeV1,
    now: i64,
) -> Result<BTreeMap<String, ResolvedMessageAnchor>, LcmError> {
    let encoded_ids =
        serde_json::to_string(message_ids).map_err(|error| LcmError::Db(error.to_string()))?;
    let mut rows = conn
        .query(
            "SELECT DISTINCT occurrence.message_id, occurrence.retrieval_anchor_id,
                    anchor.anchor_json, anchor.owner_json, occurrence.knowledge_at,
                    observation.observation_json, observation.receipt_id
             FROM session_occurrences occurrence
             JOIN retrieval_anchors anchor
               ON anchor.anchor_id = occurrence.retrieval_anchor_id
             JOIN observations observation
               ON observation.observation_id = occurrence.source_observation_id
             WHERE occurrence.session_id = ?1
               AND occurrence.generation = ?2
               AND occurrence.message_id IN (SELECT value FROM json_each(?3))
             ORDER BY occurrence.message_id, occurrence.retrieval_anchor_id",
            params![session_id, generation, encoded_ids],
        )
        .await?;
    let mut by_message: BTreeMap<String, Vec<MaterializedOccurrence>> = BTreeMap::new();
    while let Some(row) = rows.next().await? {
        let message_id: String = row.get(0)?;
        by_message
            .entry(message_id)
            .or_default()
            .push(MaterializedOccurrence {
                anchor_id: row.get(1)?,
                anchor_json: row.get(2)?,
                owner_json: row.get(3)?,
                knowledge_at: row.get(4)?,
                observation_json: row.get(5)?,
                receipt_id: row.get(6)?,
            });
    }
    let mut resolved = BTreeMap::new();
    for message_id in message_ids {
        let Some(occurrences) = by_message.remove(message_id) else {
            continue;
        };
        let mut occurrences = occurrences.into_iter();
        let Some(retained) = occurrences.next() else {
            continue;
        };
        if occurrences.next().is_some() {
            return Err(LcmError::SummarySourceUnavailable {
                source_id: message_id.clone(),
                reason: "ambiguous_anchor".to_string(),
            });
        }
        let anchor: RetrievalAnchorRecord = serde_json::from_str(&retained.anchor_json)
            .map_err(|_| unavailable(&retained.anchor_id, "unverifiable_anchor"))?;
        let observation: DurableObservationV1 = serde_json::from_str(&retained.observation_json)
            .map_err(|_| unavailable(&retained.anchor_id, "unverifiable_observation"))?;
        require_session_owned_observation(
            &observation,
            &anchor,
            &retained.owner_json,
            &retained.receipt_id,
            provider,
            session_id,
            expected_scope,
        )?;
        require_readable_anchor(&anchor, &retained.anchor_id, now)?;
        resolved.insert(
            message_id.clone(),
            (retained.anchor_id, false, retained.knowledge_at),
        );
    }
    Ok(resolved)
}

/// Resolves the still-unresolved messages through the durable observation
/// authority in one pass over the session's positive-output effects. Each
/// observation is decoded and projected once and its anchor is bound to every
/// unresolved message it projects; two different anchors for one message are
/// an ambiguity refusal, exactly as for a single message.
async fn resolve_canonical_observations(
    conn: &impl crate::handle::SessionTemporalExec,
    provider: &str,
    session_id: &str,
    unresolved: &[&str],
    expected_scope: &ObservationScopeV1,
    now: i64,
    resolved: &mut BTreeMap<String, ResolvedMessageAnchor>,
) -> Result<(), LcmError> {
    let Some(first_unresolved) = unresolved.first().copied() else {
        return Ok(());
    };
    let wanted = unresolved.iter().copied().collect::<BTreeSet<_>>();
    let mut rows = conn
        .query(
            "SELECT observation.observation_json, observation.receipt_id,
                    effect.receipt_id, link.anchor_id, anchor.anchor_json,
                    anchor.owner_json
             FROM session_temporal_observation_effects AS effect
             LEFT JOIN observations AS observation
               ON observation.observation_id = effect.observation_id
             LEFT JOIN observation_retrieval_anchors AS link
               ON link.observation_id = observation.observation_id
             LEFT JOIN retrieval_anchors AS anchor
               ON anchor.anchor_id = link.anchor_id
             WHERE effect.session_id = ?1
               AND effect.output_count > 0
             ORDER BY effect.observation_sequence, link.anchor_id",
            params![session_id],
        )
        .await?;
    while let Some(row) = rows.next().await? {
        let observation_raw = row
            .get::<Option<String>>(0)?
            .ok_or_else(|| unavailable(first_unresolved, "missing_observation_authority"))?;
        let observation = serde_json::from_str::<DurableObservationV1>(&observation_raw)
            .map_err(|_| unavailable(first_unresolved, "unverifiable_observation"))?;
        if observation.source().provider().as_str() != provider
            || observation.source().session_id().as_str() != session_id
        {
            continue;
        }
        let projected = projected_messages(&observation, unresolved, &wanted, first_unresolved)?;
        let Some(attributed) = projected.first().copied() else {
            continue;
        };
        let receipt_id = row
            .get::<Option<String>>(1)?
            .ok_or_else(|| unavailable(attributed, "missing_observation_receipt"))?;
        let effect_receipt_id = row.get::<String>(2)?;
        if effect_receipt_id != receipt_id {
            return Err(LcmError::SummarySourceNotOwnedBySession);
        }
        let retained_anchor_id = row
            .get::<Option<String>>(3)?
            .ok_or_else(|| unavailable(attributed, "missing_anchor_binding"))?;
        let anchor_json = row
            .get::<Option<String>>(4)?
            .ok_or_else(|| unavailable(&retained_anchor_id, "missing_anchor_authority"))?;
        let owner_json = row
            .get::<Option<String>>(5)?
            .ok_or_else(|| unavailable(&retained_anchor_id, "missing_anchor_owner"))?;
        let anchor = serde_json::from_str::<RetrievalAnchorRecord>(&anchor_json)
            .map_err(|_| unavailable(&retained_anchor_id, "unverifiable_anchor"))?;
        require_session_owned_observation(
            &observation,
            &anchor,
            &owner_json,
            &receipt_id,
            provider,
            session_id,
            expected_scope,
        )?;
        require_exact_observation_anchor(&observation, &anchor)?;
        let anchor_id = anchor.anchor_id().as_str().to_owned();
        require_readable_anchor(&anchor, &anchor_id, now)?;
        let candidate = (anchor_id, false, anchor.ingested_at().0);
        for message_id in projected {
            match resolved.get(message_id) {
                Some(existing) if existing.0 != candidate.0 => {
                    return Err(LcmError::SummarySourceUnavailable {
                        source_id: message_id.to_string(),
                        reason: "ambiguous_anchor".to_string(),
                    });
                }
                Some(_) => {}
                None => {
                    resolved.insert(message_id.to_owned(), candidate.clone());
                }
            }
        }
    }
    Ok(())
}

/// The unresolved messages (in source order) that `observation` projects.
fn projected_messages<'a>(
    observation: &DurableObservationV1,
    unresolved: &[&'a str],
    wanted: &BTreeSet<&str>,
    attributed: &str,
) -> Result<Vec<&'a str>, LcmError> {
    let projection = derive_canonical_projection(observation)
        .map_err(|_| unavailable(attributed, "unverifiable_observation"))?;
    let projected = projection
        .messages()
        .map(|output| output.message().message_id.as_str())
        .filter(|message_id| wanted.contains(message_id))
        .collect::<BTreeSet<_>>();
    Ok(unresolved
        .iter()
        .copied()
        .filter(|message_id| projected.contains(message_id))
        .collect())
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

/// The observation scope a session publishes under, derived from its owner's
/// project key.
fn publishing_scope(project_key: &str) -> Result<ObservationScopeV1, LcmError> {
    if project_key == "user" {
        return Ok(ObservationScopeV1::Profile);
    }
    ProjectId::new(project_key.to_owned())
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
    use tracedecay_lcm::types::{
        LcmError, LcmImmutableSummaryPublication, LcmSourceRef, LcmSummaryNodeDraft,
    };
    use tracedecay_runtime_core::db::engine::params;

    use crate::relations::{SessionRelationProjection, SessionRelationScope};
    use crate::test_support::QueryCountingConnection;
    use tracedecay_global_db::tests::harness::{HostAdmissionScope, HostAdmissionTestRuntimeV1};

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
        fixture_observation_projecting(provider, session_id, message_id, message_id, ordinal)
    }

    /// An observation whose native record is `record_id` but whose canonical
    /// projection names `message_id`; distinct records projecting one message
    /// are how an ambiguous canonical anchor arises.
    fn fixture_observation_projecting(
        provider: &str,
        session_id: &str,
        record_id: &str,
        message_id: &str,
        ordinal: u64,
    ) -> DurableObservationV1 {
        let provider_id = ProviderId::new(provider).expect("provider");
        let session_id = SessionId::new(session_id).expect("session");
        let record_id = ObservationId::new(record_id).expect("record id");
        let message_id = ObservationId::new(message_id).expect("message id");
        let range = ObservationSourceRangeV1::new(ordinal, ordinal + 1).expect("source range");
        let envelope = CanonicalObservationEnvelopeV1::new(
            provider_id.clone(),
            "message",
            record_id.clone(),
            CanonicalObservationRelationsV1::new(session_id.clone()).with_message_id(message_id),
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

    async fn seed_raw_source(conn: &impl crate::handle::SessionTemporalExec, timestamp_sql: &str) {
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
        conn: &impl crate::handle::SessionTemporalExec,
        observation_json: &str,
        observation: &DurableObservationV1,
        anchor: &tracedecay_domain::RetrievalAnchorRecordV2,
        owner_json: &str,
    ) {
        seed_canonical_binding_at(conn, observation_json, observation, anchor, owner_json, 1).await;
    }

    async fn seed_canonical_binding_at(
        conn: &impl crate::handle::SessionTemporalExec,
        observation_json: &str,
        observation: &DurableObservationV1,
        anchor: &tracedecay_domain::RetrievalAnchorRecordV2,
        owner_json: &str,
        sequence: i64,
    ) {
        seed_canonical_observation_at(conn, observation_json, observation, sequence).await;
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
    }

    async fn seed_canonical_observation(
        conn: &impl crate::handle::SessionTemporalExec,
        observation_json: &str,
        observation: &DurableObservationV1,
    ) {
        seed_canonical_observation_at(conn, observation_json, observation, 1).await;
    }

    async fn seed_canonical_observation_at(
        conn: &impl crate::handle::SessionTemporalExec,
        observation_json: &str,
        observation: &DurableObservationV1,
        sequence: i64,
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
            "INSERT INTO session_temporal_observation_effects (
                observation_id, observation_sequence, session_id, receipt_id,
                effect_digest, output_count, recorded_at
             ) VALUES (?1, ?3, 'session.message-anchor', ?2, 'effect.fixture', 1, 1)",
            params![
                observation.observation_id().as_str(),
                receipt.receipt().receipt_id().as_str(),
                sequence,
            ],
        )
        .await
        .expect("temporal observation effect");
    }

    /// Seeds the session row plus `count` inline raw messages with store ids
    /// `41..41 + count` and message ids `message.source.<index>`.
    async fn seed_raw_sources(conn: &impl crate::handle::SessionTemporalExec, count: i64) {
        conn.execute(
            "INSERT INTO sessions (provider, session_id, project_key, project_path)
             VALUES ('codex', 'session.message-anchor', 'user', '/fixture')",
            (),
        )
        .await
        .expect("session owner");
        for index in 0..count {
            conn.execute(
                "INSERT INTO lcm_raw_messages (
                    provider, message_id, session_id, store_id, role, ordinal, timestamp,
                    content, content_hash, storage_kind, payload_ref, snippet_text,
                    index_text, legacy_source, legacy_truncated, metadata_json
                 ) VALUES (
                    'codex', ?1, 'session.message-anchor', ?2, 'assistant', ?3, 1715000001,
                    'source body', 'sha256:source-body', 'inline', NULL, 'source body',
                    'source body', 0, 0, NULL
                 )",
                params![format!("message.source.{index}"), 41 + index, index],
            )
            .await
            .expect("raw source");
        }
    }

    /// Materializes `observation`'s message into the active generation, the
    /// state a temporal refresh leaves behind.
    async fn materialize_occurrence(
        conn: &impl crate::handle::SessionTemporalExec,
        observation: &DurableObservationV1,
        anchor: &tracedecay_domain::RetrievalAnchorRecordV2,
        message_id: &str,
    ) {
        // The generation lifecycle guards admit only building -> ready -> active.
        conn.execute_batch(
            "INSERT OR IGNORE INTO session_temporal_generations (
                session_id, generation, state, frozen_watermarks_json, created_at
             ) VALUES ('session.message-anchor', 1, 'building', '{}', 1);
             UPDATE session_temporal_generations SET state = 'ready', ready_at = 1
              WHERE session_id = 'session.message-anchor' AND generation = 1
                AND state = 'building';
             UPDATE session_temporal_generations SET state = 'active', activated_at = 1
              WHERE session_id = 'session.message-anchor' AND generation = 1
                AND state = 'ready';",
        )
        .await
        .expect("active generation");
        conn.execute(
            "INSERT INTO session_occurrences (
                session_id, generation, occurrence_id, source_observation_id, source_provider,
                projection_output_ordinal, retrieval_anchor_id, message_id, role, knowledge_at,
                valid_time_json, evidence_json, sanitized_content_digest,
                sanitized_content_bytes, snippet_text, index_text
             ) VALUES (
                'session.message-anchor', 1, ?1, ?2, 'codex', 0, ?3, ?1, 'assistant',
                1715000002, '{\"kind\":\"unknown\"}', '{}',
                '0000000000000000000000000000000000000000000000000000000000000000', 0, '', ''
             )",
            params![
                message_id,
                observation.observation_id().as_str(),
                anchor.anchor_id().as_str(),
            ],
        )
        .await
        .expect("materialized occurrence");
    }

    fn publication_over(store_ids: &[i64]) -> LcmImmutableSummaryPublication {
        let mut publication = publication();
        publication.draft.source_refs = store_ids
            .iter()
            .map(|store_id| LcmSourceRef::RawMessage {
                store_id: *store_id,
            })
            .collect();
        publication
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

    fn parent_publication() -> LcmImmutableSummaryPublication {
        LcmImmutableSummaryPublication {
            summary_id: "summary.message-anchor.parent".to_string(),
            predecessor_summary_id: None,
            draft: LcmSummaryNodeDraft {
                provider: "codex".to_string(),
                conversation_id: "conversation.message-anchor".to_string(),
                session_id: "session.message-anchor".to_string(),
                depth: 1,
                summary_text: "parent summary body".to_string(),
                source_refs: vec![LcmSourceRef::SummaryNode {
                    node_id: "summary.message-anchor".to_string(),
                }],
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
        conn: &impl crate::handle::SessionTemporalExec,
    ) -> Result<tracedecay_lcm::types::LcmSummaryPublicationReceipt, LcmError> {
        super::super::publication::publish_immutable_summary(
            conn,
            publication(),
            &empty_relation_projection(),
        )
        .await
    }

    async fn summary_node_count(conn: &impl crate::handle::SessionTemporalExec) -> i64 {
        let mut rows = conn
            .query("SELECT COUNT(*) FROM session_summary_nodes", ())
            .await
            .expect("summary node count");
        rows.next()
            .await
            .expect("summary node row")
            .expect("summary node count row")
            .get(0)
            .expect("summary node count value")
    }

    async fn legacy_anchor_count(conn: &impl crate::handle::SessionTemporalExec) -> i64 {
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
    async fn malformed_canonical_message_identity_is_not_hidden_by_candidate_filtering() {
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
        malformed["payload"]["relations"]["message_id"] = Value::from(7);
        malformed["payload"]["stable_record_id"] = Value::from(8);
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
    async fn missing_canonical_anchor_binding_never_falls_back_to_a_legacy_anchor() {
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
        seed_canonical_observation(
            &conn,
            &serde_json::to_string(&observation).expect("observation json"),
            &observation,
        )
        .await;

        let result = publish(&conn).await;

        assert_eq!(legacy_anchor_count(&conn).await, 0);
        assert!(matches!(
            result,
            Err(LcmError::SummarySourceUnavailable { ref reason, .. })
                if reason == "missing_anchor_binding"
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

    #[tokio::test]
    async fn malformed_summary_horizon_never_fabricates_a_zero_knowledge_time() {
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
        publish(&conn).await.expect("leaf summary publication");
        // Summary nodes are immutable, so the malformed horizon is written as
        // its own node rather than by rewriting the published one: the schema
        // rejects the update, and a fixture that depends on rewriting history
        // is testing something the store cannot produce.
        conn.execute(
            "INSERT INTO session_summary_nodes (
                summary_id, session_id, summary_anchor_id, summary_text,
                index_text, source_horizon_json, publication_json, created_at
             )
             SELECT 'summary.message-anchor.malformed', session_id, summary_anchor_id,
                    summary_text, index_text, '{}', publication_json, created_at
               FROM session_summary_nodes
              WHERE summary_id = 'summary.message-anchor'",
            (),
        )
        .await
        .expect("malformed source horizon");
        // Availability is generation-scoped and checked before the horizon is
        // read, so the copied node needs the published node's availability row
        // or the refusal under test is never reached.
        conn.execute(
            "INSERT INTO session_summary_availability (
                session_id, generation, summary_id, availability,
                source_horizon_json, reason, checked_at
             )
             SELECT session_id, generation, 'summary.message-anchor.malformed',
                    availability, source_horizon_json, reason, checked_at
               FROM session_summary_availability
              WHERE summary_id = 'summary.message-anchor'",
            (),
        )
        .await
        .expect("malformed node availability");

        let mut publication = parent_publication();
        publication.draft.source_refs = vec![LcmSourceRef::SummaryNode {
            node_id: "summary.message-anchor.malformed".to_string(),
        }];

        let result = super::super::sources::prepare_sources(&conn, &publication).await;

        assert!(matches!(
            result,
            Err(LcmError::SummarySourceUnavailable {
                ref source_id,
                ref reason,
            }) if source_id == "summary.message-anchor.malformed"
                && reason == "unverifiable_source_horizon"
        ));
    }

    /// `K` raw sources published before any refresh resolve through one scan
    /// of the session's `N` observation effects, and the same bindings come
    /// back once the refresh has materialized some or all of the occurrences.
    #[tokio::test]
    async fn many_raw_sources_resolve_their_anchors_in_one_bounded_pass() {
        const SOURCES: i64 = 6;
        const UNRELATED_OBSERVATIONS: i64 = 4;
        let directory = tempdir().expect("temporary directory");
        let runtime = HostAdmissionTestRuntimeV1::profile(directory.path())
            .await
            .expect("registered profile runtime");
        let conn = runtime
            .registered_database(HostAdmissionScope::Profile)
            .expect("profile database")
            .writer_connection()
            .expect("profile writer");
        seed_raw_sources(&conn, SOURCES).await;
        let mut bindings = Vec::new();
        for index in 0..SOURCES {
            let message_id = format!("message.source.{index}");
            let observation = fixture_observation(
                "codex",
                "session.message-anchor",
                &message_id,
                (index + 1) as u64,
            );
            let anchor = fixture_anchor(&observation);
            seed_canonical_binding_at(
                &conn,
                &serde_json::to_string(&observation).expect("observation json"),
                &observation,
                &anchor,
                &serde_json::to_string(anchor.owner()).expect("owner json"),
                index + 1,
            )
            .await;
            bindings.push((message_id, observation, anchor));
        }
        // Same-session observations that project none of the sources are still
        // part of every scan and must be decoded once per publication, not
        // once per source.
        for index in 0..UNRELATED_OBSERVATIONS {
            let observation = fixture_observation(
                "codex",
                "session.message-anchor",
                &format!("message.unrelated.{index}"),
                (SOURCES + index + 1) as u64,
            );
            seed_canonical_observation_at(
                &conn,
                &serde_json::to_string(&observation).expect("observation json"),
                &observation,
                SOURCES + index + 1,
            )
            .await;
        }
        let effects = SOURCES + UNRELATED_OBSERVATIONS;
        let publication = publication_over(&(41..41 + SOURCES).collect::<Vec<_>>());
        let expected_bindings = bindings
            .iter()
            .map(|(_, _, anchor)| (anchor.anchor_id().as_str().to_owned(), false))
            .collect::<Vec<_>>();
        let prepared_bindings = |sources: &[super::super::PreparedSource]| {
            sources
                .iter()
                .map(|source| (source.canonical.id.clone(), source.compatibility_anchor))
                .collect::<Vec<_>>()
        };

        // Before any refresh: every source goes through the canonical
        // observation authority.
        let counted = QueryCountingConnection::new(&conn);
        let before_refresh = super::super::sources::prepare_sources(&counted, &publication)
            .await
            .expect("publication sources before refresh");
        assert_eq!(prepared_bindings(&before_refresh), expected_bindings);
        let unmaterialized_statements = counted.query_count();
        println!(
            "prepare_sources before refresh: {SOURCES} sources, {effects} effects -> \
             {unmaterialized_statements} statements"
        );
        assert!(
            unmaterialized_statements <= 5,
            "{SOURCES} unmaterialized sources issued {unmaterialized_statements} statements"
        );

        // A refresh that has materialized half the sources: the materialized
        // half resolves through the generation, the rest through one scan.
        for (message_id, observation, anchor) in bindings.iter().take(SOURCES as usize / 2) {
            materialize_occurrence(&conn, observation, anchor, message_id).await;
        }
        let counted = QueryCountingConnection::new(&conn);
        let partially_materialized = super::super::sources::prepare_sources(&counted, &publication)
            .await
            .expect("publication sources after partial refresh");
        assert_eq!(
            prepared_bindings(&partially_materialized),
            expected_bindings
        );
        println!(
            "prepare_sources after partial refresh: {SOURCES} sources -> {} statements",
            counted.query_count()
        );
        assert!(
            counted.query_count() <= 6,
            "partially materialized sources issued {} statements",
            counted.query_count()
        );

        // Fully materialized: the occurrence lookup answers everything and the
        // observation scan is skipped.
        for (message_id, observation, anchor) in bindings.iter().skip(SOURCES as usize / 2) {
            materialize_occurrence(&conn, observation, anchor, message_id).await;
        }
        let counted = QueryCountingConnection::new(&conn);
        let materialized = super::super::sources::prepare_sources(&counted, &publication)
            .await
            .expect("publication sources after refresh");
        assert_eq!(prepared_bindings(&materialized), expected_bindings);
        println!(
            "prepare_sources after refresh: {SOURCES} sources -> {} statements",
            counted.query_count()
        );
        assert!(
            counted.query_count() <= 5,
            "materialized sources issued {} statements",
            counted.query_count()
        );
    }

    /// A mixed publication refuses on the first source (in source order) that
    /// fails its own check, with that source's typed refusal, and leaves
    /// nothing published: no summary node and no legacy anchor.
    #[tokio::test]
    async fn mixed_source_publication_refuses_on_the_first_failing_source() {
        let directory = tempdir().expect("temporary directory");
        let runtime = HostAdmissionTestRuntimeV1::profile(directory.path())
            .await
            .expect("registered profile runtime");
        let conn = runtime
            .registered_database(HostAdmissionScope::Profile)
            .expect("profile database")
            .writer_connection()
            .expect("profile writer");
        // 41: canonical anchor; 42: no canonical evidence (legacy fallback);
        // 43: ambiguous (two exact anchors project it); 44: retention-expired;
        // 45: foreign session.
        seed_raw_sources(&conn, 4).await;
        let canonical =
            fixture_observation("codex", "session.message-anchor", "message.source.0", 1);
        let canonical_anchor = fixture_anchor(&canonical);
        seed_canonical_binding_at(
            &conn,
            &serde_json::to_string(&canonical).expect("observation json"),
            &canonical,
            &canonical_anchor,
            &serde_json::to_string(canonical_anchor.owner()).expect("owner json"),
            1,
        )
        .await;
        for (sequence, ordinal, record_id) in [
            (2_i64, 2_u64, "message.source.2"),
            (3, 3, "message.source.2.duplicate"),
        ] {
            let observation = fixture_observation_projecting(
                "codex",
                "session.message-anchor",
                record_id,
                "message.source.2",
                ordinal,
            );
            let anchor = fixture_anchor(&observation);
            seed_canonical_binding_at(
                &conn,
                &serde_json::to_string(&observation).expect("observation json"),
                &observation,
                &anchor,
                &serde_json::to_string(anchor.owner()).expect("owner json"),
                sequence,
            )
            .await;
        }
        conn.execute(
            "UPDATE lcm_raw_messages SET metadata_json = '{\"retention_expires_at\": 1}'
             WHERE store_id = 44",
            (),
        )
        .await
        .expect("expired source");
        conn.execute_batch(
            "INSERT INTO sessions (provider, session_id, project_key, project_path)
             VALUES ('codex', 'session.foreign', 'user', '/foreign');
             INSERT INTO lcm_raw_messages (
                provider, message_id, session_id, store_id, role, ordinal, timestamp,
                content, content_hash, storage_kind, payload_ref, snippet_text,
                index_text, legacy_source, legacy_truncated, metadata_json
             ) VALUES (
                'codex', 'message.foreign', 'session.foreign', 45, 'assistant', 0, 1715000001,
                'foreign body', 'sha256:foreign-body', 'inline', NULL, 'foreign body',
                'foreign body', 0, 0, NULL
             );",
        )
        .await
        .expect("foreign source");
        // Expired (44) precedes foreign (45) in source order, so the expiry is
        // the refusal even though both fail.
        let result = super::super::publication::publish_immutable_summary(
            &conn,
            publication_over(&[41, 42, 44, 45]),
            &empty_relation_projection(),
        )
        .await;
        assert!(matches!(
            result,
            Err(LcmError::SummarySourceUnavailable { ref source_id, ref reason })
                if source_id == "44" && reason == "retention_expired"
        ));
        assert_eq!(legacy_anchor_count(&conn).await, 0);
        assert_eq!(summary_node_count(&conn).await, 0);

        // The ambiguous source (43) is refused by its own evidence even though
        // the canonical (41) and legacy-fallback (42) sources ahead of it are
        // fine; the fallback anchor for 42 is never written.
        let result = super::super::publication::publish_immutable_summary(
            &conn,
            publication_over(&[41, 42, 43]),
            &empty_relation_projection(),
        )
        .await;
        assert!(matches!(
            result,
            Err(LcmError::SummarySourceUnavailable { ref source_id, ref reason })
                if source_id == "message.source.2" && reason == "ambiguous_anchor"
        ));
        assert_eq!(legacy_anchor_count(&conn).await, 0);
        assert_eq!(summary_node_count(&conn).await, 0);

        // Without the failing sources the same publication commits: 41 keeps
        // its canonical anchor and 42 falls back to exactly one legacy anchor.
        super::super::publication::publish_immutable_summary(
            &conn,
            publication_over(&[41, 42]),
            &empty_relation_projection(),
        )
        .await
        .expect("publication over canonical and legacy sources");
        assert_eq!(legacy_anchor_count(&conn).await, 1);
        assert_eq!(summary_node_count(&conn).await, 1);
    }
}
