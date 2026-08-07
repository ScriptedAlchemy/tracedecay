//! Execution-placement lowering for admitted Work runs.
//!
//! Plan 32 (`docs/plans/tracedecay-v2/32-dynamic-workflow-runtime-and-sdk.md`,
//! "Placement, topology, and safe Git effects") fixes the supported set:
//! "no managed placement, explicitly acknowledged strictly clean in-place,
//! linked worktree, or isolated local clone", and requires linked and isolated
//! placements to be "canonical, exclusive, fenced, network-free where declared,
//! and retained/quarantined rather than cleaned when dirty, conflicted,
//! unknown, or uniquely valuable". Plan 24 ("Optional topology, placement,
//! review, and integration") adds that placement is "an independent versioned
//! relation attached to a work-item version" and that "changing any of them
//! preserves TaskId".
//!
//! Three rules are structural here rather than documented:
//!
//! 1. **Release is not delete.** [`WorkPlacementV1::release`] publishes either
//!    `Released` or `Quarantined`; it never reports a removal. The plan is
//!    explicit that "retention expiry is eligibility for a fresh cleanup
//!    preflight, not delete authority".
//! 2. **A blocker set is the reason, not a boolean.** Every refusal names the
//!    exact typed blockers observed, so "we did not look" cannot be spelled the
//!    same way as "nothing blocks".
//! 3. **In-place is acknowledged, never inferred.** `CleanInPlace` carries an
//!    explicit acknowledgement flag; a placement cannot become in-place because
//!    a caller left the target root empty.

use std::collections::BTreeSet;
use std::path::Path;

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

use crate::{RunId, TaskId, UtcMicros};

/// Ceiling on a placement target path, matching the execution envelope's.
pub const MAX_WORK_PLACEMENT_ROOT_BYTES: usize = 4_096;

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum WorkPlacementContractError {
    #[error("Work placement target root is required for this placement kind")]
    MissingTargetRoot,
    #[error("Work placement target root is not permitted for this placement kind")]
    UnexpectedTargetRoot,
    #[error("Work placement target root must be an absolute path within its bound")]
    InvalidTargetRoot,
    #[error("clean in-place execution must be explicitly acknowledged")]
    UnacknowledgedInPlace,
    #[error("Work placement cannot be admitted while blockers are observed")]
    BlockedPlacement,
    #[error("Work placement authority version must be non-zero")]
    InvalidAuthorityVersion,
    #[error("Work placement authority version overflowed")]
    AuthorityVersionOverflow,
    #[error("Work placement transition moved backwards in time")]
    NonMonotonicTransition,
    #[error("Work placement is already released")]
    AlreadyReleased,
    #[error("a quarantined placement records at least one blocker")]
    QuarantineWithoutBlocker,
}

/// The supported placement choices, and only those.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
#[schemars(title = "WorkPlacementKindV1")]
pub enum WorkPlacementKindV1 {
    /// TraceDecay manages no checkout for this run. No-Git work is first class.
    NoManagedPlacement,
    /// The caller's own strictly clean checkout, explicitly acknowledged.
    CleanInPlace,
    /// A linked worktree of the same repository.
    LinkedWorktree,
    /// An isolated local clone.
    IsolatedClone,
}

impl WorkPlacementKindV1 {
    /// Whether this kind names a filesystem root TraceDecay manages.
    pub const fn requires_target_root(self) -> bool {
        matches!(self, Self::LinkedWorktree | Self::IsolatedClone)
    }

    /// Whether at most one admitted placement may hold this root at a time.
    ///
    /// Linked and isolated placements are canonical and exclusive; in-place
    /// execution is the caller's own checkout and TraceDecay does not claim it.
    pub const fn is_exclusive(self) -> bool {
        self.requires_target_root()
    }
}

/// Exactly the conditions Plan 32 names as blocking admission or removal.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
#[schemars(title = "WorkPlacementBlockerV1")]
pub enum WorkPlacementBlockerV1 {
    /// Tracked files differ from the index or HEAD.
    DirtyTrackedFiles,
    /// Untracked data is present in the target.
    UntrackedData,
    /// The target holds commits reachable from nowhere else.
    UniqueCommits,
    /// Another admitted placement holds this target.
    ActiveHolder,
    /// An effect against this placement is unresolved.
    UnresolvedEffect,
    /// A receipt produced against this placement is unacknowledged.
    UnacknowledgedReceipt,
    /// A pull request produced from this placement is in an uncertain state.
    UncertainPullRequest,
    /// A ref in this target is shared with another holder.
    SharedRef,
    /// A retrieval or evidence anchor this placement referenced is missing.
    MissingAnchor,
    /// The authorized scope no longer matches the placement.
    StaleScope,
    /// Authorization for this placement was lost.
    AuthorizationLost,
    /// The target could not be read, so its state is unknown.
    TargetUnreadable,
    /// The placement declared itself network-free but the action needs network.
    NetworkRequired,
}

/// Which run a placement belongs to. Placement never redefines TaskId.
#[derive(
    Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(deny_unknown_fields)]
#[schemars(title = "WorkPlacementIdentityV1")]
pub struct WorkPlacementIdentityV1 {
    task_id: TaskId,
    run_id: RunId,
}

impl WorkPlacementIdentityV1 {
    pub const fn new(task_id: TaskId, run_id: RunId) -> Self {
        Self { task_id, run_id }
    }

    pub fn task_id(&self) -> &TaskId {
        &self.task_id
    }

    pub fn run_id(&self) -> &RunId {
        &self.run_id
    }
}

/// The exact placement a caller asked for.
#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[schemars(title = "WorkPlacementTargetV1")]
pub struct WorkPlacementTargetV1 {
    kind: WorkPlacementKindV1,
    /// Absolute root, present exactly when the kind manages one.
    root: Option<String>,
    /// Set only for `CleanInPlace`: the caller states it accepts running in
    /// its own checkout. Plan 32 calls this "explicitly acknowledged".
    in_place_acknowledged: bool,
    /// The placement declares it needs no network. Declared, not detected.
    network_free: bool,
}

impl WorkPlacementTargetV1 {
    pub fn new(
        kind: WorkPlacementKindV1,
        root: Option<String>,
        in_place_acknowledged: bool,
        network_free: bool,
    ) -> Result<Self, WorkPlacementContractError> {
        match (kind.requires_target_root(), root.as_deref()) {
            (true, None) => return Err(WorkPlacementContractError::MissingTargetRoot),
            (false, Some(_)) => return Err(WorkPlacementContractError::UnexpectedTargetRoot),
            (true, Some(root)) => {
                if root.is_empty()
                    || root.len() > MAX_WORK_PLACEMENT_ROOT_BYTES
                    || root.contains('\0')
                    || !Path::new(root).is_absolute()
                {
                    return Err(WorkPlacementContractError::InvalidTargetRoot);
                }
            }
            (false, None) => {}
        }
        if kind == WorkPlacementKindV1::CleanInPlace && !in_place_acknowledged {
            return Err(WorkPlacementContractError::UnacknowledgedInPlace);
        }
        Ok(Self {
            kind,
            root,
            in_place_acknowledged,
            network_free,
        })
    }

    pub const fn kind(&self) -> WorkPlacementKindV1 {
        self.kind
    }

    pub fn root(&self) -> Option<&str> {
        self.root.as_deref()
    }

    pub const fn in_place_acknowledged(&self) -> bool {
        self.in_place_acknowledged
    }

    pub const fn network_free(&self) -> bool {
        self.network_free
    }
}

impl<'de> Deserialize<'de> for WorkPlacementTargetV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            kind: WorkPlacementKindV1,
            #[serde(default)]
            root: Option<String>,
            #[serde(default)]
            in_place_acknowledged: bool,
            #[serde(default)]
            network_free: bool,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::new(
            wire.kind,
            wire.root,
            wire.in_place_acknowledged,
            wire.network_free,
        )
        .map_err(serde::de::Error::custom)
    }
}

/// What was actually observed at the target, in counts rather than prose.
///
/// Counts are measurements: a zero is "we looked and found none", and
/// `readable: false` is the separate state for "we could not look". Collapsing
/// them would let an unreadable target read as a clean one.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[schemars(title = "WorkPlacementObservationV1")]
pub struct WorkPlacementObservationV1 {
    /// Tracked paths that differ from the index or HEAD.
    pub dirty_tracked_paths: u32,
    /// Untracked paths present in the target.
    pub untracked_paths: u32,
    /// Commits in the target reachable from nowhere else.
    ///
    /// `None` means reachability was not measured, and it blocks removal
    /// exactly as a positive count does: Plan 32 forbids cleaning when the
    /// state is "unknown", so an unmeasured target is retained rather than
    /// assumed worthless. Only removal consults this; admission does not.
    pub unique_commits: Option<u32>,
    /// Whether the target could be read at all.
    pub readable: bool,
    /// Whether another admitted placement already holds this target.
    pub active_holder: bool,
    /// Whether the declared network-free placement would require network.
    pub network_required: bool,
    pub observed_at: UtcMicros,
}

impl WorkPlacementObservationV1 {
    /// The typed blockers this observation implies, in stable order.
    ///
    /// An unreadable target yields exactly `TargetUnreadable` and no cleanliness
    /// claim, because counts taken from a target that could not be read would
    /// be fabricated.
    pub fn blockers(&self, target: &WorkPlacementTargetV1) -> BTreeSet<WorkPlacementBlockerV1> {
        let mut blockers = BTreeSet::new();
        if !self.readable {
            blockers.insert(WorkPlacementBlockerV1::TargetUnreadable);
            if self.active_holder {
                blockers.insert(WorkPlacementBlockerV1::ActiveHolder);
            }
            return blockers;
        }
        if self.active_holder {
            blockers.insert(WorkPlacementBlockerV1::ActiveHolder);
        }
        // Cleanliness only constrains a placement that runs in an existing
        // checkout. A fresh linked worktree or clone is created, not adopted,
        // so another checkout's dirt is not its blocker.
        if target.kind() == WorkPlacementKindV1::CleanInPlace {
            if self.dirty_tracked_paths > 0 {
                blockers.insert(WorkPlacementBlockerV1::DirtyTrackedFiles);
            }
            if self.untracked_paths > 0 {
                blockers.insert(WorkPlacementBlockerV1::UntrackedData);
            }
        }
        if target.network_free() && self.network_required {
            blockers.insert(WorkPlacementBlockerV1::NetworkRequired);
        }
        blockers
    }

    /// The typed blockers that forbid *removing* this placement's bytes.
    ///
    /// Removal is judged more strictly than admission, and deliberately so:
    /// Plan 32 forbids cleaning "when dirty, conflicted, unknown, or uniquely
    /// valuable", so dirt in a linked worktree — which does not block creating
    /// one — does block deleting one. An unmanaged placement owns no bytes, so
    /// it has nothing removal could destroy.
    pub fn removal_blockers(
        &self,
        target: &WorkPlacementTargetV1,
    ) -> BTreeSet<WorkPlacementBlockerV1> {
        let mut blockers = BTreeSet::new();
        if !target.kind().requires_target_root() {
            return blockers;
        }
        if !self.readable {
            // Unknown is not empty: a target we cannot read may hold anything.
            blockers.insert(WorkPlacementBlockerV1::TargetUnreadable);
            return blockers;
        }
        if self.dirty_tracked_paths > 0 {
            blockers.insert(WorkPlacementBlockerV1::DirtyTrackedFiles);
        }
        if self.untracked_paths > 0 {
            blockers.insert(WorkPlacementBlockerV1::UntrackedData);
        }
        // A proved zero is the only reading that clears this blocker.
        if self.unique_commits != Some(0) {
            blockers.insert(WorkPlacementBlockerV1::UniqueCommits);
        }
        blockers
    }
}

/// One placement preflight reading: what was asked for, what was seen, and
/// exactly what blocks it.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[schemars(title = "WorkPlacementPreflightV1")]
pub struct WorkPlacementPreflightV1 {
    pub identity: WorkPlacementIdentityV1,
    pub target: WorkPlacementTargetV1,
    pub observation: WorkPlacementObservationV1,
    pub blockers: BTreeSet<WorkPlacementBlockerV1>,
}

impl WorkPlacementPreflightV1 {
    pub fn evaluate(
        identity: WorkPlacementIdentityV1,
        target: WorkPlacementTargetV1,
        observation: WorkPlacementObservationV1,
    ) -> Self {
        let blockers = observation.blockers(&target);
        Self {
            identity,
            target,
            observation,
            blockers,
        }
    }

    /// Whether admission may proceed. Never true with a blocker present.
    pub fn is_admissible(&self) -> bool {
        self.blockers.is_empty()
    }
}

/// The durable state of one admitted placement.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[schemars(title = "WorkPlacementStateV1")]
pub enum WorkPlacementStateV1 {
    /// The placement holds its target.
    Admitted,
    /// The placement gave its target up cleanly. Nothing was deleted by this
    /// transition; removal is a separate cleanup preflight.
    Released,
    /// The placement was retained because removal is blocked.
    Quarantined,
}

/// One run's durable placement relation.
#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[schemars(title = "WorkPlacementV1")]
pub struct WorkPlacementV1 {
    identity: WorkPlacementIdentityV1,
    target: WorkPlacementTargetV1,
    state: WorkPlacementStateV1,
    authority_version: u64,
    transitioned_at: UtcMicros,
    blockers: BTreeSet<WorkPlacementBlockerV1>,
    /// When retention makes this placement *eligible* for a fresh cleanup
    /// preflight. Eligibility is not delete authority.
    retention_eligible_at: Option<UtcMicros>,
}

impl WorkPlacementV1 {
    /// Admits a placement from an unblocked preflight.
    pub fn admit(
        preflight: &WorkPlacementPreflightV1,
        retention_eligible_at: Option<UtcMicros>,
        occurred_at: UtcMicros,
    ) -> Result<Self, WorkPlacementContractError> {
        if !preflight.is_admissible() {
            return Err(WorkPlacementContractError::BlockedPlacement);
        }
        Ok(Self {
            identity: preflight.identity.clone(),
            target: preflight.target.clone(),
            state: WorkPlacementStateV1::Admitted,
            authority_version: 1,
            transitioned_at: occurred_at,
            blockers: BTreeSet::new(),
            retention_eligible_at,
        })
    }

    pub fn identity(&self) -> &WorkPlacementIdentityV1 {
        &self.identity
    }

    pub fn target(&self) -> &WorkPlacementTargetV1 {
        &self.target
    }

    pub const fn state(&self) -> WorkPlacementStateV1 {
        self.state
    }

    pub const fn authority_version(&self) -> u64 {
        self.authority_version
    }

    pub const fn transitioned_at(&self) -> UtcMicros {
        self.transitioned_at
    }

    pub fn blockers(&self) -> &BTreeSet<WorkPlacementBlockerV1> {
        &self.blockers
    }

    pub const fn retention_eligible_at(&self) -> Option<UtcMicros> {
        self.retention_eligible_at
    }

    /// Whether this placement still holds its target exclusively.
    pub const fn holds_target(&self) -> bool {
        matches!(self.state, WorkPlacementStateV1::Admitted)
            || matches!(self.state, WorkPlacementStateV1::Quarantined)
    }

    /// Gives the target up, or retains it when removal is blocked.
    ///
    /// A quarantine is not a failure to release: it is the release, with the
    /// exact reasons the bytes were kept. Plan 32 forbids cleaning "when dirty,
    /// conflicted, unknown, or uniquely valuable".
    pub fn release(
        &self,
        blockers: BTreeSet<WorkPlacementBlockerV1>,
        occurred_at: UtcMicros,
    ) -> Result<Self, WorkPlacementContractError> {
        if self.state == WorkPlacementStateV1::Released {
            return Err(WorkPlacementContractError::AlreadyReleased);
        }
        if occurred_at.0 < self.transitioned_at.0 {
            return Err(WorkPlacementContractError::NonMonotonicTransition);
        }
        let state = if blockers.is_empty() {
            WorkPlacementStateV1::Released
        } else {
            WorkPlacementStateV1::Quarantined
        };
        Ok(Self {
            identity: self.identity.clone(),
            target: self.target.clone(),
            state,
            authority_version: self
                .authority_version
                .checked_add(1)
                .ok_or(WorkPlacementContractError::AuthorityVersionOverflow)?,
            transitioned_at: occurred_at,
            blockers,
            retention_eligible_at: self.retention_eligible_at,
        })
    }
}

fn validate_placement(
    state: WorkPlacementStateV1,
    authority_version: u64,
    blockers: &BTreeSet<WorkPlacementBlockerV1>,
) -> Result<(), WorkPlacementContractError> {
    if authority_version == 0 {
        return Err(WorkPlacementContractError::InvalidAuthorityVersion);
    }
    match state {
        WorkPlacementStateV1::Quarantined if blockers.is_empty() => {
            Err(WorkPlacementContractError::QuarantineWithoutBlocker)
        }
        WorkPlacementStateV1::Admitted | WorkPlacementStateV1::Released
            if !blockers.is_empty() =>
        {
            Err(WorkPlacementContractError::BlockedPlacement)
        }
        _ => Ok(()),
    }
}

impl<'de> Deserialize<'de> for WorkPlacementV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            identity: WorkPlacementIdentityV1,
            target: WorkPlacementTargetV1,
            state: WorkPlacementStateV1,
            authority_version: u64,
            transitioned_at: UtcMicros,
            blockers: BTreeSet<WorkPlacementBlockerV1>,
            retention_eligible_at: Option<UtcMicros>,
        }

        let wire = Wire::deserialize(deserializer)?;
        validate_placement(wire.state, wire.authority_version, &wire.blockers)
            .map_err(serde::de::Error::custom)?;
        Ok(Self {
            identity: wire.identity,
            target: wire.target,
            state: wire.state,
            authority_version: wire.authority_version,
            transitioned_at: wire.transitioned_at,
            blockers: wire.blockers,
            retention_eligible_at: wire.retention_eligible_at,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> WorkPlacementIdentityV1 {
        WorkPlacementIdentityV1::new(
            TaskId::new("task.placement").expect("task id"),
            RunId::new("run.placement").expect("run id"),
        )
    }

    fn clean_observation() -> WorkPlacementObservationV1 {
        WorkPlacementObservationV1 {
            dirty_tracked_paths: 0,
            untracked_paths: 0,
            unique_commits: Some(0),
            readable: true,
            active_holder: false,
            network_required: false,
            observed_at: UtcMicros(100),
        }
    }

    fn linked() -> WorkPlacementTargetV1 {
        WorkPlacementTargetV1::new(
            WorkPlacementKindV1::LinkedWorktree,
            Some("/workspace/linked".to_owned()),
            false,
            true,
        )
        .expect("linked target")
    }

    #[test]
    fn only_the_managed_kinds_carry_a_root_and_in_place_must_be_acknowledged() {
        assert_eq!(
            WorkPlacementTargetV1::new(WorkPlacementKindV1::LinkedWorktree, None, false, false)
                .expect_err("linked needs a root"),
            WorkPlacementContractError::MissingTargetRoot
        );
        assert_eq!(
            WorkPlacementTargetV1::new(
                WorkPlacementKindV1::NoManagedPlacement,
                Some("/workspace".to_owned()),
                false,
                false,
            )
            .expect_err("unmanaged placement owns no root"),
            WorkPlacementContractError::UnexpectedTargetRoot
        );
        assert_eq!(
            WorkPlacementTargetV1::new(WorkPlacementKindV1::CleanInPlace, None, false, false)
                .expect_err("in-place must be acknowledged"),
            WorkPlacementContractError::UnacknowledgedInPlace
        );
        assert_eq!(
            WorkPlacementTargetV1::new(
                WorkPlacementKindV1::IsolatedClone,
                Some("relative/path".to_owned()),
                false,
                false,
            )
            .expect_err("a managed root is absolute"),
            WorkPlacementContractError::InvalidTargetRoot
        );
        WorkPlacementTargetV1::new(WorkPlacementKindV1::CleanInPlace, None, true, false)
            .expect("acknowledged in-place");
    }

    #[test]
    fn an_unreadable_target_blocks_without_claiming_cleanliness() {
        let target =
            WorkPlacementTargetV1::new(WorkPlacementKindV1::CleanInPlace, None, true, false)
                .expect("in-place target");
        let observation = WorkPlacementObservationV1 {
            readable: false,
            ..clean_observation()
        };
        let blockers = observation.blockers(&target);
        assert_eq!(
            blockers,
            BTreeSet::from([WorkPlacementBlockerV1::TargetUnreadable])
        );
    }

    #[test]
    fn dirt_blocks_in_place_but_not_a_freshly_created_placement() {
        let dirty = WorkPlacementObservationV1 {
            dirty_tracked_paths: 3,
            untracked_paths: 1,
            ..clean_observation()
        };
        let in_place =
            WorkPlacementTargetV1::new(WorkPlacementKindV1::CleanInPlace, None, true, false)
                .expect("in-place target");
        assert_eq!(
            dirty.blockers(&in_place),
            BTreeSet::from([
                WorkPlacementBlockerV1::DirtyTrackedFiles,
                WorkPlacementBlockerV1::UntrackedData,
            ])
        );
        // A linked worktree is created rather than adopted, so the caller's own
        // dirty checkout is not its blocker.
        assert!(dirty.blockers(&linked()).is_empty());
        // The same dirt does block *removing* that linked worktree: admission
        // and removal are judged by different rules on purpose.
        assert_eq!(
            dirty.removal_blockers(&linked()),
            BTreeSet::from([
                WorkPlacementBlockerV1::DirtyTrackedFiles,
                WorkPlacementBlockerV1::UntrackedData,
            ])
        );
    }

    #[test]
    fn removal_keeps_uniquely_valuable_bytes_and_never_guesses_an_unreadable_target() {
        let valuable = WorkPlacementObservationV1 {
            unique_commits: Some(2),
            ..clean_observation()
        };
        assert_eq!(
            valuable.removal_blockers(&linked()),
            BTreeSet::from([WorkPlacementBlockerV1::UniqueCommits])
        );
        let unknown = WorkPlacementObservationV1 {
            readable: false,
            ..clean_observation()
        };
        assert_eq!(
            unknown.removal_blockers(&linked()),
            BTreeSet::from([WorkPlacementBlockerV1::TargetUnreadable])
        );
        // An unmanaged placement owns no bytes, so removal destroys nothing.
        let unmanaged = WorkPlacementTargetV1::new(
            WorkPlacementKindV1::NoManagedPlacement,
            None,
            false,
            false,
        )
        .expect("unmanaged target");
        assert!(unknown.removal_blockers(&unmanaged).is_empty());
    }

    #[test]
    fn an_active_holder_blocks_and_a_blocked_preflight_cannot_be_admitted() {
        let held = WorkPlacementObservationV1 {
            active_holder: true,
            ..clean_observation()
        };
        let preflight = WorkPlacementPreflightV1::evaluate(identity(), linked(), held);
        assert!(!preflight.is_admissible());
        assert_eq!(
            preflight.blockers,
            BTreeSet::from([WorkPlacementBlockerV1::ActiveHolder])
        );
        assert_eq!(
            WorkPlacementV1::admit(&preflight, None, UtcMicros(200))
                .expect_err("a blocked preflight cannot be admitted"),
            WorkPlacementContractError::BlockedPlacement
        );
    }

    #[test]
    fn a_declared_network_free_placement_blocks_when_network_is_required() {
        let observation = WorkPlacementObservationV1 {
            network_required: true,
            ..clean_observation()
        };
        assert_eq!(
            observation.blockers(&linked()),
            BTreeSet::from([WorkPlacementBlockerV1::NetworkRequired])
        );
    }

    #[test]
    fn release_publishes_released_or_quarantined_and_never_a_removal() {
        let preflight = WorkPlacementPreflightV1::evaluate(identity(), linked(), clean_observation());
        let admitted =
            WorkPlacementV1::admit(&preflight, Some(UtcMicros(5_000)), UtcMicros(200)).expect("admit");
        assert_eq!(admitted.state(), WorkPlacementStateV1::Admitted);
        assert!(admitted.holds_target());

        let quarantined = admitted
            .release(
                BTreeSet::from([WorkPlacementBlockerV1::UniqueCommits]),
                UtcMicros(400),
            )
            .expect("release with blockers");
        assert_eq!(quarantined.state(), WorkPlacementStateV1::Quarantined);
        // The bytes are still held: quarantine is retention, not deletion.
        assert!(quarantined.holds_target());
        assert_eq!(quarantined.authority_version(), 2);
        // Retention eligibility survives the transition; it is not delete
        // authority, so it does not decide the state.
        assert_eq!(quarantined.retention_eligible_at(), Some(UtcMicros(5_000)));

        let released = quarantined
            .release(BTreeSet::new(), UtcMicros(600))
            .expect("a fresh cleanup preflight cleared the blockers");
        assert_eq!(released.state(), WorkPlacementStateV1::Released);
        assert!(!released.holds_target());
        assert_eq!(
            released
                .release(BTreeSet::new(), UtcMicros(700))
                .expect_err("a released placement has nothing left to release"),
            WorkPlacementContractError::AlreadyReleased
        );
    }

    #[test]
    fn the_wire_shape_round_trips_and_refuses_a_quarantine_with_no_reason() {
        let preflight = WorkPlacementPreflightV1::evaluate(identity(), linked(), clean_observation());
        let admitted = WorkPlacementV1::admit(&preflight, None, UtcMicros(200)).expect("admit");
        let quarantined = admitted
            .release(
                BTreeSet::from([WorkPlacementBlockerV1::UnresolvedEffect]),
                UtcMicros(400),
            )
            .expect("quarantine");
        let encoded = serde_json::to_value(&quarantined).expect("encode");
        assert_eq!(
            serde_json::from_value::<WorkPlacementV1>(encoded.clone()).expect("decode"),
            quarantined
        );

        let mut reasonless = encoded;
        reasonless["blockers"] = serde_json::json!([]);
        assert!(serde_json::from_value::<WorkPlacementV1>(reasonless).is_err());
    }
}
