use std::fmt::{self, Write as _};
use std::future::Future;
use std::path::Path;
use std::pin::Pin;

use serde::Deserialize;
use sha2::{Digest, Sha256};
use tracedecay_application::now_micros;
use tracedecay_domain::{
    AnchorDurabilityClass, CanonicalObservationEnvelopeV1, DurableObservationV1, HydrationStateV1,
    ObservationScopeV1, PayloadAccessState, ProjectId, RetrievalAnchorId, RetrievalAnchorRecord,
};
use tracedecay_runtime_core::db::engine::params;
use tracedecay_store::SessionMessageRecord;
use zeroize::Zeroizing;

use crate::observation_projection::derive_projection;
use tracedecay_query::temporal::hydration::{
    HydrationAuthorization, HydrationDenial, HydrationError, HydrationFuture, HydrationGrant,
    HydrationSink, TemporalHydrationPort,
};
use tracedecay_query::temporal::ports::{
    ExecutionControl, TemporalExecutionSnapshot, TemporalRetrievalScope, TemporalSourceAccess,
};
use tracedecay_runtime_core::db::engine;
use tracedecay_sessions::lcm::contracts::validate_payload_ref;
use tracedecay_sessions::runtime::lcm::payload::read_verified_payload_content;

use super::operations::CanonicalPublicationManifest;
use super::sql::TemporalSqlRead;

type BackendFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, HydrationError>> + Send + 'a>>;

#[derive(Clone)]
pub enum HydrationResolution {
    Available(PayloadDescriptor),
    Unavailable(HydrationStateV1),
}

#[derive(Clone)]
pub struct PayloadDescriptor {
    source: PayloadSource,
    byte_count: usize,
    content_hash: String,
}

impl fmt::Debug for PayloadDescriptor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PayloadDescriptor")
            .field("source", &self.source)
            .field("byte_count", &self.byte_count)
            .field("content_hash", &self.content_hash)
            .finish()
    }
}

#[derive(Clone)]
enum PayloadSource {
    Occurrence {
        content: Zeroizing<Vec<u8>>,
    },
    Summary {
        session_id: String,
        summary_id: String,
    },
    External {
        provider: String,
        session_id: String,
        payload_ref: String,
        char_count: usize,
    },
}

impl fmt::Debug for PayloadSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Occurrence { .. } => formatter.write_str("Occurrence(<redacted>)"),
            Self::Summary { .. } => formatter.write_str("Summary(<redacted>)"),
            Self::External { .. } => formatter.write_str("External(<redacted>)"),
        }
    }
}

pub trait TemporalHydrationBackend: Send + Sync {
    fn resolve_current<'a>(
        &'a self,
        snapshot: &'a TemporalExecutionSnapshot,
        anchor_id: &'a RetrievalAnchorId,
    ) -> BackendFuture<'a, HydrationResolution>;

    fn read_bounded<'a>(
        &'a self,
        descriptor: &'a PayloadDescriptor,
        max_bytes: usize,
        control: &'a ExecutionControl,
    ) -> BackendFuture<'a, Zeroizing<Vec<u8>>>;
}

pub struct SessionTemporalHydrationAdapter<B> {
    backend: B,
}

impl<B> SessionTemporalHydrationAdapter<B> {
    pub const fn new(backend: B) -> Self {
        Self { backend }
    }
}

impl<B: TemporalHydrationBackend> SessionTemporalHydrationAdapter<B> {
    async fn authorize(
        &self,
        snapshot: &TemporalExecutionSnapshot,
        anchor_id: &RetrievalAnchorId,
    ) -> Result<HydrationAuthorization, HydrationError> {
        snapshot.request().execution_control().checkpoint()?;
        let resolution = self.backend.resolve_current(snapshot, anchor_id).await?;
        snapshot.request().execution_control().checkpoint()?;
        match resolution {
            HydrationResolution::Available(_) => Ok(HydrationAuthorization::Authorized),
            HydrationResolution::Unavailable(state) => {
                Ok(HydrationAuthorization::Denied(HydrationDenial::new(state)?))
            }
        }
    }

    async fn read_after_recheck(
        &self,
        snapshot: &TemporalExecutionSnapshot,
        anchor_id: &RetrievalAnchorId,
        max_bytes: usize,
        max_chunk_bytes: usize,
        emit: &mut (dyn FnMut(&[u8]) -> Result<(), HydrationError> + Send),
    ) -> Result<(), HydrationError> {
        let control = snapshot.request().execution_control();
        control.checkpoint()?;
        let descriptor = match self.backend.resolve_current(snapshot, anchor_id).await? {
            HydrationResolution::Available(descriptor) => descriptor,
            HydrationResolution::Unavailable(_) => return Err(HydrationError::Unavailable),
        };
        if descriptor.byte_count > max_bytes {
            return Err(HydrationError::BudgetExceeded {
                resource: "payload bytes",
            });
        }
        control.checkpoint()?;
        let bytes = self
            .backend
            .read_bounded(&descriptor, max_bytes, control)
            .await?;
        if bytes.len() != descriptor.byte_count
            || !content_hash_matches(&descriptor.content_hash, &bytes)
        {
            return Err(HydrationError::Unavailable);
        }
        if max_chunk_bytes == 0 && !bytes.is_empty() {
            return Err(HydrationError::BudgetExceeded {
                resource: "chunk bytes",
            });
        }
        for chunk in bytes.chunks(max_chunk_bytes.max(1)) {
            control.checkpoint()?;
            emit(chunk)?;
        }
        Ok(control.checkpoint()?)
    }
}

impl<B: TemporalHydrationBackend> TemporalHydrationPort for SessionTemporalHydrationAdapter<B> {
    fn authorize_hydration<'a>(
        &'a self,
        snapshot: &'a TemporalExecutionSnapshot,
        anchor_id: &'a RetrievalAnchorId,
    ) -> HydrationFuture<'a, HydrationAuthorization> {
        Box::pin(self.authorize(snapshot, anchor_id))
    }

    fn read_authorized<'a>(
        &'a self,
        grant: &'a HydrationGrant<'_>,
        sink: &'a mut HydrationSink<'_>,
    ) -> HydrationFuture<'a, ()> {
        Box::pin(async move {
            self.read_after_recheck(
                grant.snapshot(),
                grant.anchor_id(),
                grant.max_bytes(),
                grant.max_chunk_bytes(),
                &mut |chunk| sink.write_chunk(chunk),
            )
            .await
        })
    }
}

pub struct GlobalDbHydrationBackend<'snapshot> {
    read: TemporalSqlRead<'snapshot>,
    storage_root: &'snapshot Path,
}

impl<'snapshot> GlobalDbHydrationBackend<'snapshot> {
    pub const fn new_registered(
        read: &'snapshot engine::ReadSnapshot,
        storage_root: &'snapshot Path,
    ) -> Self {
        Self {
            read: TemporalSqlRead::registered(read),
            storage_root,
        }
    }
}

pub type GlobalDbTemporalHydrationPort<'snapshot> =
    SessionTemporalHydrationAdapter<GlobalDbHydrationBackend<'snapshot>>;

impl<'snapshot> SessionTemporalHydrationAdapter<GlobalDbHydrationBackend<'snapshot>> {
    pub const fn for_registered_snapshot(
        read: &'snapshot engine::ReadSnapshot,
        storage_root: &'snapshot Path,
    ) -> Self {
        Self::new(GlobalDbHydrationBackend::new_registered(read, storage_root))
    }
}

pub(super) async fn session_message_from_hydrated_bytes(
    read: &TemporalSqlRead<'_>,
    snapshot: &TemporalExecutionSnapshot,
    anchor_id: &RetrievalAnchorId,
    expected_provider: &str,
    expected_session: &str,
    bytes: &[u8],
) -> Result<SessionMessageRecord, HydrationError> {
    let text = String::from_utf8(bytes.to_vec()).map_err(|_| HydrationError::Unavailable)?;

    let generation =
        i64::try_from(snapshot.watermarks().generation).map_err(|_| HydrationError::Unavailable)?;
    let project_key = snapshot
        .request()
        .authorized_root()
        .ok_or(HydrationError::Unavailable)?
        .project_key();
    let mut rows = match snapshot.retrieval_scope() {
        TemporalRetrievalScope::Session(session_id) => {
            if session_id.as_str() != expected_session {
                return Err(HydrationError::Unavailable);
            }
            read.query(
                "SELECT occurrence.message_id, occurrence.role,
                        occurrence.projection_output_ordinal,
                        source.provider, occurrence.session_id,
                        message.timestamp, message.kind, message.model,
                        message.tool_names, message.source_path, message.source_offset,
                        message.metadata_json, message.role, message.session_id
                 FROM session_occurrences AS occurrence
                 JOIN sessions AS source
                   ON source.session_id = occurrence.session_id
                 LEFT JOIN session_messages AS message
                   ON message.provider = source.provider
                  AND message.message_id = occurrence.message_id
                  AND message.session_id = occurrence.session_id
                 WHERE occurrence.session_id = ?1
                   AND occurrence.generation = ?2
                   AND occurrence.retrieval_anchor_id = ?3
                   AND source.project_key = ?4
                   AND source.provider = ?5
                 ORDER BY occurrence.occurrence_id
                 LIMIT 2",
                params![
                    session_id.as_str(),
                    generation,
                    anchor_id.as_str(),
                    project_key,
                    expected_provider
                ],
            )
            .await
        }
        TemporalRetrievalScope::AllSessionsInAuthorizedRoot => {
            read.query(
                "SELECT occurrence.message_id, occurrence.role,
                        occurrence.projection_output_ordinal,
                        source.provider, occurrence.session_id,
                        message.timestamp, message.kind, message.model,
                        message.tool_names, message.source_path, message.source_offset,
                        message.metadata_json, message.role, message.session_id
                 FROM session_occurrences AS occurrence
                 JOIN session_temporal_generations AS generation
                   ON generation.session_id = occurrence.session_id
                  AND generation.generation = occurrence.generation
                  AND generation.state = 'active'
                 JOIN sessions AS source
                   ON source.session_id = occurrence.session_id
                 LEFT JOIN session_messages AS message
                   ON message.provider = source.provider
                  AND message.message_id = occurrence.message_id
                  AND message.session_id = occurrence.session_id
                 WHERE occurrence.retrieval_anchor_id = ?1
                   AND source.project_key = ?2
                   AND occurrence.session_id = ?3
                   AND source.provider = ?4
                 ORDER BY occurrence.session_id, occurrence.occurrence_id
                 LIMIT 2",
                params![
                    anchor_id.as_str(),
                    project_key,
                    expected_session,
                    expected_provider
                ],
            )
            .await
        }
    }
    .map_err(|_| HydrationError::Unavailable)?;
    let row = rows
        .next()
        .await
        .map_err(|_| HydrationError::Unavailable)?
        .ok_or(HydrationError::Unavailable)?;
    let message_id: String = row.get(0).map_err(|_| HydrationError::Unavailable)?;
    let role: String = row.get(1).map_err(|_| HydrationError::Unavailable)?;
    let ordinal: i64 = row.get(2).map_err(|_| HydrationError::Unavailable)?;
    let provider: String = row.get(3).map_err(|_| HydrationError::Unavailable)?;
    let session_id: String = row.get(4).map_err(|_| HydrationError::Unavailable)?;
    let timestamp = row.get(5).ok();
    let kind = row.get(6).ok().or_else(|| Some("message".to_string()));
    let model = row.get(7).ok();
    let tool_names = row.get(8).ok();
    let source_path = row.get(9).ok();
    let source_offset = row.get(10).ok();
    let metadata_json = row.get(11).ok();
    let compatibility_role: Option<String> = row.get(12).ok();
    let compatibility_session: Option<String> = row.get(13).ok();
    if compatibility_role
        .as_deref()
        .is_some_and(|compatibility_role| compatibility_role != role)
        || compatibility_session
            .as_deref()
            .is_some_and(|compatibility_session| compatibility_session != session_id)
        || rows
            .next()
            .await
            .map_err(|_| HydrationError::Unavailable)?
            .is_some()
    {
        return Err(HydrationError::Unavailable);
    }
    Ok(SessionMessageRecord {
        provider,
        message_id,
        session_id,
        role,
        timestamp,
        ordinal,
        text,
        kind,
        model,
        tool_names,
        source_path,
        source_offset,
        metadata_json,
    })
}

fn canonical_projected_message(
    observation: &DurableObservationV1,
    message_id: &str,
    output_ordinal: i64,
) -> Option<SessionMessageRecord> {
    let output_ordinal = u32::try_from(output_ordinal).ok()?;
    let projection = derive_projection(observation).ok()?;
    projection
        .messages()
        .find(|output| {
            output.output_ordinal() == output_ordinal
                && output.message().provider == observation.source().provider().as_str()
                && output.message().session_id == observation.source().session_id().as_str()
                && output.message().message_id == message_id
        })
        .map(|output| output.message().clone())
}

impl TemporalHydrationBackend for GlobalDbHydrationBackend<'_> {
    fn resolve_current<'a>(
        &'a self,
        snapshot: &'a TemporalExecutionSnapshot,
        anchor_id: &'a RetrievalAnchorId,
    ) -> BackendFuture<'a, HydrationResolution> {
        Box::pin(async move {
            let control = snapshot.request().execution_control();
            control.checkpoint()?;
            let resolution = resolve_current(&self.read, snapshot, anchor_id)
                .await
                .unwrap_or(HydrationResolution::Unavailable(
                    HydrationStateV1::RetainedButUnavailable,
                ));
            control.checkpoint()?;
            Ok(resolution)
        })
    }

    fn read_bounded<'a>(
        &'a self,
        descriptor: &'a PayloadDescriptor,
        max_bytes: usize,
        control: &'a ExecutionControl,
    ) -> BackendFuture<'a, Zeroizing<Vec<u8>>> {
        Box::pin(async move {
            control.checkpoint()?;
            match &descriptor.source {
                PayloadSource::Occurrence { content } => {
                    bounded_copy(content.as_slice(), max_bytes, control)
                }
                PayloadSource::Summary {
                    session_id,
                    summary_id,
                } => {
                    let mut rows = self
                        .read
                        .query(
                            "SELECT summary_text
                             FROM session_summary_nodes
                             WHERE session_id = ?1 AND summary_id = ?2
                               AND length(CAST(summary_text AS BLOB)) <= ?3",
                            params![
                                session_id.as_str(),
                                summary_id.as_str(),
                                i64::try_from(max_bytes).unwrap_or(i64::MAX)
                            ],
                        )
                        .await
                        .map_err(|_| HydrationError::Unavailable)?;
                    let row = rows
                        .next()
                        .await
                        .map_err(|_| HydrationError::Unavailable)?
                        .ok_or(HydrationError::Unavailable)?;
                    let content = Zeroizing::new(
                        row.get::<String>(0)
                            .map_err(|_| HydrationError::Unavailable)?,
                    );
                    bounded_copy(content.as_bytes(), max_bytes, control)
                }
                PayloadSource::External {
                    provider,
                    session_id,
                    payload_ref,
                    char_count,
                } => {
                    let content = read_verified_payload_content(
                        self.storage_root,
                        payload_ref,
                        &descriptor.content_hash,
                        descriptor.byte_count,
                        *char_count,
                    )
                    .map_err(|_| HydrationError::Unavailable)?;
                    let _ = (provider, session_id);
                    bounded_copy(content.as_bytes(), max_bytes, control)
                }
            }
        })
    }
}

async fn resolve_current(
    conn: &TemporalSqlRead<'_>,
    snapshot: &TemporalExecutionSnapshot,
    requested_anchor: &RetrievalAnchorId,
) -> Result<HydrationResolution, ()> {
    let mut anchor_rows = conn
        .query(
            "SELECT anchor_json, owner_json
             FROM retrieval_anchors WHERE anchor_id = ?1",
            params![requested_anchor.as_str()],
        )
        .await
        .map_err(|_| ())?;
    let Some(anchor_row) = anchor_rows.next().await.map_err(|_| ())? else {
        return Ok(HydrationResolution::Unavailable(
            HydrationStateV1::Unauthorized,
        ));
    };
    let anchor_json: String = anchor_row.get(0).map_err(|_| ())?;
    let owner_json: String = anchor_row.get(1).map_err(|_| ())?;
    let anchor: RetrievalAnchorRecord = match serde_json::from_str(&anchor_json) {
        Ok(anchor) => anchor,
        Err(_) => {
            return Ok(HydrationResolution::Unavailable(
                HydrationStateV1::UnverifiableLegacy,
            ));
        }
    };
    if anchor.validate().is_err()
        || anchor.anchor_id() != requested_anchor
        || serde_json::to_string(anchor.owner()).ok().as_deref() != Some(owner_json.as_str())
    {
        return Ok(HydrationResolution::Unavailable(
            HydrationStateV1::UnverifiableLegacy,
        ));
    }
    if anchor.authorization().validate().is_err()
        || anchor.authorization().access_policy_digest.as_str()
            != snapshot.request().access_digest().as_str()
    {
        return Ok(HydrationResolution::Unavailable(
            HydrationStateV1::Unauthorized,
        ));
    }
    if matches!(
        snapshot.retrieval_scope(),
        TemporalRetrievalScope::AllSessionsInAuthorizedRoot
    ) && authorized_root_owner(snapshot).as_ref() != Some(anchor.owner())
    {
        return Ok(HydrationResolution::Unavailable(
            HydrationStateV1::Unauthorized,
        ));
    }
    if let Some(state) = classify_current_access(
        anchor.payload_access(),
        anchor.durability(),
        now_micros().0,
        true,
        true,
    ) {
        return Ok(HydrationResolution::Unavailable(state));
    }

    if let Some(resolution) =
        resolve_occurrence(conn, snapshot, requested_anchor, &anchor, &owner_json).await?
    {
        return Ok(resolution);
    }
    if let Some(resolution) =
        resolve_summary(conn, snapshot, requested_anchor, &anchor, &owner_json).await?
    {
        return Ok(resolution);
    }
    Ok(HydrationResolution::Unavailable(
        HydrationStateV1::UnverifiableLegacy,
    ))
}

async fn resolve_occurrence(
    conn: &TemporalSqlRead<'_>,
    snapshot: &TemporalExecutionSnapshot,
    anchor_id: &RetrievalAnchorId,
    anchor: &RetrievalAnchorRecord,
    owner_json: &str,
) -> Result<Option<HydrationResolution>, ()> {
    let generation = i64::try_from(snapshot.watermarks().generation).map_err(|_| ())?;
    let mut rows = match snapshot.retrieval_scope() {
        TemporalRetrievalScope::Session(session_id) => {
            conn.query(
                "SELECT occurrence.session_id, COALESCE(occurrence.message_id, ''),
                        occurrence.projection_output_ordinal,
                        observation.observation_id, observation.observation_json
                 FROM session_occurrences occurrence
                 JOIN observations observation
                   ON observation.observation_id = occurrence.source_observation_id
                 WHERE occurrence.session_id = ?1
                   AND occurrence.generation = ?2
                   AND occurrence.retrieval_anchor_id = ?3
                 ORDER BY occurrence.occurrence_id
                 LIMIT 2",
                params![session_id.as_str(), generation, anchor_id.as_str()],
            )
            .await
        }
        TemporalRetrievalScope::AllSessionsInAuthorizedRoot => {
            let project_key = snapshot
                .request()
                .authorized_root()
                .ok_or(())?
                .project_key();
            conn.query(
                "SELECT occurrence.session_id, COALESCE(occurrence.message_id, ''),
                        occurrence.projection_output_ordinal,
                        observation.observation_id, observation.observation_json
                 FROM session_occurrences occurrence
                 JOIN session_temporal_generations generation
                   ON generation.session_id = occurrence.session_id
                  AND generation.generation = occurrence.generation
                  AND generation.state = 'active'
                 JOIN observations observation
                   ON observation.observation_id = occurrence.source_observation_id
                 JOIN sessions authority_session
                   ON authority_session.session_id = occurrence.session_id
                  AND authority_session.provider = COALESCE(
                      json_extract(observation.observation_json, '$.identity.source.provider'),
                      'claude'
                  )
                  AND authority_session.project_key = ?2
                 WHERE occurrence.retrieval_anchor_id = ?1
                   AND (?3 IS NULL OR authority_session.provider = ?3)
                 ORDER BY occurrence.session_id, occurrence.occurrence_id
                 LIMIT 2",
                params![anchor_id.as_str(), project_key, snapshot.provider_scope()],
            )
            .await
        }
    }
    .map_err(|_| ())?;
    let Some(row) = rows.next().await.map_err(|_| ())? else {
        return Ok(None);
    };
    let session_id: String = row.get(0).map_err(|_| ())?;
    let message_id: String = row.get(1).map_err(|_| ())?;
    let projection_output_ordinal: i64 = row.get(2).map_err(|_| ())?;
    let stored_observation_id: String = row.get(3).map_err(|_| ())?;
    let observation_json: String = row.get(4).map_err(|_| ())?;
    if rows.next().await.map_err(|_| ())?.is_some() {
        return Ok(Some(HydrationResolution::Unavailable(
            HydrationStateV1::RetainedButUnavailable,
        )));
    }
    if message_id.is_empty() {
        return Ok(Some(HydrationResolution::Unavailable(
            HydrationStateV1::RetainedButUnavailable,
        )));
    }
    let observation: DurableObservationV1 = match serde_json::from_str(&observation_json) {
        Ok(observation) => observation,
        Err(_) => {
            return Ok(Some(HydrationResolution::Unavailable(
                HydrationStateV1::UnverifiableLegacy,
            )));
        }
    };
    if observation.observation_id().as_str() != stored_observation_id {
        return Ok(Some(HydrationResolution::Unavailable(
            HydrationStateV1::UnverifiableLegacy,
        )));
    }
    let provider = observation.source().provider().as_str();
    if let Some(state) = participant_access_state(snapshot, &session_id, provider) {
        return Ok(Some(HydrationResolution::Unavailable(state)));
    }
    let expected_owner = session_owner(conn, provider, &session_id).await?;
    let provider_matches = snapshot
        .provider_scope()
        .is_none_or(|expected| expected == provider);
    if let Some(state) = classify_current_access(
        anchor.payload_access(),
        anchor.durability(),
        now_micros().0,
        anchor.owner() == observation.scope()
            && serde_json::to_string(observation.scope()).ok().as_deref() == Some(owner_json),
        provider_matches
            && observation.source().session_id().as_str() == session_id
            && observation.source().provider().as_str() == provider
            && expected_owner.as_ref() == Some(observation.scope()),
    ) {
        return Ok(Some(HydrationResolution::Unavailable(state)));
    }

    let envelope: CanonicalObservationEnvelopeV1 =
        match serde_json::from_value(observation.payload().clone()) {
            Ok(envelope) => envelope,
            Err(_) => {
                return Ok(Some(HydrationResolution::Unavailable(
                    HydrationStateV1::UnverifiableLegacy,
                )));
            }
        };
    // Bind the occurrence's message_id to a canonical projection output of this
    // digest-verified observation. `derive_canonical_projection` derives the
    // message id from the tool-dispatch identity, else the envelope's
    // relations.message_id, else its stable record id — so an occurrence whose
    // id legitimately falls back to the stable record id has no
    // relations.message_id at all. Verifying the binding through the projection
    // (rather than the single relations.message_id field) accepts every message
    // the projection legitimately produced while still refusing an occurrence
    // whose message_id does not correspond to this envelope. The relations
    // fast-path preserves prior acceptance for messages the envelope names
    // directly even when the projection re-keys them (derived/tool outputs).
    let canonical_message =
        canonical_projected_message(&observation, &message_id, projection_output_ordinal);
    let relations_bind = envelope
        .relations()
        .message_id()
        .is_some_and(|candidate| candidate.as_str() == message_id);
    if !relations_bind && canonical_message.is_none() {
        return Ok(Some(HydrationResolution::Unavailable(
            HydrationStateV1::UnverifiableLegacy,
        )));
    }
    if let Some(message) = canonical_message
        .as_ref()
        .filter(|message| !message.text.is_empty())
    {
        let content = Zeroizing::new(message.text.as_bytes().to_vec());
        let content_hash = sha256_hex(content.as_slice());
        return Ok(Some(HydrationResolution::Available(PayloadDescriptor {
            byte_count: content.len(),
            source: PayloadSource::Occurrence { content },
            content_hash,
        })));
    }
    let mut raw_rows = conn
        .query(
            "SELECT storage_kind, CAST(COALESCE(content, '') AS TEXT), content_hash,
                    CAST(COALESCE(payload_ref, '') AS TEXT)
             FROM lcm_raw_messages
             WHERE provider = ?1 AND session_id = ?2 AND message_id = ?3
             ORDER BY store_id
             LIMIT 2",
            params![provider, session_id.as_str(), message_id.as_str()],
        )
        .await
        .map_err(|_| ())?;
    let raw_payload = match raw_rows.next().await.map_err(|_| ())? {
        Some(row) => {
            let storage_kind: String = row.get(0).map_err(|_| ())?;
            let content: String = row.get(1).map_err(|_| ())?;
            let content_hash: String = row.get(2).map_err(|_| ())?;
            let payload_ref: String = row.get(3).map_err(|_| ())?;
            if raw_rows.next().await.map_err(|_| ())?.is_some() {
                return Ok(Some(HydrationResolution::Unavailable(
                    HydrationStateV1::RetainedButUnavailable,
                )));
            }
            Some((storage_kind, content, content_hash, payload_ref))
        }
        None => None,
    };
    if let Some((storage_kind, content, content_hash, payload_ref)) = raw_payload {
        match storage_kind.as_str() {
            "inline" => {
                let content = Zeroizing::new(content.into_bytes());
                if content_hash_matches(&content_hash, content.as_slice()) {
                    return Ok(Some(HydrationResolution::Available(PayloadDescriptor {
                        byte_count: content.len(),
                        source: PayloadSource::Occurrence { content },
                        content_hash,
                    })));
                }
            }
            "external" if !payload_ref.is_empty() => {
                let resolution = resolve_external_manifest(
                    conn,
                    provider,
                    &session_id,
                    &message_id,
                    &payload_ref,
                    &content_hash,
                )
                .await?;
                if matches!(resolution, HydrationResolution::Available(_)) {
                    return Ok(Some(resolution));
                }
            }
            _ => {}
        }
    }
    if let Some(message) = canonical_message {
        let content = Zeroizing::new(message.text.into_bytes());
        let content_hash = sha256_hex(content.as_slice());
        return Ok(Some(HydrationResolution::Available(PayloadDescriptor {
            byte_count: content.len(),
            source: PayloadSource::Occurrence { content },
            content_hash,
        })));
    }
    Ok(Some(HydrationResolution::Unavailable(
        HydrationStateV1::RetainedButUnavailable,
    )))
}

async fn resolve_summary(
    conn: &TemporalSqlRead<'_>,
    snapshot: &TemporalExecutionSnapshot,
    anchor_id: &RetrievalAnchorId,
    anchor: &RetrievalAnchorRecord,
    owner_json: &str,
) -> Result<Option<HydrationResolution>, ()> {
    let generation = i64::try_from(snapshot.watermarks().generation).map_err(|_| ())?;
    let mut rows = match snapshot.retrieval_scope() {
        TemporalRetrievalScope::Session(session_id) => {
            conn.query(
                "SELECT node.session_id, ?1, node.summary_id,
                        CAST(COALESCE(node.publication_json, '') AS TEXT),
                        COALESCE(length(CAST(node.summary_text AS BLOB)), 0),
                        CAST(COALESCE(availability.availability, '') AS TEXT)
                 FROM session_summary_nodes node
                 LEFT JOIN session_summary_availability availability
                   ON availability.session_id = node.session_id
                  AND availability.generation = ?1
                  AND availability.summary_id = node.summary_id
                 WHERE node.session_id = ?2 AND node.summary_anchor_id = ?3
                 LIMIT 2",
                params![generation, session_id.as_str(), anchor_id.as_str()],
            )
            .await
        }
        TemporalRetrievalScope::AllSessionsInAuthorizedRoot => {
            let project_key = snapshot
                .request()
                .authorized_root()
                .ok_or(())?
                .project_key();
            conn.query(
                "SELECT node.session_id, generation.generation,
                        node.summary_id,
                        CAST(COALESCE(node.publication_json, '') AS TEXT),
                        COALESCE(length(CAST(node.summary_text AS BLOB)), 0),
                        CAST(COALESCE(availability.availability, '') AS TEXT)
                 FROM session_summary_nodes node
                 JOIN session_temporal_generations generation
                   ON generation.session_id = node.session_id
                  AND generation.state = 'active'
                 JOIN sessions authority_session
                   ON authority_session.session_id = node.session_id
                  AND authority_session.provider =
                      json_extract(node.publication_json, '$.provider')
                  AND authority_session.project_key = ?2
                 LEFT JOIN session_summary_availability availability
                   ON availability.session_id = node.session_id
                  AND availability.generation = generation.generation
                  AND availability.summary_id = node.summary_id
                 WHERE node.summary_anchor_id = ?1
                   AND (?3 IS NULL OR authority_session.provider = ?3)
                 ORDER BY node.session_id, node.summary_id
                 LIMIT 2",
                params![anchor_id.as_str(), project_key, snapshot.provider_scope()],
            )
            .await
        }
    }
    .map_err(|_| ())?;
    let Some(row) = rows.next().await.map_err(|_| ())? else {
        return Ok(None);
    };
    // Copy the selected values before advancing for the uniqueness probe.
    let session_id: String = row.get(0).map_err(|_| ())?;
    let generation: i64 = row.get(1).map_err(|_| ())?;
    let summary_id: String = row.get(2).map_err(|_| ())?;
    let publication_json: String = row.get(3).map_err(|_| ())?;
    let summary_bytes = row.get::<i64>(4).map_err(|_| ())?;
    let availability: String = row.get(5).map_err(|_| ())?;
    if rows.next().await.map_err(|_| ())?.is_some() {
        return Ok(Some(HydrationResolution::Unavailable(
            HydrationStateV1::RetainedButUnavailable,
        )));
    }
    if publication_json.is_empty() {
        return Ok(Some(HydrationResolution::Unavailable(
            HydrationStateV1::UnverifiableLegacy,
        )));
    }
    let manifest: CanonicalPublicationManifest = match serde_json::from_str(&publication_json) {
        Ok(manifest) => manifest,
        Err(_) => {
            return Ok(Some(HydrationResolution::Unavailable(
                HydrationStateV1::UnverifiableLegacy,
            )));
        }
    };
    if let Some(state) = participant_access_state(snapshot, &session_id, manifest.provider.as_str())
    {
        return Ok(Some(HydrationResolution::Unavailable(state)));
    }
    let expected_owner = session_owner(conn, &manifest.provider, &session_id).await?;
    let owner_matches = expected_owner.as_ref() == Some(anchor.owner())
        && serde_json::to_string(anchor.owner()).ok().as_deref() == Some(owner_json);
    let provider_matches = match snapshot.provider_scope() {
        Some(provider) => {
            summary_has_provider_evidence(conn, &session_id, generation, &summary_id, provider)
                .await?
        }
        None => true,
    };
    let session_matches = manifest.session_id == session_id
        && manifest.summary_anchor_id == anchor_id.as_str()
        && provider_matches;
    if let Some(state) = classify_current_access(
        anchor.payload_access(),
        anchor.durability(),
        now_micros().0,
        owner_matches,
        session_matches,
    ) {
        return Ok(Some(HydrationResolution::Unavailable(state)));
    }
    if availability != "available" {
        return Ok(Some(HydrationResolution::Unavailable(
            HydrationStateV1::RetainedButUnavailable,
        )));
    }
    Ok(Some(HydrationResolution::Available(PayloadDescriptor {
        source: PayloadSource::Summary {
            session_id,
            summary_id,
        },
        byte_count: nonnegative_usize(Some(summary_bytes))?,
        content_hash: manifest.summary_hash,
    })))
}

async fn summary_has_provider_evidence(
    conn: &TemporalSqlRead<'_>,
    session_id: &str,
    generation: i64,
    summary_id: &str,
    provider: &str,
) -> Result<bool, ()> {
    let mut rows = conn
        .query(
            "WITH RECURSIVE retained_sources(
                 source_anchor_id, source_summary_id, depth
             ) AS (
                 SELECT source_anchor_id, source_summary_id, 0
                 FROM session_summary_sources
                 WHERE summary_id = ?1
                 UNION ALL
                 SELECT nested.source_anchor_id, nested.source_summary_id,
                        retained.depth + 1
                 FROM retained_sources AS retained
                 JOIN session_summary_nodes AS retained_summary
                   ON retained_summary.summary_id = retained.source_summary_id
                  AND retained_summary.session_id = ?2
                 JOIN session_summary_sources AS nested
                   ON nested.summary_id = retained_summary.summary_id
                 WHERE retained.depth < 63
                 LIMIT 257
             )
             SELECT EXISTS (
                 SELECT 1
                 FROM retained_sources AS retained
                 JOIN session_occurrences AS source_occurrence
                   ON source_occurrence.retrieval_anchor_id =
                      retained.source_anchor_id
                  AND source_occurrence.session_id = ?2
                  AND source_occurrence.generation = ?3
                 JOIN observations AS source_observation
                   ON source_observation.observation_id =
                      source_occurrence.source_observation_id
                 WHERE json_extract(
                     source_observation.observation_json,
                     '$.identity.source.provider'
                 ) = ?4
                 LIMIT 1
             )",
            params![summary_id, session_id, generation, provider],
        )
        .await
        .map_err(|_| ())?;
    let row = rows.next().await.map_err(|_| ())?.ok_or(())?;
    row.get::<i64>(0).map(|value| value == 1).map_err(|_| ())
}

#[derive(Deserialize)]
struct ExternalManifest {
    provider: String,
    session_id: String,
    message_id: String,
    byte_count: i64,
    char_count: i64,
}

async fn resolve_external_manifest(
    conn: &TemporalSqlRead<'_>,
    provider: &str,
    session_id: &str,
    message_id: &str,
    payload_ref: &str,
    content_hash: &str,
) -> Result<HydrationResolution, ()> {
    if validate_payload_ref(payload_ref).is_err() {
        return Ok(HydrationResolution::Unavailable(
            HydrationStateV1::UnverifiableLegacy,
        ));
    }
    let mut rows = conn
        .query(
            "SELECT external.content_hash, external.byte_count, external.char_count,
                    manifest.payload_digest, manifest.manifest_json
             FROM lcm_external_payloads external
             JOIN session_external_payload_manifests manifest
               ON manifest.payload_ref = external.payload_ref
              AND manifest.session_id = external.session_id
             JOIN session_summary_nodes summary
               ON summary.session_id = manifest.session_id
             JOIN json_each(summary.publication_json, '$.payloads') payload
               ON json_extract(payload.value, '$.payload_ref') = manifest.payload_ref
              AND json_extract(payload.value, '$.digest') = manifest.payload_digest
              AND json_extract(payload.value, '$.manifest_json') = manifest.manifest_json
             JOIN sanitization_receipts receipt
               ON receipt.receipt_id = manifest.receipt_id
              AND receipt.receipt_id = json_extract(summary.publication_json, '$.receipt_id')
             WHERE external.payload_ref = ?1
               AND external.provider = ?2
               AND external.session_id = ?3
               AND external.message_id = ?4
             LIMIT 2",
            params![payload_ref, provider, session_id, message_id],
        )
        .await
        .map_err(|_| ())?;
    let Some(row) = rows.next().await.map_err(|_| ())? else {
        return Ok(HydrationResolution::Unavailable(HydrationStateV1::Deleted));
    };
    // Copy the selected values before advancing for the uniqueness probe.
    let stored_hash: String = row.get(0).map_err(|_| ())?;
    let byte_count = nonnegative_usize(row.get::<Option<i64>>(1).map_err(|_| ())?)?;
    let char_count = nonnegative_usize(row.get::<Option<i64>>(2).map_err(|_| ())?)?;
    let manifest_digest: String = row.get(3).map_err(|_| ())?;
    let manifest_json: String = row.get(4).map_err(|_| ())?;
    if rows.next().await.map_err(|_| ())?.is_some() {
        return Ok(HydrationResolution::Unavailable(
            HydrationStateV1::RetainedButUnavailable,
        ));
    }
    let manifest: ExternalManifest = match serde_json::from_str(&manifest_json) {
        Ok(manifest) => manifest,
        Err(_) => {
            return Ok(HydrationResolution::Unavailable(
                HydrationStateV1::UnverifiableLegacy,
            ));
        }
    };
    if stored_hash != content_hash
        || manifest_digest != content_hash
        || manifest.provider != provider
        || manifest.session_id != session_id
        || manifest.message_id != message_id
        || manifest.byte_count != i64::try_from(byte_count).map_err(|_| ())?
        || manifest.char_count != i64::try_from(char_count).map_err(|_| ())?
    {
        return Ok(HydrationResolution::Unavailable(
            HydrationStateV1::UnverifiableLegacy,
        ));
    }
    Ok(HydrationResolution::Available(PayloadDescriptor {
        source: PayloadSource::External {
            provider: provider.to_string(),
            session_id: session_id.to_string(),
            payload_ref: payload_ref.to_string(),
            char_count,
        },
        byte_count,
        content_hash: stored_hash,
    }))
}

fn authorized_root_owner(snapshot: &TemporalExecutionSnapshot) -> Option<ObservationScopeV1> {
    snapshot
        .request()
        .authorized_root()
        .and_then(|root| owner_from_project_key(root.project_key().to_string()))
}

async fn session_owner(
    conn: &TemporalSqlRead<'_>,
    provider: &str,
    session_id: &str,
) -> Result<Option<ObservationScopeV1>, ()> {
    let mut rows = conn
        .query(
            "SELECT project_key
             FROM sessions
             WHERE provider = ?1 AND session_id = ?2
             LIMIT 2",
            params![provider, session_id],
        )
        .await
        .map_err(|_| ())?;
    let Some(row) = rows.next().await.map_err(|_| ())? else {
        return Ok(None);
    };
    let project_key: String = row.get(0).map_err(|_| ())?;
    if rows.next().await.map_err(|_| ())?.is_some() {
        return Ok(None);
    }
    Ok(owner_from_project_key(project_key))
}

fn owner_from_project_key(project_key: String) -> Option<ObservationScopeV1> {
    if project_key == "user" {
        return Some(ObservationScopeV1::Profile);
    }
    ProjectId::new(project_key)
        .ok()
        .map(|project_id| ObservationScopeV1::Project { project_id })
}

fn participant_access_state(
    snapshot: &TemporalExecutionSnapshot,
    session_id: &str,
    source_id: &str,
) -> Option<HydrationStateV1> {
    if !snapshot.has_authoritative_participant_manifest() {
        return None;
    }
    let Some(participant) = snapshot
        .participant_manifest()
        .entries()
        .iter()
        .find(|participant| {
            participant.session_id().as_str() == session_id && participant.source_id() == source_id
        })
    else {
        return Some(HydrationStateV1::Unauthorized);
    };
    if !participant.is_authorized_for_snapshot() {
        return Some(HydrationStateV1::Unauthorized);
    }
    source_access_hydration_state(participant.access())
}

fn source_access_hydration_state(access: TemporalSourceAccess) -> Option<HydrationStateV1> {
    match access {
        TemporalSourceAccess::Available => None,
        TemporalSourceAccess::Unavailable => Some(HydrationStateV1::RetainedButUnavailable),
        TemporalSourceAccess::Locked => Some(HydrationStateV1::Locked),
        TemporalSourceAccess::RetentionWithheld => Some(HydrationStateV1::RetentionExpired),
        TemporalSourceAccess::Deleted => Some(HydrationStateV1::Deleted),
        TemporalSourceAccess::Redacted => Some(HydrationStateV1::Redacted),
        TemporalSourceAccess::LegacyUnauthorized => Some(HydrationStateV1::Unauthorized),
    }
}

fn classify_current_access(
    payload_access: PayloadAccessState,
    durability: &AnchorDurabilityClass,
    now: i64,
    owner_matches: bool,
    session_matches: bool,
) -> Option<HydrationStateV1> {
    if !owner_matches || !session_matches {
        return Some(HydrationStateV1::Unauthorized);
    }
    let state = match payload_access {
        PayloadAccessState::Eligible => None,
        PayloadAccessState::Redacted => Some(HydrationStateV1::Redacted),
        PayloadAccessState::Quarantined => Some(HydrationStateV1::Locked),
        PayloadAccessState::RetentionExpired => Some(HydrationStateV1::RetentionExpired),
        PayloadAccessState::Deleted => Some(HydrationStateV1::Deleted),
        PayloadAccessState::Unavailable | PayloadAccessState::Ambiguous => {
            Some(HydrationStateV1::RetainedButUnavailable)
        }
    };
    if state.is_some() {
        return state;
    }
    match durability {
        AnchorDurabilityClass::RetentionBound { expires_at } if expires_at.0 <= now => {
            Some(HydrationStateV1::RetentionExpired)
        }
        _ => None,
    }
}

fn bounded_copy(
    bytes: &[u8],
    max_bytes: usize,
    control: &ExecutionControl,
) -> Result<Zeroizing<Vec<u8>>, HydrationError> {
    if bytes.len() > max_bytes {
        return Err(HydrationError::BudgetExceeded {
            resource: "payload bytes",
        });
    }
    control.checkpoint()?;
    let mut copy = Zeroizing::new(Vec::with_capacity(bytes.len()));
    for chunk in bytes.chunks(64 * 1024) {
        control.checkpoint()?;
        copy.extend_from_slice(chunk);
    }
    control.checkpoint()?;
    Ok(copy)
}

fn nonnegative_usize(value: Option<i64>) -> Result<usize, ()> {
    value
        .and_then(|value| usize::try_from(value).ok())
        .ok_or(())
}

fn content_hash_matches(expected: &str, bytes: &[u8]) -> bool {
    expected.strip_prefix("sha256:").unwrap_or(expected) == sha256_hex(bytes)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut actual = String::with_capacity(64);
    for byte in digest {
        let _ = write!(&mut actual, "{byte:02x}");
    }
    actual
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::future::Future;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::task::{Context, Poll, Wake, Waker};
    use std::thread;
    use std::time::{Duration, Instant};

    use serde_json::{Value, json};
    use tempfile::tempdir;
    use tracedecay_domain::{
        CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1, CanonicalObservationEvidenceV1,
        CanonicalObservationFactV1, CanonicalObservationRelationsV1, DurableObservationV1,
        ObservationId, ObservationIdentityMaterialV1, ObservationOrderingDomainV1,
        ObservationScopeV1, ObservationSourceCursorV1, ObservationSourceGenerationV1,
        ObservationSourceIdentityV1, ObservationSourceRangeV1, PayloadReferenceV1,
        ProjectionGenerationId, ProviderId, RetentionClass, RetrievalGrainV1,
        SanitizationReceiptId, SanitizationReceiptRefV1, SanitizationReceiptV1,
        SanitizerDispositionV1, SensitivityV1, SessionId, TemporalModeV1, UtcMicros,
    };
    use tracedecay_runtime_core::db::engine::{Executor, ReadSnapshot, params};
    use tracedecay_store::{
        AnchoredObservationWrite, ObservationStore, ObservationWrite,
        build_observation_resolution_authorization_v1, build_observation_retrieval_anchor_v2,
    };

    use super::*;
    use crate::tests::harness::{HostAdmissionScope, HostAdmissionTestRuntimeV1};
    use tracedecay_query::temporal::ports::{
        BindingDigest, ExecutionLimits, KernelVersions, TemporalAuthorizedRoot, TemporalPortError,
        TemporalSnapshotRequest, TemporalWatermarks,
    };
    use tracedecay_query::temporal::resolution::ValidatedAuthorization;

    struct RegisteredHydrationRead {
        read: ReadSnapshot,
        storage_root: PathBuf,
    }

    #[derive(Debug, PartialEq, Eq)]
    struct HydrationStorageFingerprint {
        database: [u8; 32],
        payload: [u8; 32],
        payload_entries: usize,
    }

    impl RegisteredHydrationRead {
        fn adapter(&self) -> GlobalDbTemporalHydrationPort<'_> {
            GlobalDbTemporalHydrationPort::for_registered_snapshot(
                &self.read,
                self.storage_root.as_path(),
            )
        }
    }

    impl HostAdmissionTestRuntimeV1 {
        async fn hydration_read_for_test(&self) -> RegisteredHydrationRead {
            let database = self
                .registered_database(HostAdmissionScope::Profile)
                .expect("registered profile database");
            RegisteredHydrationRead {
                read: database.read_snapshot().await.expect("read snapshot"),
                storage_root: database
                    .db_path()
                    .parent()
                    .expect("registered profile storage root")
                    .to_path_buf(),
            }
        }

        async fn activate_temporal_generation_for_hydration_test(&self, session_id: &str) {
            let database = self
                .registered_database(HostAdmissionScope::Profile)
                .expect("registered profile database");
            let frozen = serde_json::json!({
                "active_generation": 1,
                "cursor_key": null,
                "projection_frontier": 0,
                "source_frontier": 0,
                "summary_frontier": 0
            })
            .to_string();
            let writer = database
                .writer_connection()
                .expect("registered profile writer");
            Executor::execute(
                &writer,
                "INSERT INTO session_temporal_generations (
                    session_id, generation, state, frozen_watermarks_json, created_at,
                    ready_at, activated_at, completed_at
                 ) VALUES (?1, 1, 'building', ?2, 1, NULL, NULL, NULL)",
                params![session_id, frozen],
            )
            .await
            .expect("building generation");
            Executor::execute(
                &writer,
                "UPDATE session_temporal_generations
                 SET state = 'ready', ready_at = 1
                 WHERE session_id = ?1 AND generation = 1",
                [session_id],
            )
            .await
            .expect("ready generation");
            Executor::execute(
                &writer,
                "UPDATE session_temporal_generations
                 SET state = 'active', activated_at = 1
                 WHERE session_id = ?1 AND generation = 1",
                [session_id],
            )
            .await
            .expect("active generation");
        }

        /// Seed a single-session occurrence whose `message_id` is supplied by
        /// the caller, so a reproduction can key the row on the stable record id
        /// exactly as `derive_canonical_projection` does when the envelope omits
        /// `relations.message_id`.
        async fn seed_session_occurrence_for_test(
            &self,
            provider: &str,
            session_id: &str,
            observation: &DurableObservationV1,
            anchor: &RetrievalAnchorRecord,
            message_id: &str,
            canonical_payload: &str,
        ) {
            let database = self
                .registered_database(HostAdmissionScope::Profile)
                .expect("registered profile database");
            let writer = database
                .writer_connection()
                .expect("registered profile writer");
            Executor::execute(
                &writer,
                "INSERT INTO sessions (
                    provider, session_id, project_key, project_path
                 ) VALUES (?1, ?2, 'user', '/session-occurrence-test')",
                params![provider, session_id],
            )
            .await
            .expect("session owner");
            self.activate_temporal_generation_for_hydration_test(session_id)
                .await;
            Executor::execute(
                &writer,
                "INSERT INTO session_occurrences (
                    session_id, generation, occurrence_id, source_observation_id,
                    projection_output_ordinal, retrieval_anchor_id, message_id,
                    role, knowledge_at, valid_time_json, evidence_json,
                    snippet_text, index_text
                 ) VALUES (
                    ?1, 1, 'occurrence-1', ?2, 0, ?3, ?4,
                    'assistant', 1, '{\"kind\":\"unknown\"}', '{}', ?5, ?5
                 )",
                params![
                    session_id,
                    observation.observation_id().as_str(),
                    anchor.anchor_id().as_str(),
                    message_id,
                    canonical_payload
                ],
            )
            .await
            .expect("occurrence");
        }

        /// Rewrite the seeded occurrence's `message_id` to a value that
        /// corresponds to no projection output of its observation, exercising the
        /// genuine `UnverifiableLegacy` refusal path.
        async fn corrupt_hydration_occurrence_message_id_for_test(
            &self,
            session_id: &str,
            message_id: &str,
        ) {
            let database = self
                .registered_database(HostAdmissionScope::Profile)
                .expect("registered profile database");
            Executor::execute(
                &database
                    .writer_connection()
                    .expect("registered profile writer"),
                "UPDATE session_occurrences
                 SET message_id = ?2
                 WHERE session_id = ?1 AND generation = 1 AND occurrence_id = 'occurrence-1'",
                params![session_id, message_id],
            )
            .await
            .expect("corrupt occurrence message id");
        }

        async fn seed_root_hydration_fixture_for_test(
            &self,
            provider: &str,
            observation: &DurableObservationV1,
            anchor: &RetrievalAnchorRecord,
            canonical_payload: &str,
            legacy_projection_poison: &str,
        ) {
            let database = self
                .registered_database(HostAdmissionScope::Profile)
                .expect("registered profile database");
            let writer = database
                .writer_connection()
                .expect("registered profile writer");
            Executor::execute(
                &writer,
                "INSERT INTO sessions (
                    provider, session_id, project_key, project_path
                 ) VALUES
                    (?1, 'session-1', 'user', '/root-hydration-test'),
                    (?1, 'session-2', 'user', '/root-hydration-test')",
                [provider],
            )
            .await
            .expect("session owner");
            self.activate_temporal_generation_for_hydration_test("session-2")
                .await;
            Executor::execute(
                &writer,
                "INSERT INTO lcm_raw_messages (
                    provider, message_id, session_id, role, ordinal, timestamp,
                    content, content_hash, storage_kind, payload_ref,
                    snippet_text, index_text, legacy_source, legacy_truncated
                 ) VALUES (
                    ?1, 'message-1', 'session-2', 'assistant', 1, 1,
                    ?2, ?3, 'inline', NULL, ?2, ?2, 0, 0
                 )",
                params![
                    provider,
                    legacy_projection_poison,
                    hash(legacy_projection_poison.as_bytes())
                ],
            )
            .await
            .expect("raw message");
            Executor::execute(
                &writer,
                "INSERT INTO session_occurrences (
                    session_id, generation, occurrence_id, source_observation_id,
                    projection_output_ordinal, retrieval_anchor_id, message_id,
                    role, knowledge_at, valid_time_json, evidence_json,
                    snippet_text, index_text
                 ) VALUES (
                    'session-2', 1, 'occurrence-1', ?1, 0, ?2, 'message-1',
                    'assistant', 1, '{\"kind\":\"unknown\"}', '{}', ?3, ?3
                 )",
                params![
                    observation.observation_id().as_str(),
                    anchor.anchor_id().as_str(),
                    canonical_payload
                ],
            )
            .await
            .expect("occurrence");
        }

        async fn move_hydration_session_outside_root_for_test(
            &self,
            provider: &str,
            session_id: &str,
        ) {
            let database = self
                .registered_database(HostAdmissionScope::Profile)
                .expect("registered profile database");
            Executor::execute(
                &database
                    .writer_connection()
                    .expect("registered profile writer"),
                "UPDATE sessions
                 SET project_key = 'different-project'
                 WHERE provider = ?1 AND session_id = ?2",
                params![provider, session_id],
            )
            .await
            .expect("move target session outside authorized root");
        }

        async fn seed_snapshot_hydration_fixture_for_test(
            &self,
            occurrence_observation: &DurableObservationV1,
            occurrence_anchor: &RetrievalAnchorRecord,
            summary_anchor: &RetrievalAnchorRecord,
            authority_anchor: &RetrievalAnchorRecord,
        ) {
            let database = self
                .registered_database(HostAdmissionScope::Profile)
                .expect("registered profile database");
            let writer = database
                .writer_connection()
                .expect("registered profile writer");
            let provider = occurrence_observation.source().provider().as_str();
            Executor::execute(
                &writer,
                "INSERT INTO sessions (
                    provider, session_id, project_key, project_path
                 ) VALUES (?1, 'session-1', 'user', '/snapshot-test')",
                [provider],
            )
            .await
            .expect("session owner");
            self.activate_temporal_generation_for_hydration_test("session-1")
                .await;

            let occurrence_payload = "non-empty occurrence payload";
            let occurrence_hash = hash(occurrence_payload.as_bytes());
            let payload_ref = "snapshot-payload.bin";
            let storage_root = database
                .db_path()
                .parent()
                .expect("registered profile storage root");
            let payload_dir = storage_root.join("lcm-payloads");
            fs::create_dir(&payload_dir).expect("payload directory");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;

                fs::set_permissions(&payload_dir, fs::Permissions::from_mode(0o700))
                    .expect("private payload directory");
            }
            let payload_path = payload_dir.join(payload_ref);
            fs::write(&payload_path, occurrence_payload).expect("external payload");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;

                fs::set_permissions(&payload_path, fs::Permissions::from_mode(0o600))
                    .expect("private payload file");
            }
            Executor::execute(
                &writer,
                "INSERT INTO lcm_raw_messages (
                    provider, message_id, session_id, role, ordinal, timestamp,
                    content, content_hash, storage_kind, payload_ref,
                    snippet_text, index_text, legacy_source, legacy_truncated
                 ) VALUES (
                    ?1, 'message-1', 'session-1', 'assistant', 1, 1,
                    NULL, ?2, 'external', ?3, ?4, ?4, 0, 0
                 )",
                params![
                    provider,
                    occurrence_hash.as_str(),
                    payload_ref,
                    occurrence_payload
                ],
            )
            .await
            .expect("raw message");
            Executor::execute(
                &writer,
                "INSERT INTO lcm_external_payloads (
                    payload_ref, provider, session_id, message_id, kind,
                    content_hash, byte_count, char_count, created_at
                 ) VALUES (?1, ?2, 'session-1', 'message-1', 'message', ?3, ?4, ?5, 1)",
                params![
                    payload_ref,
                    provider,
                    occurrence_hash.as_str(),
                    i64::try_from(occurrence_payload.len()).expect("payload bytes"),
                    i64::try_from(occurrence_payload.chars().count()).expect("payload chars")
                ],
            )
            .await
            .expect("external payload metadata");
            let external_manifest = serde_json::json!({
                "provider": provider,
                "session_id": "session-1",
                "message_id": "message-1",
                "byte_count": occurrence_payload.len(),
                "char_count": occurrence_payload.chars().count()
            })
            .to_string();
            let authority_publication = serde_json::json!({
                "receipt_id": "receipt-3",
                "payloads": [{
                    "payload_ref": payload_ref,
                    "digest": occurrence_hash.as_str(),
                    "manifest_json": external_manifest.clone()
                }]
            })
            .to_string();
            Executor::execute(
                &writer,
                "INSERT INTO session_summary_nodes (
                    summary_id, session_id, summary_anchor_id, summary_text,
                    index_text, source_horizon_json, publication_json, created_at
                 ) VALUES (
                    'summary-authority', 'session-1', ?1, 'authority',
                    'authority', '{}', ?2, 1
                 )",
                params![authority_anchor.anchor_id().as_str(), authority_publication],
            )
            .await
            .expect("payload authority summary");
            Executor::execute(
                &writer,
                "INSERT INTO session_external_payload_manifests (
                    payload_ref, session_id, payload_digest, manifest_json, receipt_id, created_at
                 ) VALUES (?1, 'session-1', ?2, ?3, 'receipt-3', 1)",
                params![
                    payload_ref,
                    occurrence_hash.as_str(),
                    external_manifest.as_str()
                ],
            )
            .await
            .expect("external payload manifest");
            Executor::execute(
                &writer,
                "INSERT INTO session_occurrences (
                    session_id, generation, occurrence_id, source_observation_id,
                    projection_output_ordinal, retrieval_anchor_id, message_id,
                    role, knowledge_at, valid_time_json, evidence_json,
                    snippet_text, index_text
                 ) VALUES (
                    'session-1', 1, 'occurrence-1', ?1, 0, ?2, 'message-1',
                    'assistant', 1, '{\"kind\":\"unknown\"}', '{}',
                    'non-empty occurrence payload', 'non-empty occurrence payload'
                 )",
                params![
                    occurrence_observation.observation_id().as_str(),
                    occurrence_anchor.anchor_id().as_str()
                ],
            )
            .await
            .expect("occurrence");

            let summary_payload = "non-empty summary payload";
            let publication = CanonicalPublicationManifest {
                version: 1,
                provider: provider.to_string(),
                conversation_id: "session-1".to_string(),
                session_id: "session-1".to_string(),
                depth: 1,
                summary_text: summary_payload.to_string(),
                summary_hash: hash(summary_payload.as_bytes()),
                source_refs: Vec::new(),
                canonical_sources: Vec::new(),
                source_token_count: 1,
                summary_token_count: 1,
                source_time_start: None,
                source_time_end: None,
                expand_hint: None,
                metadata_json: None,
                source_horizon_json: "{}".to_string(),
                owner_json: serde_json::to_string(summary_anchor.owner()).expect("owner json"),
                summary_anchor_id: summary_anchor.anchor_id().to_string(),
                receipt_id: "summary-receipt".to_string(),
                predecessor_summary_id: None,
                logical_identity_digest: digest('a'),
                payloads: Vec::new(),
                model_route: "snapshot-test".to_string(),
                configuration_digest: digest('b'),
                sanitization_receipt: "summary-receipt".to_string(),
                route: Value::Null,
            };
            Executor::execute(
                &writer,
                "INSERT INTO session_summary_nodes (
                    summary_id, session_id, summary_anchor_id, summary_text,
                    index_text, source_horizon_json, publication_json, created_at
                 ) VALUES ('summary-1', 'session-1', ?1, ?2, ?2, '{}', ?3, 1)",
                params![
                    summary_anchor.anchor_id().as_str(),
                    summary_payload,
                    serde_json::to_string(&publication).expect("publication json")
                ],
            )
            .await
            .expect("summary node");
            Executor::execute(
                &writer,
                "INSERT INTO session_summary_availability (
                    session_id, generation, summary_id, availability,
                    source_horizon_json, reason, checked_at
                 ) VALUES ('session-1', 1, 'summary-1', 'available', '{}', NULL, 1)",
                (),
            )
            .await
            .expect("summary availability");
            Executor::execute_batch(&writer, "DROP TRIGGER retrieval_anchors_immutable_update;")
                .await
                .expect("allow drift fixture");
        }

        fn hydration_storage_fingerprint_for_test(&self) -> HydrationStorageFingerprint {
            let database = self
                .registered_database(HostAdmissionScope::Profile)
                .expect("registered profile database");
            let storage_root = database
                .db_path()
                .parent()
                .expect("registered profile storage root");
            let payload_dir = storage_root.join("lcm-payloads");
            HydrationStorageFingerprint {
                database: Sha256::digest(fs::read(database.db_path()).expect("database bytes"))
                    .into(),
                payload: Sha256::digest(
                    fs::read(payload_dir.join("snapshot-payload.bin")).expect("payload bytes"),
                )
                .into(),
                payload_entries: fs::read_dir(payload_dir).expect("payload entries").count(),
            }
        }

        async fn drift_hydration_anchor_owner_for_test(&self, anchor_id: &RetrievalAnchorId) {
            let database = self
                .registered_database(HostAdmissionScope::Profile)
                .expect("registered profile database");
            Executor::execute(
                &database
                    .writer_connection()
                    .expect("registered profile writer"),
                "UPDATE retrieval_anchors
                 SET anchor_json = json_set(
                     anchor_json,
                     '$.owner',
                     json('{\"kind\":\"project\",\"project_id\":\"drifted-project\"}')
                 )
                 WHERE anchor_id = ?1",
                [anchor_id.as_str()],
            )
            .await
            .expect("drift anchor owner");
        }
    }

    struct ThreadWake(thread::Thread);

    impl Wake for ThreadWake {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }

        fn wake_by_ref(self: &Arc<Self>) {
            self.0.unpark();
        }
    }

    fn block_on<F: Future>(future: F) -> F::Output {
        let mut future = Box::pin(future);
        let waker = Waker::from(Arc::new(ThreadWake(thread::current())));
        let mut context = Context::from_waker(&waker);
        loop {
            match future.as_mut().poll(&mut context) {
                Poll::Ready(output) => return output,
                Poll::Pending => thread::park_timeout(Duration::from_millis(10)),
            }
        }
    }

    struct FakeBackend {
        resolutions: Mutex<Vec<HydrationResolution>>,
        payload: Mutex<Result<Vec<u8>, HydrationError>>,
        calls: Mutex<Vec<&'static str>>,
    }

    impl FakeBackend {
        fn available(payload: &[u8]) -> Self {
            Self {
                resolutions: Mutex::new(vec![
                    available(payload.len(), &hash(payload)),
                    available(payload.len(), &hash(payload)),
                ]),
                payload: Mutex::new(Ok(payload.to_vec())),
                calls: Mutex::new(Vec::new()),
            }
        }

        fn denied(state: HydrationStateV1) -> Self {
            Self {
                resolutions: Mutex::new(vec![HydrationResolution::Unavailable(state)]),
                payload: Mutex::new(Ok(Vec::new())),
                calls: Mutex::new(Vec::new()),
            }
        }
    }

    impl TemporalHydrationBackend for FakeBackend {
        fn resolve_current<'a>(
            &'a self,
            _snapshot: &'a TemporalExecutionSnapshot,
            _anchor_id: &'a RetrievalAnchorId,
        ) -> BackendFuture<'a, HydrationResolution> {
            Box::pin(async move {
                self.calls.lock().expect("calls").push("resolve");
                let mut resolutions = self.resolutions.lock().expect("resolutions");
                if resolutions.len() > 1 {
                    Ok(resolutions.remove(0))
                } else {
                    Ok(resolutions[0].clone())
                }
            })
        }

        fn read_bounded<'a>(
            &'a self,
            _descriptor: &'a PayloadDescriptor,
            _max_bytes: usize,
            control: &'a ExecutionControl,
        ) -> BackendFuture<'a, Zeroizing<Vec<u8>>> {
            Box::pin(async move {
                self.calls.lock().expect("calls").push("read");
                control.checkpoint()?;
                match &*self.payload.lock().expect("payload") {
                    Ok(payload) => Ok(Zeroizing::new(payload.clone())),
                    Err(error) => Err(error.clone()),
                }
            })
        }
    }

    fn available(byte_count: usize, content_hash: &str) -> HydrationResolution {
        HydrationResolution::Available(PayloadDescriptor {
            source: PayloadSource::Summary {
                session_id: "session-1".to_string(),
                summary_id: "summary-1".to_string(),
            },
            byte_count,
            content_hash: content_hash.to_string(),
        })
    }

    fn anchor() -> RetrievalAnchorId {
        RetrievalAnchorId::new("anchor-1").expect("anchor")
    }

    fn digest(byte: char) -> String {
        format!("sha256:{}", byte.to_string().repeat(64))
    }

    fn snapshot(control: ExecutionControl) -> TemporalExecutionSnapshot {
        TemporalExecutionSnapshot::new_authorized(
            TemporalSnapshotRequest::new(
                SessionId::new("session-1").expect("session"),
                digest('0'),
                digest('1'),
                digest('2'),
                TemporalModeV1::Current,
                RetrievalGrainV1::LogicalMessage,
            )
            .expect("request")
            .with_limits(ExecutionLimits {
                hydration_payload_bytes: 32,
                hydration_chunk_bytes: 4,
                ..ExecutionLimits::default()
            })
            .with_execution_control(control),
            TemporalWatermarks {
                generation: 1,
                source: 2,
                projection: 3,
                index: 4,
                summary: 5,
            },
            KernelVersions {
                schema: 1,
                ranking: 1,
                configuration_digest: BindingDigest::new("configuration", digest('3'))
                    .expect("digest"),
            },
            None,
            ValidatedAuthorization::Authorized,
        )
        .expect("snapshot")
    }

    fn hash(bytes: &[u8]) -> String {
        use std::fmt::Write as _;
        let digest = Sha256::digest(bytes);
        digest.iter().fold(String::new(), |mut output, byte| {
            let _ = write!(output, "{byte:02x}");
            output
        })
    }

    fn receipt(receipt_id: &str, payload: &Value) -> SanitizationReceiptV1 {
        SanitizationReceiptV1::new(
            SanitizationReceiptRefV1::new(
                SanitizationReceiptId::new(receipt_id).expect("receipt id"),
                tracedecay_domain::ComponentVersion::new("sanitizer.snapshot-test.v1")
                    .expect("component version"),
            )
            .expect("receipt reference"),
            SanitizerDispositionV1::Accepted,
            SensitivityV1::NonSensitive,
            Some(PayloadReferenceV1::for_payload(payload).expect("payload reference")),
        )
        .expect("receipt")
    }

    fn observation_for_session(ordinal: u64, session_id: &str) -> DurableObservationV1 {
        let session_id = SessionId::new(session_id).expect("session");
        let provider = ProviderId::new("provider-1").expect("provider");
        let source =
            ObservationSourceIdentityV1::for_provider(provider.clone(), session_id.clone())
                .expect("source");
        let range = ObservationSourceRangeV1::new(ordinal, ordinal + 1).expect("source range");
        let message_id = ObservationId::new(format!("message-{ordinal}")).expect("message");
        let record_id = ObservationId::new(format!("record-{ordinal}")).expect("record");
        let relations =
            CanonicalObservationRelationsV1::new(session_id).with_message_id(message_id);
        let envelope = CanonicalObservationEnvelopeV1::new(
            provider,
            "message",
            record_id.clone(),
            relations,
            vec![CanonicalObservationFactV1::Message {
                role: CanonicalMessageRoleV1::Assistant,
                content: json!({"text": format!("payload-{ordinal}")}),
                model: None,
                timestamp: Some(ordinal as i64),
            }],
            CanonicalObservationEvidenceV1::new(ObservationOrderingDomainV1::SnapshotOrder, range),
        )
        .expect("envelope");
        let payload = serde_json::to_value(envelope).expect("payload");
        let identity = ObservationIdentityMaterialV1::for_native_record(
            source,
            ObservationScopeV1::Profile,
            ObservationSourceGenerationV1::new(1).expect("source generation"),
            range,
            ObservationOrderingDomainV1::SnapshotOrder,
            record_id,
        )
        .expect("identity");
        DurableObservationV1::new(
            identity,
            receipt(&format!("receipt-{ordinal}"), &payload),
            RetentionClass::new("retention.snapshot-test").expect("retention"),
            payload,
        )
        .expect("observation")
    }

    /// Build an observation whose canonical envelope carries no
    /// `relations.message_id`. `derive_canonical_projection` then keys the
    /// projected message on the stable record id, so the persisted occurrence's
    /// `message_id` equals the record id — the exact shape that previously
    /// tripped the hydration `UnverifiableLegacy` misclassification.
    fn observation_without_relation_message_id(
        ordinal: u64,
        session_id: &str,
    ) -> DurableObservationV1 {
        let session_id = SessionId::new(session_id).expect("session");
        let provider = ProviderId::new("provider-1").expect("provider");
        let source =
            ObservationSourceIdentityV1::for_provider(provider.clone(), session_id.clone())
                .expect("source");
        let range = ObservationSourceRangeV1::new(ordinal, ordinal + 1).expect("source range");
        let record_id = ObservationId::new(format!("record-{ordinal}")).expect("record");
        // No `.with_message_id(...)`: the projection must fall back to the
        // stable record id when deriving the occurrence's message_id.
        let relations = CanonicalObservationRelationsV1::new(session_id);
        let envelope = CanonicalObservationEnvelopeV1::new(
            provider,
            "message",
            record_id.clone(),
            relations,
            vec![CanonicalObservationFactV1::Message {
                role: CanonicalMessageRoleV1::Assistant,
                content: json!({"text": format!("payload-{ordinal}")}),
                model: None,
                timestamp: Some(ordinal as i64),
            }],
            CanonicalObservationEvidenceV1::new(ObservationOrderingDomainV1::SnapshotOrder, range),
        )
        .expect("envelope");
        let payload = serde_json::to_value(envelope).expect("payload");
        let identity = ObservationIdentityMaterialV1::for_native_record(
            source,
            ObservationScopeV1::Profile,
            ObservationSourceGenerationV1::new(1).expect("source generation"),
            range,
            ObservationOrderingDomainV1::SnapshotOrder,
            record_id,
        )
        .expect("identity");
        DurableObservationV1::new(
            identity,
            receipt(&format!("receipt-{ordinal}"), &payload),
            RetentionClass::new("retention.snapshot-test").expect("retention"),
            payload,
        )
        .expect("observation")
    }

    fn checked_in_goal_observation() -> DurableObservationV1 {
        let record_id = ObservationId::new("record.goal.fixture").expect("record");
        let encoded = include_str!(
            "../../../../tests/fixtures/provider_normalization/codex/thread_goal_updated.expected_envelope.json"
        )
        .replace("$STABLE_RECORD_ID", record_id.as_str());
        let envelope: CanonicalObservationEnvelopeV1 =
            serde_json::from_str(&encoded).expect("checked-in goal envelope");
        let source = ObservationSourceIdentityV1::for_provider(
            envelope.provider().clone(),
            envelope.relations().session_id().clone(),
        )
        .expect("source");
        let range = envelope.evidence().range();
        let payload = serde_json::to_value(&envelope).expect("payload");
        let identity = ObservationIdentityMaterialV1::for_native_record(
            source,
            ObservationScopeV1::Profile,
            ObservationSourceGenerationV1::new(1).expect("source generation"),
            range,
            envelope.evidence().ordering_domain(),
            record_id,
        )
        .expect("identity");
        DurableObservationV1::new(
            identity,
            receipt("receipt-goal", &payload),
            RetentionClass::new("retention.snapshot-test").expect("retention"),
            payload,
        )
        .expect("observation")
    }

    #[test]
    fn checked_in_codex_goal_hydrates_as_typed_session_message() {
        let observation = checked_in_goal_observation();
        let message = canonical_projected_message(&observation, "record.goal.fixture", 0)
            .expect("canonical goal projection");

        assert_eq!(message.provider, "codex");
        assert_eq!(message.session_id, "codex-golden-session");
        assert_eq!(message.message_id, "record.goal.fixture");
        assert_eq!(message.role, "system");
        assert_eq!(message.kind.as_deref(), Some("goal"));
        assert_eq!(
            message.text,
            "phlogiston pipeline overhaul and reconciliation"
        );
        assert_eq!(message.timestamp, Some(1_783_500_569));
        assert_eq!(message.ordinal, 0);
        assert_eq!(message.source_offset, Some(0));
    }

    /// Pure-projection proof for the `resolve_occurrence` fix: when the
    /// canonical envelope omits `relations.message_id`, the projection keys the
    /// occurrence on the stable record id. The pre-fix gate
    /// (`relations().message_id().is_none_or(..)`) therefore misclassified every
    /// such occurrence as `UnverifiableLegacy`, dropping real matches. The fix
    /// binds through `canonical_projected_message` instead, which resolves the
    /// stable-record-id key yet still returns `None` for a message_id the
    /// projection never produced — so acceptance widens only to
    /// projection-verified bindings.
    #[test]
    fn stable_record_id_binds_projection_when_relations_message_id_absent() {
        let observation = observation_without_relation_message_id(1, "session-1");
        let envelope: CanonicalObservationEnvelopeV1 =
            serde_json::from_value(observation.payload().clone()).expect("canonical envelope");
        assert!(
            envelope.relations().message_id().is_none(),
            "fixture must omit relations.message_id so the pre-fix gate rejected it"
        );
        let record_message_id = envelope.stable_record_id().as_str().to_string();

        // Accepted direction: the stable-record-id key binds to a projection
        // output, which is exactly the signal the fix now trusts.
        let bound = canonical_projected_message(&observation, &record_message_id, 0)
            .expect("stable-record-id message must bind to a projection output");
        assert_eq!(bound.text, "payload-1");
        assert_eq!(bound.message_id, record_message_id);

        // Refused direction: a message_id that no projection output produces has
        // no binding, so the fix still returns UnverifiableLegacy for it.
        assert!(
            canonical_projected_message(&observation, "does-not-project", 0).is_none(),
            "an unprojected message_id must not bind, preserving the legacy refusal"
        );
    }

    async fn persist_anchor(
        runtime: &HostAdmissionTestRuntimeV1,
        ordinal: u64,
    ) -> (DurableObservationV1, RetrievalAnchorRecord) {
        Box::pin(persist_anchor_for_session(runtime, ordinal, "session-1")).await
    }

    async fn persist_anchor_for_session(
        runtime: &HostAdmissionTestRuntimeV1,
        ordinal: u64,
        session_id: &str,
    ) -> (DurableObservationV1, RetrievalAnchorRecord) {
        let observation = observation_for_session(ordinal, session_id);
        Box::pin(persist_observation(runtime, observation, ordinal)).await
    }

    async fn persist_observation(
        runtime: &HostAdmissionTestRuntimeV1,
        observation: DurableObservationV1,
        ordinal: u64,
    ) -> (DurableObservationV1, RetrievalAnchorRecord) {
        let identity = observation.identity();
        let next_cursor = ObservationSourceCursorV1::for_ordering(
            observation.source().clone(),
            observation.scope().clone(),
            identity.generation(),
            identity.ordering_domain(),
            identity.position().end(),
        )
        .expect("cursor");
        // Contiguous ranges are [ordinal, ordinal+1); appends must expect the
        // prior write's next cursor (position == ordinal) or CursorConflict.
        let expected_cursor = (ordinal > 1).then(|| {
            ObservationSourceCursorV1::for_ordering(
                observation.source().clone(),
                observation.scope().clone(),
                identity.generation(),
                identity.ordering_domain(),
                ordinal,
            )
            .expect("expected cursor")
        });
        let write = ObservationWrite::new(observation.clone(), expected_cursor, next_cursor)
            .expect("write");
        let projection =
            ProjectionGenerationId::new("projection.snapshot-test.v1").expect("projection");
        let authorization =
            build_observation_resolution_authorization_v1(write.observation(), "snapshot-test")
                .expect("authorization");
        let anchor = build_observation_retrieval_anchor_v2(
            write.observation(),
            projection.clone(),
            UtcMicros(1),
            authorization,
        )
        .expect("anchor");
        runtime
            .observation_store(HostAdmissionScope::Profile)
            .expect("registered profile observation store")
            .persist_observation(
                AnchoredObservationWrite::new(write, anchor.clone(), projection)
                    .expect("anchored write"),
            )
            .await
            .expect("persist observation");
        (observation, anchor)
    }

    #[tokio::test]
    async fn persist_anchor_appends_two_observations_without_cursor_conflict() {
        let dir = tempdir().expect("temporary directory");
        let runtime = HostAdmissionTestRuntimeV1::profile(dir.path())
            .await
            .expect("registered profile runtime");
        let (first, first_anchor) = Box::pin(persist_anchor(&runtime, 1)).await;
        let (second, second_anchor) = Box::pin(persist_anchor(&runtime, 2)).await;
        assert_ne!(
            first.observation_id(),
            second.observation_id(),
            "two-observation setup must persist distinct observation identities"
        );
        assert_ne!(
            first_anchor.anchor_id(),
            second_anchor.anchor_id(),
            "two-observation setup must persist distinct retrieval anchors"
        );
        assert_eq!(first.identity().position().end(), 2);
        assert_eq!(second.identity().position().start(), 2);
        assert_eq!(second.identity().position().end(), 3);
    }

    fn authorized_snapshot(anchor: &RetrievalAnchorRecord) -> TemporalExecutionSnapshot {
        authorized_snapshot_for_scope(
            anchor,
            tracedecay_query::temporal::ports::TemporalRetrievalScope::Session(
                SessionId::new("session-1").expect("session"),
            ),
        )
    }

    fn authorized_root_snapshot(anchor: &RetrievalAnchorRecord) -> TemporalExecutionSnapshot {
        authorized_snapshot_for_scope(
            anchor,
            tracedecay_query::temporal::ports::TemporalRetrievalScope::AllSessionsInAuthorizedRoot,
        )
    }

    fn authorized_snapshot_for_scope(
        anchor: &RetrievalAnchorRecord,
        scope: tracedecay_query::temporal::ports::TemporalRetrievalScope,
    ) -> TemporalExecutionSnapshot {
        TemporalExecutionSnapshot::new_authorized(
            TemporalSnapshotRequest::new(
                SessionId::new("session-1").expect("session"),
                digest('7'),
                digest('8'),
                anchor.authorization().access_policy_digest.as_str(),
                TemporalModeV1::Current,
                RetrievalGrainV1::LogicalMessage,
            )
            .expect("request")
            .with_authorized_root(
                TemporalAuthorizedRoot::profile("profile-1", "store-1", "root-1")
                    .expect("profile root"),
            )
            .expect("authorized root")
            .with_retrieval_scope(scope),
            TemporalWatermarks {
                generation: 1,
                source: 0,
                projection: 0,
                index: 0,
                summary: 0,
            },
            KernelVersions {
                schema: 1,
                ranking: 1,
                configuration_digest: BindingDigest::new("configuration", digest('9'))
                    .expect("configuration"),
            },
            None,
            ValidatedAuthorization::Authorized,
        )
        .expect("snapshot")
    }

    #[tokio::test]
    async fn root_snapshot_hydrates_an_authorized_cross_session_occurrence() {
        let dir = tempdir().expect("temporary directory");
        let runtime = HostAdmissionTestRuntimeV1::profile(dir.path())
            .await
            .expect("registered profile runtime");
        let (observation, anchor) =
            Box::pin(persist_anchor_for_session(&runtime, 1, "session-2")).await;
        let provider = observation.source().provider().as_str();
        let canonical_payload = "payload-1";
        let legacy_projection_poison = "legacy projection poison";
        runtime
            .seed_root_hydration_fixture_for_test(
                provider,
                &observation,
                &anchor,
                canonical_payload,
                legacy_projection_poison,
            )
            .await;

        let snapshot = authorized_root_snapshot(&anchor);
        let read = runtime.hydration_read_for_test().await;
        let adapter = read.adapter();
        assert_eq!(
            adapter.authorize(&snapshot, anchor.anchor_id()).await,
            Ok(HydrationAuthorization::Authorized)
        );
        let mut output = Vec::new();
        adapter
            .read_after_recheck(&snapshot, anchor.anchor_id(), 1024, 7, &mut |chunk| {
                output.extend_from_slice(chunk);
                Ok(())
            })
            .await
            .expect("cross-session hydration");
        assert_eq!(output, canonical_payload.as_bytes());

        drop(read);
        runtime
            .move_hydration_session_outside_root_for_test(provider, "session-2")
            .await;
        let different_root = runtime.hydration_read_for_test().await;
        let different_root_adapter = different_root.adapter();
        let authorization = different_root_adapter
            .authorize(&snapshot, anchor.anchor_id())
            .await;
        assert!(
            matches!(
                authorization,
                Ok(HydrationAuthorization::Denied(ref denial))
                    if denial.state() == HydrationStateV1::UnverifiableLegacy
            ),
            "{authorization:?}"
        );
    }

    #[tokio::test]
    async fn one_snapshot_hydrates_occurrence_and_summary_without_request_digest_binding() {
        let dir = tempdir().expect("temporary directory");
        let runtime = HostAdmissionTestRuntimeV1::profile(dir.path())
            .await
            .expect("registered profile runtime");
        let (occurrence_observation, occurrence_anchor) =
            Box::pin(persist_anchor(&runtime, 1)).await;
        let (_, summary_anchor) = Box::pin(persist_anchor(&runtime, 2)).await;
        let (_, authority_anchor) = Box::pin(persist_anchor(&runtime, 3)).await;
        runtime
            .seed_snapshot_hydration_fixture_for_test(
                &occurrence_observation,
                &occurrence_anchor,
                &summary_anchor,
                &authority_anchor,
            )
            .await;

        let snapshot = authorized_snapshot(&occurrence_anchor);
        assert_ne!(
            occurrence_anchor
                .authorization()
                .canonical_request_digest
                .as_str(),
            snapshot.request_digest().as_str()
        );
        let before = runtime.hydration_storage_fingerprint_for_test();
        let read = runtime.hydration_read_for_test().await;
        let adapter = read.adapter();

        let canonical_occurrence_payload = "payload-1";
        let summary_payload = "non-empty summary payload";
        for (anchor, expected) in [
            (occurrence_anchor.anchor_id(), canonical_occurrence_payload),
            (summary_anchor.anchor_id(), summary_payload),
        ] {
            assert_eq!(
                adapter.authorize(&snapshot, anchor).await,
                Ok(HydrationAuthorization::Authorized)
            );
            let mut output = Vec::new();
            adapter
                .read_after_recheck(&snapshot, anchor, 1024, 7, &mut |chunk| {
                    output.extend_from_slice(chunk);
                    Ok(())
                })
                .await
                .expect("hydrate payload");
            assert_eq!(output, expected.as_bytes());
        }
        assert_eq!(runtime.hydration_storage_fingerprint_for_test(), before);

        // Drift embedded owner without rewriting owner_json (composite FKs).
        runtime
            .drift_hydration_anchor_owner_for_test(occurrence_anchor.anchor_id())
            .await;

        let mut frozen_output = Vec::new();
        adapter
            .read_after_recheck(
                &snapshot,
                occurrence_anchor.anchor_id(),
                1024,
                7,
                &mut |chunk| {
                    frozen_output.extend_from_slice(chunk);
                    Ok(())
                },
            )
            .await
            .expect("frozen snapshot remains authorized");
        assert_eq!(frozen_output, canonical_occurrence_payload.as_bytes());

        let fresh_read = runtime.hydration_read_for_test().await;
        let fresh_adapter = fresh_read.adapter();
        assert!(matches!(
            fresh_adapter
                .authorize(&snapshot, occurrence_anchor.anchor_id())
                .await,
            Ok(HydrationAuthorization::Denied(ref denial))
                if denial.state() == HydrationStateV1::UnverifiableLegacy
        ));
        let mut denied_output = Vec::new();
        assert_eq!(
            fresh_adapter
                .read_after_recheck(
                    &snapshot,
                    occurrence_anchor.anchor_id(),
                    1024,
                    7,
                    &mut |chunk| {
                        denied_output.extend_from_slice(chunk);
                        Ok(())
                    },
                )
                .await,
            Err(HydrationError::Unavailable)
        );
        assert!(denied_output.is_empty());
    }

    /// Regression: an occurrence whose `message_id` was projected from the
    /// stable record id (because the canonical envelope carries no
    /// `relations.message_id`) must hydrate. Before the fix, `resolve_occurrence`
    /// rejected every such occurrence as `UnverifiableLegacy` — the defect that
    /// dropped real `lcm_grep`/`lcm_expand` matches into
    /// `omissions: reason=unverifiable_legacy`.
    #[tokio::test]
    async fn occurrence_keyed_on_stable_record_id_resolves_when_relations_message_id_absent() {
        let dir = tempdir().expect("temporary directory");
        let runtime = HostAdmissionTestRuntimeV1::profile(dir.path())
            .await
            .expect("registered profile runtime");
        let observation = observation_without_relation_message_id(1, "session-1");
        let (observation, anchor) = Box::pin(persist_observation(&runtime, observation, 1)).await;
        let provider = observation.source().provider().as_str().to_string();
        let envelope: CanonicalObservationEnvelopeV1 =
            serde_json::from_value(observation.payload().clone()).expect("canonical envelope");
        assert!(
            envelope.relations().message_id().is_none(),
            "fixture must omit relations.message_id so the projection falls back to the record id"
        );
        let record_message_id = envelope.stable_record_id().as_str().to_string();
        let canonical_payload = "payload-1";
        runtime
            .seed_session_occurrence_for_test(
                &provider,
                "session-1",
                &observation,
                &anchor,
                &record_message_id,
                canonical_payload,
            )
            .await;

        let snapshot = authorized_snapshot(&anchor);
        let read = runtime.hydration_read_for_test().await;
        let adapter = read.adapter();
        // Live direction: the stable-record-id-keyed occurrence must resolve.
        assert_eq!(
            adapter.authorize(&snapshot, anchor.anchor_id()).await,
            Ok(HydrationAuthorization::Authorized)
        );
        let mut output = Vec::new();
        adapter
            .read_after_recheck(&snapshot, anchor.anchor_id(), 1024, 7, &mut |chunk| {
                output.extend_from_slice(chunk);
                Ok(())
            })
            .await
            .expect("stable-record-id occurrence hydration");
        assert_eq!(output, canonical_payload.as_bytes());

        // Negative direction: an occurrence whose message_id corresponds to no
        // projection output of its digest-verified observation must still be
        // refused, proving the fix binds through the projection rather than
        // widening acceptance for unverifiable rows.
        drop(read);
        runtime
            .corrupt_hydration_occurrence_message_id_for_test("session-1", "does-not-project")
            .await;
        let corrupted = runtime.hydration_read_for_test().await;
        let corrupted_adapter = corrupted.adapter();
        let authorization = corrupted_adapter
            .authorize(&snapshot, anchor.anchor_id())
            .await;
        assert!(
            matches!(
                authorization,
                Ok(HydrationAuthorization::Denied(ref denial))
                    if denial.state() == HydrationStateV1::UnverifiableLegacy
            ),
            "{authorization:?}"
        );
    }

    #[test]
    fn authorization_precedes_recheck_and_read() {
        block_on(async {
            let adapter = SessionTemporalHydrationAdapter::new(FakeBackend::available(b"abcdefgh"));
            let snapshot = snapshot(ExecutionControl::default());
            assert_eq!(
                adapter.authorize(&snapshot, &anchor()).await,
                Ok(HydrationAuthorization::Authorized)
            );
            let mut output = Vec::new();
            adapter
                .read_after_recheck(&snapshot, &anchor(), 32, 4, &mut |chunk| {
                    output.extend_from_slice(chunk);
                    Ok(())
                })
                .await
                .expect("read");
            assert_eq!(output, b"abcdefgh");
            assert_eq!(
                adapter.backend.calls.lock().expect("calls").as_slice(),
                ["resolve", "resolve", "read"]
            );
        });
    }

    #[test]
    fn denial_has_no_payload_and_never_reads() {
        block_on(async {
            let adapter = SessionTemporalHydrationAdapter::new(FakeBackend::denied(
                HydrationStateV1::Redacted,
            ));
            let snapshot = snapshot(ExecutionControl::default());
            let authorization = adapter
                .authorize(&snapshot, &anchor())
                .await
                .expect("typed denial");
            assert!(matches!(
                authorization,
                HydrationAuthorization::Denied(ref denial)
                    if denial.state() == HydrationStateV1::Redacted
            ));
            assert_eq!(
                adapter.backend.calls.lock().expect("calls").as_slice(),
                ["resolve"]
            );
        });
    }

    #[test]
    fn declared_oversize_is_rejected_before_read() {
        block_on(async {
            let backend = FakeBackend {
                resolutions: Mutex::new(vec![available(9, &hash(b"123456789"))]),
                payload: Mutex::new(Ok(b"123456789".to_vec())),
                calls: Mutex::new(Vec::new()),
            };
            let adapter = SessionTemporalHydrationAdapter::new(backend);
            let snapshot = snapshot(ExecutionControl::default());
            let mut output = Vec::new();
            assert_eq!(
                adapter
                    .read_after_recheck(&snapshot, &anchor(), 8, 4, &mut |chunk| {
                        output.extend_from_slice(chunk);
                        Ok(())
                    })
                    .await,
                Err(HydrationError::BudgetExceeded {
                    resource: "payload bytes"
                })
            );
            assert!(output.is_empty());
            assert_eq!(
                adapter.backend.calls.lock().expect("calls").as_slice(),
                ["resolve"]
            );
        });
    }

    #[test]
    fn verified_payload_crosses_sink_in_bounded_chunks() {
        block_on(async {
            let adapter =
                SessionTemporalHydrationAdapter::new(FakeBackend::available(b"123456789"));
            let snapshot = snapshot(ExecutionControl::default());
            let mut chunks = Vec::new();
            adapter
                .read_after_recheck(&snapshot, &anchor(), 9, 4, &mut |chunk| {
                    chunks.push(chunk.to_vec());
                    Ok(())
                })
                .await
                .expect("chunked read");
            assert_eq!(
                chunks,
                vec![b"1234".to_vec(), b"5678".to_vec(), b"9".to_vec()]
            );
        });
    }

    #[test]
    fn integrity_failure_emits_no_payload() {
        block_on(async {
            let backend = FakeBackend {
                resolutions: Mutex::new(vec![available(4, &hash(b"good"))]),
                payload: Mutex::new(Ok(b"evil".to_vec())),
                calls: Mutex::new(Vec::new()),
            };
            let adapter = SessionTemporalHydrationAdapter::new(backend);
            let snapshot = snapshot(ExecutionControl::default());
            let mut output = Vec::new();
            assert_eq!(
                adapter
                    .read_after_recheck(&snapshot, &anchor(), 4, 4, &mut |chunk| {
                        output.extend_from_slice(chunk);
                        Ok(())
                    })
                    .await,
                Err(HydrationError::Unavailable)
            );
            assert!(output.is_empty());
        });
    }

    #[test]
    fn owner_mismatch_is_unauthorized() {
        assert_eq!(
            classify_current_access(
                PayloadAccessState::Eligible,
                &AnchorDurabilityClass::DurableEvidence,
                1,
                false,
                true,
            ),
            Some(HydrationStateV1::Unauthorized)
        );
    }

    #[test]
    fn cross_session_source_is_unauthorized() {
        assert_eq!(
            classify_current_access(
                PayloadAccessState::Eligible,
                &AnchorDurabilityClass::DurableEvidence,
                1,
                true,
                false,
            ),
            Some(HydrationStateV1::Unauthorized)
        );
    }

    #[test]
    fn retention_is_rechecked_at_read_time() {
        assert_eq!(
            classify_current_access(
                PayloadAccessState::Eligible,
                &AnchorDurabilityClass::RetentionBound {
                    expires_at: UtcMicros(10),
                },
                10,
                true,
                true,
            ),
            Some(HydrationStateV1::RetentionExpired)
        );
    }

    #[test]
    fn participant_lifecycle_states_surface_without_hydrating_payloads() {
        for (access, expected) in [
            (TemporalSourceAccess::Locked, HydrationStateV1::Locked),
            (
                TemporalSourceAccess::RetentionWithheld,
                HydrationStateV1::RetentionExpired,
            ),
            (TemporalSourceAccess::Deleted, HydrationStateV1::Deleted),
            (TemporalSourceAccess::Redacted, HydrationStateV1::Redacted),
            (
                TemporalSourceAccess::Unavailable,
                HydrationStateV1::RetainedButUnavailable,
            ),
        ] {
            assert_eq!(source_access_hydration_state(access), Some(expected));
        }
    }

    #[test]
    fn cancellation_and_deadline_interrupt_before_read() {
        block_on(async {
            let cancelled = ExecutionControl::default();
            cancelled.cancel();
            let cancelled_adapter =
                SessionTemporalHydrationAdapter::new(FakeBackend::available(b"payload"));
            assert_eq!(
                cancelled_adapter
                    .authorize(&snapshot(cancelled), &anchor())
                    .await,
                Err(HydrationError::Interrupted(TemporalPortError::Cancelled))
            );
            assert!(
                cancelled_adapter
                    .backend
                    .calls
                    .lock()
                    .expect("calls")
                    .is_empty()
            );

            let deadline = ExecutionControl::new(Some(
                Instant::now()
                    .checked_sub(Duration::from_millis(1))
                    .expect("past deadline"),
            ));
            let deadline_adapter =
                SessionTemporalHydrationAdapter::new(FakeBackend::available(b"payload"));
            assert_eq!(
                deadline_adapter
                    .authorize(&snapshot(deadline), &anchor())
                    .await,
                Err(HydrationError::Interrupted(
                    TemporalPortError::DeadlineExceeded
                ))
            );
            assert!(
                deadline_adapter
                    .backend
                    .calls
                    .lock()
                    .expect("calls")
                    .is_empty()
            );
        });
    }

    #[test]
    fn debug_output_never_contains_source_identifiers() {
        let descriptor = PayloadDescriptor {
            source: PayloadSource::External {
                provider: "private-provider".to_string(),
                session_id: "private-session".to_string(),
                payload_ref: "/private/canary".to_string(),
                char_count: 1,
            },
            byte_count: 1,
            content_hash: "hash".to_string(),
        };
        assert!(!format!("{descriptor:?}").contains("private/canary"));
    }
}
