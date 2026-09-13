//! How a stored projection provenance row relates to this binary's output.
//!
//! Every release from v0.1.0-beta.25 through v0.1.0-beta.37 wrote projection
//! provenance under the same projector version, `claude-session-message-v5`,
//! and the same digest function: SHA-256 over the canonical JSON of
//! `{projector_version, output_ordinal, session, message}`. The projector
//! version, the digest chain (`message_output_digest`,
//! `PayloadReferenceV1::for_payload`, `canonical_json_bytes`,
//! `serialize_canonical`), the two digested record structs, and the canonical
//! observation envelope's serialization are byte-identical across all of those
//! tags and this tree.
//!
//! | shipped tags                     | projector version           | digest chain | digested records |
//! | -------------------------------- | --------------------------- | ------------ | ---------------- |
//! | v0.1.0-beta.25 .. v0.1.0-beta.37 | `claude-session-message-v5` | unchanged    | unchanged        |
//!
//! What did change, after the newest tag, is one *rendering*:
//! `provider_message_semantics` gives a Codex user message carrying an
//! `<codex_internal_context source="goal">` block a typed rendering — role
//! `system`, text `Codex active goal: …`, kind `goal_context`, and five extra
//! metadata keys — where every release stored the raw user message. The
//! rendering feeds the digest, so on a profile holding one such record the
//! audit recomputes a digest no release could have written, and the store was
//! refused with `projection provenance disagrees with deterministic output` —
//! leaving profile-session convergence degraded on every open with no remedy
//! but discarding 1.6 GB of session history.
//!
//! A provenance row holds no data of its own: every field is derived from the
//! durable observation it references, except the historical `message_created`
//! flag. So a row whose only disagreement is the content digest, and whose
//! digest is exactly the digest of the output row this store still holds, is a
//! released rendering: admitted by the audit and converged by the write step
//! that owns the transaction. A row disagreeing on identity — anchor, receipt,
//! output provider or message id — or carrying a digest that matches neither
//! this binary's output nor its own output row is not a rendering difference,
//! and stays refused, named.
//!
//! Convergence has two outcomes because rendering does. Some released
//! renderings are content the current LCM privacy sanitizer withholds — a
//! Codex goal-context objective carrying mixed structure renders as an
//! ambiguous structured document — and a capture running now derives no
//! servable output for them at all: it records the `sanitization_refused`
//! disposition instead. A quarantine verdict is therefore the current
//! rendering, and the released row converges to it. Only a sanitizer *fault*
//! (an unavailable detector, a receipt construction failure, a payload past
//! the bounded scan limit) still refuses the store, named.

use std::sync::Mutex;

use tracedecay_runtime_core::db::engine::Executor;
use tracedecay_store::{
    SESSION_MESSAGE_PROJECTOR_VERSION, SessionMessageProjection, message_output_digest,
};

use super::audit::ProjectionProvenanceRow;
use super::rows::authority_violation;
use crate::observation_projection::{ConvergedRendering, ProjectionRowsBatch};

/// What this binary must do with one stored provenance row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum StoredProvenanceRendering {
    /// The row is exactly the output this binary derives.
    Current,
    /// The row is the digest of the output row this store holds, under a
    /// rendering a release wrote.
    Released,
}

/// Released renderings one audit pass found, for the step that holds the write
/// transaction to converge.
///
/// The audit itself stays a read side: it runs on a read snapshot for Doctor
/// and inside a write transaction for schema convergence, and only the latter
/// may write. Collecting here keeps one classification for both instead of a
/// read-only copy that could judge a row differently.
#[derive(Default)]
pub(super) struct ReleasedRenderingLedger {
    outputs: Mutex<Vec<SessionMessageProjection>>,
}

impl ReleasedRenderingLedger {
    fn record(&self, projection: &SessionMessageProjection) {
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
    /// pass's own transaction, so the exhaustive path — the one a store needing
    /// convergence takes — retains and rewrites one resumable page at a time.
    /// The incremental suffix path audits its whole frontier in one
    /// transaction, so there a ledger's peak is that frontier's released
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
        if outputs.is_empty() {
            return Ok(0);
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
                "converging a released projection output rendering"
            );
            let converged =
                crate::observation_projection::converge_released_output_rendering(conn, projection)
                    .await
                    .map_err(|error| {
                        authority_violation(format!(
                            "failed to converge the released projection output rendering: \
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
            "converged released projection output renderings to this binary's rendering"
        );
        Ok(outputs.len())
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

/// Classifies one stored provenance row against the output this binary derives,
/// using the output rows the store holds to tell a released rendering from a
/// row that disagrees with its own output.
///
/// `stored` carries the page's output rows, read once for the whole page. An
/// absent output row is not a rendering question — the row the provenance names
/// is missing, which the caller's own row verification reports — so the digest
/// disagreement stands.
pub(super) fn classify_provenance_rendering(
    actual: &ProjectionProvenanceRow,
    projection: &SessionMessageProjection,
    stored: &ProjectionRowsBatch,
) -> tracedecay_domain::errors::Result<StoredProvenanceRendering> {
    if let Some(field) = provenance_identity_disagreement(actual, projection) {
        return Err(disagreement(field, actual, projection));
    }
    let derived_digest = projection.output_digest().map_err(|_| {
        authority_violation("projection output digest is not canonically derivable")
    })?;
    if actual.output_digest == derived_digest.as_str() {
        return Ok(StoredProvenanceRendering::Current);
    }
    let message = projection.message();
    let stored_digest = stored
        .message(&message.provider, &message.message_id)
        .map(|stored_message| {
            // The session record is derived from the observation and unchanged
            // across every shipped release, so the only released input to this
            // digest is the stored output row.
            message_output_digest(
                projection.session(),
                stored_message,
                projection.output_ordinal(),
            )
        })
        .transpose()
        .map_err(|_| authority_violation("stored projection output is not canonically digestible"))?
        .ok_or_else(|| disagreement("output_digest", actual, projection))?;
    if actual.output_digest == stored_digest.as_str() {
        Ok(StoredProvenanceRendering::Released)
    } else {
        Err(disagreement("output_digest", actual, projection))
    }
}

/// Admits one stored provenance row, recording a released rendering for the
/// write step to converge, and reports which it was.
///
/// The verdict matters to the caller: a `Released` output's stored rows are the
/// released rendering by definition, so comparing them field-by-field against
/// this binary's rendering would refuse exactly what the ledger is about to
/// rewrite.
pub(super) fn admit_provenance_row(
    actual: &ProjectionProvenanceRow,
    projection: &SessionMessageProjection,
    stored: &ProjectionRowsBatch,
    released: &ReleasedRenderingLedger,
) -> tracedecay_domain::errors::Result<StoredProvenanceRendering> {
    let rendering = classify_provenance_rendering(actual, projection, stored)?;
    if rendering == StoredProvenanceRendering::Released {
        released.record(projection);
    }
    Ok(rendering)
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
    use tracedecay_domain::derive_exact_observation_anchor_id;
    use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor};
    use tracedecay_store::{
        AnchoredObservationWrite, ObservationPersistOutcome, ObservationProjectionStore,
        ObservationStore, ObservationWrite, ProjectionSkipReason, ProjectionStoreError,
        SESSION_MESSAGE_PROJECTOR_VERSION, SessionMessageRecord, SessionRecord,
        message_output_digest,
    };

    use crate::tests::harness::{HostAdmissionScope, HostAdmissionTestRuntimeV1};

    /// The verbatim v0.1.0-beta.37 rendering of one Codex goal-context record,
    /// emitted by that tag's own `derive_canonical_projection` and
    /// `SessionMessageProjection::output_digest` rather than derived from this
    /// tree's contract. v0.1.0-beta.32 — the profile in the report — emits the
    /// same bytes.
    const RELEASED: &str = include_str!("../../../tests/fixtures/codex-goal-context-released.json");

    fn released() -> serde_json::Value {
        serde_json::from_str(RELEASED).expect("released rendering fixture")
    }

    /// The native record id, which is also the projected output's message id.
    const RECORD_ID: &str = "record.codex-goal-context";
    const SESSION: &str = "codex-goal-context-session";
    /// The record whose current rendering the LCM privacy sanitizer withholds:
    /// its goal objective carries mixed structure, so `Codex active goal: …`
    /// reads as an ambiguous structured document.
    const QUARANTINED_RECORD_ID: &str = "record.codex-goal-quarantine";
    const QUARANTINED_SESSION: &str = "codex-goal-quarantine-session";

    /// The derived canonical observation id the provenance row is keyed by.
    fn canonical_observation_id() -> String {
        observation().observation_id().as_str().to_owned()
    }

    /// The durable observation a Codex ingest wrote for the fixture's record.
    /// Identity material, receipt and payload are what any release persisted:
    /// only the *rendering* derived from it changed after the newest tag.
    fn observation() -> DurableObservationV1 {
        observation_for(released()["envelope"].clone(), RECORD_ID, SESSION)
    }

    fn quarantined_observation() -> DurableObservationV1 {
        observation_for(
            released()["quarantined"]["envelope"].clone(),
            QUARANTINED_RECORD_ID,
            QUARANTINED_SESSION,
        )
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
        let anchor = tracedecay_store::build_observation_retrieval_anchor_v2(
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

    /// Every persisted byte of one projected output: the message row, its LCM
    /// raw twin's indexed text, and the provenance digest that pairs them.
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
                "SELECT m.session_id, m.role, m.timestamp, m.ordinal, m.text, m.kind, m.model,
                        m.tool_names, m.source_path, m.source_offset, m.metadata_json,
                        raw.index_text, p.output_digest
                 FROM session_messages AS m
                 JOIN lcm_raw_messages AS raw
                   ON raw.provider = m.provider AND raw.message_id = m.message_id
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

    /// Rewrites the drained output to the bytes a release persisted, and arms
    /// an exhaustive audit. This is the shipped store the report describes: the
    /// released rendering, paired with the digest that release computed for it.
    async fn downgrade_to_released(conn: &impl Executor, digest: &str) {
        let fixture = released();
        let message: SessionMessageRecord =
            serde_json::from_value(fixture["released_message"].clone()).unwrap();
        let session: SessionRecord =
            serde_json::from_value(fixture["released_session"].clone()).unwrap();
        assert_eq!(
            message.session_id, session.session_id,
            "the fixture's released output must belong to its released session"
        );
        conn.execute(
            "UPDATE session_messages
             SET session_id = ?3, role = ?4, timestamp = ?5, ordinal = ?6,
                 text = ?7, kind = ?8, model = ?9, tool_names = ?10,
                 source_path = ?11, source_offset = ?12, metadata_json = ?13
             WHERE provider = ?1 AND message_id = ?2",
            tracedecay_runtime_core::params![
                message.provider.as_str(),
                message.message_id.as_str(),
                message.session_id.as_str(),
                message.role.as_str(),
                message.timestamp,
                message.ordinal,
                message.text.as_str(),
                message.kind.as_deref(),
                message.model.as_deref(),
                message.tool_names.as_deref(),
                message.source_path.as_deref(),
                message.source_offset,
                message.metadata_json.as_deref(),
            ],
        )
        .await
        .expect("restore the released message row");
        tracedecay_lcm::raw::upsert_projection_raw_message(conn, &message)
            .await
            .expect("restore the released LCM raw twin");
        conn.execute(
            "UPDATE observation_projection_provenance SET output_digest = ?2
             WHERE projector_version = ?1 AND observation_id = ?3",
            tracedecay_runtime_core::params![
                SESSION_MESSAGE_PROJECTOR_VERSION,
                digest,
                canonical_observation_id()
            ],
        )
        .await
        .expect("restore the released provenance digest");
        conn.execute("DELETE FROM authority_audit_checkpoints", ())
            .await
            .expect("arm the exhaustive audit");
    }

    /// The report's store: a profile holding one Codex goal-context record
    /// rendered by a release. Before the released variant existed, HEAD's audit
    /// compared its own rendering's digest against the released one and refused
    /// the whole store, so profile-session convergence reported `degraded` on
    /// every open and never advanced. It must instead admit the released
    /// rendering and re-stamp it, byte-for-byte, to what this binary derives.
    #[tokio::test]
    async fn released_codex_goal_rendering_converges_instead_of_degrading() {
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
        let fixture = released();
        let released_digest = fixture["released_output_digest"].as_str().unwrap();
        assert_eq!(
            current.role, "system",
            "this binary renders a Codex goal-context record as typed goal context"
        );
        assert_ne!(
            current.digest, released_digest,
            "the fixture must be a rendering no release could share with this binary, \
             or it proves nothing"
        );

        let transaction = database
            .runtime_database()
            .begin_write_transaction("seed the released rendering")
            .await
            .unwrap();
        downgrade_to_released(&transaction, released_digest).await;
        transaction.commit().await.unwrap();
        let snapshot = database.read_snapshot().await.unwrap();
        let shipped = stored_output(&snapshot, RECORD_ID).await;
        drop(snapshot);
        assert_eq!(shipped.digest, released_digest);
        assert_eq!(shipped.role, "user");

        super::super::ensure_authority_invariants(database.runtime_database(), true, false)
            .await
            .expect("a released output rendering must converge, not degrade the store");

        let snapshot = database.read_snapshot().await.unwrap();
        let converged = stored_output(&snapshot, RECORD_ID).await;
        drop(snapshot);
        assert_eq!(
            converged, current,
            "convergence must restore this binary's exact rendering and digest"
        );

        super::super::ensure_authority_invariants(database.runtime_database(), true, false)
            .await
            .expect("the converged store must stay admitted");
        let snapshot = database.read_snapshot().await.unwrap();
        assert_eq!(
            stored_output(&snapshot, RECORD_ID).await,
            converged,
            "a second open must be a no-op"
        );
    }

    /// One observation's projection authority: whether it still owns a served
    /// output, and what durable disposition stands in its place.
    #[derive(Debug, Eq, PartialEq)]
    struct ProjectionOutcome {
        message_rows: i64,
        raw_rows: i64,
        provenance_rows: i64,
        disposition: Option<(String, String)>,
    }

    async fn projection_outcome(
        conn: &impl QueryExecutor,
        observation: &DurableObservationV1,
        message_id: &str,
    ) -> ProjectionOutcome {
        let mut rows = conn
            .query(
                "SELECT
                    (SELECT COUNT(*) FROM session_messages
                     WHERE provider = 'codex' AND message_id = ?2),
                    (SELECT COUNT(*) FROM lcm_raw_messages
                     WHERE provider = 'codex' AND message_id = ?2),
                    (SELECT COUNT(*) FROM observation_projection_provenance
                     WHERE projector_version = ?1 AND observation_id = ?3),
                    (SELECT receipt_id FROM observation_projection_dispositions
                     WHERE projector_version = ?1 AND observation_id = ?3),
                    (SELECT reason FROM observation_projection_dispositions
                     WHERE projector_version = ?1 AND observation_id = ?3)",
                tracedecay_runtime_core::params![
                    SESSION_MESSAGE_PROJECTOR_VERSION,
                    message_id,
                    observation.observation_id().as_str(),
                ],
            )
            .await
            .expect("read the observation's projection authority");
        let row = rows
            .next()
            .await
            .expect("read the projection authority row")
            .expect("the aggregate row is always present");
        let receipt_id: Option<String> = row.get(3).unwrap();
        let reason: Option<String> = row.get(4).unwrap();
        ProjectionOutcome {
            message_rows: row.get(0).unwrap(),
            raw_rows: row.get(1).unwrap(),
            provenance_rows: row.get(2).unwrap(),
            disposition: receipt_id.zip(reason),
        }
    }

    /// Writes the output rows and provenance a release persisted for the
    /// quarantined record, and arms an exhaustive audit.
    ///
    /// The rows are inserted rather than downgraded because this binary's drain
    /// produces none for this envelope: the shipped store is the only place
    /// this output ever existed. The provenance digest is the digest of those
    /// released rows under the digest chain every tag from v0.1.0-beta.25
    /// through v0.1.0-beta.37 shares with this tree, which is the digest that
    /// release wrote.
    async fn install_released_quarantined_output(
        conn: &impl Executor,
        observation: &DurableObservationV1,
    ) -> String {
        let fixture = released()["quarantined"].clone();
        let message: SessionMessageRecord =
            serde_json::from_value(fixture["released_message"].clone()).unwrap();
        let session: SessionRecord =
            serde_json::from_value(fixture["released_session"].clone()).unwrap();
        assert_eq!(
            message.session_id, session.session_id,
            "the fixture's released output must belong to its released session"
        );
        conn.execute(
            "INSERT INTO sessions
                (provider, session_id, project_key, project_path, title, started_at, ended_at,
                 transcript_path, metadata_json, parent_session_id, is_subagent, agent_id,
                 parent_tool_use_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            tracedecay_runtime_core::params![
                session.provider.as_str(),
                session.session_id.as_str(),
                session.project_key.as_str(),
                session.project_path.as_str(),
                session.title.as_deref(),
                session.started_at,
                session.ended_at,
                session.transcript_path.as_deref(),
                session.metadata_json.as_deref(),
                session.parent_session_id.as_deref(),
                i64::from(session.is_subagent),
                session.agent_id.as_deref(),
                session.parent_tool_use_id.as_deref(),
            ],
        )
        .await
        .expect("install the released session row");
        conn.execute(
            "INSERT INTO session_messages
                (provider, message_id, session_id, role, timestamp, ordinal, text, kind, model,
                 tool_names, source_path, source_offset, metadata_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            tracedecay_runtime_core::params![
                message.provider.as_str(),
                message.message_id.as_str(),
                message.session_id.as_str(),
                message.role.as_str(),
                message.timestamp,
                message.ordinal,
                message.text.as_str(),
                message.kind.as_deref(),
                message.model.as_deref(),
                message.tool_names.as_deref(),
                message.source_path.as_deref(),
                message.source_offset,
                message.metadata_json.as_deref(),
            ],
        )
        .await
        .expect("install the released message row");
        tracedecay_lcm::raw::upsert_projection_raw_message(conn, &message)
            .await
            .expect("the released rendering must still be servable by this binary's sanitizer");
        let digest = message_output_digest(&session, &message, 0)
            .expect("digest the released output")
            .as_str()
            .to_owned();
        let anchor =
            derive_exact_observation_anchor_id(observation.scope(), observation.observation_id())
                .unwrap();
        conn.execute(
            "INSERT INTO observation_projection_provenance
                (projector_version, observation_id, output_ordinal, retrieval_anchor_id,
                 receipt_id, output_provider, output_message_id, output_digest, message_created)
             VALUES (?1, ?2, 0, ?3, ?4, ?5, ?6, ?7, 1)",
            tracedecay_runtime_core::params![
                SESSION_MESSAGE_PROJECTOR_VERSION,
                observation.observation_id().as_str(),
                anchor.as_str(),
                observation.receipt().receipt().receipt_id().as_str(),
                message.provider.as_str(),
                message.message_id.as_str(),
                digest.as_str(),
            ],
        )
        .await
        .expect("install the released provenance row");
        conn.execute(
            "DELETE FROM observation_projection_dispositions
             WHERE projector_version = ?1 AND observation_id = ?2",
            tracedecay_runtime_core::params![
                SESSION_MESSAGE_PROJECTOR_VERSION,
                observation.observation_id().as_str(),
            ],
        )
        .await
        .expect("a release wrote an output here, not a refusal");
        conn.execute("DELETE FROM authority_audit_checkpoints", ())
            .await
            .expect("arm the exhaustive audit");
        digest
    }

    /// The same profile holding a Codex goal-context record whose *current*
    /// rendering the LCM privacy sanitizer withholds — its objective carries
    /// mixed structure, so `Codex active goal: …` reads as an ambiguous
    /// structured document.
    ///
    /// The re-stamp path renders the shipped output through this binary's
    /// projector, so it meets that verdict. Treating the verdict as a
    /// convergence failure refused the whole store, leaving profile-session
    /// convergence `degraded` on every open with no remedy but discarding the
    /// profile's session history. A quarantine is a legitimate current
    /// rendering, so the released row must converge to exactly the durable
    /// refusal a fresh capture of the same envelope writes.
    #[tokio::test]
    async fn quarantined_codex_goal_rendering_converges_to_the_withheld_capture() {
        let directory = TempDir::new().unwrap();
        let runtime = HostAdmissionTestRuntimeV1::profile(directory.path())
            .await
            .unwrap();
        let observation = quarantined_observation();
        let error = seed(&runtime, &observation)
            .await
            .expect_err("this binary's sanitizer must withhold the fixture's rendering");
        match &error {
            ProjectionStoreError::SanitizationRefused {
                reason,
                quarantined,
            } => {
                assert!(
                    *quarantined,
                    "an ambiguous structured document is a quarantine verdict, not a fault: \
                     {reason}"
                );
                assert!(
                    reason.contains("privacy sanitizer quarantined an ambiguous structured \
                                     document"),
                    "the capture path must name the sanitizer's verdict: {reason}"
                );
            }
            other => panic!("the capture path must quarantine, not {other:?}"),
        }

        let database = runtime
            .registered_database(HostAdmissionScope::Profile)
            .expect("registered profile database");
        let snapshot = database.read_snapshot().await.unwrap();
        // What a fresh capture of this envelope leaves behind: no output, and
        // the observation's own receipt bound to the durable refusal.
        let fresh_capture = projection_outcome(&snapshot, &observation, QUARANTINED_RECORD_ID).await;
        drop(snapshot);
        assert_eq!(
            fresh_capture,
            ProjectionOutcome {
                message_rows: 0,
                raw_rows: 0,
                provenance_rows: 0,
                disposition: Some((
                    observation.receipt().receipt().receipt_id().as_str().to_owned(),
                    ProjectionSkipReason::SanitizationRefused.as_str().to_owned(),
                )),
            }
        );

        let transaction = database
            .runtime_database()
            .begin_write_transaction("install the released rendering")
            .await
            .unwrap();
        let released_digest = install_released_quarantined_output(&transaction, &observation).await;
        transaction.commit().await.unwrap();
        let snapshot = database.read_snapshot().await.unwrap();
        let shipped = stored_output(&snapshot, QUARANTINED_RECORD_ID).await;
        drop(snapshot);
        assert_eq!(shipped.digest, released_digest);
        assert_eq!(shipped.role, "user");

        super::super::ensure_authority_invariants(database.runtime_database(), true, false)
            .await
            .expect("a quarantine verdict must converge, not degrade the store");

        let snapshot = database.read_snapshot().await.unwrap();
        assert_eq!(
            projection_outcome(&snapshot, &observation, QUARANTINED_RECORD_ID).await,
            fresh_capture,
            "convergence must reach exactly what a fresh capture of this envelope writes"
        );
        // The projection is derived state; the durable observation it came from
        // is the authority and must survive, so a binary whose sanitizer admits
        // this content can project it again.
        let retained: i64 = snapshot
            .query(
                "SELECT COUNT(*) FROM observations WHERE observation_id = ?1 AND receipt_id = ?2",
                tracedecay_runtime_core::params![
                    observation.observation_id().as_str(),
                    observation.receipt().receipt().receipt_id().as_str(),
                ],
            )
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap()
            .get(0)
            .unwrap();
        assert_eq!(retained, 1, "retirement must not touch the observation");
        drop(snapshot);

        super::super::ensure_authority_invariants(database.runtime_database(), true, false)
            .await
            .expect("the converged store must stay admitted");
        let snapshot = database.read_snapshot().await.unwrap();
        assert_eq!(
            projection_outcome(&snapshot, &observation, QUARANTINED_RECORD_ID).await,
            fresh_capture,
            "a second open must be a no-op"
        );
    }

    /// A digest matching neither this binary's rendering nor the output row it
    /// is stored beside is corruption, not a release: it stays refused, and the
    /// refusal names the field, the projector, and the row so an operator log
    /// says which output to look at.
    #[tokio::test]
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
