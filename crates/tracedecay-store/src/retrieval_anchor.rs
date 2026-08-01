//! Driver-neutral retrieval-anchor disposition and tombstone contracts.
//!
//! Immutable anchors remain domain records. This module owns only the
//! append-only store projections that govern whether an anchor and its
//! derivatives may still resolve.

use std::future::Future;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::canonical_text::{CANONICAL_TEXT_MAX_BYTES, is_canonical_text_within};
use tracedecay_domain::{
    AnchorOwnerBindingV1, FactOwnerV1, ProjectionGenerationId, RetrievalAnchorId,
    RetrievalAnchorRecordV2, RetrievalAnchorRecordV3, UtcMicros,
};

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum RetrievalAnchorStoreError {
    #[error("retrieval anchor store data is invalid: {0}")]
    InvalidData(String),
    #[error("retrieval anchor disposition conflicts with current authority")]
    DispositionConflict,
    #[error("retrieval anchor store unavailable")]
    Unavailable,
}

pub type RetrievalAnchorStoreResult<T> = Result<T, RetrievalAnchorStoreError>;

/// Exact physical owner encoding for both byte-compatible V2 anchors and V3
/// profile/privacy-bound anchors. Untagged serialization preserves the
/// canonical owner JSON embedded in existing anchor rows.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum RetrievalAnchorOwnerV1 {
    V3(AnchorOwnerBindingV1),
    V2(FactOwnerV1),
}

impl RetrievalAnchorOwnerV1 {
    pub fn validate(&self) -> RetrievalAnchorStoreResult<()> {
        match self {
            Self::V3(owner) => owner.validate().map_err(domain),
            Self::V2(owner) => owner.validate().map_err(domain),
        }
    }

    pub fn v3(&self) -> Option<&AnchorOwnerBindingV1> {
        match self {
            Self::V3(owner) => Some(owner),
            Self::V2(_) => None,
        }
    }

    pub fn v2(&self) -> Option<&FactOwnerV1> {
        match self {
            Self::V3(_) => None,
            Self::V2(owner) => Some(owner),
        }
    }
}

impl From<AnchorOwnerBindingV1> for RetrievalAnchorOwnerV1 {
    fn from(owner: AnchorOwnerBindingV1) -> Self {
        Self::V3(owner)
    }
}

impl From<FactOwnerV1> for RetrievalAnchorOwnerV1 {
    fn from(owner: FactOwnerV1) -> Self {
        Self::V2(owner)
    }
}

/// Byte-compatible persisted anchor record across the V2/V3 cutover.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum StoredRetrievalAnchorRecordV1 {
    V3(RetrievalAnchorRecordV3),
    V2(RetrievalAnchorRecordV2),
}

impl StoredRetrievalAnchorRecordV1 {
    pub fn validate(&self) -> RetrievalAnchorStoreResult<()> {
        match self {
            Self::V3(record) => record.validate().map_err(domain),
            Self::V2(record) => record.validate().map_err(domain),
        }
    }

    pub fn anchor_id(&self) -> &RetrievalAnchorId {
        match self {
            Self::V3(record) => record.anchor_id(),
            Self::V2(record) => record.anchor_id(),
        }
    }

    pub fn owner(&self) -> RetrievalAnchorOwnerV1 {
        match self {
            Self::V3(record) => RetrievalAnchorOwnerV1::V3(record.owner().clone()),
            Self::V2(record) => {
                RetrievalAnchorOwnerV1::V2(FactOwnerV1::from(record.owner().clone()))
            }
        }
    }

    pub fn projection_generation(&self) -> &ProjectionGenerationId {
        match self {
            Self::V3(record) => record.projection_generation(),
            Self::V2(record) => record.projection_generation(),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AnchorDispositionStateV1 {
    Active,
    Superseded,
    Redacted,
    Expired,
    Quarantined,
    Deleted,
    Unavailable,
}

impl AnchorDispositionStateV1 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Superseded => "superseded",
            Self::Redacted => "redacted",
            Self::Expired => "expired",
            Self::Quarantined => "quarantined",
            Self::Deleted => "deleted",
            Self::Unavailable => "unavailable",
        }
    }

    pub fn parse(value: &str) -> RetrievalAnchorStoreResult<Self> {
        match value {
            "active" => Ok(Self::Active),
            "superseded" => Ok(Self::Superseded),
            "redacted" => Ok(Self::Redacted),
            "expired" => Ok(Self::Expired),
            "quarantined" => Ok(Self::Quarantined),
            "deleted" => Ok(Self::Deleted),
            "unavailable" => Ok(Self::Unavailable),
            _ => Err(invalid("unknown anchor disposition state")),
        }
    }

    /// Whether appending `next` on top of `current` is legal, where `None`
    /// means the anchor has no disposition history yet.
    ///
    /// This is the one canonical disposition state machine. Two SQLite engines
    /// append to `retrieval_anchor_dispositions` — the root authority in
    /// `src/db/retrieval_anchor_authority.rs` and the `RetrievalAnchorExecutor`
    /// in the rusqlite-runtime crate — and an anchor may be written by either
    /// during the migration. If the two disagree about a transition, the same
    /// anchor becomes reachable or unreachable depending on which writer it
    /// happened to pass through. Each engine still renders its own refusal
    /// message; only the decision is shared.
    ///
    /// The rules: `Redacted`, `Expired`, and `Deleted` are terminal, so no
    /// transition leaves them. `Superseded` may only advance to `Deleted` — a
    /// superseded anchor can be erased but never resurrected. `Active`,
    /// `Quarantined`, `Unavailable`, and a fresh anchor accept any next state,
    /// which is what lets a quarantine or an outage be reversed.
    pub fn transition_allowed(current: Option<Self>, next: Self) -> bool {
        match current {
            Some(Self::Redacted | Self::Expired | Self::Deleted) => false,
            Some(Self::Superseded) => next == Self::Deleted,
            Some(Self::Active | Self::Quarantined | Self::Unavailable) | None => true,
        }
    }

    /// Whether entering this state tombstones the anchor's reverse lineage.
    ///
    /// `Quarantined` and `Unavailable` are deliberately excluded: both are
    /// recoverable, and tombstoning their derivatives would make the recovery
    /// lossy. The complementary read-side rule is
    /// [`serves_derivatives`](Self::serves_derivatives).
    pub const fn suppresses_derivatives(self) -> bool {
        matches!(
            self,
            Self::Superseded | Self::Redacted | Self::Expired | Self::Deleted
        )
    }

    /// Whether an anchor in this disposition may publish or serve lineage.
    ///
    /// `None` means no disposition has ever been recorded, which is servable:
    /// an anchor is active until something says otherwise.
    pub fn serves_derivatives(current: Option<Self>) -> bool {
        matches!(current, None | Some(Self::Active))
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AnchorDispositionReasonClassV1 {
    UserRequest,
    Retention,
    Redaction,
    Quarantine,
    Correction,
    LegalHold,
    SourceUnavailable,
}

impl AnchorDispositionReasonClassV1 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UserRequest => "user_request",
            Self::Retention => "retention",
            Self::Redaction => "redaction",
            Self::Quarantine => "quarantine",
            Self::Correction => "correction",
            Self::LegalHold => "legal_hold",
            Self::SourceUnavailable => "source_unavailable",
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AnchorDerivativeKindV1 {
    Span,
    Contribution,
    Finding,
}

impl AnchorDerivativeKindV1 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Span => "span",
            Self::Contribution => "contribution",
            Self::Finding => "finding",
        }
    }

    pub fn parse(value: &str) -> RetrievalAnchorStoreResult<Self> {
        match value {
            "span" => Ok(Self::Span),
            "contribution" => Ok(Self::Contribution),
            "finding" => Ok(Self::Finding),
            _ => Err(invalid("unknown anchor derivative kind")),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RetrievalAnchorDispositionRecordV1 {
    disposition_id: String,
    anchor_id: RetrievalAnchorId,
    owner: RetrievalAnchorOwnerV1,
    state: AnchorDispositionStateV1,
    superseded_by: Option<RetrievalAnchorId>,
    reason_class: AnchorDispositionReasonClassV1,
    effective_at: UtcMicros,
}

impl RetrievalAnchorDispositionRecordV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        disposition_id: impl Into<String>,
        anchor_id: RetrievalAnchorId,
        owner: impl Into<RetrievalAnchorOwnerV1>,
        state: AnchorDispositionStateV1,
        superseded_by: Option<RetrievalAnchorId>,
        reason_class: AnchorDispositionReasonClassV1,
        effective_at: UtcMicros,
    ) -> RetrievalAnchorStoreResult<Self> {
        let record = Self {
            disposition_id: disposition_id.into(),
            anchor_id,
            owner: owner.into(),
            state,
            superseded_by,
            reason_class,
            effective_at,
        };
        record.validate()?;
        Ok(record)
    }

    pub fn disposition_id(&self) -> &str {
        &self.disposition_id
    }

    pub fn anchor_id(&self) -> &RetrievalAnchorId {
        &self.anchor_id
    }

    pub fn owner(&self) -> &RetrievalAnchorOwnerV1 {
        &self.owner
    }

    pub const fn state(&self) -> AnchorDispositionStateV1 {
        self.state
    }

    pub fn superseded_by(&self) -> Option<&RetrievalAnchorId> {
        self.superseded_by.as_ref()
    }

    pub const fn reason_class(&self) -> AnchorDispositionReasonClassV1 {
        self.reason_class
    }

    pub const fn effective_at(&self) -> UtcMicros {
        self.effective_at
    }

    pub fn validate(&self) -> RetrievalAnchorStoreResult<()> {
        validate_label(&self.disposition_id, "disposition id")?;
        self.anchor_id.validate().map_err(domain)?;
        self.owner.validate()?;
        if let Some(successor) = &self.superseded_by {
            successor.validate().map_err(domain)?;
            if successor == &self.anchor_id {
                return Err(invalid("an anchor cannot supersede itself"));
            }
        }
        if (self.state == AnchorDispositionStateV1::Superseded) != self.superseded_by.is_some() {
            return Err(invalid(
                "only a superseded disposition may name a successor",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RetrievalAnchorDerivativeV1 {
    source_anchor_id: RetrievalAnchorId,
    owner: RetrievalAnchorOwnerV1,
    kind: AnchorDerivativeKindV1,
    derivative_id: String,
    direct_evidence: bool,
}

impl RetrievalAnchorDerivativeV1 {
    pub fn new(
        source_anchor_id: RetrievalAnchorId,
        owner: impl Into<RetrievalAnchorOwnerV1>,
        kind: AnchorDerivativeKindV1,
        derivative_id: impl Into<String>,
        direct_evidence: bool,
    ) -> RetrievalAnchorStoreResult<Self> {
        let derivative = Self {
            source_anchor_id,
            owner: owner.into(),
            kind,
            derivative_id: derivative_id.into(),
            direct_evidence,
        };
        derivative.validate()?;
        Ok(derivative)
    }

    pub fn source_anchor_id(&self) -> &RetrievalAnchorId {
        &self.source_anchor_id
    }

    pub fn owner(&self) -> &RetrievalAnchorOwnerV1 {
        &self.owner
    }

    pub const fn kind(&self) -> AnchorDerivativeKindV1 {
        self.kind
    }

    pub fn derivative_id(&self) -> &str {
        &self.derivative_id
    }

    pub const fn is_direct_evidence(&self) -> bool {
        self.direct_evidence
    }

    pub fn validate(&self) -> RetrievalAnchorStoreResult<()> {
        self.source_anchor_id.validate().map_err(domain)?;
        self.owner.validate()?;
        validate_label(&self.derivative_id, "anchor derivative id")
    }
}

/// Safe terminal routing record. It contains no source coordinate, payload,
/// alias, query, rank, path, or native locator.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RetrievalAnchorTombstoneV1 {
    anchor_id: RetrievalAnchorId,
    owner: RetrievalAnchorOwnerV1,
    terminal_state: AnchorDispositionStateV1,
    reason_class: AnchorDispositionReasonClassV1,
    effective_at: UtcMicros,
}

impl RetrievalAnchorTombstoneV1 {
    pub fn new(
        anchor_id: RetrievalAnchorId,
        owner: impl Into<RetrievalAnchorOwnerV1>,
        terminal_state: AnchorDispositionStateV1,
        reason_class: AnchorDispositionReasonClassV1,
        effective_at: UtcMicros,
    ) -> RetrievalAnchorStoreResult<Self> {
        let record = Self {
            anchor_id,
            owner: owner.into(),
            terminal_state,
            reason_class,
            effective_at,
        };
        record.validate()?;
        Ok(record)
    }

    pub fn validate(&self) -> RetrievalAnchorStoreResult<()> {
        if !matches!(
            self.terminal_state,
            AnchorDispositionStateV1::Redacted
                | AnchorDispositionStateV1::Expired
                | AnchorDispositionStateV1::Quarantined
                | AnchorDispositionStateV1::Deleted
                | AnchorDispositionStateV1::Unavailable
        ) {
            return Err(invalid("retrieval anchor tombstone terminal state"));
        }
        self.anchor_id.validate().map_err(domain)?;
        self.owner.validate()
    }

    pub fn anchor_id(&self) -> &RetrievalAnchorId {
        &self.anchor_id
    }

    pub fn owner(&self) -> &RetrievalAnchorOwnerV1 {
        &self.owner
    }

    pub const fn terminal_state(&self) -> AnchorDispositionStateV1 {
        self.terminal_state
    }

    pub const fn reason_class(&self) -> AnchorDispositionReasonClassV1 {
        self.reason_class
    }

    pub const fn effective_at(&self) -> UtcMicros {
        self.effective_at
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnchorDispositionAppendOutcomeV1 {
    Appended,
    Replayed,
}

/// Append-only disposition and derivative-lineage authority. Authorization
/// remains an application concern and must be checked before any returned
/// value is disclosed.
pub trait RetrievalAnchorDispositionStore: Send + Sync {
    fn append_disposition(
        &self,
        record: RetrievalAnchorDispositionRecordV1,
    ) -> impl Future<Output = RetrievalAnchorStoreResult<AnchorDispositionAppendOutcomeV1>> + Send;

    fn publish_derivative(
        &self,
        derivative: RetrievalAnchorDerivativeV1,
    ) -> impl Future<Output = RetrievalAnchorStoreResult<AnchorDispositionAppendOutcomeV1>> + Send;

    fn current_disposition(
        &self,
        anchor_id: &RetrievalAnchorId,
        owner: &RetrievalAnchorOwnerV1,
    ) -> impl Future<Output = RetrievalAnchorStoreResult<Option<RetrievalAnchorDispositionRecordV1>>>
    + Send;

    fn tombstone(
        &self,
        anchor_id: &RetrievalAnchorId,
        owner: &RetrievalAnchorOwnerV1,
    ) -> impl Future<Output = RetrievalAnchorStoreResult<Option<RetrievalAnchorTombstoneV1>>> + Send;

    fn derivatives(
        &self,
        anchor_id: &RetrievalAnchorId,
        owner: &RetrievalAnchorOwnerV1,
    ) -> impl Future<Output = RetrievalAnchorStoreResult<Vec<RetrievalAnchorDerivativeV1>>> + Send;
}

fn validate_label(value: &str, field: &'static str) -> RetrievalAnchorStoreResult<()> {
    if !is_canonical_text_within(value, CANONICAL_TEXT_MAX_BYTES) {
        return Err(invalid(format!("{field} is not canonical")));
    }
    Ok(())
}

fn domain(error: impl std::fmt::Display) -> RetrievalAnchorStoreError {
    invalid(error.to_string())
}

fn invalid(message: impl Into<String>) -> RetrievalAnchorStoreError {
    RetrievalAnchorStoreError::InvalidData(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_domain::{PrivacyDomainId, ProjectId, UserProfileId};

    fn owner() -> FactOwnerV1 {
        FactOwnerV1::Project {
            project_id: ProjectId::new("project.fixture").unwrap(),
        }
    }

    const ALL_STATES: [AnchorDispositionStateV1; 7] = [
        AnchorDispositionStateV1::Active,
        AnchorDispositionStateV1::Superseded,
        AnchorDispositionStateV1::Redacted,
        AnchorDispositionStateV1::Expired,
        AnchorDispositionStateV1::Quarantined,
        AnchorDispositionStateV1::Deleted,
        AnchorDispositionStateV1::Unavailable,
    ];

    /// Pins the full transition matrix. Both SQLite writers call this one
    /// function, so this test is the only place the matrix is asserted; a
    /// divergence between the two engines is now impossible by construction,
    /// and a deliberate change to the matrix has to be made here.
    #[test]
    fn the_disposition_matrix_is_exhaustively_pinned() {
        for next in ALL_STATES {
            assert!(
                AnchorDispositionStateV1::transition_allowed(None, next),
                "a fresh anchor must accept {next:?}"
            );
            for terminal in [
                AnchorDispositionStateV1::Redacted,
                AnchorDispositionStateV1::Expired,
                AnchorDispositionStateV1::Deleted,
            ] {
                assert!(
                    !AnchorDispositionStateV1::transition_allowed(Some(terminal), next),
                    "{terminal:?} is terminal and must refuse {next:?}"
                );
            }
            for recoverable in [
                AnchorDispositionStateV1::Active,
                AnchorDispositionStateV1::Quarantined,
                AnchorDispositionStateV1::Unavailable,
            ] {
                assert!(
                    AnchorDispositionStateV1::transition_allowed(Some(recoverable), next),
                    "{recoverable:?} is recoverable and must accept {next:?}"
                );
            }
            assert_eq!(
                AnchorDispositionStateV1::transition_allowed(
                    Some(AnchorDispositionStateV1::Superseded),
                    next
                ),
                next == AnchorDispositionStateV1::Deleted,
                "a superseded anchor may only be deleted, never resurrected"
            );
        }
    }

    #[test]
    fn only_unrecoverable_states_suppress_lineage() {
        for state in ALL_STATES {
            let suppresses = state.suppresses_derivatives();
            assert_eq!(
                suppresses,
                matches!(
                    state,
                    AnchorDispositionStateV1::Superseded
                        | AnchorDispositionStateV1::Redacted
                        | AnchorDispositionStateV1::Expired
                        | AnchorDispositionStateV1::Deleted
                ),
                "{state:?} classified against the wrong lineage rule"
            );
            // Suppression is permanent, so anything that suppresses must also
            // refuse to serve; the converse does not hold, because a
            // recoverable outage stops serving without tombstoning.
            assert!(
                !suppresses || !AnchorDispositionStateV1::serves_derivatives(Some(state)),
                "{state:?} tombstones lineage yet still claims to serve it"
            );
        }
        assert!(AnchorDispositionStateV1::serves_derivatives(None));
        assert!(AnchorDispositionStateV1::serves_derivatives(Some(
            AnchorDispositionStateV1::Active
        )));
        assert!(!AnchorDispositionStateV1::serves_derivatives(Some(
            AnchorDispositionStateV1::Quarantined
        )));
        assert!(!AnchorDispositionStateV1::serves_derivatives(Some(
            AnchorDispositionStateV1::Unavailable
        )));
    }

    #[test]
    fn tombstones_admit_only_terminal_safe_states() {
        let anchor = RetrievalAnchorId::new("retrieval.fixture").unwrap();
        assert!(
            RetrievalAnchorTombstoneV1::new(
                anchor.clone(),
                owner(),
                AnchorDispositionStateV1::Deleted,
                AnchorDispositionReasonClassV1::UserRequest,
                UtcMicros(1),
            )
            .is_ok()
        );
        assert!(matches!(
            RetrievalAnchorTombstoneV1::new(
                anchor,
                owner(),
                AnchorDispositionStateV1::Active,
                AnchorDispositionReasonClassV1::Correction,
                UtcMicros(1),
            ),
            Err(RetrievalAnchorStoreError::InvalidData(_))
        ));
    }

    #[test]
    fn authority_owner_preserves_v2_wire_and_admits_exact_v3_owner() {
        let legacy = owner();
        let authority = RetrievalAnchorOwnerV1::from(legacy.clone());
        assert_eq!(
            serde_json::to_value(&authority).unwrap(),
            serde_json::to_value(&legacy).unwrap()
        );
        assert_eq!(
            serde_json::from_value::<RetrievalAnchorOwnerV1>(
                serde_json::to_value(&legacy).unwrap()
            )
            .unwrap(),
            authority
        );

        let v3 = AnchorOwnerBindingV1::for_project(
            UserProfileId::new("profile.fixture").unwrap(),
            ProjectId::new("project.fixture").unwrap(),
            PrivacyDomainId::new("privacy.fixture").unwrap(),
        )
        .unwrap();
        let authority = RetrievalAnchorOwnerV1::from(v3.clone());
        assert_eq!(
            serde_json::to_value(&authority).unwrap(),
            serde_json::to_value(&v3).unwrap()
        );
        assert_eq!(authority.v3(), Some(&v3));
    }
}
