use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracedecay_domain::{
    RetrievalGrainV1, SESSION_TEMPORAL_CURSOR_MAX_CANONICAL_BYTES,
    SESSION_TEMPORAL_CURSOR_MAX_PARTICIPANTS, SessionContractError, SessionId,
    SessionSourceCoverageReasonV1, SessionSourceCoverageReceiptV1, SessionSourceCoverageStateV1,
    SessionSourceCoverageV1, SessionSourceFrontierV1, SessionSourceIdV1,
    SessionTemporalCoverageRequestV1, SignedCursorKeyRefV1, TemporalModeV1,
};

use super::request::validate_label;
use super::{
    BindingDigest, ExecutionLimitTighteningError, ExecutionLimits, TemporalPortError,
    TemporalRetrievalScope, TemporalSnapshotRequest,
};
use crate::resolution::types::ValidatedAuthorization;

pub const MAX_TEMPORAL_PARTICIPANTS: usize = SESSION_TEMPORAL_CURSOR_MAX_PARTICIPANTS;
pub const MAX_TEMPORAL_PARTICIPANT_MANIFEST_BYTES: usize =
    SESSION_TEMPORAL_CURSOR_MAX_CANONICAL_BYTES;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TemporalWatermarks {
    pub generation: u64,
    pub source: u64,
    pub projection: u64,
    pub index: u64,
    pub summary: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KernelVersions {
    pub schema: u32,
    pub ranking: u32,
    pub configuration_digest: BindingDigest,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum TemporalParticipantAuthorization {
    #[serde(rename = "a")]
    Authorized,
    #[default]
    #[serde(rename = "n")]
    Denied,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum TemporalSourceAccess {
    #[serde(rename = "a")]
    Available,
    #[serde(rename = "u")]
    Unavailable,
    #[serde(rename = "l")]
    Locked,
    #[serde(rename = "r")]
    RetentionWithheld,
    #[serde(rename = "d")]
    Deleted,
    #[serde(rename = "x")]
    Redacted,
    #[serde(rename = "n")]
    LegacyUnauthorized,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TemporalParticipantGeneration {
    #[serde(rename = "s")]
    session_id: SessionId,
    #[serde(rename = "i")]
    pub(super) source_id: String,
    #[serde(rename = "g")]
    generation: u64,
    #[serde(rename = "w")]
    source_watermark: u64,
    #[serde(rename = "p")]
    projection_watermark: u64,
    #[serde(rename = "r")]
    graph_watermark: u64,
    #[serde(rename = "x")]
    index_watermark: u64,
    #[serde(rename = "m")]
    summary_watermark: u64,
    #[serde(rename = "c")]
    configuration_digest: String,
    #[serde(rename = "a")]
    authorization_digest: String,
    #[serde(default, rename = "q")]
    authorization: TemporalParticipantAuthorization,
    #[serde(rename = "z")]
    access: TemporalSourceAccess,
}

impl TemporalParticipantGeneration {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        session_id: SessionId,
        source_id: impl Into<String>,
        watermarks: TemporalWatermarks,
        graph_watermark: u64,
        configuration_digest: &BindingDigest,
        authorization_digest: &BindingDigest,
        authorization: TemporalParticipantAuthorization,
        access: TemporalSourceAccess,
    ) -> Result<Self, TemporalPortError> {
        let source_id = source_id.into();
        validate_label("source_id", &source_id)?;
        if watermarks.generation == 0 {
            return Err(TemporalPortError::ZeroGeneration);
        }
        Ok(Self {
            session_id,
            source_id,
            generation: watermarks.generation,
            source_watermark: watermarks.source,
            projection_watermark: watermarks.projection,
            graph_watermark,
            index_watermark: watermarks.index,
            summary_watermark: watermarks.summary,
            configuration_digest: configuration_digest.as_str().to_string(),
            authorization_digest: authorization_digest.as_str().to_string(),
            authorization,
            access,
        })
    }

    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    pub fn source_id(&self) -> &str {
        &self.source_id
    }

    pub const fn generation(&self) -> u64 {
        self.generation
    }

    pub const fn watermarks(&self) -> TemporalWatermarks {
        TemporalWatermarks {
            generation: self.generation,
            source: self.source_watermark,
            projection: self.projection_watermark,
            index: self.index_watermark,
            summary: self.summary_watermark,
        }
    }

    pub const fn graph_watermark(&self) -> u64 {
        self.graph_watermark
    }

    pub fn configuration_digest(&self) -> &str {
        &self.configuration_digest
    }

    pub fn authorization_digest(&self) -> &str {
        &self.authorization_digest
    }

    pub const fn authorization(&self) -> TemporalParticipantAuthorization {
        self.authorization
    }

    /// Snapshot authority is independent from per-source lifecycle state.
    ///
    /// The legacy unauthorized source wire state remains denied for old signed
    /// manifests, while every newly built manifest uses the dedicated,
    /// fail-closed authorization field.
    pub const fn is_authorized_for_snapshot(&self) -> bool {
        matches!(
            self.authorization,
            TemporalParticipantAuthorization::Authorized
        ) && !matches!(self.access, TemporalSourceAccess::LegacyUnauthorized)
    }

    pub const fn access(&self) -> TemporalSourceAccess {
        self.access
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TemporalParticipantManifest {
    #[serde(rename = "p")]
    entries: Vec<TemporalParticipantGeneration>,
    #[serde(rename = "e")]
    epoch_digest: String,
}

impl TemporalParticipantManifest {
    pub fn new(mut entries: Vec<TemporalParticipantGeneration>) -> Result<Self, TemporalPortError> {
        if entries.is_empty() {
            return Err(TemporalPortError::EmptyParticipantManifest);
        }
        if entries.len() > MAX_TEMPORAL_PARTICIPANTS {
            return Err(TemporalPortError::ParticipantLimitExceeded {
                observed: entries.len(),
                maximum: MAX_TEMPORAL_PARTICIPANTS,
            });
        }
        entries.sort_by(|left, right| {
            left.session_id
                .cmp(&right.session_id)
                .then_with(|| left.source_id.cmp(&right.source_id))
        });
        if entries.windows(2).any(|pair| {
            pair[0].session_id == pair[1].session_id && pair[0].source_id == pair[1].source_id
        }) {
            return Err(TemporalPortError::DuplicateParticipant);
        }
        let canonical = serde_json::to_vec(&entries).map_err(|error| TemporalPortError::Read {
            operation: "encode participant manifest",
            message: error.to_string(),
        })?;
        if canonical.len() > MAX_TEMPORAL_PARTICIPANT_MANIFEST_BYTES {
            return Err(TemporalPortError::ParticipantManifestBytesExceeded {
                observed: canonical.len(),
                maximum: MAX_TEMPORAL_PARTICIPANT_MANIFEST_BYTES,
            });
        }
        let epoch_digest = format!("sha256:{}", hex::encode(Sha256::digest(&canonical)));
        Ok(Self {
            entries,
            epoch_digest,
        })
    }

    pub fn entries(&self) -> &[TemporalParticipantGeneration] {
        &self.entries
    }

    pub fn epoch_digest(&self) -> &str {
        &self.epoch_digest
    }

    pub fn source_coverage(
        &self,
        mode: TemporalModeV1,
    ) -> Result<SessionSourceCoverageReceiptV1, SessionContractError> {
        let request = SessionTemporalCoverageRequestV1::new(mode);
        let sources = self
            .entries
            .iter()
            .map(|entry| {
                let source_id = SessionSourceIdV1::new(format!(
                    "{}:{}",
                    entry.session_id.as_str(),
                    entry.source_id
                ))?;
                let observed = SessionSourceFrontierV1::new(entry.source_watermark);
                let committed = SessionSourceFrontierV1::new(entry.projection_watermark);
                if entry.is_authorized_for_snapshot()
                    && entry.access == TemporalSourceAccess::Available
                {
                    return SessionSourceCoverageV1::new(
                        source_id,
                        observed,
                        committed,
                        observed,
                        request.clone(),
                        Vec::new(),
                        Vec::new(),
                        if committed == observed {
                            SessionSourceCoverageStateV1::Fresh
                        } else {
                            SessionSourceCoverageStateV1::Stale
                        },
                        if committed == observed {
                            SessionSourceCoverageReasonV1::CaughtUp
                        } else {
                            SessionSourceCoverageReasonV1::ProjectionBehindSource {
                                lag: committed.lag_from(observed),
                            }
                        },
                    );
                }
                let (state, reason) = match entry.access {
                    TemporalSourceAccess::Locked => (
                        SessionSourceCoverageStateV1::Locked,
                        SessionSourceCoverageReasonV1::Locked,
                    ),
                    TemporalSourceAccess::RetentionWithheld | TemporalSourceAccess::Deleted => (
                        SessionSourceCoverageStateV1::RetentionWithheld,
                        SessionSourceCoverageReasonV1::RetentionWithheld,
                    ),
                    TemporalSourceAccess::Redacted => (
                        SessionSourceCoverageStateV1::Redacted,
                        SessionSourceCoverageReasonV1::Redacted,
                    ),
                    TemporalSourceAccess::Unavailable
                    | TemporalSourceAccess::LegacyUnauthorized => (
                        SessionSourceCoverageStateV1::Unavailable,
                        SessionSourceCoverageReasonV1::Unavailable,
                    ),
                    TemporalSourceAccess::Available => unreachable!(),
                };
                SessionSourceCoverageV1::new(
                    source_id,
                    observed,
                    committed,
                    observed,
                    request.clone(),
                    Vec::new(),
                    Vec::new(),
                    state,
                    reason,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        SessionSourceCoverageReceiptV1::new(request, sources)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TemporalExecutionSnapshot {
    request: TemporalSnapshotRequest,
    watermarks: TemporalWatermarks,
    versions: KernelVersions,
    cursor_key: Option<SignedCursorKeyRefV1>,
    authorization: ValidatedAuthorization,
    participants: TemporalParticipantManifest,
    participant_manifest_authoritative: bool,
}

impl TemporalExecutionSnapshot {
    pub fn new_authorized(
        request: TemporalSnapshotRequest,
        watermarks: TemporalWatermarks,
        versions: KernelVersions,
        cursor_key: Option<SignedCursorKeyRefV1>,
        authorization: ValidatedAuthorization,
    ) -> Result<Self, TemporalPortError> {
        if !authorization.is_authorized() {
            return Err(TemporalPortError::UnauthorizedSnapshot);
        }
        request.limits().validate()?;
        if watermarks.generation == 0 {
            return Err(TemporalPortError::ZeroGeneration);
        }
        if versions.schema == 0 {
            return Err(TemporalPortError::ZeroVersion { field: "schema" });
        }
        if versions.ranking == 0 {
            return Err(TemporalPortError::ZeroVersion { field: "ranking" });
        }
        let participants =
            TemporalParticipantManifest::new(vec![TemporalParticipantGeneration::new(
                request.session_id().clone(),
                request.provider_scope().unwrap_or("all"),
                watermarks,
                watermarks.projection,
                &versions.configuration_digest,
                request.access_digest(),
                TemporalParticipantAuthorization::Authorized,
                TemporalSourceAccess::Available,
            )?])?;
        Ok(Self {
            request,
            watermarks,
            versions,
            cursor_key,
            authorization,
            participants,
            participant_manifest_authoritative: false,
        })
    }

    #[cfg(test)]
    pub fn new(
        request: TemporalSnapshotRequest,
        watermarks: TemporalWatermarks,
        versions: KernelVersions,
        cursor_key: Option<SignedCursorKeyRefV1>,
    ) -> Result<Self, TemporalPortError> {
        Self::new_authorized(
            request,
            watermarks,
            versions,
            cursor_key,
            ValidatedAuthorization::Authorized,
        )
    }

    pub fn request(&self) -> &TemporalSnapshotRequest {
        &self.request
    }

    pub fn with_limits(
        mut self,
        limits: ExecutionLimits,
    ) -> Result<Self, ExecutionLimitTighteningError> {
        let authorized = self.request.limits();
        // Keep this guard exhaustive so adding a limit field forces the
        // monotonic comparison and its parameterized tests to be updated.
        let ExecutionLimits {
            candidate_limit: _,
            candidate_total_bytes: _,
            candidate_item_bytes: _,
            candidate_key_bytes: _,
            candidate_stable_id_bytes: _,
            candidate_anchor_id_bytes: _,
            candidate_metadata_field_bytes: _,
            record_limit: _,
            record_total_bytes: _,
            record_item_bytes: _,
            record_key_bytes: _,
            hydration_limit: _,
            hydration_total_bytes: _,
            hydration_payload_bytes: _,
            hydration_chunk_bytes: _,
        } = authorized;
        for (field, authorized, requested) in [
            (
                "candidate_limit",
                authorized.candidate_limit,
                limits.candidate_limit,
            ),
            (
                "candidate_total_bytes",
                authorized.candidate_total_bytes,
                limits.candidate_total_bytes,
            ),
            (
                "candidate_item_bytes",
                authorized.candidate_item_bytes,
                limits.candidate_item_bytes,
            ),
            (
                "candidate_key_bytes",
                authorized.candidate_key_bytes,
                limits.candidate_key_bytes,
            ),
            (
                "candidate_stable_id_bytes",
                authorized.candidate_stable_id_bytes,
                limits.candidate_stable_id_bytes,
            ),
            (
                "candidate_anchor_id_bytes",
                authorized.candidate_anchor_id_bytes,
                limits.candidate_anchor_id_bytes,
            ),
            (
                "candidate_metadata_field_bytes",
                authorized.candidate_metadata_field_bytes,
                limits.candidate_metadata_field_bytes,
            ),
            ("record_limit", authorized.record_limit, limits.record_limit),
            (
                "record_total_bytes",
                authorized.record_total_bytes,
                limits.record_total_bytes,
            ),
            (
                "record_item_bytes",
                authorized.record_item_bytes,
                limits.record_item_bytes,
            ),
            (
                "record_key_bytes",
                authorized.record_key_bytes,
                limits.record_key_bytes,
            ),
            (
                "hydration_limit",
                authorized.hydration_limit,
                limits.hydration_limit,
            ),
            (
                "hydration_total_bytes",
                authorized.hydration_total_bytes,
                limits.hydration_total_bytes,
            ),
            (
                "hydration_payload_bytes",
                authorized.hydration_payload_bytes,
                limits.hydration_payload_bytes,
            ),
            (
                "hydration_chunk_bytes",
                authorized.hydration_chunk_bytes,
                limits.hydration_chunk_bytes,
            ),
        ] {
            if requested > authorized {
                return Err(ExecutionLimitTighteningError::WouldLoosen {
                    field,
                    authorized,
                    requested,
                });
            }
        }
        self.request = self.request.with_limits(limits.validate()?);
        Ok(self)
    }

    pub const fn authorization(&self) -> ValidatedAuthorization {
        self.authorization
    }

    pub fn with_participant_manifest(
        mut self,
        participants: TemporalParticipantManifest,
    ) -> Result<Self, TemporalPortError> {
        if matches!(
            self.request.retrieval_scope(),
            TemporalRetrievalScope::Session(session_id)
                if participants.entries().iter().any(|entry| entry.session_id() != session_id)
        ) {
            return Err(TemporalPortError::UnauthorizedSnapshot);
        }
        self.participants = participants;
        self.participant_manifest_authoritative = true;
        Ok(self)
    }

    pub fn participant_manifest(&self) -> &TemporalParticipantManifest {
        &self.participants
    }

    pub fn source_coverage(&self) -> Result<SessionSourceCoverageReceiptV1, SessionContractError> {
        self.participants.source_coverage(self.temporal_mode())
    }

    pub const fn has_authoritative_participant_manifest(&self) -> bool {
        self.participant_manifest_authoritative
    }

    pub fn root_digest(&self) -> &BindingDigest {
        self.request.root_digest()
    }

    pub fn request_digest(&self) -> &BindingDigest {
        self.request.request_digest()
    }

    pub fn filter_digest(&self) -> &BindingDigest {
        self.request.filter_digest()
    }

    pub fn provider_scope(&self) -> Option<&str> {
        self.request.provider_scope()
    }

    pub fn retrieval_scope(&self) -> &TemporalRetrievalScope {
        self.request.retrieval_scope()
    }

    pub fn access_digest(&self) -> &BindingDigest {
        self.request.access_digest()
    }

    pub const fn temporal_mode(&self) -> TemporalModeV1 {
        self.request.temporal_mode()
    }

    pub const fn grain(&self) -> RetrievalGrainV1 {
        self.request.grain()
    }

    pub const fn watermarks(&self) -> TemporalWatermarks {
        self.watermarks
    }

    pub fn versions(&self) -> &KernelVersions {
        &self.versions
    }

    pub fn cursor_key(&self) -> Option<&SignedCursorKeyRefV1> {
        self.cursor_key.as_ref()
    }
}
