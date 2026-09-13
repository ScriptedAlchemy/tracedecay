//! Central orchestrator for registered `TraceDecay` project storage.
//!
//! This module root holds the [`TraceDecay`] struct and its shared result
//! types; the behavior is implemented in focused submodules:
//! [`lifecycle`] (init/open/branch provenance), [`queries`]
//! (read-side graph queries), [`diagnostics`] (branch state), [`facts`]
//! (session memory), and source-edit orchestration.
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use tracedecay_configuration::TraceDecayConfig;
use tracedecay_contracts::context_scout::ContextScoutAddressV1;
use tracedecay_domain::errors::Result;
use tracedecay_graph_query::SourceReadContext;
use tracedecay_runtime_core::db::{Database, DatabaseStorageTelemetryHandle};
use tracedecay_runtime_core::storage::{self, StoreLayout};
use tracedecay_store_runtime::DaemonSessionRuntimeRegistryV1;

mod automation_context;
#[cfg(test)]
mod concrete_runtime_tests;
mod diagnostics;
pub mod facts;
mod lifecycle;
pub mod queries;
mod source_edit_runtime;

pub use diagnostics::{BranchDiagnostics, TrackedBranchDiagnostic};
pub use lifecycle::MovedStoreAdoption;

/// Why a `TraceDecay` instance has no Context Scout owner.
#[derive(Clone)]
pub enum ContextScoutOwnerLookupV1 {
    Ready(Arc<tracedecay_agent_hosts::agents::context_scout::owner::ProjectContextScoutOwnerV1>),
    ReadOnly,
    Unregistered,
}

/// Central orchestrator that coordinates all subsystems of the code graph.
///
/// Provides a high-level API for initializing, indexing, querying, and
/// syncing a Rust codebase's semantic knowledge graph.
pub struct TraceDecay {
    db: Database,
    profile_database: tracedecay_global_db::RegisteredGlobalDbLeaseV1,
    pub store_runtime_registry: Arc<DaemonSessionRuntimeRegistryV1>,
    config: TraceDecayConfig,
    configuration_runtime: Arc<tracedecay_configuration::ProjectConfigurationRuntime>,
    project_root: PathBuf,
    store_layout: StoreLayout,
    open_options: TraceDecayOpenOptions,
    /// The active git branch (None if detached HEAD or not a git repo).
    active_branch: Option<String>,
    /// The branch whose DB is actually being served (may differ from `active_branch` on fallback).
    serving_branch: Option<String>,
    /// Set when serving from a fallback (ancestor) DB instead of the exact branch.
    fallback_warning: Option<String>,
    read_only: bool,
    /// Memoized result of [`diagnostics::TraceDecay::db_path`]. All inputs
    /// (`project_root`, `store_layout.data_root`, `serving_branch`) are
    /// immutable for the lifetime of an instance — branch changes produce a
    /// new `TraceDecay` rather than mutating an existing one, so the resolved
    /// path is safe to cache for the instance's lifetime.
    db_path_cache: OnceLock<PathBuf>,
    /// The registered test runtime a fixture open went through, kept alive
    /// for as long as the graph is; `None` for every production open.
    #[cfg(any(test, feature = "test-helpers"))]
    test_runtime_guard:
        Option<Arc<crate::test_support::host_admission::HostAdmissionTestRuntimeV1>>,
    _standalone_maintenance_scope:
        Option<Arc<tracedecay_runtime_core::db::OwnedMaintenanceDatabaseScope>>,
}

impl TraceDecay {
    pub fn storage_telemetry_handle(&self) -> Result<DatabaseStorageTelemetryHandle> {
        self.db.storage_telemetry_handle()
    }

    #[hotpath::skip]
    pub async fn storage_page_counts(&self) -> Result<(u64, u64, u64)> {
        self.db.storage_page_counts().await
    }

    pub fn configuration_runtime(
        &self,
    ) -> &Arc<tracedecay_configuration::ProjectConfigurationRuntime> {
        &self.configuration_runtime
    }

    pub fn project_store_runtime(&self) -> &DaemonSessionRuntimeRegistryV1 {
        self.store_runtime_registry.as_ref()
    }

    pub fn profile_database(&self) -> &tracedecay_global_db::RegisteredGlobalDbLeaseV1 {
        &self.profile_database
    }

    #[doc(hidden)]
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn test_runtime_for_test(
        &self,
    ) -> Option<Arc<crate::test_support::host_admission::HostAdmissionTestRuntimeV1>> {
        self.test_runtime_guard.clone()
    }

    pub fn hook_store_layout(&self) -> &StoreLayout {
        &self.store_layout
    }

    pub fn source_read_context(&self) -> Option<SourceReadContext> {
        Some(SourceReadContext::new(
            self.project_root.clone(),
            self.db.clone(),
            self.read_only,
            self.store_layout.identity.project_id.clone()?,
        ))
    }

    pub fn context_scout_owner(
        &self,
    ) -> Option<Arc<tracedecay_agent_hosts::agents::context_scout::owner::ProjectContextScoutOwnerV1>>
    {
        match self.context_scout_owner_lookup() {
            ContextScoutOwnerLookupV1::Ready(owner) => Some(owner),
            ContextScoutOwnerLookupV1::ReadOnly | ContextScoutOwnerLookupV1::Unregistered => None,
        }
    }

    pub fn context_scout_owner_lookup(&self) -> ContextScoutOwnerLookupV1 {
        if self.read_only {
            return ContextScoutOwnerLookupV1::ReadOnly;
        }
        let Some(project_id) =
            tracedecay_agent_hosts::hooks::hook_project_id_for_layout(&self.store_layout)
        else {
            return ContextScoutOwnerLookupV1::Unregistered;
        };
        let mut owners =
            tracedecay_agent_hosts::agents::context_scout::owner::lookup_registered_context_scout_owners(
                project_id,
            );
        match owners.len() {
            1 => ContextScoutOwnerLookupV1::Ready(owners.remove(0)),
            _ => ContextScoutOwnerLookupV1::Unregistered,
        }
    }

    /// Publishes one hook-admissible Context Scout claim authority for an
    /// enqueued producer generation. The mount re-validates the durable
    /// address registry and the current Plan 20 configuration before the
    /// authority becomes claimable; a stale pin or a foreign address never
    /// mounts.
    #[allow(clippy::too_many_arguments)]
    #[hotpath::skip]
    pub async fn mount_current_context_scout_claim_authority(
        &self,
        registry: Arc<tracedecay_agent_hosts::agents::context_scout::ports::ProjectContextScoutAddressRegistryV1>,
        hook: &tracedecay_agent_hosts::agents::context_scout::ports::AdmittedContextScoutHookV1,
        pin: tracedecay_agent_hosts::agents::context_scout::ports::ContextScoutAuthorityPinV1,
        context: tracedecay_contracts::RequestContext,
        lifecycle: tracedecay_agent_hosts::agents::context_scout::ports::ContextScoutLifecycleAddressV1,
        address: ContextScoutAddressV1,
        input_watermark: [u8; 32],
        observed_at: tracedecay_domain::UtcMicros,
    ) -> bool {
        let Some(owner) = self.context_scout_owner() else {
            return false;
        };
        let configuration_is_current = self.context_scout_configuration_is_current(&pin).await;
        matches!(
            owner
                .mount_current_claim_authority(
                    registry,
                    hook,
                    pin,
                    context,
                    lifecycle,
                    address,
                    input_watermark,
                    observed_at,
                    configuration_is_current,
                )
                .await,
            tracedecay_agent_hosts::agents::context_scout::owner::ContextScoutClaimAdmissionV1::Mounted
                | tracedecay_agent_hosts::agents::context_scout::owner::ContextScoutClaimAdmissionV1::Replaced
        )
    }

    /// Resolves the claim authority mounted for one exact lifecycle, or
    /// `None` when nothing was mounted, the Plan 20 configuration moved past
    /// the mounted pin, or the durable address registry no longer resolves
    /// the mounted address for this hook.
    #[hotpath::skip]
    pub async fn resolve_current_context_scout_claim_authority(
        &self,
        hook: &tracedecay_agent_hosts::agents::context_scout::ports::AdmittedContextScoutHookV1,
        lifecycle: &tracedecay_agent_hosts::agents::context_scout::ports::ContextScoutLifecycleAddressV1,
        observed_at: tracedecay_domain::UtcMicros,
    ) -> Option<(ContextScoutAddressV1, [u8; 32])> {
        let owner = self.context_scout_owner()?;
        let pin = owner.mounted_claim_pin(lifecycle).await?;
        let configuration_is_current = self.context_scout_configuration_is_current(&pin).await;
        let resolved = owner
            .resolve_current_claim_authority(hook, lifecycle, observed_at, configuration_is_current)
            .await;
        // Re-check currentness after the registry read: a configuration
        // revision that lands mid-resolve must not hand out a stale claim.
        if resolved.is_some() && self.context_scout_configuration_is_current(&pin).await {
            resolved
        } else {
            None
        }
    }

    /// Resolves the authority mounted by completed producer work for an exact
    /// hook replay. Public Scout operations still authorize the returned
    /// address against the durable registry; this avoids repeating that store
    /// read inside the bounded hook acknowledgement path.
    pub async fn resolve_mounted_context_scout_claim_authority(
        &self,
        lifecycle: &tracedecay_agent_hosts::agents::context_scout::ports::ContextScoutLifecycleAddressV1,
    ) -> Option<(ContextScoutAddressV1, [u8; 32])> {
        let owner = self.context_scout_owner()?;
        let pin = owner.mounted_claim_pin(lifecycle).await?;
        if !self.context_scout_configuration_is_current(&pin).await {
            return None;
        }
        owner.resolve_admitted_claim(lifecycle).await
    }

    /// Resolves a unique mounted producer from an earlier event in the same
    /// routed session. The owner reuses and revalidates that producer's exact
    /// lifecycle authority; the later event supplies no replacement identity.
    #[hotpath::skip]
    pub async fn resolve_current_context_scout_session_claim_authority(
        &self,
        hook: &tracedecay_agent_hosts::agents::context_scout::ports::AdmittedContextScoutHookV1,
        lifecycle: &tracedecay_agent_hosts::agents::context_scout::ports::ContextScoutLifecycleAddressV1,
        observed_at: tracedecay_domain::UtcMicros,
    ) -> Option<(ContextScoutAddressV1, [u8; 32])> {
        let owner = self.context_scout_owner()?;
        let pin = owner.unique_session_claim_pin(lifecycle).await?;
        let configuration_is_current = self.context_scout_configuration_is_current(&pin).await;
        let resolved = owner
            .resolve_current_session_claim_authority(
                hook,
                lifecycle,
                &pin,
                observed_at,
                configuration_is_current,
            )
            .await;
        if resolved.is_some() && self.context_scout_configuration_is_current(&pin).await {
            resolved
        } else {
            None
        }
    }

    #[hotpath::skip]
    async fn context_scout_configuration_is_current(
        &self,
        pin: &tracedecay_agent_hosts::agents::context_scout::ports::ContextScoutAuthorityPinV1,
    ) -> bool {
        self.configuration_runtime
            .client()
            .current()
            .await
            .ok()
            .map(tracedecay_configuration::PinnedRuntimeConfiguration::into_current_state)
            .is_some_and(|current| pin.configuration().matches_current(&current))
    }
}

#[derive(Debug, Clone, Default)]
pub struct TraceDecayOpenOptions {
    pub profile_root: Option<PathBuf>,
    pub global_db_path: Option<PathBuf>,
}

impl TraceDecayOpenOptions {
    fn resolved_profile_root(&self) -> Result<PathBuf> {
        if let Some(profile_root) = &self.profile_root {
            return Ok(profile_root.clone());
        }
        if let Some(parent) = self
            .global_db_path
            .as_deref()
            .and_then(std::path::Path::parent)
        {
            return Ok(parent.to_path_buf());
        }
        storage::default_profile_root()
    }
}

/// Returns the current UNIX timestamp in seconds.
///
/// Defined in `tracedecay_runtime_core::tracedecay` because the memory and
/// `memory_v2` writers stamp records with it and those layers moved into the
/// kernel crate.
pub use tracedecay_runtime_core::tracedecay::current_timestamp;

/// Returns `true` if the file path looks like a test file.
///
/// Re-exported from the code-index crate so the segment list has one
/// definition shared by extraction and the orchestrator's read paths.
pub use tracedecay_code_index::is_test_file;
