//! The recorded frontier a Work handoff carries.
//!
//! Plan 24 requires a handoff to record the exact work/evidence frontier,
//! unknowns, blockers, legal actions, and lineage "so rediscovery and
//! reliance can be measured", and requires checkpoint evidence that cannot
//! renew a lease, establish task acceptance, or mutate graph or runtime
//! state. This module owns that record: it is pure typed data with bounded
//! validation and a canonical digest, and it deliberately carries no lease,
//! fence, acceptance, or projection authority a redeemer could replay into
//! runtime state.

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    ActorId, ManifestDigest, TaskId, UtcMicros, WorkAttemptIdentityV1, WorkAttemptStateV1,
    WorkVersion, canonical_sha256,
};

/// Upper bound for each free-text frontier entry, in bytes.
pub const MAX_WORK_HANDOFF_ENTRY_BYTES: usize = 4_096;
/// Upper bound for each frontier list (unknowns, blockers, legal actions,
/// attempts).
pub const MAX_WORK_HANDOFF_ENTRIES: usize = 64;

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum WorkHandoffFrontierError {
    #[error("work handoff frontier entry is empty, oversized, or malformed")]
    InvalidEntry,
    #[error("work handoff frontier list exceeds its bound or repeats an entry")]
    InvalidList,
    #[error("work handoff frontier lineage is inconsistent")]
    InvalidLineage,
    #[error("work handoff frontier could not be canonically digested")]
    DigestUnavailable,
}

/// One attempt on the evidence frontier: exactly which attempt, in which
/// state, backed by which sealed evidence digest (when the attempt has
/// reported one). No lease or fence is part of the frontier — those are
/// runtime authority, not checkpoint evidence.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkHandoffAttemptFrontierV1 {
    pub identity: WorkAttemptIdentityV1,
    pub state: WorkAttemptStateV1,
    /// The digest of the sealed terminal evidence record, when the attempt
    /// has one. `None` is the typed not-yet-reported state.
    pub evidence_digest: Option<ManifestDigest>,
}

/// Who issued this frontier and what it supersedes.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkHandoffLineageV1 {
    pub issued_by: ActorId,
    pub issued_at: UtcMicros,
    /// The canonical digest of the frontier this one supersedes, when the
    /// task has been handed off before. `None` states a first handoff.
    pub prior_frontier_digest: Option<ManifestDigest>,
}

/// The exact work/evidence frontier one handoff records.
#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkHandoffFrontierV1 {
    task_id: TaskId,
    /// The exact Work version the frontier was cut at.
    work_version: WorkVersion,
    /// The evidence frontier: every attempt the issuer knows about, in
    /// stable identity order.
    attempts: Vec<WorkHandoffAttemptFrontierV1>,
    /// What the issuer does not know yet. Bounded, non-empty entries.
    unknowns: Vec<String>,
    /// What is blocking progress. Bounded, non-empty entries.
    blockers: Vec<String>,
    /// The actions the issuer believes are legal next steps.
    legal_actions: Vec<String>,
    lineage: WorkHandoffLineageV1,
}

impl WorkHandoffFrontierV1 {
    pub fn new(
        task_id: TaskId,
        work_version: WorkVersion,
        attempts: Vec<WorkHandoffAttemptFrontierV1>,
        unknowns: Vec<String>,
        blockers: Vec<String>,
        legal_actions: Vec<String>,
        lineage: WorkHandoffLineageV1,
    ) -> Result<Self, WorkHandoffFrontierError> {
        if attempts.len() > MAX_WORK_HANDOFF_ENTRIES {
            return Err(WorkHandoffFrontierError::InvalidList);
        }
        if attempts
            .windows(2)
            .any(|pair| pair[0].identity >= pair[1].identity)
        {
            // Strictly ascending identity order also refuses duplicates.
            return Err(WorkHandoffFrontierError::InvalidList);
        }
        for list in [&unknowns, &blockers, &legal_actions] {
            validate_entry_list(list)?;
        }
        if lineage.issued_at == UtcMicros(0) {
            return Err(WorkHandoffFrontierError::InvalidLineage);
        }
        Ok(Self {
            task_id,
            work_version,
            attempts,
            unknowns,
            blockers,
            legal_actions,
            lineage,
        })
    }

    pub fn task_id(&self) -> &TaskId {
        &self.task_id
    }

    pub const fn work_version(&self) -> WorkVersion {
        self.work_version
    }

    pub fn attempts(&self) -> &[WorkHandoffAttemptFrontierV1] {
        &self.attempts
    }

    pub fn unknowns(&self) -> &[String] {
        &self.unknowns
    }

    pub fn blockers(&self) -> &[String] {
        &self.blockers
    }

    pub fn legal_actions(&self) -> &[String] {
        &self.legal_actions
    }

    pub fn lineage(&self) -> &WorkHandoffLineageV1 {
        &self.lineage
    }

    /// The canonical content digest of this frontier; lineage chains hold
    /// exactly this value.
    pub fn digest(&self) -> Result<ManifestDigest, WorkHandoffFrontierError> {
        canonical_sha256(self).map_err(|_| WorkHandoffFrontierError::DigestUnavailable)
    }
}

impl<'de> Deserialize<'de> for WorkHandoffFrontierV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            task_id: TaskId,
            work_version: WorkVersion,
            attempts: Vec<WorkHandoffAttemptFrontierV1>,
            unknowns: Vec<String>,
            blockers: Vec<String>,
            legal_actions: Vec<String>,
            lineage: WorkHandoffLineageV1,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::new(
            wire.task_id,
            wire.work_version,
            wire.attempts,
            wire.unknowns,
            wire.blockers,
            wire.legal_actions,
            wire.lineage,
        )
        .map_err(serde::de::Error::custom)
    }
}

fn validate_entry_list(entries: &[String]) -> Result<(), WorkHandoffFrontierError> {
    if entries.len() > MAX_WORK_HANDOFF_ENTRIES {
        return Err(WorkHandoffFrontierError::InvalidList);
    }
    for entry in entries {
        if entry.is_empty() || entry.len() > MAX_WORK_HANDOFF_ENTRY_BYTES || entry.contains('\0') {
            return Err(WorkHandoffFrontierError::InvalidEntry);
        }
    }
    Ok(())
}
