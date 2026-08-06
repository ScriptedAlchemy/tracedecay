//! Store durability classification for the upgrade/migration path.
//!
//! A large profile update tried to mount and repair a 15GB `sessions.db`,
//! triggering a full-table rewrite of the `observations` table mid-migration.
//! The rewrite was interrupted and the mount failed, even though the store's
//! bulk transcript and evidence data could be safely retried later.
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
    /// A Remote Brain node's enrollment, authority, and encrypted replay state.
    RemoteNode,
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
            StoreShardScopeV1::RemoteNode { .. } => Self::RemoteNode,
            StoreShardScopeV1::Project { .. } => Self::Project,
            StoreShardScopeV1::ProjectSessions { .. } => Self::ProjectSessions,
            StoreShardScopeV1::Code { .. } => Self::Code,
        }
    }
}

/// Classifies a whole physical store by its shard kind, **for the single
/// question of how a failure touching it should be escalated**. Exhaustive
/// over [`StoreShardKind`] -- no wildcard arm -- so a new shard scope added
/// to `tracedecay_store` fails to compile here until someone deliberately
/// decides its class, rather than silently inheriting one.
///
/// # This is an escalation verdict, not a deletion permit
///
/// Three of these kinds name one physical file that mixes durability classes
/// at the table level:
///
/// - [`StoreShardKind::Project`] -- derived code-graph tables *and* durable
///   `memory_*` tables in one graph DB.
/// - [`StoreShardKind::ProjectSessions`] / [`StoreShardKind::ProfileSessions`]
///   -- bulk recoverable transcript/observation evidence, but also the
///   registry, configuration, savings-ledger and analytics tables that
///   `ensure_registered_schema` (`src/global_db/schema_stages.rs`) applies to
///   every sessions store, all of which [`session_authority_table_class`]
///   itself calls `Durable`.
///
/// The sessions stores are `Recoverable` here because the operations this
/// verdict gates -- mount, repair, migrate -- are *non-destructive retries*:
/// failing one leaves every byte on disk for the next open, so escalating it
/// to a fatal upgrade failure buys nothing and cost the owner a disabled
/// daemon (see the module docs). `Project` is `Durable` because its
/// dominant tables are the durable ones.
///
/// **A `Recoverable` verdict here therefore never means "safe to delete this
/// file".** Any caller contemplating a whole-file destructive operation must
/// ask [`whole_store_may_be_dropped`] instead, which is closed over
/// [`StoreShardKind::mixes_durability_classes`] and answers `false` for every
/// mixed store regardless of its escalation class.
pub const fn shard_kind_durability_class(kind: StoreShardKind) -> StoreDurabilityClass {
    match kind {
        StoreShardKind::Profile
        | StoreShardKind::ProfileMemory
        | StoreShardKind::RemoteNode
        | StoreShardKind::Project => StoreDurabilityClass::Durable,
        StoreShardKind::ProfileSessions | StoreShardKind::ProjectSessions => {
            StoreDurabilityClass::Recoverable
        }
        StoreShardKind::Code => StoreDurabilityClass::Derived,
    }
}

impl StoreShardKind {
    /// Whether one physical file of this kind holds tables of more than one
    /// [`StoreDurabilityClass`], so that no single whole-file verdict can
    /// describe everything inside it.
    ///
    /// Exhaustive, no wildcard arm: a new shard kind must state this
    /// deliberately rather than inherit "single-class" by omission.
    pub(crate) const fn mixes_durability_classes(self) -> bool {
        match self {
            // Derived code-graph tables plus durable `memory_*` tables.
            Self::Project
            // Bulk recoverable evidence plus the registry/configuration/
            // savings-ledger/analytics tables `ensure_registered_schema`
            // creates in every sessions store.
            | Self::ProfileSessions
            | Self::ProjectSessions => true,
            Self::Profile | Self::ProfileMemory | Self::RemoteNode | Self::Code => false,
        }
    }
}

/// Whether a whole store file of this kind may be dropped and rebuilt outright
/// rather than migrated -- the question a *destructive* whole-file operation
/// must ask.
///
/// This is deliberately stricter than [`shard_kind_durability_class`]: a store
/// that [`StoreShardKind::mixes_durability_classes`] can never be dropped
/// wholesale, however its failures are escalated, because dropping it would
/// take the durable tables sharing the file with it. Only a store that is
/// single-class *and* fully rebuildable from the git checkout qualifies.
pub const fn whole_store_may_be_dropped(kind: StoreShardKind) -> bool {
    !kind.mixes_durability_classes()
        && matches!(
            shard_kind_durability_class(kind),
            StoreDurabilityClass::Derived
        )
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
    // FTS5 keeps its bytes in shadow tables (`<name>_data`, `_idx`,
    // `_docsize`, `_config`, `_content`), not in the virtual table itself.
    // Classifying only `session_messages_fts` matched the empty shell and
    // silently left the ~600MB of measured shadow storage at the Durable
    // default. A shadow inherits its base virtual table's class.
    for suffix in ["_data", "_idx", "_docsize", "_config", "_content"] {
        if let Some(base) = table.strip_suffix(suffix)
            && base.ends_with("_fts")
        {
            return session_authority_table_class(base);
        }
    }
    match table {
        "lcm_raw_messages"
        | "lcm_raw_messages_fts"
        | "session_messages"
        | "session_messages_fts"
        | "sessions"
        | "observations"
        | "observation_retrieval_anchors"
        | "observation_repository_provenance"
        | "observation_projection_dispositions"
        | "retrieval_anchors"
        | "retrieval_anchor_aliases"
        // Ingest cursor positions: rebuilt from scratch when the source
        // JSONL is re-ingested, which is the definition of Recoverable.
        | "source_cursor_advances"
        // Receipts of the sanitization pass over raw messages; re-running
        // sanitization over re-ingested raw content reproduces them.
        | "sanitization_receipts" => StoreDurabilityClass::Recoverable,
        // Deliberately NOT listed despite their measured size (129MB/62MB in
        // the plan-38 profile): `lcm_summary_sources` and `lcm_summary_nodes`
        // are LCM compaction output produced by paid model calls. They are
        // expensive to regenerate, not mechanically re-derivable, so they
        // stay Durable by the default arm.
        _ => StoreDurabilityClass::Durable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_domain::{BrainNodeId, ProjectId, RepositoryId, WorktreeId};
    use tracedecay_store::CodeShardScopeV1;

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        <T as TryFrom<String>>::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).unwrap()
    }

    #[test]
    fn profile_and_profile_memory_remote_node_and_project_are_durable() {
        assert_eq!(
            shard_kind_durability_class(StoreShardKind::Profile),
            StoreDurabilityClass::Durable
        );
        assert_eq!(
            shard_kind_durability_class(StoreShardKind::ProfileMemory),
            StoreDurabilityClass::Durable
        );
        assert_eq!(
            shard_kind_durability_class(StoreShardKind::RemoteNode),
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
            StoreShardKind::from(&StoreShardScopeV1::RemoteNode {
                node_id: id::<BrainNodeId>("node.durability-test"),
            }),
            StoreShardKind::RemoteNode
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
    }

    /// The whole-file destructive question is strictly stronger than the
    /// escalation question: `Recoverable` must never be readable as "safe to
    /// delete this file".
    #[test]
    fn no_mixed_class_store_may_ever_be_dropped_wholesale() {
        for kind in [
            StoreShardKind::Project,
            StoreShardKind::ProfileSessions,
            StoreShardKind::ProjectSessions,
        ] {
            assert!(
                kind.mixes_durability_classes(),
                "{kind:?} shares one file across durability classes"
            );
            assert!(
                !whole_store_may_be_dropped(kind),
                "{kind:?} must never be droppable wholesale -- it would take \
                 the durable tables sharing its file"
            );
        }
        // Specifically the store the health-pass gate calls `Recoverable`.
        assert_eq!(
            shard_kind_durability_class(StoreShardKind::ProjectSessions),
            StoreDurabilityClass::Recoverable
        );
        assert!(!whole_store_may_be_dropped(StoreShardKind::ProjectSessions));
    }

    #[test]
    fn only_code_shards_may_be_dropped_and_rebuilt() {
        assert!(whole_store_may_be_dropped(StoreShardKind::Code));
        for kind in [
            StoreShardKind::Profile,
            StoreShardKind::ProfileMemory,
            StoreShardKind::RemoteNode,
            StoreShardKind::Project,
            StoreShardKind::ProfileSessions,
            StoreShardKind::ProjectSessions,
        ] {
            assert!(
                !whole_store_may_be_dropped(kind),
                "{kind:?} must not be droppable"
            );
        }
    }

    #[test]
    fn session_authority_bulk_tables_are_recoverable() {
        for table in [
            "lcm_raw_messages",
            "session_messages",
            "session_messages_fts",
            "sessions",
            "observations",
            "observation_retrieval_anchors",
            "observation_repository_provenance",
            "retrieval_anchors",
            "retrieval_anchor_aliases",
            "observation_projection_dispositions",
            "source_cursor_advances",
            "sanitization_receipts",
            "lcm_raw_messages_fts",
            // FTS5 shadow tables, where the bytes actually live (the virtual
            // table itself is an empty shell): must inherit the base class.
            "session_messages_fts_data",
            "session_messages_fts_idx",
            "session_messages_fts_docsize",
            "session_messages_fts_config",
            "session_messages_fts_content",
            "lcm_raw_messages_fts_data",
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
            // LCM summaries are paid-model output: expensive to regenerate,
            // not mechanically re-derivable. Deliberately Durable.
            "lcm_summary_sources",
            "lcm_summary_nodes",
            // A `_data` suffix without an `_fts` base is NOT an FTS shadow
            // and must not sneak through the shadow rule.
            "important_data",
            "audit_config",
        ] {
            assert_eq!(
                session_authority_table_class(table),
                StoreDurabilityClass::Durable,
                "{table} must default to Durable"
            );
        }
    }
}
