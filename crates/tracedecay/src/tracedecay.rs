//! Central orchestrator for registered `TraceDecay` project storage.
//!
//! This module root holds the [`TraceDecay`] struct and its shared result
//! types; the behavior is implemented in focused submodules:
//! [`lifecycle`] (init/open/branch provenance), [`queries`]
//! (read-side graph queries), [`diagnostics`] (branch state), [`facts`]
//! (session memory), and source-edit orchestration.
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use crate::config::TraceDecayConfig;
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
pub(crate) mod facts;
mod lifecycle;
pub(crate) mod queries;
mod source_edit_runtime;

pub use diagnostics::{BranchDiagnostics, TrackedBranchDiagnostic};
pub use lifecycle::MovedStoreAdoption;

/// Central orchestrator that coordinates all subsystems of the code graph.
///
/// Provides a high-level API for initializing, indexing, querying, and
/// syncing a Rust codebase's semantic knowledge graph.
pub struct TraceDecay {
    db: Database,
    profile_database: tracedecay_global_db::RegisteredGlobalDbLeaseV1,
    pub(crate) store_runtime_registry: Arc<DaemonSessionRuntimeRegistryV1>,
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
    context_scout_owner: Option<
        Arc<tracedecay_agent_hosts::agents::context_scout_owner::ProjectContextScoutOwnerV1>,
    >,
    context_scout_claim_authorities: tokio::sync::RwLock<Vec<MountedContextScoutClaimAuthorityV1>>,
    #[cfg(any(test, feature = "test-transport"))]
    test_runtime_guard: Option<Arc<crate::host_admission::HostAdmissionTestRuntimeV1>>,
    _standalone_maintenance_scope:
        Option<Arc<tracedecay_runtime_core::db::OwnedMaintenanceDatabaseScope>>,
}

const MAX_MOUNTED_CONTEXT_SCOUT_CLAIM_AUTHORITIES: usize = 256;

#[derive(Clone)]
struct MountedContextScoutClaimAuthorityV1 {
    registry: Arc<
        tracedecay_agent_hosts::agents::context_scout_ports::ProjectContextScoutAddressRegistryV1,
    >,
    pin: tracedecay_agent_hosts::agents::context_scout_ports::ContextScoutAuthorityPinV1,
    context: tracedecay_contracts::RequestContext,
    lifecycle: tracedecay_agent_hosts::agents::context_scout_ports::ContextScoutLifecycleAddressV1,
    address: ContextScoutAddressV1,
    input_watermark: [u8; 32],
}

impl TraceDecay {
    pub(crate) fn storage_telemetry_handle(&self) -> Result<DatabaseStorageTelemetryHandle> {
        self.db.storage_telemetry_handle()
    }

    #[hotpath::skip]
    pub(crate) async fn storage_page_counts(&self) -> Result<(u64, u64, u64)> {
        self.db.storage_page_counts().await
    }

    pub(crate) fn configuration_runtime(
        &self,
    ) -> &Arc<tracedecay_configuration::ProjectConfigurationRuntime> {
        &self.configuration_runtime
    }

    pub(crate) fn project_store_runtime(&self) -> &DaemonSessionRuntimeRegistryV1 {
        self.store_runtime_registry.as_ref()
    }

    pub(crate) fn profile_database(&self) -> &tracedecay_global_db::RegisteredGlobalDbLeaseV1 {
        &self.profile_database
    }

    #[doc(hidden)]
    #[cfg(any(test, feature = "test-transport"))]
    pub fn test_runtime_for_test(
        &self,
    ) -> Option<Arc<crate::host_admission::HostAdmissionTestRuntimeV1>> {
        self.test_runtime_guard.clone()
    }

    pub(crate) fn hook_store_layout(&self) -> &StoreLayout {
        &self.store_layout
    }

    pub(crate) fn source_read_context(&self) -> Option<SourceReadContext> {
        Some(SourceReadContext::new(
            self.project_root.clone(),
            self.db.clone(),
            self.read_only,
            self.store_layout.identity.project_id.clone()?,
        ))
    }

    pub(crate) fn context_scout_owner(
        &self,
    ) -> Option<&Arc<tracedecay_agent_hosts::agents::context_scout_owner::ProjectContextScoutOwnerV1>>
    {
        self.context_scout_owner.as_ref()
    }

    /// Publishes one hook-admissible Context Scout claim authority for an
    /// enqueued producer generation. The mount re-validates the durable
    /// address registry and the current Plan 20 configuration before the
    /// authority becomes claimable; a stale pin or a foreign address never
    /// mounts.
    #[allow(clippy::too_many_arguments)]
    #[hotpath::skip]
    pub(crate) async fn mount_current_context_scout_claim_authority(
        &self,
        registry: Arc<tracedecay_agent_hosts::agents::context_scout_ports::ProjectContextScoutAddressRegistryV1>,
        hook: &tracedecay_agent_hosts::agents::context_scout_ports::AdmittedContextScoutHookV1,
        pin: tracedecay_agent_hosts::agents::context_scout_ports::ContextScoutAuthorityPinV1,
        context: tracedecay_contracts::RequestContext,
        lifecycle: tracedecay_agent_hosts::agents::context_scout_ports::ContextScoutLifecycleAddressV1,
        address: ContextScoutAddressV1,
        input_watermark: [u8; 32],
        observed_at: tracedecay_domain::UtcMicros,
    ) -> bool {
        if input_watermark == [0; 32]
            || !self.context_scout_configuration_is_current(&pin).await
            || registry
                .resolve_current_exact(hook, &pin, &lifecycle, &context, observed_at)
                .await
                != tracedecay_agent_hosts::agents::context_scout_ports::ContextScoutAddressResolveOutcomeV1::Resolved(
                    address,
                )
        {
            return false;
        }
        let mounted = MountedContextScoutClaimAuthorityV1 {
            registry,
            pin,
            context,
            lifecycle,
            address,
            input_watermark,
        };
        let mut authorities = self.context_scout_claim_authorities.write().await;
        if let Some(existing) = authorities
            .iter_mut()
            .find(|existing| existing.lifecycle == mounted.lifecycle)
        {
            *existing = mounted;
            return true;
        }
        if authorities.len() == MAX_MOUNTED_CONTEXT_SCOUT_CLAIM_AUTHORITIES {
            authorities.remove(0);
        }
        authorities.push(mounted);
        true
    }

    /// Resolves the claim authority mounted for one exact lifecycle, or
    /// `None` when nothing was mounted, the Plan 20 configuration moved past
    /// the mounted pin, or the durable address registry no longer resolves
    /// the mounted address for this hook.
    #[hotpath::skip]
    pub(crate) async fn resolve_current_context_scout_claim_authority(
        &self,
        hook: &tracedecay_agent_hosts::agents::context_scout_ports::AdmittedContextScoutHookV1,
        lifecycle: &tracedecay_agent_hosts::agents::context_scout_ports::ContextScoutLifecycleAddressV1,
        observed_at: tracedecay_domain::UtcMicros,
    ) -> Option<(ContextScoutAddressV1, [u8; 32])> {
        let mounted = self
            .context_scout_claim_authorities
            .read()
            .await
            .iter()
            .find(|mounted| mounted.lifecycle == *lifecycle)
            .cloned()?;
        if !self
            .context_scout_configuration_is_current(&mounted.pin)
            .await
        {
            return None;
        }
        let resolved = mounted
            .registry
            .resolve_current_exact(hook, &mounted.pin, lifecycle, &mounted.context, observed_at)
            .await;
        let resolved = (resolved
            == tracedecay_agent_hosts::agents::context_scout_ports::ContextScoutAddressResolveOutcomeV1::Resolved(
                mounted.address,
            ))
        .then_some((mounted.address, mounted.input_watermark));
        // Re-check currentness after the registry read: a configuration
        // revision that lands mid-resolve must not hand out a stale claim.
        if resolved.is_some()
            && self
                .context_scout_configuration_is_current(&mounted.pin)
                .await
        {
            resolved
        } else {
            None
        }
    }

    #[hotpath::skip]
    async fn context_scout_configuration_is_current(
        &self,
        pin: &tracedecay_agent_hosts::agents::context_scout_ports::ContextScoutAuthorityPinV1,
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
