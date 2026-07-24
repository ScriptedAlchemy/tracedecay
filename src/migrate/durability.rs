//! Store durability classification for the upgrade/migration path.
//!
//! Diagnosed failure: `cargo dogfood` on a real 91GB profile failed because
//! the post-update health pass tried to mount and repair a 15GB
//! `sessions.db`, which triggered a full-table rewrite of the `observations`
//! table mid-migration. That rewrite was interrupted, the mount failed, and
//! because the health pass's `--strict` gate treated *every* warning as
//! fatal, the whole upgrade failed -- recording
//! `outcome=forward-recovery-required` and disabling the daemon.
//!
//! The root cause: the upgrade path treated every store as equally precious.
//! It is not. This module gives the migration path a typed vocabulary for
//! that difference, mirroring the product contract in
//! `docs/plans/tracedecay-v2/38-storage-retention-size-and-efficiency.md`:
//!
//! - [`StoreDurabilityClass::Derived`] -- fully rebuildable from the git
//!   checkout (branch code-graph DBs, `code-index-v1/`, FTS shadow tables
//!   over code). Never migrate; drop and reindex instead.
//! - [`StoreDurabilityClass::Durable`] -- irreplaceable curated knowledge
//!   (`user-memory.db`, per-project `memory_*` tables, the `global.db`
//!   registry, configuration tables). Migrate carefully, with verification;
//!   never skip, drop, or silently lose it.
//! - [`StoreDurabilityClass::Recoverable`] -- re-ingestable from an upstream
//!   source of truth (provider JSONL transcripts, and the sanitized/derived
//!   evidence built from them: `lcm_raw_messages`, `session_messages`,
//!   `observations`, `retrieval_anchors`,
//!   `observation_repository_provenance`). Migrate opportunistically; a
//!   failure or interruption here must never block or fail an upgrade.
//!
//! # Safety guarantee
//!
//! [`shard_kind_durability_class`] and [`session_authority_table_class`] are
//! exhaustive/closed-default: [`StoreShardKind`] is matched without a
//! wildcard arm (a new shard scope variant fails to compile until it is
//! deliberately classified), and any table name not on the explicit
//! `Recoverable` allow-list defaults to `Durable`. Nothing gets treated as
//! safe to lose or skip by omission -- only by a reviewed, named entry in
//! this file.

use tracedecay_store::StoreShardScopeV1;

/// How precious a store's data is to the owner, and therefore how the
/// upgrade/migration path is permitted to treat it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StoreDurabilityClass {
    /// Fully rebuildable from the git checkout. An upgrade must never
    /// migrate this data in place -- if its schema changed, drop and
    /// reindex instead of rewriting rows.
    Derived,
    /// Irreplaceable curated knowledge. An upgrade must migrate this
    /// carefully, with verification, and must never skip, drop, or
    /// silently lose it. A failure here is worth blocking the upgrade over.
    Durable,
    /// Re-ingestable from an upstream source of truth (provider JSONL, or
    /// re-derivable from other `Recoverable`/raw data). An upgrade may
    /// migrate this opportunistically -- best effort -- but a failure or
    /// interruption here must never block or fail the upgrade.
    Recoverable,
}

impl StoreDurabilityClass {
    /// Whether a failure to migrate/mount/repair data of this class is
    /// worth failing a `--strict` upgrade over. Only [`Self::Durable`] data
    /// qualifies -- it is the only class this model treats as irreplaceable.
    pub const fn may_block_upgrade(self) -> bool {
        matches!(self, Self::Durable)
    }

    /// Whether this class may be handled best-effort: skipped, retried
    /// later, or (for [`Self::Derived`]) dropped and rebuilt outright,
    /// without operator intervention.
    pub const fn is_opportunistic(self) -> bool {
        matches!(self, Self::Derived | Self::Recoverable)
    }
}

/// Mirrors [`tracedecay_store::StoreShardScopeV1`]'s cases without carrying
/// its identifiers, so a caller can classify "the kind of store this is"
/// without constructing a real project/repository/worktree id first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StoreShardKind {
    /// The profile-level `global.db` registry and configuration store.
    Profile,
    /// The profile-level `user-memory.db` curated-memory store.
    ProfileMemory,
    /// The profile-level sessions/transcript/observation-authority store.
    ProfileSessions,
    /// A project's graph database: code graph (derived) plus that project's
    /// curated `memory_*` tables (durable) in one physical file.
    Project,
    /// A project's sessions/transcript/observation-authority store.
    ProjectSessions,
    /// A code-graph shard for one worktree, branch, or immutable snapshot.
    Code,
}

impl From<&StoreShardScopeV1> for StoreShardKind {
    fn from(scope: &StoreShardScopeV1) -> Self {
        match scope {
            StoreShardScopeV1::Profile => Self::Profile,
            StoreShardScopeV1::ProfileMemory => Self::ProfileMemory,
            StoreShardScopeV1::ProfileSessions => Self::ProfileSessions,
            StoreShardScopeV1::Project { .. } => Self::Project,
            StoreShardScopeV1::ProjectSessions { .. } => Self::ProjectSessions,
            StoreShardScopeV1::Code { .. } => Self::Code,
        }
    }
}

/// Classifies a whole physical store by its shard kind. Exhaustive over
/// [`StoreShardKind`] -- no wildcard arm -- so a new shard scope added to
/// `tracedecay_store` fails to compile here until someone deliberately
/// decides its class, rather than silently inheriting one.
///
/// [`StoreShardKind::Project`] and [`StoreShardKind::ProjectSessions`] /
/// [`StoreShardKind::ProfileSessions`] each name one physical file that
/// mixes durability classes at the table level (a project's graph DB holds
/// both derived code-graph tables and durable `memory_*` tables; a sessions
/// store holds both small authority bookkeeping and bulk recoverable
/// evidence). This function answers "is it ever safe to treat *the whole
/// file* as blocking, or as droppable" -- for `Project` the conservative
/// answer is `Durable` (it contains data that must never be dropped, so no
/// whole-file operation may treat it as disposable); for the sessions
/// stores the answer is `Recoverable`, matching the diagnosed failure and
/// plan 38's storage-retention contract, because the tables that actually
/// dominate their size are all re-ingestable or re-derivable (see
/// [`session_authority_table_class`]). Callers that need to reason about one
/// table inside a mixed store should classify that table directly instead
/// of relying on the whole-store verdict.
pub const fn shard_kind_durability_class(kind: StoreShardKind) -> StoreDurabilityClass {
    match kind {
        StoreShardKind::Profile | StoreShardKind::ProfileMemory | StoreShardKind::Project => {
            StoreDurabilityClass::Durable
        }
        StoreShardKind::ProfileSessions | StoreShardKind::ProjectSessions => {
            StoreDurabilityClass::Recoverable
        }
        StoreShardKind::Code => StoreDurabilityClass::Derived,
    }
}

/// Convenience wrapper over [`shard_kind_durability_class`] for callers that
/// already hold a real [`StoreShardScopeV1`].
pub fn shard_scope_durability_class(scope: &StoreShardScopeV1) -> StoreDurabilityClass {
    shard_kind_durability_class(StoreShardKind::from(scope))
}

/// Classifies one table inside the session/observation-authority schema
/// applied to `sessions.db` (see `src/global_db/schema_stages.rs`'s
/// `ensure_registered_schema` and `src/sessions/lcm/schema.rs`).
///
/// This is the evidence behind [`StoreShardKind::ProjectSessions`] /
/// [`StoreShardKind::ProfileSessions`] being classified `Recoverable`: these
/// are the tables plan 38 measured as the bulk of a 15GB `sessions.db`
/// (`lcm_raw_messages` 3.8GB, `session_messages` 2.4GB, `observations`
/// 1.8GB, `retrieval_anchors` 1.6GB, `observation_repository_provenance`
/// 1.4GB, plus their FTS shadow), and every one is either raw transcript
/// content re-ingestable from provider JSONL, or evidence re-derivable by
/// re-running sanitization/projection over that raw content.
///
/// Any table name not on this list defaults to `Durable` -- the safe
/// default -- so a newly added table is protected until someone
/// deliberately proves it disposable.
pub fn session_authority_table_class(table: &str) -> StoreDurabilityClass {
    match table {
        "lcm_raw_messages"
        | "session_messages"
        | "session_messages_fts"
        | "sessions"
        | "turns"
        | "observations"
        | "observation_retrieval_anchors"
        | "observation_repository_provenance"
        | "retrieval_anchors" => StoreDurabilityClass::Recoverable,
        _ => StoreDurabilityClass::Durable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_domain::{BrainId, ProjectId, RepositoryId, UserProfileId, WorktreeId};
    use tracedecay_store::CodeShardScopeV1;

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        <T as TryFrom<String>>::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).unwrap()
    }

    #[test]
    fn only_durable_may_block_an_upgrade() {
        assert!(StoreDurabilityClass::Durable.may_block_upgrade());
        assert!(!StoreDurabilityClass::Derived.may_block_upgrade());
        assert!(!StoreDurabilityClass::Recoverable.may_block_upgrade());
    }

    #[test]
    fn only_derived_and_recoverable_are_opportunistic() {
        assert!(!StoreDurabilityClass::Durable.is_opportunistic());
        assert!(StoreDurabilityClass::Derived.is_opportunistic());
        assert!(StoreDurabilityClass::Recoverable.is_opportunistic());
    }

    #[test]
    fn profile_and_profile_memory_and_project_are_durable() {
        assert_eq!(
            shard_kind_durability_class(StoreShardKind::Profile),
            StoreDurabilityClass::Durable
        );
        assert_eq!(
            shard_kind_durability_class(StoreShardKind::ProfileMemory),
            StoreDurabilityClass::Durable
        );
        assert_eq!(
            shard_kind_durability_class(StoreShardKind::Project),
            StoreDurabilityClass::Durable
        );
    }

    #[test]
    fn session_stores_are_recoverable_never_durable() {
        assert_eq!(
            shard_kind_durability_class(StoreShardKind::ProfileSessions),
            StoreDurabilityClass::Recoverable
        );
        assert_eq!(
            shard_kind_durability_class(StoreShardKind::ProjectSessions),
            StoreDurabilityClass::Recoverable
        );
        // The diagnosed bug: mounting/migrating a sessions store must never
        // be able to block a strict upgrade.
        assert!(!shard_kind_durability_class(StoreShardKind::ProjectSessions).may_block_upgrade());
    }

    #[test]
    fn code_shards_are_derived() {
        assert_eq!(
            shard_kind_durability_class(StoreShardKind::Code),
            StoreDurabilityClass::Derived
        );
    }

    #[test]
    fn real_shard_scopes_convert_to_the_expected_kind() {
        let brain = id::<BrainId>("brain.durability-test");
        let profile = id::<UserProfileId>("profile.durability-test");
        let project = id::<ProjectId>("project.durability-test");
        let repository = id::<RepositoryId>("repository.durability-test");
        let worktree = id::<WorktreeId>("worktree.durability-test");

        assert_eq!(
            StoreShardKind::from(&StoreShardScopeV1::Profile),
            StoreShardKind::Profile
        );
        assert_eq!(
            StoreShardKind::from(&StoreShardScopeV1::ProfileMemory),
            StoreShardKind::ProfileMemory
        );
        assert_eq!(
            StoreShardKind::from(&StoreShardScopeV1::ProfileSessions),
            StoreShardKind::ProfileSessions
        );
        assert_eq!(
            StoreShardKind::from(&StoreShardScopeV1::Project {
                project_id: project.clone(),
            }),
            StoreShardKind::Project
        );
        assert_eq!(
            StoreShardKind::from(&StoreShardScopeV1::ProjectSessions {
                project_id: project.clone(),
            }),
            StoreShardKind::ProjectSessions
        );
        assert_eq!(
            StoreShardKind::from(&StoreShardScopeV1::Code {
                project_id: project,
                repository_id: repository,
                scope: CodeShardScopeV1::Worktree {
                    worktree_id: worktree,
                },
            }),
            StoreShardKind::Code
        );
        let _ = brain;
        let _ = profile;
    }

    #[test]
    fn session_authority_bulk_tables_are_recoverable() {
        for table in [
            "lcm_raw_messages",
            "session_messages",
            "session_messages_fts",
            "sessions",
            "turns",
            "observations",
            "observation_retrieval_anchors",
            "observation_repository_provenance",
            "retrieval_anchors",
        ] {
            assert_eq!(
                session_authority_table_class(table),
                StoreDurabilityClass::Recoverable,
                "{table} must be classified Recoverable"
            );
        }
    }

    #[test]
    fn unrecognized_and_registry_tables_default_to_durable() {
        for table in [
            "code_projects",
            "store_instances",
            "configuration_entries",
            "some_future_table_nobody_classified_yet",
        ] {
            assert_eq!(
                session_authority_table_class(table),
                StoreDurabilityClass::Durable,
                "{table} must default to Durable"
            );
        }
    }
}
