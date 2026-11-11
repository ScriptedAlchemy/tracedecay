use std::borrow::Cow;
use std::future::Future;
use std::pin::Pin;
use std::time::{Duration, Instant};

use tracedecay_domain::errors::ProjectOpenFailureKind;

mod workflow_artifact_ceiling_upgrade;
#[cfg(test)]
pub(crate) use workflow_artifact_ceiling_upgrade::upgrade_published_artifact_ceiling;
use workflow_artifact_ceiling_upgrade::{
    PublishedArtifactCeilingShape, classify_published_artifact_ceiling,
    rehearse_published_artifact_ceiling_upgrade,
};
pub(crate) use workflow_artifact_ceiling_upgrade::{
    reclaim_freed_artifact_pages, recover_published_artifact_ceiling,
};

use super::schema_contract::{
    authority_invariant_triggers_intact, ensure_authority_audit_checkpoint_schema,
    ensure_authority_invariant_schema, ensure_authority_invariants,
    ensure_fresh_authority_invariants, require_foreign_key_audit,
    validate_authority_rows_exhaustive, validate_authority_schema_contract,
    validate_existing_observation_table, validate_registry_schema_contract,
    validate_remote_deletion_schema_contract,
};
use super::{
    configuration, git_index_transactions, global_db_operation_error, global_db_operation_message,
    managed_test_runs, observability_rollup, observation, observation_projection, project_registry,
    session_temporal_schema, stack_delivery,
};
use crate::registered::RefusedAuthorityV1;
use tracedecay_runtime_core::{
    cancellation::CancellationToken,
    db::{
        Database, DatabaseEngineReadConnection, DatabaseWriteTransaction,
        engine::{self, Executor, IntoParams, QueryExecutor, Rows},
    },
    ports::registered_schema::{
        RegisteredSchemaInstallationTransactionV1, RegisteredSchemaInstallationV1,
    },
};
use tracedecay_rusqlite_runtime::handoff::HANDOFF_OPEN_SCHEMA_V1;
use tracedecay_rusqlite_runtime::repository::AUTHORIZED_SCOPE_SET_SCHEMA_V1;
use tracedecay_rusqlite_runtime::runtime_ledger::RUNTIME_LEDGER_SCHEMA;
use tracedecay_rusqlite_runtime::work::{
    WORK_PRODUCT_SCHEMA_V1 as WORK_PRODUCT_GRAPH_JOURNAL_SCHEMA_V1, WORK_SCHEMA_V1,
};
use tracedecay_rusqlite_runtime::workflow::{
    WORKFLOW_SCHEMA_DEFINITION_DIGEST_V1, WORKFLOW_SCHEMA_IDENTITY_V1, WORKFLOW_SCHEMA_VERSION_V1,
    WORKFLOW_TABLE_CONTRACTS_V1, WorkflowTableContractV1,
};
use tracedecay_sessions::runtime::git_correlation::{
    GIT_CORRELATION_SCHEMA_VERSION, recorded_git_correlation_schema_version,
};
use tracedecay_store::StoreShardScopeV1;

const CANONICAL_BODY_RECLAIM_PAGES: u64 = 4096;

const REGISTRY_SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS projects (
        path TEXT PRIMARY KEY,
        tokens_saved INTEGER NOT NULL DEFAULT 0
    );
    CREATE TABLE IF NOT EXISTS code_projects (
        project_id TEXT PRIMARY KEY,
        canonical_root TEXT NOT NULL,
        display_root TEXT NOT NULL,
        primary_root_platform TEXT,
        primary_root_bytes BLOB,
        primary_root_last_seen_at INTEGER,
        git_common_dir TEXT,
        git_remote_url TEXT,
        default_branch TEXT,
        created_at INTEGER NOT NULL,
        last_seen_at INTEGER NOT NULL
    );
    CREATE TABLE IF NOT EXISTS project_aliases (
        alias_path TEXT PRIMARY KEY,
        project_id TEXT NOT NULL,
        last_seen_at INTEGER NOT NULL,
        FOREIGN KEY(project_id) REFERENCES code_projects(project_id) ON DELETE CASCADE
    );
    CREATE TABLE IF NOT EXISTS store_instances (
        store_id TEXT PRIMARY KEY,
        project_id TEXT NOT NULL,
        store_kind TEXT NOT NULL,
        storage_mode TEXT NOT NULL,
        store_relpath TEXT NOT NULL,
        manifest_relpath TEXT,
        created_at INTEGER NOT NULL,
        last_verified_at INTEGER,
        last_write_at INTEGER,
        FOREIGN KEY(project_id) REFERENCES code_projects(project_id) ON DELETE CASCADE
    );
    CREATE TABLE IF NOT EXISTS graph_scopes (
        graph_scope_id TEXT PRIMARY KEY,
        project_id TEXT NOT NULL,
        store_id TEXT NOT NULL,
        branch_name TEXT NOT NULL,
        parent_scope_id TEXT,
        last_synced_at INTEGER,
        writable INTEGER NOT NULL DEFAULT 1,
        FOREIGN KEY(project_id) REFERENCES code_projects(project_id) ON DELETE CASCADE,
        FOREIGN KEY(store_id) REFERENCES store_instances(store_id) ON DELETE CASCADE
    );
    CREATE TABLE IF NOT EXISTS store_artifacts (
        store_id TEXT NOT NULL,
        artifact_kind TEXT NOT NULL,
        relpath TEXT NOT NULL,
        size_bytes INTEGER,
        schema_version TEXT,
        updated_at INTEGER,
        PRIMARY KEY (store_id, artifact_kind, relpath),
        FOREIGN KEY(store_id) REFERENCES store_instances(store_id) ON DELETE CASCADE
    );
    CREATE INDEX IF NOT EXISTS idx_project_aliases_project_id
        ON project_aliases(project_id);
    CREATE INDEX IF NOT EXISTS idx_store_instances_project_id
        ON store_instances(project_id);
    CREATE INDEX IF NOT EXISTS idx_graph_scopes_project_store
        ON graph_scopes(project_id, store_id);
";

const REMOTE_DELETION_SCHEMA: &str = "
    CREATE TABLE remote_deletion_tombstones (
        profile_id TEXT NOT NULL,
        target_kind TEXT NOT NULL,
        project_id TEXT NOT NULL,
        tombstone_id TEXT NOT NULL,
        recorded_at_micros INTEGER NOT NULL,
        cleanup_status TEXT NOT NULL,
        failure_code TEXT,
        failure_phase TEXT,
        retryable INTEGER,
        PRIMARY KEY (profile_id, target_kind, project_id),
        CHECK (length(profile_id) BETWEEN 1 AND 256),
        CHECK (target_kind IN ('account', 'project')),
        CHECK (
            (target_kind = 'account' AND project_id = '')
            OR (target_kind = 'project' AND length(project_id) BETWEEN 1 AND 256)
        ),
        CHECK (length(tombstone_id) BETWEEN 1 AND 256),
        CHECK (recorded_at_micros > 0),
        CHECK (cleanup_status IN ('pending', 'settling', 'partial', 'deleted')),
        CHECK (
            (cleanup_status IN ('pending', 'deleted')
                AND failure_code IS NULL AND failure_phase IS NULL AND retryable IS NULL)
            OR (cleanup_status IN ('settling', 'partial')
                AND failure_code IS NOT NULL AND failure_phase IS NOT NULL
                AND retryable IN (0, 1))
        )
    );
";

const TRANSCRIPT_SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS parse_offsets (
        file_path TEXT PRIMARY KEY,
        byte_offset INTEGER NOT NULL,
        mtime INTEGER NOT NULL,
        file_id INTEGER NOT NULL DEFAULT 0
    );
    CREATE TABLE IF NOT EXISTS savings_ledger (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        ts INTEGER NOT NULL,
        project_path TEXT NOT NULL,
        tool_name TEXT NOT NULL,
        before_tokens INTEGER NOT NULL,
        after_tokens INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_savings_ledger_ts ON savings_ledger(ts);
    CREATE INDEX IF NOT EXISTS idx_savings_ledger_project ON savings_ledger(project_path);
    CREATE TABLE IF NOT EXISTS analytics_events (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        provider TEXT NOT NULL,
        project_id TEXT NOT NULL,
        session_id TEXT,
        timestamp INTEGER NOT NULL,
        event_kind TEXT NOT NULL,
        hook_name TEXT,
        tool_name TEXT,
        tool_category TEXT,
        skill_name TEXT,
        hint_category TEXT,
        hint_id TEXT,
        outcome TEXT,
        metadata_json TEXT
    );
    CREATE INDEX IF NOT EXISTS idx_analytics_events_provider_project_session
        ON analytics_events(provider, project_id, session_id, timestamp);
    CREATE INDEX IF NOT EXISTS idx_analytics_events_kind
        ON analytics_events(event_kind, timestamp);
    CREATE INDEX IF NOT EXISTS idx_analytics_events_project_time
        ON analytics_events(project_id, timestamp);
    CREATE INDEX IF NOT EXISTS idx_analytics_events_timestamp
        ON analytics_events(timestamp);
    CREATE UNIQUE INDEX IF NOT EXISTS idx_observability_event_idempotency
        ON analytics_events(provider, project_id, hint_id)
        WHERE provider = 'tracedecay-observability' AND hint_id IS NOT NULL;
    CREATE INDEX IF NOT EXISTS idx_observability_event_retention
        ON analytics_events(json_extract(metadata_json, '$.retention_class'), timestamp)
        WHERE provider = 'tracedecay-observability';
    CREATE TABLE IF NOT EXISTS observability_emission_outbox (
        project_id TEXT NOT NULL,
        owner_event_id TEXT NOT NULL,
        owner_fact_json TEXT NOT NULL CHECK(json_valid(owner_fact_json)),
        delivery_envelope_json TEXT NOT NULL CHECK(json_valid(delivery_envelope_json)),
        state TEXT NOT NULL CHECK(state IN ('pending', 'settled')),
        analytics_event_id INTEGER,
        PRIMARY KEY(project_id, owner_event_id),
        CHECK (
            (state = 'pending' AND analytics_event_id IS NULL)
            OR (state = 'settled' AND analytics_event_id IS NOT NULL)
        ),
        FOREIGN KEY(analytics_event_id) REFERENCES analytics_events(id)
    ) STRICT;
    CREATE INDEX IF NOT EXISTS idx_observability_emission_outbox_pending
        ON observability_emission_outbox(project_id, owner_event_id)
        WHERE state = 'pending';
    CREATE INDEX IF NOT EXISTS idx_observability_emission_outbox_analytics_event
        ON observability_emission_outbox(analytics_event_id);
    CREATE TRIGGER IF NOT EXISTS observability_emission_outbox_identity_immutable
    BEFORE UPDATE ON observability_emission_outbox
    WHEN OLD.project_id != NEW.project_id
      OR OLD.owner_event_id != NEW.owner_event_id
      OR OLD.owner_fact_json != NEW.owner_fact_json
      OR OLD.state = 'settled'
      OR (OLD.delivery_envelope_json != NEW.delivery_envelope_json
          AND (OLD.state != 'pending' OR NEW.state != 'pending'))
    BEGIN
        SELECT RAISE(ABORT, 'observability emission outbox identity is immutable');
    END;
    DROP TRIGGER IF EXISTS observability_emission_outbox_no_delete;
    CREATE TABLE IF NOT EXISTS sessions (
        provider TEXT NOT NULL,
        session_id TEXT NOT NULL,
        project_key TEXT NOT NULL,
        project_path TEXT NOT NULL,
        title TEXT,
        started_at INTEGER,
        ended_at INTEGER,
        transcript_path TEXT,
        metadata_json TEXT,
        parent_session_id TEXT,
        is_subagent INTEGER NOT NULL DEFAULT 0,
        agent_id TEXT,
        parent_tool_use_id TEXT,
        PRIMARY KEY(provider, session_id)
    );
    CREATE INDEX IF NOT EXISTS idx_sessions_project ON sessions(provider, project_key);
    CREATE INDEX IF NOT EXISTS idx_sessions_parent ON sessions(provider, parent_session_id);
    CREATE INDEX IF NOT EXISTS idx_sessions_project_provider_session
        ON sessions(project_key, provider, session_id);
    CREATE INDEX IF NOT EXISTS idx_sessions_started_at ON sessions(started_at);
    CREATE INDEX IF NOT EXISTS idx_sessions_activity_fallback
        ON sessions(COALESCE(ended_at, started_at));
    CREATE INDEX IF NOT EXISTS idx_sessions_session_provider
        ON sessions(session_id, provider, parent_session_id);
    CREATE INDEX IF NOT EXISTS idx_sessions_active_project_path
        ON sessions(project_path, provider, session_id)
        WHERE ended_at IS NULL;
    CREATE TABLE IF NOT EXISTS session_backfill_meta (
        key TEXT PRIMARY KEY,
        value TEXT NOT NULL,
        updated_at INTEGER NOT NULL DEFAULT (unixepoch())
    );
";

// The host drain picks the next pending projection across every binding in
// this order; without the index each pick scans and sorts the whole pending
// table. The canonical project database admits only its exact final shape and
// never runs that pick, so the index lives with the registered stores.
const EXTERNAL_SOURCE_PENDING_ORDER_INDEX: &str = "
    CREATE INDEX IF NOT EXISTS idx_external_source_pending_order_v1
        ON external_source_pending_projections_v1(successor_sequence, binding_id);
";

const DELIVERY_SETTLEMENT_SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS delivery_fanout_events (
        project_id TEXT NOT NULL,
        owner_event_id TEXT NOT NULL,
        surface TEXT NOT NULL CHECK(surface IN ('hook', 'mcp', 'lsp', 'dashboard', 'cli', 'other')),
        event_class TEXT NOT NULL CHECK(event_class IN (
            'operation_accepted', 'operation_progress', 'operation_terminal',
            'diagnostic', 'activity', 'other'
        )),
        eligible INTEGER NOT NULL CHECK(eligible BETWEEN 1 AND 64),
        valid_at_micros INTEGER NOT NULL CHECK(valid_at_micros > 0),
        -- Canonical optional source binding for Work-attempt delivery. The
        -- JSON is the authority; the digest is only a bounded exact lookup
        -- key and is revalidated before use.
        work_attempt_json TEXT CHECK(work_attempt_json IS NULL OR json_valid(work_attempt_json)),
        work_attempt_digest TEXT,
        CHECK(
            (work_attempt_json IS NULL AND work_attempt_digest IS NULL)
            OR (work_attempt_json IS NOT NULL AND work_attempt_digest IS NOT NULL)
        ),
        PRIMARY KEY(project_id, owner_event_id, surface)
    ) STRICT;
    CREATE TABLE IF NOT EXISTS delivery_settlements (
        project_id TEXT NOT NULL,
        owner_event_id TEXT NOT NULL,
        surface TEXT NOT NULL,
        channel_ref TEXT NOT NULL,
        attempted_at_micros INTEGER NOT NULL,
        outcome TEXT CHECK(outcome IN ('delivered', 'deduplicated', 'dropped')),
        settled_at_micros INTEGER,
        drop_reason TEXT CHECK(drop_reason IN (
            'backpressure', 'cancelled', 'deadline', 'disconnected',
            'invalid', 'rejected', 'unknown'
        )),
        census_json TEXT CHECK(census_json IS NULL OR json_valid(census_json)),
        PRIMARY KEY(project_id, owner_event_id, surface, channel_ref),
        FOREIGN KEY(project_id, owner_event_id, surface)
            REFERENCES delivery_fanout_events(project_id, owner_event_id, surface),
        CHECK (
            (outcome IS NULL AND settled_at_micros IS NULL
                AND drop_reason IS NULL AND census_json IS NULL)
            OR (outcome IN ('delivered', 'deduplicated')
                AND settled_at_micros IS NOT NULL
                AND drop_reason IS NULL AND census_json IS NOT NULL)
            OR (outcome = 'dropped'
                AND settled_at_micros IS NOT NULL
                AND drop_reason IS NOT NULL AND census_json IS NOT NULL)
        )
    ) STRICT;
    CREATE INDEX IF NOT EXISTS idx_delivery_settlements_pending
        ON delivery_settlements(project_id, owner_event_id, surface)
        WHERE outcome IS NULL;
    CREATE INDEX IF NOT EXISTS idx_delivery_settlements_pending_due
        ON delivery_settlements(
            project_id, surface, attempted_at_micros, channel_ref
        )
        WHERE outcome IS NULL;
    CREATE INDEX IF NOT EXISTS idx_delivery_fanout_work_attempt
        ON delivery_fanout_events(project_id, work_attempt_digest)
        WHERE work_attempt_digest IS NOT NULL;
    CREATE TABLE IF NOT EXISTS delivery_source_receipts (
        project_id TEXT NOT NULL,
        receipt_ref TEXT NOT NULL,
        owner_event_id TEXT NOT NULL,
        surface TEXT NOT NULL,
        channel_ref TEXT NOT NULL,
        PRIMARY KEY(project_id, receipt_ref),
        UNIQUE(project_id, owner_event_id, surface, channel_ref),
        FOREIGN KEY(project_id, owner_event_id, surface, channel_ref)
            REFERENCES delivery_settlements(project_id, owner_event_id, surface, channel_ref)
    ) STRICT;
    CREATE TRIGGER IF NOT EXISTS delivery_fanout_events_immutable
    BEFORE UPDATE ON delivery_fanout_events BEGIN
        SELECT RAISE(ABORT, 'delivery fanout identity is immutable');
    END;
    CREATE TRIGGER IF NOT EXISTS delivery_fanout_events_no_delete
    BEFORE DELETE ON delivery_fanout_events BEGIN
        SELECT RAISE(ABORT, 'delivery fanout receipts are retained');
    END;
    CREATE TRIGGER IF NOT EXISTS delivery_settlements_identity_immutable
    BEFORE UPDATE ON delivery_settlements
    WHEN OLD.project_id != NEW.project_id
      OR OLD.owner_event_id != NEW.owner_event_id
      OR OLD.surface != NEW.surface
      OR OLD.channel_ref != NEW.channel_ref
      OR OLD.attempted_at_micros != NEW.attempted_at_micros
      OR OLD.outcome IS NOT NULL
    BEGIN
        SELECT RAISE(ABORT, 'delivery settlement identity is immutable');
    END;
    CREATE TRIGGER IF NOT EXISTS delivery_settlements_no_delete
    BEFORE DELETE ON delivery_settlements BEGIN
        SELECT RAISE(ABORT, 'delivery settlement receipts are retained');
    END;
    CREATE TRIGGER IF NOT EXISTS delivery_source_receipts_immutable
    BEFORE UPDATE ON delivery_source_receipts BEGIN
        SELECT RAISE(ABORT, 'delivery source receipt identity is immutable');
    END;
    CREATE TRIGGER IF NOT EXISTS delivery_source_receipts_no_delete
    BEFORE DELETE ON delivery_source_receipts BEGIN
        SELECT RAISE(ABORT, 'delivery source receipts are retained');
    END;
";

/// Installs the global/session schema at its final shape through the exact
/// registered runtime connection, or verifies that an existing store already
/// carries it. No database path is resolved or reopened, and no store is
/// stepped forward from an older shape.
pub async fn ensure_registered_schema(
    installation: &RegisteredSchemaInstallationV1,
) -> tracedecay_domain::errors::Result<()> {
    let convergence = ensure_registered_schema_for_admission(installation).await?;
    if !convergence.is_fresh {
        return Err(global_db_operation_message(
            "initialize registered global database schema",
            "new registered schema installation was not classified fresh",
        ));
    }
    ensure_fresh_authority_invariants(installation).await
}

#[derive(Clone, Copy)]
pub struct RegisteredSchemaConvergence {
    force_exhaustive: bool,
    is_fresh: bool,
    recovers_published_workflow_shape: bool,
}

impl RegisteredSchemaConvergence {
    /// Whether this store's workflow tables still carry the one published
    /// shape, which convergence upgrades before anything else.
    #[must_use]
    pub fn recovers_published_workflow_shape(self) -> bool {
        self.recovers_published_workflow_shape
    }
}

/// Typed schema states an admissible store was classified into, carried from
/// the read-only classification pass to the installation stages that consume
/// them.
struct RegisteredSchemaAdmissionClassification {
    configuration_fresh: Option<configuration::FreshConfigurationStoreEvidence>,
    temporal_admission: session_temporal_schema::SessionTemporalSchemaAdmission,
    session_features: SessionFeatureSchema,
}

/// Whether admission gates, installs, and converges the version-gated
/// session-feature schemas (LCM, git correlation, workflows) of a store.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionFeatureSchema {
    Hosted {
        workflow: WorkflowSchemaAdmission,
    },
    /// An existing profile authority: these schemas are left exactly as
    /// found and never read.
    NotHosted,
}

impl SessionFeatureSchema {
    fn upgrades_published_workflow_shape(self) -> bool {
        self == Self::Hosted {
            workflow: WorkflowSchemaAdmission::UpgradePublishedArtifactCeiling,
        }
    }
}

/// The profile authority (`global.db`) holds the project registry, usage
/// accounting, and remote-deletion records, never session rows: sessions,
/// LCM, git correlation, and workflows live in the profile and project
/// session stores, and LCM refuses writes to any other store. A fresh
/// profile authority still receives the whole registered schema, whose
/// authority triggers reference the LCM tables, but an existing one never
/// admits, refuses, re-ensures, or converges those features, so a
/// session-feature schema change cannot reset the registry.
fn hosts_session_features(scope: &StoreShardScopeV1) -> bool {
    !matches!(scope, StoreShardScopeV1::Profile)
}

/// A store's admission verdict. A store whose other authorities admit but
/// whose LCM, workflow, or git correlation shape this binary refuses is
/// admitted untouched for those other authorities: it is never installed or
/// converged, and its session features refuse until the store is reset.
enum RegisteredSchemaAdmission {
    Admissible(RegisteredSchemaAdmissionClassification),
    SessionAuthorityRefused(RefusedAuthorityV1),
}

/// Read-only classification of every schema authority's admission state,
/// shared by initialization admission and existing-store attach. Each
/// authority surfaces its own typed reset state; nothing here mutates the
/// store.
///
/// One generic `async fn` nests every authority check and overflows the next
/// solver's `Send` proof (rust-lang/rust#159228). Each concrete connection
/// returns an already-erased future whose body awaits erased children.
trait RegisteredSchemaAdmissionClassify: Sync {
    fn classify_registered_schema<'a>(
        &'a self,
        scope: &'a StoreShardScopeV1,
    ) -> Pin<
        Box<
            dyn Future<Output = tracedecay_domain::errors::Result<RegisteredSchemaAdmission>>
                + Send
                + 'a,
        >,
    >;
}

macro_rules! impl_registered_schema_admission_classify {
    ($ty:ty) => {
        impl RegisteredSchemaAdmissionClassify for $ty {
            fn classify_registered_schema<'a>(
                &'a self,
                scope: &'a StoreShardScopeV1,
            ) -> Pin<
                Box<
                    dyn Future<
                            Output = tracedecay_domain::errors::Result<RegisteredSchemaAdmission>,
                        > + Send
                        + 'a,
                >,
            > {
                Box::pin(async move {
                    let hosts_session_features = hosts_session_features(scope);
                    // The LCM authority classifies profile content first. Whenever a later
                    // authority also fails, the earliest session refusal is the store's hard
                    // verdict: a legacy or version-skewed session store surfaces its own
                    // reset identity instead of being masked by the coarser configuration
                    // schema resets, which would also flag a store those features were simply
                    // never installed in.
                    let refused_lcm = if hosts_session_features {
                        let refused: Pin<
                            Box<
                                dyn Future<
                                        Output = tracedecay_domain::errors::Result<
                                            Option<RefusedAuthorityV1>,
                                        >,
                                    > + Send
                                    + '_,
                            >,
                        > = Box::pin(lcm_schema_refusal(self));
                        refused.await?
                    } else {
                        None
                    };
                    let surface = |refused: Option<RefusedAuthorityV1>| {
                        move |error| refused.map_or(error, RefusedAuthorityV1::error)
                    };
                    let configuration_fresh: Pin<
                        Box<
                            dyn Future<
                                    Output = Result<
                                        Option<configuration::FreshConfigurationStoreEvidence>,
                                        configuration::ConfigurationSchemaError,
                                    >,
                                > + Send
                                + '_,
                        >,
                    > = Box::pin(configuration::fresh_configuration_store_evidence(self));
                    let configuration_fresh = configuration_fresh
                        .await
                        .map_err(configuration_schema_error(
                            "inspect configuration schema freshness",
                        ))
                        .map_err(surface(refused_lcm.clone()))?;
                    let temporal_admission: Pin<
                        Box<
                            dyn Future<
                                    Output = tracedecay_domain::errors::Result<
                                        Result<
                                            session_temporal_schema::SessionTemporalSchemaAdmission,
                                            RefusedAuthorityV1,
                                        >,
                                    >,
                                > + Send
                                + '_,
                        >,
                    > = Box::pin(
                        session_temporal_schema::require_admissible_session_temporal_schema(
                            self,
                            configuration_fresh.as_ref(),
                        ),
                    );
                    let temporal_admission = temporal_admission
                        .await
                        .map_err(surface(refused_lcm.clone()))?;
                    let refused = refused_lcm
                        .clone()
                        .or(temporal_admission.as_ref().err().cloned());
                    let session_features =
                        if hosts_session_features || configuration_fresh.is_some() {
                            let workflow: Pin<
                                Box<
                                    dyn Future<
                                            Output = tracedecay_domain::errors::Result<
                                                Result<WorkflowSchemaAdmission, RefusedAuthorityV1>,
                                            >,
                                        > + Send
                                        + '_,
                                >,
                            > = Box::pin(inspect_workflow_schema_for_admission(self));
                            workflow
                                .await
                                .map_err(surface(refused.clone()))?
                                .map(|workflow| SessionFeatureSchema::Hosted { workflow })
                        } else {
                            Ok(SessionFeatureSchema::NotHosted)
                        };
                    let refused = refused.or(session_features.as_ref().err().cloned());
                    let refused_git_correlation = if hosts_session_features {
                        let refused_git: Pin<
                            Box<
                                dyn Future<
                                        Output = tracedecay_domain::errors::Result<
                                            Option<RefusedAuthorityV1>,
                                        >,
                                    > + Send
                                    + '_,
                            >,
                        > = Box::pin(git_correlation_schema_refusal(self));
                        refused_git.await.map_err(surface(refused.clone()))?
                    } else {
                        None
                    };
                    let refused = refused.or(refused_git_correlation);
                    let admitted: Pin<
                        Box<
                            dyn Future<Output = Result<(), configuration::ConfigurationSchemaError>>
                                + Send
                                + '_,
                        >,
                    > = Box::pin(configuration::admit_configuration_schema(
                        self,
                        configuration_fresh.as_ref(),
                    ));
                    admitted
                        .await
                        .map_err(configuration_schema_error("admit configuration schema"))
                        .map_err(surface(refused.clone()))?;
                    // An existing catalog whose remote-deletion tombstone table drifted from the
                    // contract cannot be trusted to gate replay or admission, so admission fails
                    // closed with the tip's typed reset authority rather than silently
                    // continuing on a shape that no longer proves deletion state.
                    if configuration_fresh.is_none() {
                        let remote: Pin<
                            Box<
                                dyn Future<Output = tracedecay_domain::errors::Result<()>>
                                    + Send
                                    + '_,
                            >,
                        > = Box::pin(validate_remote_deletion_schema_contract(self));
                        if let Err(error) = remote.await {
                            return Err(surface(refused.clone())(
                                tracedecay_domain::errors::TraceDecayError::reset_required(
                                    "remote deletion tombstones",
                                    error.to_string(),
                                ),
                            ));
                        }
                    }
                    Ok(match (refused, temporal_admission, session_features) {
                        (None, Ok(temporal_admission), Ok(session_features)) => {
                            RegisteredSchemaAdmission::Admissible(
                                RegisteredSchemaAdmissionClassification {
                                    configuration_fresh,
                                    temporal_admission,
                                    session_features,
                                },
                            )
                        }
                        (Some(refused), _, _)
                        | (None, Err(refused), _)
                        | (None, _, Err(refused)) => {
                            RegisteredSchemaAdmission::SessionAuthorityRefused(refused)
                        }
                    })
                })
            }
        }
    };
}

impl_registered_schema_admission_classify!(RegisteredSchemaInstallationV1);
impl_registered_schema_admission_classify!(DatabaseEngineReadConnection);

fn classify_registered_schema_admission<'a>(
    connection: &'a impl RegisteredSchemaAdmissionClassify,
    scope: &'a StoreShardScopeV1,
) -> Pin<
    Box<
        dyn Future<Output = tracedecay_domain::errors::Result<RegisteredSchemaAdmission>>
            + Send
            + 'a,
    >,
> {
    connection.classify_registered_schema(scope)
}

fn configuration_schema_error(
    operation: &'static str,
) -> impl Fn(configuration::ConfigurationSchemaError) -> tracedecay_domain::errors::TraceDecayError
{
    move |error| match error {
        configuration::ConfigurationSchemaError::ResetRequired { reason } => {
            tracedecay_domain::errors::TraceDecayError::reset_required("configuration", reason)
        }
        configuration::ConfigurationSchemaError::Storage(error) => {
            global_db_operation_error(operation, error)
        }
    }
}

async fn lcm_schema_refusal(
    connection: &(impl QueryExecutor + Sync),
) -> tracedecay_domain::errors::Result<Option<RefusedAuthorityV1>> {
    match tracedecay_lcm::schema::require_admissible_lcm_schema(connection).await {
        Ok(_) => Ok(None),
        Err(tracedecay_lcm::LcmError::ProfileResetRequired {
            found_version,
            required_version,
        }) => Ok(Some(RefusedAuthorityV1::Version {
            component: "LCM",
            found_version,
            required_version,
        })),
        Err(error) => Err(global_db_operation_error(
            "classify LCM schema admission",
            error,
        )),
    }
}

/// Git evidence is stored as per-session rows since schema version 6. A store
/// recorded at any other version holds a shape nothing converts, so it keeps
/// its data untouched behind the typed, versioned reset.
async fn git_correlation_schema_refusal(
    connection: &(impl QueryExecutor + Sync),
) -> tracedecay_domain::errors::Result<Option<RefusedAuthorityV1>> {
    let recorded = recorded_git_correlation_schema_version(connection)
        .await
        .map_err(|error| global_db_operation_error("inspect git correlation schema", error))?;
    Ok(match recorded {
        Some(found) if found != GIT_CORRELATION_SCHEMA_VERSION => {
            Some(RefusedAuthorityV1::Version {
                component: "git correlation",
                found_version: Some(found),
                required_version: GIT_CORRELATION_SCHEMA_VERSION,
            })
        }
        _ => None,
    })
}

/// Authority named by the typed reset an existing store receives when the
/// composed authority schema (observation, projection, and anchor tables with
/// their invariant triggers) drifted from the contract. No such shape migrates
/// in place: the store stays untouched for the operator's reset decision. A
/// fresh install failing the same check is a programming error and keeps its
/// storage classification.
const AUTHORITY_SCHEMA_AUTHORITY: &str = "authority schema";

/// Refuse a drifted observation table before maintenance installs triggers.
/// Both initial admission and subsequent attachments use the same read-only
/// check so a missing table's triggers never mask its typed reset.
async fn validate_existing_observation_for_admission(
    conn: &impl QueryExecutor,
    is_fresh: bool,
) -> tracedecay_domain::errors::Result<()> {
    if is_fresh {
        return Ok(());
    }
    validate_existing_observation_table(conn)
        .await
        .map_err(|error| {
            tracedecay_domain::errors::TraceDecayError::reset_required(
                AUTHORITY_SCHEMA_AUTHORITY,
                error.to_string(),
            )
        })
}

async fn validate_admitted_authority_schema(
    conn: &RegisteredSchemaInstallationV1,
    is_fresh: bool,
) -> tracedecay_domain::errors::Result<()> {
    validate_authority_schema_contract(conn)
        .await
        .map_err(|error| {
            if is_fresh {
                error
            } else {
                tracedecay_domain::errors::TraceDecayError::reset_required(
                    AUTHORITY_SCHEMA_AUTHORITY,
                    error.to_string(),
                )
            }
        })
}

/// Installs the minimum schema and write guards required before a registered
/// runtime may be published. Historical convergence remains separately
/// resumable so daemon admission never waits for whole-store scans.
#[tracing::instrument(name = "global_db.schema.persist.admission", level = "trace", skip_all)]
pub async fn ensure_registered_schema_for_admission(
    installation: &RegisteredSchemaInstallationV1,
) -> tracedecay_domain::errors::Result<RegisteredSchemaConvergence> {
    const OPERATION: &str = "initialize registered global database schema";
    let RegisteredSchemaAdmissionClassification {
        configuration_fresh,
        temporal_admission,
        session_features,
    } = match classify_registered_schema_admission(
        installation,
        &installation.binding().shard_id.scope,
    )
    .await?
    {
        RegisteredSchemaAdmission::Admissible(classification) => classification,
        RegisteredSchemaAdmission::SessionAuthorityRefused(refused) => {
            return Err(refused.error());
        }
    };
    let is_fresh = configuration_fresh.is_some();
    validate_existing_observation_for_admission(installation, is_fresh).await?;
    let triggers_intact: Pin<
        Box<dyn Future<Output = tracedecay_domain::errors::Result<bool>> + Send + '_>,
    > = Box::pin(authority_invariant_triggers_intact(installation));
    let force_exhaustive = !triggers_intact.await?;
    schema_install_checkpoint(installation.cancellation(), OPERATION)?;
    let begun: Pin<
        Box<
            dyn Future<Output = engine::Result<RegisteredSchemaInstallationTransactionV1<'_>>>
                + Send
                + '_,
        >,
    > = Box::pin(installation.begin_atomic_schema_transaction());
    let transaction = begun
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;

    let committed: Pin<
        Box<
            dyn Future<Output = tracedecay_domain::errors::Result<Option<RefusedAuthorityV1>>>
                + Send
                + '_,
        >,
    > = Box::pin(install_and_commit_registered_schema(
        CancellableSchemaTransaction {
            transaction,
            cancellation: installation.cancellation(),
        },
        configuration_fresh.as_ref(),
        temporal_admission,
        session_features,
        force_exhaustive,
        "commit registered global schema",
        "roll back registered global schema",
    ));
    if let Some(refused) = committed.await? {
        return Err(refused.error());
    }

    let indexes: Pin<Box<dyn Future<Output = engine::Result<()>> + Send + '_>> = Box::pin(
        observation_projection::ensure_observation_projection_performance_indexes(installation),
    );
    indexes.await.map_err(|error| {
        global_db_operation_error("initialize observation projection indexes", error)
    })?;
    // Fresh stores have no history to index. Existing stores build this on
    // the independently durable convergence writer after required admission.
    if is_fresh {
        installation
            .execute_authority_revalidated_batch(
                crate::registered_analytics::HOOK_ROUTE_PERFORMANCE_INDEX_SQL,
            )
            .await
            .map_err(|error| global_db_operation_error("initialize hook route index", error))?;
    }
    // Fresh installations publish the final LCM index shape. Keep each build
    // independently durable outside the shared schema transaction so a
    // real-scale index build gets the long lease without holding every other
    // installation stage open.
    if session_features != SessionFeatureSchema::NotHosted {
        for sql in tracedecay_lcm::schema::LCM_STATUS_PERFORMANCE_INDEX_SQL {
            let built: Pin<Box<dyn Future<Output = engine::Result<()>> + Send + '_>> =
                Box::pin(installation.execute_authority_revalidated_batch(sql));
            built.await.map_err(|error| {
                global_db_operation_error("initialize LCM status performance indexes", error)
            })?;
        }
    }
    let admitted: Pin<Box<dyn Future<Output = tracedecay_domain::errors::Result<()>> + Send + '_>> =
        Box::pin(validate_admitted_authority_schema(installation, is_fresh));
    admitted.await?;
    Ok(RegisteredSchemaConvergence {
        force_exhaustive,
        is_fresh,
        recovers_published_workflow_shape: session_features.upgrades_published_workflow_shape(),
    })
}

/// Installs (or idempotently re-ensures) every registered schema stage inside
/// the caller's admission transaction. Callers classify admission first, so
/// this stage runs only for stores classified fresh, exactly current, or
/// exactly the one published workflow shape convergence upgrades afterwards,
/// whose artifact table no stage here touches.
///
/// Heap boundary: see [`RegisteredSchemaAdmissionClassify`].
#[tracing::instrument(name = "global_db.schema.persist.install", level = "trace", skip_all)]
async fn install_registered_schema_stages(
    transaction: &(impl Executor + Sync),
    configuration_fresh: Option<&configuration::FreshConfigurationStoreEvidence>,
    temporal_admission: session_temporal_schema::SessionTemporalSchemaAdmission,
    session_features: SessionFeatureSchema,
    force_exhaustive: bool,
) -> tracedecay_domain::errors::Result<Option<RefusedAuthorityV1>> {
    Box::pin(install_registered_schema_stage_sequence(
        transaction,
        configuration_fresh,
        temporal_admission,
        session_features,
        force_exhaustive,
    ))
    .await
}

async fn install_and_commit_registered_schema<T>(
    install: CancellableSchemaTransaction<'_, T>,
    configuration_fresh: Option<&configuration::FreshConfigurationStoreEvidence>,
    temporal_admission: session_temporal_schema::SessionTemporalSchemaAdmission,
    session_features: SessionFeatureSchema,
    force_exhaustive: bool,
    commit_operation: &'static str,
    rollback_operation: &'static str,
) -> tracedecay_domain::errors::Result<Option<RefusedAuthorityV1>>
where
    T: Executor + Sync + SchemaInstallTransaction,
{
    let admission = install_registered_schema_stages(
        &install,
        configuration_fresh,
        temporal_admission,
        session_features,
        force_exhaustive,
    )
    .await
    .and_then(|refused_authority| {
        schema_install_checkpoint(install.cancellation, commit_operation)
            .map(|()| refused_authority)
    });
    settle_schema_transaction(install, admission, commit_operation, rollback_operation).await
}

/// Commits the transaction when `outcome` succeeded and rolls it back when it
/// failed, naming shutdown as the failure when it is what stopped the work.
async fn settle_schema_transaction<T>(
    install: CancellableSchemaTransaction<'_, T>,
    outcome: tracedecay_domain::errors::Result<Option<RefusedAuthorityV1>>,
    commit_operation: &'static str,
    rollback_operation: &'static str,
) -> tracedecay_domain::errors::Result<Option<RefusedAuthorityV1>>
where
    T: SchemaInstallTransaction,
{
    let CancellableSchemaTransaction {
        transaction,
        cancellation,
    } = install;
    match outcome {
        Ok(refused_authority) => {
            transaction.commit().await.map_err(|error| {
                global_db_operation_error(commit_operation, std::io::Error::other(error))
            })?;
            Ok(refused_authority)
        }
        Err(error) => match transaction.rollback().await {
            Ok(()) if cancellation.is_cancelled() => Err(
                tracedecay_domain::errors::TraceDecayError::store_open_cancelled(
                    rollback_operation,
                ),
            ),
            Ok(()) => Err(error),
            Err(rollback_error) => Err(global_db_operation_error(
                rollback_operation,
                std::io::Error::other(format!("{error}; rollback failed: {rollback_error}")),
            )),
        },
    }
}

fn schema_install_checkpoint(
    cancellation: &CancellationToken,
    operation: &'static str,
) -> tracedecay_domain::errors::Result<()> {
    if cancellation.is_cancelled() {
        return Err(tracedecay_domain::errors::TraceDecayError::store_open_cancelled(operation));
    }
    Ok(())
}

/// A schema transaction that refuses its next statement once shutdown cancels
/// the Store open, so it rolls back at a statement boundary instead of running
/// the whole install first.
struct CancellableSchemaTransaction<'a, T> {
    transaction: T,
    cancellation: &'a CancellationToken,
}

impl<T> CancellableSchemaTransaction<'_, T> {
    fn checkpoint(&self) -> engine::Result<()> {
        if self.cancellation.is_cancelled() {
            return Err(engine::Error::InvalidOperation(
                "registered schema install cancelled by daemon shutdown".to_owned(),
            ));
        }
        Ok(())
    }
}

impl<T: QueryExecutor + Sync> QueryExecutor for CancellableSchemaTransaction<'_, T> {
    async fn query<P>(&self, sql: &str, params: P) -> engine::Result<Rows>
    where
        P: IntoParams,
    {
        self.checkpoint()?;
        self.transaction.query(sql, params).await
    }
}

impl<T: Executor + Sync> Executor for CancellableSchemaTransaction<'_, T> {
    async fn execute<P>(&self, sql: &str, params: P) -> engine::Result<u64>
    where
        P: IntoParams,
    {
        self.checkpoint()?;
        self.transaction.execute(sql, params).await
    }

    async fn execute_statements(
        &self,
        statements: Vec<engine::WriteStatement>,
    ) -> engine::Result<Vec<u64>> {
        self.checkpoint()?;
        self.transaction.execute_statements(statements).await
    }

    async fn execute_batch(&self, sql: &str) -> engine::Result<()> {
        self.checkpoint()?;
        self.transaction.execute_batch(sql).await
    }
}

trait SchemaInstallTransaction: Sized {
    fn commit(self) -> impl std::future::Future<Output = Result<(), String>> + Send;
    fn rollback(self) -> impl std::future::Future<Output = Result<(), String>> + Send;
}

impl SchemaInstallTransaction for RegisteredSchemaInstallationTransactionV1<'_> {
    async fn commit(self) -> Result<(), String> {
        RegisteredSchemaInstallationTransactionV1::commit(self)
            .await
            .map_err(|error| error.to_string())
    }

    async fn rollback(self) -> Result<(), String> {
        RegisteredSchemaInstallationTransactionV1::rollback(self)
            .await
            .map_err(|error| error.to_string())
    }
}

impl SchemaInstallTransaction for DatabaseWriteTransaction<'_> {
    async fn commit(self) -> Result<(), String> {
        DatabaseWriteTransaction::commit(self)
            .await
            .map_err(|error| error.to_string())
    }

    async fn rollback(self) -> Result<(), String> {
        DatabaseWriteTransaction::rollback(self)
            .await
            .map_err(|error| error.to_string())
    }
}

async fn install_registered_schema_stage_sequence(
    transaction: &(impl Executor + Sync),
    configuration_fresh: Option<&configuration::FreshConfigurationStoreEvidence>,
    temporal_admission: session_temporal_schema::SessionTemporalSchemaAdmission,
    session_features: SessionFeatureSchema,
    force_exhaustive: bool,
) -> tracedecay_domain::errors::Result<Option<RefusedAuthorityV1>> {
    let is_fresh = configuration_fresh.is_some();
    configuration::ensure_configuration_schema(transaction, configuration_fresh)
        .await
        .map_err(configuration_schema_error(
            "initialize configuration schema",
        ))?;
    ensure_authority_audit_checkpoint_schema(transaction).await?;
    if force_exhaustive && !is_fresh {
        // Persist the requirement before later schema work repairs the
        // trigger evidence that armed it. The progress row doubles as the
        // resumable cursor and is removed only by a completed FK sweep.
        require_foreign_key_audit(transaction).await?;
    }
    if is_fresh {
        transaction
            .execute_batch(REGISTRY_SCHEMA)
            .await
            .map_err(|error| {
                global_db_operation_error("initialize global project registry", error)
            })?;
    }
    transaction
        .execute_batch(project_registry::PROJECT_REGISTRY_PERFORMANCE_INDEX_SQL)
        .await
        .map_err(|error| {
            global_db_operation_error("initialize project registry performance indexes", error)
        })?;
    if is_fresh {
        transaction
            .execute_batch(REMOTE_DELETION_SCHEMA)
            .await
            .map_err(|error| {
                global_db_operation_error("initialize remote deletion catalog", error)
            })?;
    }
    if let Err(error) = validate_registry_schema_contract(transaction).await {
        if is_fresh {
            return Err(error);
        }
        return Err(tracedecay_domain::errors::TraceDecayError::reset_required(
            project_registry::PROJECT_REGISTRY_AUTHORITY,
            error.to_string(),
        ));
    }
    project_registry::validate_project_rows_have_canonical_keys(transaction).await?;

    git_index_transactions::ensure_git_index_transaction_schema(transaction).await?;
    crate::native_integration::ensure_native_integration_schema(transaction).await?;

    transaction
        .execute_batch(TRANSCRIPT_SCHEMA)
        .await
        .map_err(|error| global_db_operation_error("initialize transcript schema", error))?;
    managed_test_runs::ensure_managed_test_run_schema(transaction).await?;
    transaction
        .execute_batch(DELIVERY_SETTLEMENT_SCHEMA)
        .await
        .map_err(|error| {
            global_db_operation_error("initialize delivery settlement schema", error)
        })?;
    stack_delivery::ensure_github_stack_delivery_schema(transaction).await?;
    transaction
        .execute_batch(observability_rollup::OBSERVABILITY_ROLLUP_SCHEMA_V1)
        .await
        .map_err(|error| {
            global_db_operation_error("initialize observability rollup schema", error)
        })?;
    if session_features
        == (SessionFeatureSchema::Hosted {
            workflow: WorkflowSchemaAdmission::Create,
        })
    {
        for table in WORKFLOW_TABLE_CONTRACTS_V1 {
            transaction
                .execute_batch(table.sql)
                .await
                .map_err(|error| global_db_operation_error("initialize workflow schema", error))?;
        }
        transaction
            .execute_batch(WORKFLOW_SCHEMA_IDENTITY_V1)
            .await
            .map_err(|error| global_db_operation_error("initialize workflow schema", error))?;
    }
    transaction
        .execute_batch(WORK_SCHEMA_V1)
        .await
        .map_err(|error| global_db_operation_error("initialize Work runtime schema", error))?;
    transaction
        .execute_batch(WORK_PRODUCT_GRAPH_JOURNAL_SCHEMA_V1)
        .await
        .map_err(|error| {
            global_db_operation_error("initialize Work product graph journal", error)
        })?;
    transaction
        .execute_batch(AUTHORIZED_SCOPE_SET_SCHEMA_V1)
        .await
        .map_err(|error| {
            global_db_operation_error("initialize authorized scope-set schema", error)
        })?;
    // The daemon's handoff runtime attaches its grant store to the same
    // registered database as the Work runtime.
    transaction
        .execute_batch(HANDOFF_OPEN_SCHEMA_V1)
        .await
        .map_err(|error| global_db_operation_error("initialize handoff-open schema", error))?;

    ensure_authority_audit_checkpoint_schema(transaction).await?;
    match temporal_admission {
        session_temporal_schema::SessionTemporalSchemaAdmission::Fresh => {
            session_temporal_schema::install_session_temporal_schema(transaction).await?;
        }
        session_temporal_schema::SessionTemporalSchemaAdmission::EmptyEarlier => {
            session_temporal_schema::drop_empty_session_temporal_schema(transaction).await?;
            session_temporal_schema::install_session_temporal_schema(transaction).await?;
        }
        session_temporal_schema::SessionTemporalSchemaAdmission::Current => {}
    }
    let refused_authority = observation::ensure_observation_schema(transaction).await?;
    observation_projection::ensure_observation_projection_schema(transaction)
        .await
        .map_err(|error| global_db_operation_error("initialize observation projection", error))?;
    tracedecay_runtime_core::db::install_external_source_schema(
        transaction,
        "initialize registered external source state",
    )
    .await?;
    transaction
        .execute_batch(EXTERNAL_SOURCE_PENDING_ORDER_INDEX)
        .await
        .map_err(|error| {
            global_db_operation_error("initialize external source pending order", error)
        })?;
    transaction
        .execute_batch(RUNTIME_LEDGER_SCHEMA)
        .await
        .map_err(|error| global_db_operation_error("initialize runtime writer ledger", error))?;
    // Projection raw-twin triggers sit on `lcm_raw_messages`. The table has to
    // exist before those triggers are created, including on a fresh store
    // whose authority triggers are installed in this same transaction.
    if session_features != SessionFeatureSchema::NotHosted {
        tracedecay_lcm::schema::ensure_lcm_schema_in_transaction(transaction)
            .await
            .map_err(|error| match error {
                tracedecay_lcm::LcmError::ProfileResetRequired {
                    found_version,
                    required_version,
                } => tracedecay_domain::errors::TraceDecayError::ProfileResetRequired {
                    component: "LCM",
                    found_version,
                    required_version,
                },
                error => global_db_operation_error("initialize LCM schema", error),
            })?;
        observation::canonical_body_gc::schema::install(transaction).await?;
    }
    // `force_exhaustive` means admission observed damaged or missing guard
    // triggers (for example a dropped guarded table takes its triggers with
    // it). Reinstall them here so the post-commit contract validation sees a
    // whole schema, and let the armed exhaustive audit re-earn trust; leaving
    // them broken would fail every subsequent open of an otherwise
    // repairable store.
    if force_exhaustive
        || temporal_admission != session_temporal_schema::SessionTemporalSchemaAdmission::Current
    {
        ensure_authority_invariant_schema(transaction).await?;
        if !authority_invariant_triggers_intact(transaction).await? {
            return Err(global_db_operation_message(
                "initialize registered global database schema",
                "final authority triggers were not restored before schema commit",
            ));
        }
    }
    if session_features != SessionFeatureSchema::NotHosted {
        tracedecay_sessions::runtime::git_correlation::ensure_git_correlation_receipt_schema_in_transaction(
            transaction,
        )
        .await
        .map_err(|error| global_db_operation_error("initialize git correlation schema", error))?;
        tracedecay_sessions::runtime::git_correlation::install_history_change_schema(transaction)
            .await
            .map_err(|error| {
                global_db_operation_error("initialize Git history change journal", error)
            })?;
        tracedecay_sessions::runtime::workflow_index::ensure_workflow_index_schema(transaction)
            .await
            .map_err(|error| {
                global_db_operation_error("initialize workflow index schema", error)
            })?;
    }
    Ok(refused_authority)
}

/// Completes resumable authority convergence after the registered runtime is
/// available. Every stage retains its existing durable checkpoint semantics.
///
/// Stores are created at the final schema by
/// [`ensure_registered_schema_for_admission`], so there is nothing here to step
/// an older shape forward: the historical projection-anchor binding, retrieval
/// anchor, repository provenance, projector version migration, and session
/// project-path passes were all one-time legacy upgrades and have been removed.
#[tracing::instrument(name = "global_db.schema.persist.converge", level = "trace", skip_all)]
pub async fn converge_registered_schema(
    database: &Database,
    convergence: RegisteredSchemaConvergence,
) -> tracedecay_domain::errors::Result<()> {
    // The invariant pass pages historical authority rows and can legitimately
    // outlive an ordinary open on a large store. The admission phase has
    // already installed and validated its guard triggers, so daemon reads and
    // guarded writes may proceed while these idempotent repairs advance.
    // Completed repairs survive interruption, while the trusted checkpoint is
    // still written only after every audit succeeds.
    let invariants: Pin<
        Box<dyn Future<Output = tracedecay_domain::errors::Result<()>> + Send + '_>,
    > = Box::pin(converge_registered_schema_on(database, convergence));
    invariants.await
}

#[tracing::instrument(
    name = "global_db.schema.persist.converge_invariants",
    level = "trace",
    skip_all
)]
async fn converge_registered_schema_on(
    database: &Database,
    convergence: RegisteredSchemaConvergence,
) -> tracedecay_domain::errors::Result<()> {
    let rearmed: Pin<
        Box<
            dyn Future<
                    Output = tracedecay_store::ProjectionStoreResult<
                        observation_projection::RearmedProjectionRetries,
                    >,
                > + Send
                + '_,
        >,
    > = Box::pin(observation_projection::rearm_queued_projection_retries(
        database,
        observation_projection::REARM_PROJECTION_RETRY_BATCH_ROWS,
    ));
    rearmed.await.map_err(|error| {
        global_db_operation_message("rearm queued projection retries", error.durable_detail())
    })?;
    let ensured: Pin<Box<dyn Future<Output = tracedecay_domain::errors::Result<()>> + Send + '_>> =
        Box::pin(ensure_authority_invariants(
            database,
            convergence.force_exhaustive,
            convergence.is_fresh,
        ));
    ensured.await
}

/// Synchronously converges an attached existing store's historical schema.
///
/// Short-lived attaches have no background maintenance task, so they build the
/// LCM status indexes and run the authority audit before returning. Tamper
/// evidence must fail the attach itself: projection-output tamper triggers
/// delete the trusted audit checkpoint (arming an exhaustive re-audit here),
/// and altered guard triggers force the exhaustive pass with its foreign-key
/// sweep, mirroring
/// [`ensure_registered_schema_for_admission`]. An untampered store resumes
/// from its plausible checkpoint and pays only the bounded suffix audit.
#[tracing::instrument(
    name = "global_db.schema.persist.converge_attached",
    level = "trace",
    skip_all
)]
pub async fn converge_attached_registered_schema(
    database: &Database,
) -> tracedecay_domain::errors::Result<()> {
    let force_exhaustive =
        !authority_invariant_triggers_intact(&database.read_connection()).await?;
    converge_registered_schema_on(
        database,
        RegisteredSchemaConvergence {
            force_exhaustive,
            is_fresh: false,
            recovers_published_workflow_shape: false,
        },
    )
    .await
}

/// Verifies (or completes) an attached existing store's registered schema.
///
/// Only initialization runs the sealed registered-schema installer; reopening
/// an existing store publishes the runtime directly, so the attach boundary
/// re-runs the same admission. The read-only classification surfaces each
/// authority's exact typed reset identity (LCM `ProfileResetRequired`,
/// temporal / workflow / configuration / remote-deletion resets) while a
/// refused store stays untouched for the operator's explicit reset decision.
/// An admissible store then re-ensures every admission-critical idempotent
/// schema stage. An admissibly-fresh store, an existing database file with no
/// schema objects, receives the same admission-critical install as
/// initialization. The returned convergence plan carries the LCM status-index
/// work and any published workflow upgrade for lifecycle-owned daemon
/// maintenance; short-lived callers run that same work synchronously.
/// A store whose LCM, workflow, or git correlation shape this binary refuses
/// is admitted untouched for its other authorities; one whose observation
/// rows it refuses is installed without them. Either returns the refused
/// authority instead of a plan.
///
/// `cancellation` is observed before classification and before every
/// statement of the admission transaction: a cancelled attach rolls that
/// transaction back and fails typed, so the store keeps exactly its prior
/// schema. Once committed, the idempotent index builds and validation run to
/// completion.
#[tracing::instrument(name = "global_db.schema.persist.attach", level = "trace", skip_all)]
pub(crate) async fn ensure_attached_registered_schema(
    database: &Database,
    cancellation: &CancellationToken,
) -> tracedecay_domain::errors::Result<RegisteredSchemaAttachmentV1> {
    const OPERATION: &str = "install attached registered global database schema";
    schema_install_checkpoint(cancellation, OPERATION)?;
    let read_connection = database.read_connection();
    let classified = classify_registered_schema_admission(
        &read_connection,
        &database.registered_binding().shard_id.scope,
    );
    let RegisteredSchemaAdmissionClassification {
        configuration_fresh,
        temporal_admission,
        session_features,
    } = match classified.await? {
        RegisteredSchemaAdmission::Admissible(classification) => classification,
        RegisteredSchemaAdmission::SessionAuthorityRefused(refused) => {
            return Ok(RegisteredSchemaAttachmentV1::SessionsRefused(refused));
        }
    };
    validate_existing_observation_for_admission(&read_connection, configuration_fresh.is_some())
        .await?;
    let triggers_intact: Pin<
        Box<dyn Future<Output = tracedecay_domain::errors::Result<bool>> + Send + '_>,
    > = Box::pin(authority_invariant_triggers_intact(&read_connection));
    let force_exhaustive = !triggers_intact.await?;
    schema_install_checkpoint(cancellation, OPERATION)?;
    let transaction = database.begin_bulk_write_transaction(OPERATION).await?;
    let committed: Pin<
        Box<
            dyn Future<Output = tracedecay_domain::errors::Result<Option<RefusedAuthorityV1>>>
                + Send
                + '_,
        >,
    > = Box::pin(install_and_commit_registered_schema(
        CancellableSchemaTransaction {
            transaction,
            cancellation,
        },
        configuration_fresh.as_ref(),
        temporal_admission,
        session_features,
        force_exhaustive,
        "commit attached registered global schema",
        "roll back attached registered global schema",
    ));
    let mut refused_authority = committed.await?;
    let recovers_published_workflow_shape = session_features.upgrades_published_workflow_shape();
    // The verdict that no read-only query predicts belongs to admission, so a
    // schema that refuses the move is refused here instead of after a store has
    // been served as recovering. The later steps cannot refuse what
    // classification proved, and they run in background convergence.
    if recovers_published_workflow_shape && refused_authority.is_none() {
        schema_install_checkpoint(cancellation, OPERATION)?;
        let rehearsal = database.begin_bulk_write_transaction(OPERATION).await?;
        let rehearsed = rehearse_published_artifact_ceiling_upgrade(&rehearsal).await;
        rehearsal
            .rollback()
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        refused_authority = rehearsed?;
    }
    // Mirror initialization's independently durable index builds: each
    // historical-data index commits on its own so an interrupted later build
    // never rolls back an earlier completed one.
    for sql in observation_projection::OBSERVATION_PROJECTION_PERFORMANCE_INDEX_SQL {
        let transaction = database
            .begin_bulk_write_transaction("install observation projection performance index")
            .await?;
        transaction.execute_batch(sql).await.map_err(|error| {
            global_db_operation_error("initialize observation projection indexes", error)
        })?;
        transaction.commit().await?;
    }
    validate_authority_schema_contract(&read_connection)
        .await
        .map_err(|error| {
            if configuration_fresh.is_some() {
                error
            } else {
                tracedecay_domain::errors::TraceDecayError::reset_required(
                    AUTHORITY_SCHEMA_AUTHORITY,
                    error.to_string(),
                )
            }
        })?;
    database
        .run_incremental_vacuum(CANONICAL_BODY_RECLAIM_PAGES)
        .await?;
    Ok(match refused_authority {
        Some(refused) => RegisteredSchemaAttachmentV1::SessionsRefused(refused),
        None => RegisteredSchemaAttachmentV1::Admitted(RegisteredSchemaConvergence {
            force_exhaustive,
            is_fresh: configuration_fresh.is_some(),
            recovers_published_workflow_shape,
        }),
    })
}

/// The typed reset [`ensure_attached_registered_schema`] would refuse this
/// existing store with, `None` when it admits. The same classification and
/// install stages run inside a transaction that is always rolled back, so the
/// store keeps exactly its prior schema. The post-commit index builds and the
/// composed authority-schema validation are left to the store's own attach:
/// an older admissible store would otherwise build its historical indexes
/// only to discard them. The published workflow upgrade is left to convergence
/// for the same reason: classification proved its rows and dependents without
/// a write, and only its catalog-only rename runs here, so a census never
/// copies a payload.
#[tracing::instrument(name = "global_db.schema.persist.census", level = "trace", skip_all)]
pub(crate) async fn attached_registered_schema_reset_refusal(
    database: &Database,
) -> tracedecay_domain::errors::Result<Option<tracedecay_domain::errors::TraceDecayError>> {
    const OPERATION: &str = "inspect attached registered global database schema";
    let reset_or_error = |error: tracedecay_domain::errors::TraceDecayError| {
        if error.is_store_reset_required() {
            Ok(Some(error))
        } else {
            Err(error)
        }
    };
    let read_connection = database.read_connection();
    let RegisteredSchemaAdmissionClassification {
        configuration_fresh,
        temporal_admission,
        session_features,
    } = match classify_registered_schema_admission(
        &read_connection,
        &database.registered_binding().shard_id.scope,
    )
    .await
    {
        Ok(RegisteredSchemaAdmission::Admissible(classification)) => classification,
        Ok(RegisteredSchemaAdmission::SessionAuthorityRefused(refused)) => {
            return Ok(Some(refused.error()));
        }
        Err(error) => return reset_or_error(error),
    };
    if let Err(error) =
        validate_existing_observation_for_admission(&read_connection, configuration_fresh.is_some())
            .await
    {
        return reset_or_error(error);
    }
    let triggers_intact: Pin<
        Box<dyn Future<Output = tracedecay_domain::errors::Result<bool>> + Send + '_>,
    > = Box::pin(authority_invariant_triggers_intact(&read_connection));
    let force_exhaustive = !triggers_intact.await?;
    let upgrades_workflow = session_features.upgrades_published_workflow_shape();
    let transaction = database.begin_bulk_write_transaction(OPERATION).await?;
    let staged: Pin<
        Box<
            dyn Future<Output = tracedecay_domain::errors::Result<Option<RefusedAuthorityV1>>>
                + Send
                + '_,
        >,
    > = Box::pin(install_registered_schema_stages(
        &transaction,
        configuration_fresh.as_ref(),
        temporal_admission,
        session_features,
        force_exhaustive,
    ));
    let admission = match staged.await {
        Ok(None) if upgrades_workflow => {
            rehearse_published_artifact_ceiling_upgrade(&transaction).await
        }
        staged => staged,
    };
    transaction
        .rollback()
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    match admission {
        Ok(refused) => Ok(refused.map(RefusedAuthorityV1::error)),
        Err(error) => reset_or_error(error),
    }
}

/// An attached store's admission outcome.
pub(crate) enum RegisteredSchemaAttachmentV1 {
    /// Every authority admits; the plan completes historical convergence.
    Admitted(RegisteredSchemaConvergence),
    /// The store serves its other authorities and refuses every session
    /// feature until it is reset. It is never converged: its reset deletes
    /// it.
    SessionsRefused(RefusedAuthorityV1),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WorkflowSchemaAdmission {
    Create,
    Complete,
    /// The one published shape convergence steps forward, in one transaction
    /// of its own. See [`workflow_artifact_ceiling_upgrade`].
    UpgradePublishedArtifactCeiling,
}

async fn inspect_workflow_schema_for_admission(
    conn: &impl QueryExecutor,
) -> tracedecay_domain::errors::Result<Result<WorkflowSchemaAdmission, RefusedAuthorityV1>> {
    let mut rows = conn
        .query(
            "SELECT type, name, sql FROM sqlite_master
             WHERE name NOT LIKE 'sqlite_%'
               AND name NOT LIKE 'td_runtime_%'
             ORDER BY type, name",
            (),
        )
        .await
        .map_err(|error| global_db_operation_error("inspect workflow schema tables", error))?;
    let mut tables = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| global_db_operation_error("read workflow schema tables", error))?
    {
        tables.push((
            row.get::<String>(0).map_err(|error| {
                global_db_operation_error("decode workflow schema object type", error)
            })?,
            row.get::<String>(1).map_err(|error| {
                global_db_operation_error("decode workflow schema object name", error)
            })?,
            row.get::<Option<String>>(2).map_err(|error| {
                global_db_operation_error("decode workflow schema object SQL", error)
            })?,
        ));
    }
    if tables.is_empty() {
        return Ok(Ok(WorkflowSchemaAdmission::Create));
    }

    let actual_workflow_tables = tables
        .iter()
        .filter(|(object_type, name, _)| {
            object_type == "table"
                && WORKFLOW_TABLE_CONTRACTS_V1
                    .iter()
                    .any(|contract| contract.name == name.as_str())
        })
        .map(|(_, name, sql)| (name.as_str(), sql.as_deref()))
        .collect::<Vec<_>>();
    let expected_workflow_tables = WORKFLOW_TABLE_CONTRACTS_V1
        .iter()
        .map(|contract| (contract.name, Some(contract.sql)))
        .collect::<Vec<_>>();
    if actual_workflow_tables != expected_workflow_tables {
        match classify_published_artifact_ceiling(conn, &actual_workflow_tables).await? {
            PublishedArtifactCeilingShape::Upgradable => {
                return Ok(Ok(WorkflowSchemaAdmission::UpgradePublishedArtifactCeiling));
            }
            PublishedArtifactCeilingShape::Refused(reason) => {
                return Ok(Err(workflow_schema_refusal(reason)));
            }
            PublishedArtifactCeilingShape::Other => {}
        }
        let identity = if actual_workflow_tables
            .iter()
            .any(|(name, _)| *name == "workflow_schema")
        {
            Some(recorded_workflow_identity(conn).await?)
        } else {
            None
        };
        return Ok(Err(workflow_schema_refusal(workflow_table_drift_reason(
            &actual_workflow_tables,
            identity,
        ))));
    }

    let mut schema = conn
        .query(
            "SELECT singleton, schema_version, definition_digest FROM workflow_schema
             ORDER BY singleton",
            (),
        )
        .await
        .map_err(|error| global_db_operation_error("inspect workflow schema identity", error))?;
    let Some(identity) = schema
        .next()
        .await
        .map_err(|error| global_db_operation_error("read workflow schema identity", error))?
    else {
        return Ok(Err(workflow_schema_refusal(
            "workflow schema identity is missing",
        )));
    };
    let singleton = identity
        .get::<i64>(0)
        .map_err(|error| global_db_operation_error("decode workflow schema singleton", error))?;
    let schema_version = identity
        .get::<i64>(1)
        .map_err(|error| global_db_operation_error("decode workflow schema version", error))?;
    let definition_digest = identity
        .get::<String>(2)
        .map_err(|error| global_db_operation_error("decode workflow schema digest", error))?;
    let extra_identity = schema
        .next()
        .await
        .map_err(|error| global_db_operation_error("read workflow schema identity", error))?
        .is_some();
    if singleton != 1
        || schema_version != WORKFLOW_SCHEMA_VERSION_V1
        || definition_digest != WORKFLOW_SCHEMA_DEFINITION_DIGEST_V1
        || extra_identity
    {
        return Ok(Err(workflow_schema_refusal(
            workflow_identity_drift_reason(
                singleton,
                schema_version,
                &definition_digest,
                extra_identity,
            ),
        )));
    }

    for table in WORKFLOW_TABLE_CONTRACTS_V1 {
        let mut columns = conn
            .query(&format!("PRAGMA table_info({})", table.name), ())
            .await
            .map_err(|error| global_db_operation_error("inspect workflow table columns", error))?;
        let mut actual_columns = Vec::new();
        while let Some(row) = columns
            .next()
            .await
            .map_err(|error| global_db_operation_error("read workflow table columns", error))?
        {
            actual_columns.push((
                row.get::<String>(1).map_err(|error| {
                    global_db_operation_error("decode workflow column name", error)
                })?,
                row.get::<String>(2).map_err(|error| {
                    global_db_operation_error("decode workflow column type", error)
                })?,
                row.get::<i64>(3).map_err(|error| {
                    global_db_operation_error("decode workflow column nullability", error)
                })?,
                row.get::<i64>(5).map_err(|error| {
                    global_db_operation_error("decode workflow column key", error)
                })?,
            ));
        }
        let exact = actual_columns.len() == table.columns.len()
            && actual_columns
                .iter()
                .zip(table.columns)
                .all(|(actual, expected)| {
                    actual.0 == expected.name
                        && actual.1 == expected.sql_type
                        && actual.2 == expected.not_null
                        && actual.3 == expected.primary_key
                });
        if !exact {
            return Ok(Err(workflow_schema_refusal(workflow_column_drift_reason(
                table,
                &actual_columns,
            ))));
        }
    }

    Ok(Ok(WorkflowSchemaAdmission::Complete))
}

fn workflow_schema_refusal(reason: impl Into<Cow<'static, str>>) -> RefusedAuthorityV1 {
    RefusedAuthorityV1::Shape {
        authority: "workflow",
        reason: reason.into(),
    }
}

/// Most object names one workflow refusal lists. The contract has a fixed,
/// small table set, so the cap only bounds the message if that set grows.
const WORKFLOW_REFUSAL_NAME_LIMIT: usize = 8;

fn bounded_name_list(names: &[&str]) -> String {
    let shown = names
        .iter()
        .take(WORKFLOW_REFUSAL_NAME_LIMIT)
        .map(|name| format!("`{name}`"))
        .collect::<Vec<_>>()
        .join(", ");
    match names.len().checked_sub(WORKFLOW_REFUSAL_NAME_LIMIT) {
        Some(rest) if rest > 0 => format!("{shown} and {rest} more"),
        _ => shown,
    }
}

/// A digest echoes only in the exact form this binary writes; any other
/// stored text is reported as unrecognized rather than copied out.
fn workflow_digest_label(digest: &str) -> String {
    let hex = digest.strip_prefix("sha256:");
    if hex.is_some_and(|hex| {
        hex.len() == 64 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    }) {
        digest.to_owned()
    } else {
        "an unrecognized digest".to_owned()
    }
}

fn required_workflow_identity() -> String {
    format!(
        "this binary requires version {WORKFLOW_SCHEMA_VERSION_V1} with digest \
         {WORKFLOW_SCHEMA_DEFINITION_DIGEST_V1}"
    )
}

/// The identity a store with drifted workflow tables records, read only to
/// make the refusal actionable: comparing it with the required digest tells
/// a store written by another release from a damaged one.
///
/// A drifted `workflow_schema` may not even answer this query, which is more
/// drift to report. A database that cannot be read right now says nothing about
/// the store, so that stays a typed failure rather than a reset reason.
async fn recorded_workflow_identity(
    conn: &impl QueryExecutor,
) -> tracedecay_domain::errors::Result<String> {
    match read_recorded_workflow_identity(conn).await {
        Ok(Some(identity)) if identity.extra => Ok(format!(
            "the store records several identity rows, the first version {} with {}",
            identity.version,
            workflow_digest_label(&identity.digest)
        )),
        Ok(Some(identity)) => Ok(format!(
            "the store records version {} with {}",
            identity.version,
            workflow_digest_label(&identity.digest)
        )),
        Ok(None) => Ok("the store records no workflow identity".to_owned()),
        Err(error) if error.is_transient_read_failure() => Err(global_db_operation_error(
            "read recorded workflow identity",
            error,
        )),
        Err(_) => Ok("the store's recorded workflow identity is unreadable".to_owned()),
    }
}

struct RecordedWorkflowIdentity {
    singleton: i64,
    version: i64,
    digest: String,
    extra: bool,
}

async fn read_recorded_workflow_identity(
    conn: &impl QueryExecutor,
) -> engine::Result<Option<RecordedWorkflowIdentity>> {
    let mut rows = conn
        .query(
            "SELECT singleton, schema_version, definition_digest FROM workflow_schema
             ORDER BY singleton LIMIT 2",
            (),
        )
        .await?;
    let Some(first) = rows.next().await? else {
        return Ok(None);
    };
    let singleton = first.get::<i64>(0)?;
    let version = first.get::<i64>(1)?;
    let digest = first.get::<String>(2)?;
    let extra = rows.next().await?.is_some();
    Ok(Some(RecordedWorkflowIdentity {
        singleton,
        version,
        digest,
        extra,
    }))
}

fn workflow_table_drift_reason(
    actual: &[(&str, Option<&str>)],
    identity: Option<String>,
) -> String {
    let missing = WORKFLOW_TABLE_CONTRACTS_V1
        .iter()
        .filter(|contract| !actual.iter().any(|(name, _)| *name == contract.name))
        .map(|contract| contract.name)
        .collect::<Vec<_>>();
    let altered = WORKFLOW_TABLE_CONTRACTS_V1
        .iter()
        .filter(|contract| {
            actual
                .iter()
                .any(|(name, sql)| *name == contract.name && *sql != Some(contract.sql))
        })
        .map(|contract| contract.name)
        .collect::<Vec<_>>();
    if actual.is_empty() {
        return format!(
            "workflow tables are absent from a store that holds other tables: all {} \
             contract tables ({}) are missing",
            missing.len(),
            bounded_name_list(&missing),
        );
    }
    let mut parts = Vec::new();
    if !missing.is_empty() {
        parts.push(format!(
            "missing {} ({})",
            missing.len(),
            bounded_name_list(&missing)
        ));
    }
    if !altered.is_empty() {
        parts.push(format!(
            "definition differs from the final contract for {} ({})",
            altered.len(),
            bounded_name_list(&altered)
        ));
    }
    if parts.is_empty() {
        parts.push("table set differs from the final contract".to_owned());
    }
    let mut reason = format!("workflow tables are not exact: {}", parts.join("; "));
    if let Some(identity) = identity {
        reason.push_str(&format!("; {identity}; {}", required_workflow_identity()));
    }
    reason
}

fn workflow_identity_drift_reason(
    singleton: i64,
    schema_version: i64,
    definition_digest: &str,
    extra_identity: bool,
) -> String {
    let mut parts = Vec::new();
    if singleton != 1 {
        parts.push(format!("identity row key is {singleton}, not 1"));
    }
    if schema_version != WORKFLOW_SCHEMA_VERSION_V1 {
        parts.push(format!(
            "version is {schema_version}, not {WORKFLOW_SCHEMA_VERSION_V1}"
        ));
    }
    if definition_digest != WORKFLOW_SCHEMA_DEFINITION_DIGEST_V1 {
        parts.push(format!(
            "digest is {}, not {WORKFLOW_SCHEMA_DEFINITION_DIGEST_V1}",
            workflow_digest_label(definition_digest)
        ));
    }
    if extra_identity {
        parts.push("more than one identity row is recorded".to_owned());
    }
    format!(
        "workflow schema identity does not match the final contract: {}",
        parts.join("; ")
    )
}

fn workflow_column_drift_reason(
    table: &WorkflowTableContractV1,
    actual: &[(String, String, i64, i64)],
) -> String {
    let detail = if actual.len() != table.columns.len() {
        format!(
            "{} columns are stored, {} are required",
            actual.len(),
            table.columns.len()
        )
    } else {
        actual
            .iter()
            .zip(table.columns)
            .enumerate()
            .find_map(|(index, (actual, expected))| {
                let mut differs = Vec::new();
                if actual.0 != expected.name {
                    differs.push("name");
                }
                if actual.1 != expected.sql_type {
                    differs.push("type");
                }
                if actual.2 != expected.not_null {
                    differs.push("not-null flag");
                }
                if actual.3 != expected.primary_key {
                    differs.push("primary-key position");
                }
                (!differs.is_empty()).then(|| {
                    format!(
                        "column {} (required `{}`) differs in {}",
                        index + 1,
                        expected.name,
                        differs.join(", ")
                    )
                })
            })
            .unwrap_or_else(|| "column set differs".to_owned())
    };
    format!(
        "workflow table columns do not match the final contract: `{}`: {detail}",
        table.name
    )
}

/// Wall-clock budget for one diagnostic run of the exhaustive authority audit.
///
/// Doctor and `tracedecay_runtime` answer an interactive caller: the CLI waits
/// 90 s for the whole runtime reply. On a store too large to audit inside
/// this budget the audit reports [`ObservationAuthorityAuditV1::BudgetExceeded`];
/// schema convergence still runs the same audit without a deadline.
pub const OBSERVATION_AUTHORITY_AUDIT_BUDGET: Duration = Duration::from_secs(60);

/// Outcome of one diagnostic exhaustive observation-authority audit.
#[derive(Debug)]
pub enum ObservationAuthorityAuditV1 {
    /// The audit ran to completion and every invariant held.
    Held,
    /// The audit ran and an invariant failed.
    Violated(tracedecay_domain::errors::TraceDecayError),
    /// The audit did not finish inside its budget. This says nothing about
    /// the stored rows.
    BudgetExceeded { budget: Duration },
    /// The authority would not yield a read snapshot to audit.
    Unavailable(tracedecay_domain::errors::TraceDecayError),
}

impl ObservationAuthorityAuditV1 {
    /// `Some(true)` held, `Some(false)` violated, `None` not finished.
    #[must_use]
    pub fn verdict(&self) -> Option<bool> {
        match self {
            Self::Held => Some(true),
            Self::Violated(_) => Some(false),
            Self::BudgetExceeded { .. } | Self::Unavailable(_) => None,
        }
    }

    /// The typed `authority_audit_reason` wire value.
    #[must_use]
    pub fn reason(&self) -> Option<&'static str> {
        match self {
            Self::Held => None,
            Self::Violated(_) => Some("authority_invariant_failed"),
            Self::BudgetExceeded { .. } => Some("authority_audit_budget_exceeded"),
            Self::Unavailable(_) => Some("authority_store_unavailable"),
        }
    }

    /// The observed `authority_audit_error` detail.
    #[must_use]
    pub fn detail(&self) -> Option<String> {
        match self {
            Self::Held => None,
            Self::Violated(error) | Self::Unavailable(error) => Some(error.to_string()),
            Self::BudgetExceeded { budget } => Some(format!(
                "audit skipped: store too large for the {} s doctor budget; schema \
                 convergence runs the exhaustive audit without a deadline",
                budget.as_secs()
            )),
        }
    }
}

/// Runs the exhaustive observation-authority audit on a health snapshot whose
/// statements are all interrupted once `budget` has elapsed.
pub async fn audit_observation_authority(
    database: &Database,
    budget: Duration,
) -> ObservationAuthorityAuditV1 {
    let deadline = Instant::now() + budget;
    let snapshot = match database.begin_engine_health_read_snapshot().await {
        Ok(snapshot) => snapshot.until(deadline),
        Err(error) => {
            return ObservationAuthorityAuditV1::Unavailable(global_db_operation_error(
                "begin observation authority audit",
                error,
            ));
        }
    };
    let audit = async {
        validate_authority_schema_contract(&snapshot).await?;
        let rows: Pin<Box<dyn Future<Output = tracedecay_domain::errors::Result<()>> + Send + '_>> =
            Box::pin(validate_authority_rows_exhaustive(&snapshot));
        rows.await
    };
    match audit.await {
        Ok(()) => ObservationAuthorityAuditV1::Held,
        Err(error)
            if matches!(
                error.project_open_failure_kind(),
                Some(ProjectOpenFailureKind::AuthorityVerdict { .. })
            ) =>
        {
            ObservationAuthorityAuditV1::Violated(error)
        }
        // Only the shared deadline interrupts these statements, so a
        // non-verdict failure past it is the budget, not the rows.
        Err(_) if Instant::now() >= deadline => {
            ObservationAuthorityAuditV1::BudgetExceeded { budget }
        }
        Err(error) => ObservationAuthorityAuditV1::Violated(error),
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;
    use tracedecay_lcm::LCM_SCHEMA_VERSION;
    use tracedecay_rusqlite_runtime::workflow::{
        WORKFLOW_SCHEMA_DEFINITION_DIGEST_V1, WORKFLOW_TABLE_CONTRACTS_V1,
    };
    use tracedecay_sessions::runtime::git_correlation::GIT_CORRELATION_SCHEMA_VERSION;

    use super::WORKFLOW_REFUSAL_NAME_LIMIT;
    use crate::tests::harness::open_registered_test_database_fixture;
    use tracedecay_runtime_core::db::TestDatabaseRuntimeScope;
    use tracedecay_runtime_core::db::engine::{
        IntoParams, QueryExecutor, Rows, TestConnection, params,
    };

    async fn install_registered_schema(database_path: &std::path::Path) {
        drop(
            open_registered_test_database_fixture(
                database_path,
                TestDatabaseRuntimeScope::ProfileSessions,
            )
            .await
            .expect("initialize final registered authority schema"),
        );
    }

    async fn registered_admission_error(
        database_path: &std::path::Path,
    ) -> tracedecay_domain::errors::TraceDecayError {
        match open_registered_test_database_fixture(
            database_path,
            TestDatabaseRuntimeScope::ProfileSessions,
        )
        .await
        {
            Ok(_) => panic!("incompatible registered schema must not be admitted"),
            Err(error) => error,
        }
    }

    /// Every registered store a released binary wrote carries the same
    /// session-feature markers, `global.db` included. A session store with an
    /// older LCM, git correlation, or workflow schema is refused for its
    /// sessions, while the profile authority, which holds no session rows,
    /// keeps serving its registry and leaves those markers exactly as found.
    #[asupersync::test]
    async fn released_session_feature_markers_refuse_session_stores_but_not_the_profile_authority()
    {
        const PREVIOUS_LCM: i64 = LCM_SCHEMA_VERSION - 1;
        const PREVIOUS_GIT_CORRELATION: i64 = GIT_CORRELATION_SCHEMA_VERSION - 1;
        let age_session_features = format!(
            "UPDATE tracedecay_schema_version SET version = {PREVIOUS_LCM} WHERE component = 'lcm';
             UPDATE tracedecay_schema_version SET version = {PREVIOUS_GIT_CORRELATION}
             WHERE component = 'git_correlation';
             UPDATE workflow_schema SET definition_digest =
                'sha256:0000000000000000000000000000000000000000000000000000000000000000';"
        );
        let directory = TempDir::new().unwrap();
        let project_root = directory.path().join("project");
        std::fs::create_dir_all(&project_root).unwrap();
        let mut verdicts = Vec::new();
        for (file, scope) in [
            ("global.db", TestDatabaseRuntimeScope::Profile),
            (
                "user-sessions.db",
                TestDatabaseRuntimeScope::ProfileSessions,
            ),
        ] {
            let path = directory.path().join(file);
            let (lease, owner) = open_registered_test_database_fixture(&path, scope.clone())
                .await
                .unwrap();
            lease
                .upsert_code_project(
                    "project.registered",
                    &project_root,
                    None,
                    None,
                    Some("main"),
                )
                .await
                .unwrap();
            drop((lease, owner));
            rusqlite::Connection::open(&path)
                .unwrap()
                .execute_batch(&age_session_features)
                .unwrap();

            let (lease, owner) = open_registered_test_database_fixture(&path, scope)
                .await
                .unwrap_or_else(|error| panic!("{file} must stay admissible: {error}"));
            let refusal = owner.reset_required().map(|error| match error {
                tracedecay_domain::errors::TraceDecayError::ProfileResetRequired {
                    component,
                    found_version,
                    required_version,
                } => (component, found_version, required_version),
                other => panic!("{file} refused with an untyped reset: {other}"),
            });
            let projects: Vec<String> = lease
                .list_code_projects(10)
                .await
                .unwrap()
                .into_iter()
                .map(|project| project.project_id)
                .collect();
            drop((lease, owner));
            let markers: (i64, i64, String) = rusqlite::Connection::open(&path)
                .unwrap()
                .query_row(
                    "SELECT
                        (SELECT version FROM tracedecay_schema_version WHERE component = 'lcm'),
                        (SELECT version FROM tracedecay_schema_version
                         WHERE component = 'git_correlation'),
                        (SELECT definition_digest FROM workflow_schema)",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .unwrap();
            verdicts.push((file, refusal, projects, markers));
        }

        let aged_markers = (
            PREVIOUS_LCM,
            PREVIOUS_GIT_CORRELATION,
            "sha256:0000000000000000000000000000000000000000000000000000000000000000".to_owned(),
        );
        assert_eq!(
            verdicts,
            vec![
                (
                    "global.db",
                    None,
                    vec!["project.registered".to_owned()],
                    aged_markers.clone(),
                ),
                (
                    "user-sessions.db",
                    Some(("LCM", Some(PREVIOUS_LCM), LCM_SCHEMA_VERSION)),
                    vec!["project.registered".to_owned()],
                    aged_markers,
                ),
            ]
        );
    }

    #[asupersync::test]
    async fn existing_store_reinstalls_the_project_leading_session_lookup_index() {
        let directory = TempDir::new().unwrap();
        let database_path = directory.path().join("sessions.db");
        install_registered_schema(&database_path).await;
        {
            let connection = rusqlite::Connection::open(&database_path).unwrap();
            connection
                .execute_batch(
                    "DROP INDEX idx_sessions_project_provider_session;
                     INSERT INTO sessions(provider, session_id, project_key, project_path)
                     VALUES
                       ('claude', 'session-a', 'project-a', '/project-a'),
                       ('codex', 'session-b', 'project-b', '/project-b');",
                )
                .expect("shape an admissible existing store without the lookup index");
        }

        install_registered_schema(&database_path).await;

        let connection = TestConnection::open(&database_path);
        let mut rows = connection
            .query(
                "SELECT 1 FROM sqlite_schema
                 WHERE type = 'index' AND name = 'idx_sessions_project_provider_session'",
                (),
            )
            .await
            .unwrap();
        assert!(
            rows.next().await.unwrap().is_some(),
            "attached-store admission must re-ensure the idempotent transcript index"
        );
        drop(rows);

        let mut rows = connection
            .query(
                "EXPLAIN QUERY PLAN
                 SELECT session_id
                 FROM sessions
                 WHERE project_key = ?1 AND provider = ?2
                 ORDER BY session_id
                 LIMIT 32",
                params!["project-a", "claude"],
            )
            .await
            .unwrap();
        let mut details = Vec::new();
        while let Some(row) = rows.next().await.unwrap() {
            details.push(row.get::<String>(3).unwrap());
        }
        assert!(
            details
                .iter()
                .any(|detail| detail.contains("idx_sessions_project_provider_session")),
            "project/provider session lookup did not use the project-leading index: {details:?}"
        );
        assert!(
            !details.iter().any(|detail| detail.contains("TEMP B-TREE")),
            "project/provider session lookup regressed to an explicit sort: {details:?}"
        );
    }

    #[asupersync::test]
    async fn existing_registry_without_remote_deletion_catalog_requires_typed_reset() {
        let directory = TempDir::new().unwrap();
        let database_path = directory.path().join("sessions.db");
        install_registered_schema(&database_path).await;
        {
            let connection = rusqlite::Connection::open(&database_path).unwrap();
            connection
                .execute_batch("DROP TABLE remote_deletion_tombstones")
                .expect("remove required final V2 catalog");
        }

        let error = registered_admission_error(&database_path).await;
        let connection = TestConnection::open(&database_path);
        let Some((authority, reason)) = error.reset_required_context() else {
            panic!("missing final V2 catalog returned the wrong typed problem: {error}");
        };
        assert_eq!(authority, "remote deletion tombstones");
        assert!(
            reason.contains("remote_deletion_tombstones"),
            "reset problem must identify the missing final catalog: {reason}"
        );
        let mut rows = connection
            .query(
                "SELECT 1 FROM sqlite_schema
                 WHERE type = 'table' AND name = 'remote_deletion_tombstones'",
                (),
            )
            .await
            .unwrap();
        assert!(
            rows.next().await.unwrap().is_none(),
            "rejected catalog must not be silently migrated"
        );
    }

    #[asupersync::test]
    async fn existing_registry_with_mismatched_remote_deletion_catalog_requires_typed_reset() {
        let directory = TempDir::new().unwrap();
        let database_path = directory.path().join("sessions.db");
        install_registered_schema(&database_path).await;
        {
            let connection = rusqlite::Connection::open(&database_path).unwrap();
            connection
                .execute_batch(
                    "ALTER TABLE remote_deletion_tombstones
                     ADD COLUMN incompatible_branch_catalog TEXT",
                )
                .expect("make the required final V2 catalog incompatible");
        }

        let error = registered_admission_error(&database_path).await;
        let connection = TestConnection::open(&database_path);
        assert!(
            matches!(
                error,
                tracedecay_domain::errors::TraceDecayError::ResetRequired { .. }
            ),
            "incompatible final V2 catalog returned the wrong typed problem: {error}"
        );
        let mut rows = connection
            .query(
                "SELECT 1 FROM pragma_table_xinfo('remote_deletion_tombstones')
                 WHERE name = 'incompatible_branch_catalog'",
                (),
            )
            .await
            .unwrap();
        assert!(
            rows.next().await.unwrap().is_some(),
            "rejected catalog must not be silently converged"
        );
    }

    /// A registry whose `code_projects` is not the final shape, whether the
    /// 8-column table released binaries through 0.0.74 created or any other
    /// drift, is refused with the project-registry reset state and left
    /// exactly as admission found it instead of being altered in place.
    #[asupersync::test]
    async fn registry_with_non_final_code_project_shape_requires_typed_reset() {
        for (drift, columns_after_refusal) in [
            (
                "ALTER TABLE code_projects DROP COLUMN primary_root_platform;
                 ALTER TABLE code_projects DROP COLUMN primary_root_bytes;
                 ALTER TABLE code_projects DROP COLUMN primary_root_last_seen_at;",
                "project_id,canonical_root,display_root,git_common_dir,git_remote_url,\
                 default_branch,created_at,last_seen_at",
            ),
            (
                "ALTER TABLE code_projects ADD COLUMN unknown_shape TEXT",
                "project_id,canonical_root,display_root,primary_root_platform,\
                 primary_root_bytes,primary_root_last_seen_at,git_common_dir,git_remote_url,\
                 default_branch,created_at,last_seen_at,unknown_shape",
            ),
        ] {
            let directory = TempDir::new().unwrap();
            let database_path = directory.path().join("sessions.db");
            install_registered_schema(&database_path).await;
            rusqlite::Connection::open(&database_path)
                .unwrap()
                .execute_batch(drift)
                .unwrap();

            let error = registered_admission_error(&database_path).await;
            assert_eq!(
                error.reset_required_context(),
                Some((
                    super::project_registry::PROJECT_REGISTRY_AUTHORITY,
                    "database error: table 'code_projects' has an incompatible number of \
                     columns (operation: validate global database authority schema)"
                )),
                "{drift}"
            );
            let columns: String = rusqlite::Connection::open(&database_path)
                .unwrap()
                .query_row(
                    "SELECT group_concat(name, ',') FROM pragma_table_xinfo('code_projects')",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(columns, columns_after_refusal, "{drift}");
        }
    }

    /// Released registries (through v1.0.0-beta.63) record a per-scope
    /// `db_relpath`. That shape is refused with the project-registry reset
    /// state, and its rows are left exactly as admission found them.
    #[asupersync::test]
    async fn registry_with_released_graph_scope_relpath_requires_typed_reset() {
        let directory = TempDir::new().unwrap();
        let database_path = directory.path().join("sessions.db");
        install_registered_schema(&database_path).await;
        {
            let connection = rusqlite::Connection::open(&database_path).unwrap();
            let dependents = connection
                .prepare(
                    "SELECT sql FROM sqlite_schema
                     WHERE tbl_name = 'graph_scopes' AND type IN ('index', 'trigger')
                       AND sql IS NOT NULL",
                )
                .unwrap()
                .query_map([], |row| row.get::<_, String>(0))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            connection
                .execute_batch(
                    "DROP TABLE graph_scopes;
                     CREATE TABLE graph_scopes (
                         graph_scope_id TEXT PRIMARY KEY,
                         project_id TEXT NOT NULL,
                         store_id TEXT NOT NULL,
                         branch_name TEXT NOT NULL,
                         db_relpath TEXT NOT NULL,
                         parent_scope_id TEXT,
                         last_synced_at INTEGER,
                         writable INTEGER NOT NULL DEFAULT 1,
                         FOREIGN KEY(project_id) REFERENCES code_projects(project_id)
                             ON DELETE CASCADE,
                         FOREIGN KEY(store_id) REFERENCES store_instances(store_id)
                             ON DELETE CASCADE
                     );",
                )
                .unwrap();
            for sql in dependents {
                connection.execute_batch(&sql).unwrap();
            }
            connection
                .execute_batch(
                    "INSERT INTO code_projects
                        (project_id, canonical_root, display_root, created_at, last_seen_at)
                     VALUES ('released', '/released', '/released', 1, 1);
                     INSERT INTO store_instances
                        (store_id, project_id, store_kind, storage_mode, store_relpath,
                         created_at)
                     VALUES ('store', 'released', 'code_project', 'profile_sharded',
                             'projects/released', 1);
                     INSERT INTO graph_scopes
                        (graph_scope_id, project_id, store_id, branch_name, db_relpath)
                     VALUES ('store:branch:main', 'released', 'store', 'main',
                             'projects/released/tracedecay.db');",
                )
                .unwrap();
        }

        let error = registered_admission_error(&database_path).await;

        assert_eq!(
            error.reset_required_context(),
            Some((
                super::project_registry::PROJECT_REGISTRY_AUTHORITY,
                "database error: table 'graph_scopes' has an incompatible number of \
                 columns (operation: validate global database authority schema)"
            ))
        );
        let row: (String, String) = rusqlite::Connection::open(&database_path)
            .unwrap()
            .query_row(
                "SELECT (SELECT group_concat(name, ',') FROM pragma_table_xinfo('graph_scopes')),
                        db_relpath
                 FROM graph_scopes",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            row,
            (
                "graph_scope_id,project_id,store_id,branch_name,db_relpath,parent_scope_id,\
                 last_synced_at,writable"
                    .to_owned(),
                "projects/released/tracedecay.db".to_owned()
            )
        );
    }

    #[asupersync::test]
    async fn restored_triggers_still_refuse_corrupt_rows_without_repairing_them() {
        let directory = TempDir::new().unwrap();
        let database_path = directory.path().join("sessions.db");
        install_registered_schema(&database_path).await;
        {
            let connection = rusqlite::Connection::open(&database_path).unwrap();
            connection
                .execute_batch(
                    "PRAGMA foreign_keys = OFF;
                 DROP TRIGGER IF EXISTS projection_queue_identity_insert_v1;
                 DROP TRIGGER IF EXISTS session_query_cursor_keys_insert_guard_v1;
                 DROP TRIGGER IF EXISTS session_query_cursor_keys_retire_update_v1;
                 DROP TRIGGER IF EXISTS session_query_cursor_keys_rotate_insert_v1;
                 INSERT INTO projection_queue(observation_id, observation_sequence)
                 VALUES ('orphaned-observation', 1);
                 INSERT INTO session_query_cursor_keys (
                    key_id, key_version, key_material, created_at, retired_at
                 ) VALUES
                    ('cursor-a', 1, X'01', 100, NULL),
                    ('cursor-b', 2, X'02', 200, NULL);
                 DELETE FROM authority_audit_checkpoints;",
                )
                .expect("seed corrupt rows behind dropped guard triggers");
        }

        // Damaged guard triggers are restored during admission so the store
        // stays repairable, but restoration must not launder the rows the
        // triggers failed to guard: the row audit still refuses the store.
        for attempt in 1..=2u8 {
            let error = registered_admission_error(&database_path).await;
            assert!(
                error
                    .to_string()
                    .contains("session cursor key rotation state is invalid"),
                "open attempt {attempt} must keep refusing the corrupt rotation state: {error}"
            );
        }
        let connection = TestConnection::open(&database_path);
        let mut rows = connection
            .query("SELECT COUNT(*) FROM projection_queue", ())
            .await
            .unwrap();
        assert_eq!(
            rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
            1,
            "audit refusal must not repair queued rows before row admission"
        );
        drop(rows);
        let mut rows = connection
            .query(
                "SELECT COUNT(*) FROM session_query_cursor_keys WHERE retired_at IS NULL",
                (),
            )
            .await
            .unwrap();
        assert_eq!(
            rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
            2,
            "audit refusal must not retire or delete the conflicting cursor keys"
        );
        drop(rows);
        let mut rows = connection
            .query(
                "SELECT 1 FROM sqlite_schema
                 WHERE type = 'trigger' AND name = 'projection_queue_identity_insert_v1'",
                (),
            )
            .await
            .unwrap();
        assert!(
            rows.next().await.unwrap().is_some(),
            "admission must restore the dropped guard trigger before refusing on rows"
        );
    }

    #[asupersync::test]
    async fn restored_triggers_arm_a_persistent_audit_that_refuses_fk_violations() {
        let directory = TempDir::new().unwrap();
        let database_path = directory.path().join("sessions.db");
        install_registered_schema(&database_path).await;
        {
            let connection = rusqlite::Connection::open(&database_path).unwrap();
            connection
                .execute_batch(
                    "PRAGMA foreign_keys = OFF;
                     DROP TRIGGER IF EXISTS projection_queue_identity_insert_v1;
                     CREATE TABLE audit_parent (id INTEGER PRIMARY KEY);
                     CREATE TABLE audit_child (
                        id INTEGER PRIMARY KEY,
                        parent_id INTEGER NOT NULL REFERENCES audit_parent(id)
                     );
                     INSERT INTO audit_child(id, parent_id) VALUES (1, 99);",
                )
                .expect("seed a foreign-key violation behind a broken trigger");
        }

        // The damaged trigger arms an exhaustive foreign-key audit whose
        // requirement is persisted before the trigger evidence is repaired,
        // so every subsequent open keeps refusing the violation even though
        // the trigger itself is already restored.
        for attempt in 1..=2u8 {
            let error = registered_admission_error(&database_path).await;
            assert!(
                error.to_string().contains("foreign-key violation"),
                "open attempt {attempt} must refuse the foreign-key violation: {error}"
            );
        }
        let connection = TestConnection::open(&database_path);
        let mut rows = connection
            .query("SELECT COUNT(*) FROM audit_child WHERE parent_id = 99", ())
            .await
            .unwrap();
        assert_eq!(
            rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
            1,
            "audit refusal must not repair or delete the violating row"
        );
        drop(rows);
        let mut rows = connection
            .query(
                "SELECT 1 FROM sqlite_schema
                 WHERE type = 'trigger' AND name = 'projection_queue_identity_insert_v1'",
                (),
            )
            .await
            .unwrap();
        assert!(
            rows.next().await.unwrap().is_some(),
            "admission must restore the dropped guard trigger while the audit refuses"
        );
    }

    #[asupersync::test]
    async fn damaged_triggers_on_a_clean_store_are_restored_and_admitted() {
        let directory = TempDir::new().unwrap();
        let database_path = directory.path().join("sessions.db");
        install_registered_schema(&database_path).await;
        {
            let connection = rusqlite::Connection::open(&database_path).unwrap();
            connection
                .execute_batch(
                    "DROP TRIGGER IF EXISTS projection_queue_identity_insert_v1;
                     DROP TRIGGER IF EXISTS session_query_cursor_keys_insert_guard_v1;",
                )
                .expect("drop guard triggers on an otherwise clean store");
        }

        // A guarded table rebuild drops its triggers with it; the next open
        // must restore them and re-admit the store once the armed exhaustive
        // audit finds nothing wrong, instead of refusing it forever.
        install_registered_schema(&database_path).await;
        let connection = TestConnection::open(&database_path);
        let mut rows = connection
            .query(
                "SELECT COUNT(*) FROM sqlite_schema
                 WHERE type = 'trigger' AND name IN (
                    'projection_queue_identity_insert_v1',
                    'session_query_cursor_keys_insert_guard_v1'
                 )",
                (),
            )
            .await
            .unwrap();
        assert_eq!(
            rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
            2,
            "admission must restore every dropped guard trigger"
        );
    }

    /// A digest the workflow schema carried in a published release.
    const BETA_92_WORKFLOW_DIGEST: &str =
        "sha256:a292df6bc47e763f0d20bdb44a4032c0b1d7ac8e4cb83173b66ae7d1ff0d03be";

    /// A ceiling no release ever shipped, under the published identity.
    const FOREIGN_ARTIFACT_PAYLOADS_SQL: &str = "CREATE TABLE workflow_artifact_payloads (
    payload_digest TEXT NOT NULL PRIMARY KEY,
    byte_length INTEGER NOT NULL CHECK (
        byte_length = length(payload) AND byte_length <= 8388608
    ),
    payload BLOB NOT NULL
) STRICT";

    /// Admit a fresh session store, run `shape` against it, and return the
    /// workflow refusal the next admission reports.
    async fn workflow_refusal_after(shape: &str) -> (std::path::PathBuf, TempDir, String) {
        let directory = TempDir::new().unwrap();
        let database_path = directory.path().join("sessions.db");
        install_registered_schema(&database_path).await;
        rusqlite::Connection::open(&database_path)
            .unwrap()
            .execute_batch(shape)
            .expect("shape the stored workflow schema");
        let (lease, owner) = open_registered_test_database_fixture(
            &database_path,
            TestDatabaseRuntimeScope::ProfileSessions,
        )
        .await
        .expect("a refused workflow schema must not stop the store's other authorities");
        let refusal = owner
            .reset_required()
            .expect("drifted workflow schema must refuse session features");
        drop((lease, owner));
        let Some(("workflow", reason)) = refusal.reset_required_context() else {
            panic!("workflow drift refused with the wrong typed state: {refusal}");
        };
        let reason = reason.to_owned();
        (database_path, directory, reason)
    }

    /// A payload table that resembles the published one but is not it stays
    /// refused, and the refusal names the drifted table and both identities so
    /// an operator can tell it from damage.
    #[asupersync::test]
    async fn altered_artifact_table_refusal_names_the_table_and_both_identities() {
        let shape = format!(
            "DROP TABLE workflow_artifact_payloads;
             {FOREIGN_ARTIFACT_PAYLOADS_SQL};
             UPDATE workflow_schema SET definition_digest = '{BETA_92_WORKFLOW_DIGEST}';"
        );
        let (database_path, _directory, reason) = workflow_refusal_after(&shape).await;

        assert!(
            reason.contains(
                "definition differs from the final contract for 1 (`workflow_artifact_payloads`)"
            ),
            "{reason}"
        );
        assert!(!reason.contains("missing"), "{reason}");
        assert!(
            reason.contains(&format!(
                "the store records version 1 with {BETA_92_WORKFLOW_DIGEST}"
            )),
            "{reason}"
        );
        assert!(
            reason.contains(&format!(
                "this binary requires version 1 with digest {WORKFLOW_SCHEMA_DEFINITION_DIGEST_V1}"
            )),
            "{reason}"
        );
        assert!(
            !reason.contains("CREATE TABLE") && !reason.contains("8388608"),
            "stored schema SQL must not leak into the refusal: {reason}"
        );
        // Admission stays read-only: the refused table is left as found.
        let stored: String = rusqlite::Connection::open(&database_path)
            .unwrap()
            .query_row(
                "SELECT sql FROM sqlite_master WHERE name = 'workflow_artifact_payloads'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored, FOREIGN_ARTIFACT_PAYLOADS_SQL);
    }

    /// `workflow_schema` that cannot answer the identity query is one more
    /// drifted object to report, not a reason to fail the open.
    #[asupersync::test]
    async fn undecodable_recorded_identity_is_reported_as_unreadable() {
        let (_database_path, _directory, reason) = workflow_refusal_after(
            "DROP TABLE workflow_schema;
             CREATE TABLE workflow_schema (singleton INTEGER PRIMARY KEY);",
        )
        .await;
        assert!(
            reason.contains("the store's recorded workflow identity is unreadable"),
            "{reason}"
        );
    }

    struct BusyConnection;

    impl QueryExecutor for BusyConnection {
        async fn query<P>(
            &self,
            _sql: &str,
            _params: P,
        ) -> tracedecay_runtime_core::db::engine::Result<Rows>
        where
            P: IntoParams,
        {
            Err(tracedecay_runtime_core::db::engine::Error::Busy)
        }
    }

    /// A held database says nothing about the store, so it must not turn into
    /// reset wording.
    #[asupersync::test]
    async fn busy_identity_read_stays_a_typed_failure_instead_of_a_reset_reason() {
        let error = super::recorded_workflow_identity(&BusyConnection)
            .await
            .expect_err("a held database is not an unreadable identity");
        assert!(!error.is_store_reset_required(), "{error}");
        assert!(
            error
                .to_string()
                .contains("read recorded workflow identity"),
            "{error}"
        );
    }

    #[asupersync::test]
    async fn missing_workflow_table_is_named_without_blaming_present_tables() {
        let (_database_path, _directory, reason) =
            workflow_refusal_after("DROP TABLE workflow_handoffs;").await;

        assert!(
            reason.contains("workflow tables are not exact: missing 1 (`workflow_handoffs`)"),
            "{reason}"
        );
        assert!(!reason.contains("definition differs"), "{reason}");
        assert!(
            reason.contains(&format!(
                "the store records version 1 with {WORKFLOW_SCHEMA_DEFINITION_DIGEST_V1}"
            )),
            "{reason}"
        );
    }

    #[asupersync::test]
    async fn store_without_any_workflow_table_reports_them_absent() {
        let drops = WORKFLOW_TABLE_CONTRACTS_V1
            .iter()
            .map(|table| format!("DROP TABLE {};", table.name))
            .collect::<String>();
        let (_database_path, _directory, reason) = workflow_refusal_after(&drops).await;

        assert!(
            reason.contains("workflow tables are absent from a store that holds other tables"),
            "{reason}"
        );
        assert!(
            reason.contains(&format!(
                "all {} contract tables",
                WORKFLOW_TABLE_CONTRACTS_V1.len()
            )),
            "{reason}"
        );
        assert!(!reason.contains("the store records"), "{reason}");
    }

    #[asupersync::test]
    async fn workflow_identity_refusal_names_the_differing_fields_and_hides_foreign_text() {
        // The stored table pins `schema_version = 1`, so a digest is the one
        // identity field a store can disagree on while its tables stay exact.
        let (_database_path, _directory, reason) = workflow_refusal_after(
            "UPDATE workflow_schema SET definition_digest = 'operator secret token';",
        )
        .await;

        assert!(
            reason.contains("workflow schema identity does not match the final contract"),
            "{reason}"
        );
        assert!(!reason.contains("version is"), "{reason}");
        assert!(
            reason.contains(&format!(
                "digest is an unrecognized digest, not {WORKFLOW_SCHEMA_DEFINITION_DIGEST_V1}"
            )),
            "{reason}"
        );
        assert!(!reason.contains("operator secret token"), "{reason}");
    }

    #[test]
    fn workflow_identity_refusal_reports_every_differing_field() {
        let reason = super::workflow_identity_drift_reason(2, 3, BETA_92_WORKFLOW_DIGEST, true);
        assert!(reason.contains("identity row key is 2, not 1"), "{reason}");
        assert!(reason.contains("version is 3, not 1"), "{reason}");
        assert!(
            reason.contains(&format!(
                "digest is {BETA_92_WORKFLOW_DIGEST}, not {WORKFLOW_SCHEMA_DEFINITION_DIGEST_V1}"
            )),
            "{reason}"
        );
        assert!(reason.contains("more than one identity row"), "{reason}");
    }

    #[test]
    fn workflow_table_refusal_lists_a_bounded_number_of_names() {
        let names = (0..WORKFLOW_REFUSAL_NAME_LIMIT + 3)
            .map(|index| format!("t{index}"))
            .collect::<Vec<_>>();
        let names = names.iter().map(String::as_str).collect::<Vec<_>>();
        let listed = super::bounded_name_list(&names);
        assert!(listed.starts_with("`t0`, `t1`"), "{listed}");
        assert!(listed.ends_with("and 3 more"), "{listed}");
        assert_eq!(listed.matches('`').count(), WORKFLOW_REFUSAL_NAME_LIMIT * 2);
    }

    #[test]
    fn workflow_column_refusal_names_the_required_column_and_attributes() {
        let table = WORKFLOW_TABLE_CONTRACTS_V1
            .iter()
            .find(|table| table.name == "workflow_artifact_payloads")
            .unwrap();
        let mut columns = table
            .columns
            .iter()
            .map(|column| {
                (
                    column.name.to_owned(),
                    column.sql_type.to_owned(),
                    column.not_null,
                    column.primary_key,
                )
            })
            .collect::<Vec<_>>();
        columns[1].1 = "secret-type-text".to_owned();
        columns[1].2 = 1 - columns[1].2;
        let reason = super::workflow_column_drift_reason(table, &columns);
        assert!(
            reason.contains(
                "`workflow_artifact_payloads`: column 2 (required `byte_length`) differs in type, not-null flag"
            ),
            "{reason}"
        );
        assert!(!reason.contains("secret-type-text"), "{reason}");

        columns.pop();
        let reason = super::workflow_column_drift_reason(table, &columns);
        assert!(
            reason.contains("2 columns are stored, 3 are required"),
            "{reason}"
        );
    }
}
