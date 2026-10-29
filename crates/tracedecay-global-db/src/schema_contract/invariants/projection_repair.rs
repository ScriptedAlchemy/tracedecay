//! How a stored projection provenance row relates to this binary's output,
//! and the repairs the write step owns.
//!
//! A provenance row holds no data of its own: every field is derived from the
//! durable observation it references, except the historical `message_created`
//! flag. A row whose identity or content digest disagrees with this binary's
//! deterministic output is refused, named. The audit records interrupted
//! writes of the current rendering (a missing output or session row, owned
//! storage columns needing a rewrite, a missing location) on
//! [`ProjectionRepairLedger`] so the step that holds the write transaction can
//! converge them.

use std::sync::Mutex;

use tracedecay_runtime_core::db::engine::{Executor, params};
use tracedecay_store::{
    ProjectionStoreError, SESSION_MESSAGE_PROJECTOR_VERSION, SessionMessageProjection,
};

use super::audit::ProjectionProvenanceRow;
use super::rows::authority_violation;
use crate::observation_projection::{ConvergedRendering, stored_output_digest};

/// Current-rendering repairs one audit pass found, for the step that holds the
/// write transaction to converge.
///
/// The audit itself stays a read side: it runs on a read snapshot for Doctor
/// and inside a write transaction for schema convergence, and only the latter
/// may write. Collecting here keeps one classification for both instead of a
/// read-only copy that could judge a row differently.
#[derive(Default)]
pub(super) struct ProjectionRepairLedger {
    outputs: Mutex<Vec<SessionMessageProjection>>,
    locations: Mutex<Vec<(String, String, String)>>,
}

impl ProjectionRepairLedger {
    /// Storage-only backfill for a verified row whose protected rendering is current.
    pub(super) fn record_location(&self, projection: &SessionMessageProjection, location: String) {
        let message = projection.message();
        self.locations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((
                message.provider.clone(),
                message.message_id.clone(),
                location,
            ));
    }

    pub(super) fn record(&self, projection: &SessionMessageProjection) {
        let mut outputs = self
            .outputs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !outputs.contains(projection) {
            outputs.push(projection.clone());
        }
    }

    /// Rewrites every recorded output to this binary's rendering, leaving the
    /// ledger empty so a resumed pass records afresh.
    ///
    /// One ledger lives for exactly one audit pass and converges inside that
    /// pass's own transaction, so the exhaustive path, the one a store needing
    /// convergence takes, retains and rewrites one resumable page at a time.
    /// The incremental suffix path audits its whole frontier in one
    /// transaction, so there a ledger's peak is that frontier's repaired
    /// outputs rather than a page's; that is the suffix pass's existing bound,
    /// not one this adds.
    pub(super) async fn converge(
        &self,
        conn: &impl Executor,
    ) -> tracedecay_domain::errors::Result<usize> {
        let outputs = std::mem::take(
            &mut *self
                .outputs
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        let locations = std::mem::take(
            &mut *self
                .locations
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        if !locations.is_empty() {
            let encoded = serde_json::to_string(&locations).map_err(|error| {
                authority_violation(format!(
                    "failed to encode projected message locations: {error}"
                ))
            })?;
            conn.execute(
                "UPDATE lcm_raw_messages AS message
                 SET location_path = json_extract(location.value, '$[2]')
                 FROM json_each(?1) AS location
                 WHERE message.provider = json_extract(location.value, '$[0]')
                   AND message.message_id = json_extract(location.value, '$[1]')
                   AND message.location_path IS NULL",
                params![encoded.as_str()],
            )
            .await
            .map_err(|error| {
                authority_violation(format!(
                    "failed to backfill projected message locations: {error}"
                ))
            })?;
        }
        if outputs.is_empty() {
            return Ok(locations.len());
        }
        // Retiring a quarantined observation removes every output it created,
        // so its remaining recorded outputs are already converged; re-running
        // the write step for them would resurrect a raw row with no message.
        let mut retired = std::collections::BTreeSet::new();
        let mut re_stamped = 0usize;
        for projection in &outputs {
            let provenance = projection.provenance();
            if retired.contains(provenance.observation_id().as_str()) {
                continue;
            }
            tracing::debug!(
                projector = provenance.projector_version(),
                observation = provenance.observation_id().as_str(),
                output_ordinal = projection.output_ordinal(),
                output_provider = projection.message().provider.as_str(),
                output_message_id = projection.message().message_id.as_str(),
                "converging a projection output repair"
            );
            let converged =
                crate::observation_projection::converge_projection_output(conn, projection)
                    .await
                    .map_err(|error| {
                        authority_violation(format!(
                            "failed to converge the projection output repair: \
                             projector={} observation={} output_ordinal={} output={}/{}: {error}",
                            provenance.projector_version(),
                            provenance.observation_id().as_str(),
                            projection.output_ordinal(),
                            projection.message().provider,
                            projection.message().message_id,
                        ))
                    })?;
            match converged {
                ConvergedRendering::Output => re_stamped += 1,
                ConvergedRendering::Quarantined => {
                    retired.insert(provenance.observation_id().as_str().to_owned());
                }
            }
        }
        tracing::info!(
            projector = SESSION_MESSAGE_PROJECTOR_VERSION,
            converged = outputs.len(),
            re_stamped,
            retired = retired.len(),
            "converged projection output repairs to this binary's rendering"
        );
        Ok(outputs.len() + locations.len())
    }
}

/// Identity fields a provenance row copies straight from its observation, in
/// the order the audit reports them. A disagreement here is never a rendering
/// difference.
fn provenance_identity_disagreement(
    actual: &ProjectionProvenanceRow,
    projection: &SessionMessageProjection,
) -> Option<&'static str> {
    let provenance = projection.provenance();
    let message = projection.message();
    if actual.retrieval_anchor_id != provenance.retrieval_anchor_id().as_str() {
        return Some("retrieval_anchor_id");
    }
    if actual.receipt_id != provenance.receipt_id() {
        return Some("receipt_id");
    }
    if actual.output_provider != message.provider {
        return Some("output_provider");
    }
    if actual.output_message_id != message.message_id {
        return Some("output_message_id");
    }
    if !matches!(actual.message_created, 0 | 1) {
        return Some("message_created");
    }
    None
}

/// Names the row, the field, and the projector an operator has to look at.
fn disagreement(
    field: &'static str,
    actual: &ProjectionProvenanceRow,
    projection: &SessionMessageProjection,
) -> tracedecay_domain::errors::TraceDecayError {
    let provenance = projection.provenance();
    authority_violation(format!(
        "projection provenance disagrees with deterministic output: field={field} \
         projector={} observation={} output_ordinal={} output={}/{}",
        provenance.projector_version(),
        provenance.observation_id().as_str(),
        projection.output_ordinal(),
        actual.output_provider,
        actual.output_message_id,
    ))
}

/// Admits one stored provenance row: its identity and content digest must be
/// exactly the output this binary derives.
pub(super) fn admit_provenance_row(
    actual: &ProjectionProvenanceRow,
    projection: &SessionMessageProjection,
) -> tracedecay_domain::errors::Result<()> {
    if let Some(field) = provenance_identity_disagreement(actual, projection) {
        return Err(disagreement(field, actual, projection));
    }
    match stored_output_digest(projection) {
        Ok(derived_digest) if actual.output_digest == derived_digest.as_str() => Ok(()),
        Ok(_) | Err(ProjectionStoreError::SanitizationRefused { .. }) => {
            Err(disagreement("output_digest", actual, projection))
        }
        Err(_) => Err(authority_violation(
            "projection output digest is not canonically derivable",
        )),
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;
    use tracedecay_domain::{
        ComponentVersion, DurableObservationV1, ObservationId, ObservationIdentityMaterialV1,
        ObservationOrderingDomainV1, ObservationScopeV1, ObservationSourceCursorV1,
        ObservationSourceGenerationV1, ObservationSourceIdentityV1, ObservationSourceRangeV1,
        PayloadReferenceV1, ProjectionGenerationId, ProviderId, RetentionClass,
        SanitizationReceiptId, SanitizationReceiptRefV1, SanitizationReceiptV1,
        SanitizerDispositionV1, SensitivityV1, SessionId, UtcMicros,
    };
    use tracedecay_runtime_core::db::engine::QueryExecutor;
    use tracedecay_store::{
        AnchoredObservationWrite, ObservationPersistOutcome, ObservationProjectionStore,
        ObservationStore, ObservationWrite, SESSION_MESSAGE_PROJECTOR_VERSION,
    };

    use crate::tests::harness::{HostAdmissionScope, HostAdmissionTestRuntimeV1};

    /// The canonical observation envelope of one Codex goal-context record.
    const ENVELOPE: &str = include_str!("../../../tests/fixtures/codex-goal-context-envelope.json");

    fn envelope() -> serde_json::Value {
        serde_json::from_str(ENVELOPE).expect("goal-context envelope fixture")
    }

    /// The native record id, which is also the projected output's message id.
    const RECORD_ID: &str = "record.codex-goal-context";
    const SESSION: &str = "codex-goal-context-session";

    /// The derived canonical observation id the provenance row is keyed by.
    fn canonical_observation_id() -> String {
        observation().observation_id().as_str().to_owned()
    }

    /// The durable observation a Codex ingest writes for the fixture's record.
    fn observation() -> DurableObservationV1 {
        observation_for(envelope(), RECORD_ID, SESSION)
    }

    fn observation_for(
        payload: serde_json::Value,
        record_id: &str,
        session_id: &str,
    ) -> DurableObservationV1 {
        let source = ObservationSourceIdentityV1::for_provider(
            ProviderId::new("codex").unwrap(),
            SessionId::new(session_id).unwrap(),
        )
        .unwrap();
        let receipt = SanitizationReceiptV1::new(
            SanitizationReceiptRefV1::new(
                SanitizationReceiptId::new(format!("receipt.{record_id}")).unwrap(),
                ComponentVersion::new("sanitizer.codex-goal-context.v1").unwrap(),
            )
            .unwrap(),
            SanitizerDispositionV1::Accepted,
            SensitivityV1::NonSensitive,
            Some(PayloadReferenceV1::for_payload(&payload).unwrap()),
        )
        .unwrap();
        DurableObservationV1::new(
            ObservationIdentityMaterialV1::for_native_record(
                source,
                ObservationScopeV1::Profile,
                ObservationSourceGenerationV1::new(1).unwrap(),
                ObservationSourceRangeV1::new(0, 1).unwrap(),
                ObservationOrderingDomainV1::FileBytes,
                ObservationId::new(record_id).unwrap(),
            )
            .unwrap(),
            receipt,
            RetentionClass::new("retention.codex-goal-context").unwrap(),
            payload,
        )
        .unwrap()
    }

    /// Commits the observation through the real store and drains it, returning
    /// the drain's own outcome so a caller can assert a deterministic refusal
    /// instead of an output. The authority rows under test are the ones
    /// production writes.
    async fn seed(
        runtime: &HostAdmissionTestRuntimeV1,
        observation: &DurableObservationV1,
    ) -> Result<(), tracedecay_store::ProjectionStoreError> {
        let store = runtime
            .observation_store(HostAdmissionScope::Profile)
            .unwrap();
        let next_cursor = ObservationSourceCursorV1::for_ordering(
            observation.source().clone(),
            observation.scope().clone(),
            observation.identity().generation(),
            observation.identity().ordering_domain(),
            observation.identity().position().end(),
        )
        .unwrap();
        let write = ObservationWrite::new(observation.clone(), None, next_cursor).unwrap();
        let generation = ProjectionGenerationId::new("projection.codex-goal-context.v1").unwrap();
        let authorization = tracedecay_store::build_observation_resolution_authorization_v1(
            write.observation(),
            "codex-goal-context",
        )
        .unwrap();
        let anchor = tracedecay_store::build_observation_retrieval_anchor(
            write.observation(),
            generation.clone(),
            UtcMicros(1),
            authorization,
        )
        .unwrap();
        assert!(matches!(
            store
                .persist_observation(
                    AnchoredObservationWrite::new(write, anchor, generation).unwrap()
                )
                .await
                .unwrap(),
            ObservationPersistOutcome::Committed(_)
        ));
        store
            .project_observation(observation.observation_id())
            .await
            .map(|_| ())
    }

    /// Every persisted byte of one projected output: the message row, its
    /// indexed text, and the provenance digest that pairs them.
    #[derive(Debug, Eq, PartialEq)]
    struct StoredOutput {
        session_id: String,
        role: String,
        timestamp: Option<i64>,
        ordinal: i64,
        text: String,
        kind: Option<String>,
        model: Option<String>,
        tool_names: Option<String>,
        source_path: Option<String>,
        source_offset: Option<i64>,
        metadata_json: Option<String>,
        raw_index_text: String,
        digest: String,
    }

    async fn stored_output(conn: &impl QueryExecutor, message_id: &str) -> StoredOutput {
        let mut rows = conn
            .query(
                "SELECT m.session_id, m.role, m.timestamp, m.ordinal,
                        COALESCE(m.content, m.placeholder_text, ''), m.kind, m.model,
                        m.tool_names, m.source_path, m.source_offset, m.metadata_json,
                        m.index_text, p.output_digest
                 FROM lcm_raw_messages AS m
                 JOIN observation_projection_provenance AS p
                   ON p.output_provider = m.provider AND p.output_message_id = m.message_id
                 WHERE m.provider = 'codex' AND m.message_id = ?1",
                tracedecay_runtime_core::params![message_id],
            )
            .await
            .expect("read the projected output");
        let row = rows
            .next()
            .await
            .expect("read the projected output row")
            .expect("the drained observation must have produced one output");
        StoredOutput {
            session_id: row.get(0).unwrap(),
            role: row.get(1).unwrap(),
            timestamp: row.get(2).unwrap(),
            ordinal: row.get(3).unwrap(),
            text: row.get(4).unwrap(),
            kind: row.get(5).unwrap(),
            model: row.get(6).unwrap(),
            tool_names: row.get(7).unwrap(),
            source_path: row.get(8).unwrap(),
            source_offset: row.get(9).unwrap(),
            metadata_json: row.get(10).unwrap(),
            raw_index_text: row.get(11).unwrap(),
            digest: row.get(12).unwrap(),
        }
    }

    /// A row whose content survived but whose content hash did not still
    /// fails hydration with `PayloadIntegrityMismatch`. Content equality alone
    /// is not the row a fresh projection write stores.
    #[asupersync::test]
    async fn current_provenance_repairs_a_row_with_a_stale_hash() {
        let directory = TempDir::new().unwrap();
        let runtime = HostAdmissionTestRuntimeV1::profile(directory.path())
            .await
            .unwrap();
        seed(&runtime, &observation()).await.unwrap();
        let database = runtime
            .registered_database(HostAdmissionScope::Profile)
            .expect("registered profile database");
        let snapshot = database.read_snapshot().await.unwrap();
        let current = stored_output(&snapshot, RECORD_ID).await;
        drop(snapshot);

        let transaction = database
            .runtime_database()
            .begin_write_transaction("stale the stored content hash")
            .await
            .unwrap();
        let updated = transaction
            .execute(
                "UPDATE lcm_raw_messages
                 SET content_hash = 'stale'
                 WHERE provider = 'codex' AND message_id = ?1",
                tracedecay_runtime_core::params![RECORD_ID],
            )
            .await
            .expect("stale the derived columns");
        assert_eq!(updated, 1);
        transaction.commit().await.unwrap();

        super::super::ensure_authority_invariants(database.runtime_database(), false, false)
            .await
            .expect("current provenance must repair a row with a stale content hash");

        let snapshot = database.read_snapshot().await.unwrap();
        assert_eq!(stored_output(&snapshot, RECORD_ID).await, current);
        let mut rows = snapshot
            .query(
                "SELECT content_hash, content FROM lcm_raw_messages
                 WHERE provider = 'codex' AND message_id = ?1",
                tracedecay_runtime_core::params![RECORD_ID],
            )
            .await
            .expect("read the repaired row");
        let row = rows
            .next()
            .await
            .expect("read the repaired row")
            .expect("message row");
        assert_eq!(
            row.get::<String>(0).unwrap(),
            tracedecay_lcm::retrieval_content::projected_content_hash(
                &row.get::<String>(1).unwrap()
            ),
            "the repaired row must carry the hash its content hydrates against"
        );
    }

    /// The repair above is the shipped rendering, not any disagreement under
    /// current provenance. A body neither this binary nor a release wrote is
    /// tamper: the audit refuses it and does not rewrite the row.
    #[asupersync::test]
    async fn current_provenance_refuses_a_tampered_output_row() {
        let directory = TempDir::new().unwrap();
        let runtime = HostAdmissionTestRuntimeV1::profile(directory.path())
            .await
            .unwrap();
        seed(&runtime, &observation()).await.unwrap();
        let database = runtime
            .registered_database(HostAdmissionScope::Profile)
            .expect("registered profile database");

        let transaction = database
            .runtime_database()
            .begin_write_transaction("tamper the projected message body")
            .await
            .unwrap();
        let updated = transaction
            .execute(
                "UPDATE lcm_raw_messages SET content = 'tampered projection body'
                 WHERE provider = 'codex' AND message_id = ?1",
                tracedecay_runtime_core::params![RECORD_ID],
            )
            .await
            .expect("tamper projected message");
        assert_eq!(updated, 1);
        transaction
            .execute("DELETE FROM authority_audit_checkpoints", ())
            .await
            .unwrap();
        transaction.commit().await.unwrap();

        let error =
            super::super::ensure_authority_invariants(database.runtime_database(), true, false)
                .await
                .expect_err(
                    "a tampered projected message under current provenance must stay refused",
                );
        let message = error.to_string();
        assert!(
            message.contains("projection output rows disagree with deterministic output"),
            "{message}"
        );

        let snapshot = database.read_snapshot().await.unwrap();
        assert_eq!(
            stored_output(&snapshot, RECORD_ID).await.text,
            "tampered projection body",
            "refusal must not rewrite the tampered row"
        );
    }

    #[asupersync::test]
    async fn current_provenance_restores_its_missing_session_row() {
        let directory = TempDir::new().unwrap();
        let runtime = HostAdmissionTestRuntimeV1::profile(directory.path())
            .await
            .unwrap();
        seed(&runtime, &observation()).await.unwrap();
        let database = runtime
            .registered_database(HostAdmissionScope::Profile)
            .expect("registered profile database");
        let snapshot = database.read_snapshot().await.unwrap();
        let current = stored_output(&snapshot, RECORD_ID).await;
        drop(snapshot);

        let transaction = database
            .runtime_database()
            .begin_write_transaction("move the projected session row to a stale identity")
            .await
            .unwrap();
        transaction
            .execute(
                "INSERT INTO sessions (
                    provider, session_id, project_key, project_path, title,
                    started_at, ended_at, transcript_path, metadata_json,
                    parent_session_id, is_subagent, agent_id, parent_tool_use_id
                 )
                 SELECT provider, ?2, project_key, project_path, title,
                    started_at, ended_at, transcript_path, metadata_json,
                    parent_session_id, is_subagent, agent_id, parent_tool_use_id
                 FROM sessions WHERE provider = 'codex' AND session_id = ?1",
                tracedecay_runtime_core::params![SESSION, "stale-session-identity"],
            )
            .await
            .expect("create stale session identity");
        transaction
            .execute(
                "UPDATE lcm_raw_messages SET session_id = ?2
                 WHERE provider = 'codex' AND session_id = ?1",
                tracedecay_runtime_core::params![SESSION, "stale-session-identity"],
            )
            .await
            .expect("move projected message to stale session identity");
        let removed = transaction
            .execute(
                "DELETE FROM sessions WHERE provider = 'codex' AND session_id = ?1",
                tracedecay_runtime_core::params![SESSION],
            )
            .await
            .expect("remove expected projected session");
        assert_eq!(removed, 1);
        transaction.commit().await.unwrap();
        let snapshot = database.read_snapshot().await.unwrap();
        let mut rows = snapshot
            .query(
                "SELECT COUNT(*) FROM sessions WHERE provider = 'codex' AND session_id = ?1",
                tracedecay_runtime_core::params![SESSION],
            )
            .await
            .expect("count missing session");
        assert_eq!(
            rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
            0
        );
        drop(rows);
        drop(snapshot);

        super::super::ensure_authority_invariants(database.runtime_database(), true, false)
            .await
            .expect("current provenance must restore its missing session row");

        let restored = database
            .get_session("codex", SESSION)
            .await
            .expect("session restored from immutable projection")
            .expect("restored session row");
        assert_eq!(restored.provider, "codex");
        assert_eq!(restored.session_id, SESSION);
        let snapshot = database.read_snapshot().await.unwrap();
        assert_eq!(stored_output(&snapshot, RECORD_ID).await, current);
    }

    /// A digest matching neither this binary's rendering nor the output row it
    /// is stored beside is corruption, not a release: it stays refused, and the
    /// refusal names the field, the projector, and the row so an operator log
    /// says which output to look at.
    #[asupersync::test]
    async fn provenance_disagreeing_with_its_own_output_stays_refused() {
        let directory = TempDir::new().unwrap();
        let runtime = HostAdmissionTestRuntimeV1::profile(directory.path())
            .await
            .unwrap();
        seed(&runtime, &observation()).await.unwrap();
        let database = runtime
            .registered_database(HostAdmissionScope::Profile)
            .expect("registered profile database");

        let transaction = database
            .runtime_database()
            .begin_write_transaction("corrupt the provenance digest")
            .await
            .unwrap();
        transaction
            .execute(
                "UPDATE observation_projection_provenance SET output_digest = ?2
                 WHERE projector_version = ?1 AND observation_id = ?3",
                tracedecay_runtime_core::params![
                    SESSION_MESSAGE_PROJECTOR_VERSION,
                    format!("sha256:{}", "0".repeat(64)),
                    canonical_observation_id()
                ],
            )
            .await
            .unwrap();
        transaction
            .execute("DELETE FROM authority_audit_checkpoints", ())
            .await
            .unwrap();
        transaction.commit().await.unwrap();

        let error =
            super::super::ensure_authority_invariants(database.runtime_database(), true, false)
                .await
                .expect_err("a digest agreeing with no rendering of its own row must stay refused");
        let message = error.to_string();
        for expected in [
            "projection provenance disagrees with deterministic output",
            "field=output_digest",
            &format!("projector={SESSION_MESSAGE_PROJECTOR_VERSION}"),
            &format!("observation={}", canonical_observation_id()),
            "output_ordinal=0",
            &format!("output=codex/{RECORD_ID}"),
        ] {
            assert!(
                message.contains(expected),
                "the refusal must name {expected}: {message}"
            );
        }
    }
}
