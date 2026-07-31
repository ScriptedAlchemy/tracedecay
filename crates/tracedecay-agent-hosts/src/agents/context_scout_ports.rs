//! Daemon-owned canonical ports for Context Scout orchestration.
//!
//! Opaque fixed-size values in [`ContextScoutAddressV1`] are locators only.
//! Exact identity remains the typed lifecycle tuple retained in the durable
//! registry and is always pinned to authenticated scope and configuration.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tracedecay_application::feedback::{
    FeedbackCompletedPublicationReadPort, FeedbackCompletedPublicationV1,
};
use tracedecay_application::{RequestAdmission, RequestContext, ResolvedScope};
use tracedecay_domain::configuration::{
    CONTEXT_SCOUT_SETTINGS_SETTING_KEY, ConfigurationRevisionId, ConfigurationValueV1,
    ContextScoutConfigurationModeV1, ContextScoutConfigurationStateV1,
    ContextScoutConfiguredModelPathV1, SettingKey,
};
use tracedecay_domain::feedback::{
    FeedbackContentIdentityV1, FeedbackFindingLifecycleV1, FeedbackScopeV1,
    ProviderEvaluationStateV1,
};
use tracedecay_domain::{
    AgentInstanceId, ManifestDigest, MessageId, ProjectId, ProviderId, SessionId, ThreadId, TurnId,
    UserProfileId, UtcMicros, WorktreeId, canonical_sha256,
};
use tracedecay_hooks::{HookEventEnvelopeV2, HookScopeBindingV1};

use super::context_scout_v2::{
    ContextScoutAddressV1, ContextScoutCandidateV1, ContextScoutCategoryV1, ContextScoutControlV1,
    ContextScoutDeliverySelectionInputV1, ContextScoutEvidenceBindingV1,
    ContextScoutEvidenceGenerationV1, ContextScoutLimitsV1, ContextScoutModelBackendV1,
    ContextScoutRuntimeModeV1, ContextScoutSelectionInputV1, ContextScoutServiceStateV1,
    select_context_scout_delivery_window,
};
use crate::application::configuration::ConfigurationCurrentStateV1;
use crate::db::Database;
use crate::db::engine::params;

const ADDRESS_LEDGER_KEY_V1: &str = "agents.context-scout.addresses.v1";
const ADDRESS_LEDGER_SCHEMA_VERSION_V1: u16 = 1;
const MAX_ADDRESS_BINDINGS_V1: usize = 256;
const MAX_ADDRESS_LEDGER_BYTES_V1: usize = 512 * 1024;
const CANDIDATE_TTL_MICROS_V1: i64 = 5 * 60 * 1_000_000;

/// Exact session lifecycle identity admitted for one Scout destination.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextScoutLifecycleAddressV1 {
    pub profile_id: UserProfileId,
    pub provider_id: ProviderId,
    pub project_id: ProjectId,
    pub worktree_id: WorktreeId,
    pub session_id: SessionId,
    pub thread_id: ThreadId,
    pub turn_id: TurnId,
    pub agent_id: AgentInstanceId,
    pub logical_message_id: MessageId,
}

impl ContextScoutLifecycleAddressV1 {
    fn validate(&self) -> bool {
        self.profile_id.validate().is_ok()
            && self.provider_id.validate().is_ok()
            && self.project_id.validate().is_ok()
            && self.worktree_id.validate().is_ok()
            && self.session_id.validate().is_ok()
            && self.thread_id.validate().is_ok()
            && self.turn_id.validate().is_ok()
            && self.agent_id.validate().is_ok()
            && self.logical_message_id.validate().is_ok()
    }

    fn session_key(&self) -> (&ProjectId, &WorktreeId, &SessionId) {
        (&self.project_id, &self.worktree_id, &self.session_id)
    }
}

/// Hook envelope whose exact daemon-issued binding has already been checked.
#[derive(Clone, Debug)]
pub struct AdmittedContextScoutHookV1 {
    envelope: HookEventEnvelopeV2,
}

impl AdmittedContextScoutHookV1 {
    pub fn new(envelope: HookEventEnvelopeV2, binding: &HookScopeBindingV1) -> Option<Self> {
        envelope.validate(binding).ok()?;
        Some(Self { envelope })
    }

    pub fn envelope(&self) -> &HookEventEnvelopeV2 {
        &self.envelope
    }
}

/// Typed configuration pin consumed by the Scout runtime. The revision ID is
/// retained separately from the behavior digest used by durable envelopes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextScoutConfigurationPinV1 {
    revision_id: ConfigurationRevisionId,
    configuration_digest: ManifestDigest,
    control: ContextScoutControlV1,
}

impl ContextScoutConfigurationPinV1 {
    pub fn from_current(current: &ConfigurationCurrentStateV1) -> Option<Self> {
        current.revision_id.validate().ok()?;
        current.snapshot.validate().ok()?;
        let key = SettingKey::new(CONTEXT_SCOUT_SETTINGS_SETTING_KEY).ok()?;
        let ConfigurationValueV1::ContextScoutSettings(settings) =
            current.snapshot.effective_values.get(&key)?
        else {
            return None;
        };
        settings.validate().ok()?;
        let limits = ContextScoutLimitsV1 {
            max_candidates: usize::try_from(settings.limits.max_candidates).ok()?,
            max_evidence: usize::try_from(settings.limits.max_evidence).ok()?,
            max_text_bytes: usize::try_from(settings.limits.max_text_bytes).ok()?,
            max_model_input_tokens: usize::try_from(settings.limits.max_model_input_tokens).ok()?,
            max_model_output_tokens: usize::try_from(settings.limits.max_model_output_tokens)
                .ok()?,
        };
        let state = match settings.state {
            ContextScoutConfigurationStateV1::Active => ContextScoutServiceStateV1::Active,
            ContextScoutConfigurationStateV1::Paused => ContextScoutServiceStateV1::Paused,
            ContextScoutConfigurationStateV1::Disabled => ContextScoutServiceStateV1::Disabled,
        };
        let mode = match settings.mode {
            ContextScoutConfigurationModeV1::Deterministic => {
                ContextScoutRuntimeModeV1::Deterministic
            }
            ContextScoutConfigurationModeV1::ConfiguredModel => {
                ContextScoutRuntimeModeV1::ConfiguredModel
            }
        };
        let model_path = settings.model_path.map(|path| match path {
            ContextScoutConfiguredModelPathV1::CodexAppServer => {
                ContextScoutModelBackendV1::CodexAppServer
            }
        });
        let configuration_digest = current.snapshot.effective_behavior_digest.clone();
        let control = ContextScoutControlV1 {
            configuration_revision: digest_bytes(&configuration_digest)?,
            state,
            mode,
            model_path,
            limits,
        };
        Some(Self {
            revision_id: current.revision_id.clone(),
            configuration_digest,
            control,
        })
    }

    pub fn revision_id(&self) -> &ConfigurationRevisionId {
        &self.revision_id
    }

    pub fn configuration_digest(&self) -> &ManifestDigest {
        &self.configuration_digest
    }

    pub const fn control(&self) -> ContextScoutControlV1 {
        self.control
    }

    /// Revalidates this pin against the authoritative current configuration.
    /// A revision or effective-behavior change invalidates in-flight Scout
    /// work even when the host/session identity remains unchanged.
    pub fn matches_current(&self, current: &ConfigurationCurrentStateV1) -> bool {
        Self::from_current(current)
            .as_ref()
            .is_some_and(|current| current == self)
    }
}

/// Exact authenticated scope and configuration used for one registry access.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextScoutAuthorityPinV1 {
    feedback_scope: FeedbackScopeV1,
    scope_digest: ManifestDigest,
    configuration: ContextScoutConfigurationPinV1,
}

impl ContextScoutAuthorityPinV1 {
    pub fn new(
        context: &RequestContext,
        feedback_scope: FeedbackScopeV1,
        configuration: ContextScoutConfigurationPinV1,
        observed_at: UtcMicros,
    ) -> Option<Self> {
        context.validate().ok()?;
        feedback_scope.validate().ok()?;
        if context.admission_at(observed_at) != RequestAdmission::Admitted
            || context.scope().project_id != feedback_scope.project_id
            || context.scope().repository_id != feedback_scope.repository_id
            || context.scope().worktree_id != feedback_scope.worktree_id
            || context
                .scope()
                .reference
                .as_ref()
                .map(tracedecay_domain::RefId::as_str)
                != Some(feedback_scope.branch_ref.as_str())
        {
            return None;
        }
        Some(Self {
            feedback_scope,
            scope_digest: context.scope().scope_digest.clone(),
            configuration,
        })
    }

    fn matches_context(&self, context: &RequestContext, observed_at: UtcMicros) -> bool {
        context.admission_at(observed_at) == RequestAdmission::Admitted
            && context.scope().scope_digest == self.scope_digest
            && context.scope().project_id == self.feedback_scope.project_id
            && context.scope().repository_id == self.feedback_scope.repository_id
            && context.scope().worktree_id == self.feedback_scope.worktree_id
            && context
                .scope()
                .reference
                .as_ref()
                .map(tracedecay_domain::RefId::as_str)
                == Some(self.feedback_scope.branch_ref.as_str())
    }

    pub fn configuration(&self) -> &ContextScoutConfigurationPinV1 {
        &self.configuration
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredContextScoutAddressBindingV1 {
    lifecycle: ContextScoutLifecycleAddressV1,
    scope_digest: ManifestDigest,
    configuration_revision: ConfigurationRevisionId,
    configuration_digest: ManifestDigest,
    hook_project_locator: [u8; 16],
    hook_worktree_locator: [u8; 16],
    protected_session_locator: [u8; 32],
    address: ContextScoutAddressV1,
}

impl StoredContextScoutAddressBindingV1 {
    fn matches_session_key(&self, lifecycle: &ContextScoutLifecycleAddressV1) -> bool {
        self.lifecycle.session_key() == lifecycle.session_key()
    }

    fn matches_hook_and_pin(
        &self,
        hook: &AdmittedContextScoutHookV1,
        pin: &ContextScoutAuthorityPinV1,
    ) -> bool {
        self.hook_project_locator == hook.envelope.project_id
            && self.hook_worktree_locator == hook.envelope.worktree_id
            && self.protected_session_locator == hook.envelope.protected_session_id
            && self.lifecycle.project_id == pin.feedback_scope.project_id
            && self.lifecycle.worktree_id == pin.feedback_scope.worktree_id
            && self.scope_digest == pin.scope_digest
            && self.configuration_revision == pin.configuration.revision_id
            && self.configuration_digest == pin.configuration.configuration_digest
    }

    fn matches_hook_locator(&self, hook: &AdmittedContextScoutHookV1) -> bool {
        self.hook_project_locator == hook.envelope.project_id
            && self.hook_worktree_locator == hook.envelope.worktree_id
            && self.protected_session_locator == hook.envelope.protected_session_id
    }

    fn matches_exact_lifecycle(
        &self,
        hook: &AdmittedContextScoutHookV1,
        pin: &ContextScoutAuthorityPinV1,
        lifecycle: &ContextScoutLifecycleAddressV1,
    ) -> bool {
        self.lifecycle == *lifecycle && self.matches_hook_and_pin(hook, pin)
    }

    fn validate(&self, project_id: &ProjectId) -> bool {
        self.lifecycle.validate()
            && &self.lifecycle.project_id == project_id
            && self.scope_digest.validate().is_ok()
            && self.configuration_revision.validate().is_ok()
            && self.configuration_digest.validate().is_ok()
            && self.hook_project_locator != [0; 16]
            && self.hook_worktree_locator != [0; 16]
            && self.protected_session_locator != [0; 32]
            && valid_opaque_address(self.address)
            && self.address.project_id == self.hook_project_locator
            && self.address.protected_session_id == self.protected_session_locator
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredContextScoutAddressLedgerV1 {
    schema_version: u16,
    project_id: ProjectId,
    bindings: Vec<StoredContextScoutAddressBindingV1>,
}

impl StoredContextScoutAddressLedgerV1 {
    fn empty(project_id: ProjectId) -> Self {
        Self {
            schema_version: ADDRESS_LEDGER_SCHEMA_VERSION_V1,
            project_id,
            bindings: Vec::new(),
        }
    }

    fn validate(&self, project_id: &ProjectId) -> bool {
        if self.schema_version != ADDRESS_LEDGER_SCHEMA_VERSION_V1
            || &self.project_id != project_id
            || self.bindings.len() > MAX_ADDRESS_BINDINGS_V1
            || self
                .bindings
                .iter()
                .any(|binding| !binding.validate(project_id))
        {
            return false;
        }
        self.bindings.iter().enumerate().all(|(index, binding)| {
            self.bindings[index.saturating_add(1)..]
                .iter()
                .all(|other| {
                    other.address != binding.address && !same_binding_authority(other, binding)
                })
        })
    }
}

fn same_binding_authority(
    left: &StoredContextScoutAddressBindingV1,
    right: &StoredContextScoutAddressBindingV1,
) -> bool {
    left.lifecycle == right.lifecycle
        && left.scope_digest == right.scope_digest
        && left.configuration_revision == right.configuration_revision
        && left.configuration_digest == right.configuration_digest
        && left.hook_project_locator == right.hook_project_locator
        && left.hook_worktree_locator == right.hook_worktree_locator
        && left.protected_session_locator == right.protected_session_locator
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContextScoutAddressBindOutcomeV1 {
    Bound(ContextScoutAddressV1),
    Existing(ContextScoutAddressV1),
    Conflict,
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContextScoutAddressResolveOutcomeV1 {
    Resolved(ContextScoutAddressV1),
    Missing,
    Ambiguous,
    Unavailable,
}

/// Restart-safe exact-address authority over the existing project database.
#[derive(Clone)]
pub struct ProjectContextScoutAddressRegistryV1 {
    database: Database,
    project_id: ProjectId,
}

impl ProjectContextScoutAddressRegistryV1 {
    pub fn new(database: Database, project_id: ProjectId) -> Option<Arc<Self>> {
        project_id.validate().ok()?;
        Some(Arc::new(Self {
            database,
            project_id,
        }))
    }

    async fn bind(
        &self,
        hook: &AdmittedContextScoutHookV1,
        pin: &ContextScoutAuthorityPinV1,
        lifecycle: ContextScoutLifecycleAddressV1,
    ) -> ContextScoutAddressBindOutcomeV1 {
        if !lifecycle.validate()
            || lifecycle.project_id != self.project_id
            || lifecycle.project_id != pin.feedback_scope.project_id
            || lifecycle.worktree_id != pin.feedback_scope.worktree_id
        {
            return ContextScoutAddressBindOutcomeV1::Unavailable;
        }
        let transaction = match self
            .database
            .begin_write_transaction("bind Context Scout exact address")
            .await
        {
            Ok(transaction) => transaction,
            Err(_) => return ContextScoutAddressBindOutcomeV1::Unavailable,
        };
        let mut ledger = match load_address_ledger(&transaction, &self.project_id).await {
            Some(ledger) => ledger,
            None => return ContextScoutAddressBindOutcomeV1::Unavailable,
        };
        let related = ledger
            .bindings
            .iter()
            .filter(|binding| {
                binding.matches_session_key(&lifecycle) || binding.matches_hook_locator(hook)
            })
            .collect::<Vec<_>>();
        if related.iter().any(|binding| {
            binding.matches_session_key(&lifecycle) != binding.matches_hook_locator(hook)
        }) {
            let _ = transaction.rollback().await;
            return ContextScoutAddressBindOutcomeV1::Conflict;
        }
        let exact = ledger
            .bindings
            .iter()
            .enumerate()
            .filter(|(_, binding)| binding.lifecycle == lifecycle)
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        if exact.len() > 1 {
            let _ = transaction.rollback().await;
            return ContextScoutAddressBindOutcomeV1::Unavailable;
        }
        if let Some(index) = exact.first().copied() {
            let existing = &ledger.bindings[index];
            if existing.matches_hook_and_pin(hook, pin) {
                let address = existing.address;
                let _ = transaction.rollback().await;
                return ContextScoutAddressBindOutcomeV1::Existing(address);
            }
            if !existing.matches_hook_locator(hook)
                || existing.scope_digest != pin.scope_digest
                || existing.lifecycle.project_id != pin.feedback_scope.project_id
                || existing.lifecycle.worktree_id != pin.feedback_scope.worktree_id
            {
                let _ = transaction.rollback().await;
                return ContextScoutAddressBindOutcomeV1::Conflict;
            }
            let Some(address) = random_address(&hook.envelope) else {
                let _ = transaction.rollback().await;
                return ContextScoutAddressBindOutcomeV1::Unavailable;
            };
            let replacement = &mut ledger.bindings[index];
            replacement.configuration_revision = pin.configuration.revision_id.clone();
            replacement.configuration_digest = pin.configuration.configuration_digest.clone();
            replacement.address = address;
            if !ledger.validate(&self.project_id) {
                let _ = transaction.rollback().await;
                return ContextScoutAddressBindOutcomeV1::Unavailable;
            }
            let Ok(encoded) = serde_json::to_string(&ledger) else {
                let _ = transaction.rollback().await;
                return ContextScoutAddressBindOutcomeV1::Unavailable;
            };
            if encoded.len() > MAX_ADDRESS_LEDGER_BYTES_V1
                || self
                    .database
                    .set_metadata_unguarded(&transaction, ADDRESS_LEDGER_KEY_V1, &encoded)
                    .await
                    .is_err()
                || transaction.commit().await.is_err()
            {
                return ContextScoutAddressBindOutcomeV1::Unavailable;
            }
            return ContextScoutAddressBindOutcomeV1::Bound(address);
        }
        if ledger.bindings.len() == MAX_ADDRESS_BINDINGS_V1 {
            let _ = transaction.rollback().await;
            return ContextScoutAddressBindOutcomeV1::Unavailable;
        }
        let Some(address) = random_address(&hook.envelope) else {
            let _ = transaction.rollback().await;
            return ContextScoutAddressBindOutcomeV1::Unavailable;
        };
        ledger.bindings.push(StoredContextScoutAddressBindingV1 {
            lifecycle,
            scope_digest: pin.scope_digest.clone(),
            configuration_revision: pin.configuration.revision_id.clone(),
            configuration_digest: pin.configuration.configuration_digest.clone(),
            hook_project_locator: hook.envelope.project_id,
            hook_worktree_locator: hook.envelope.worktree_id,
            protected_session_locator: hook.envelope.protected_session_id,
            address,
        });
        if !ledger.validate(&self.project_id) {
            let _ = transaction.rollback().await;
            return ContextScoutAddressBindOutcomeV1::Unavailable;
        }
        let Ok(encoded) = serde_json::to_string(&ledger) else {
            let _ = transaction.rollback().await;
            return ContextScoutAddressBindOutcomeV1::Unavailable;
        };
        if encoded.len() > MAX_ADDRESS_LEDGER_BYTES_V1
            || self
                .database
                .set_metadata_unguarded(&transaction, ADDRESS_LEDGER_KEY_V1, &encoded)
                .await
                .is_err()
            || transaction.commit().await.is_err()
        {
            return ContextScoutAddressBindOutcomeV1::Unavailable;
        }
        ContextScoutAddressBindOutcomeV1::Bound(address)
    }

    async fn resolve(
        &self,
        hook: &AdmittedContextScoutHookV1,
        pin: &ContextScoutAuthorityPinV1,
    ) -> ContextScoutAddressResolveOutcomeV1 {
        let Some(encoded) = self
            .database
            .get_metadata(ADDRESS_LEDGER_KEY_V1)
            .await
            .ok()
            .flatten()
        else {
            return ContextScoutAddressResolveOutcomeV1::Missing;
        };
        if encoded.len() > MAX_ADDRESS_LEDGER_BYTES_V1 {
            return ContextScoutAddressResolveOutcomeV1::Unavailable;
        }
        let Ok(ledger) = serde_json::from_str::<StoredContextScoutAddressLedgerV1>(&encoded) else {
            return ContextScoutAddressResolveOutcomeV1::Unavailable;
        };
        if !ledger.validate(&self.project_id) {
            return ContextScoutAddressResolveOutcomeV1::Unavailable;
        }
        let mut matches = ledger
            .bindings
            .iter()
            .filter(|binding| binding.matches_hook_and_pin(hook, pin))
            .map(|binding| binding.address);
        match (matches.next(), matches.next()) {
            (Some(address), None) => ContextScoutAddressResolveOutcomeV1::Resolved(address),
            (None, _) => ContextScoutAddressResolveOutcomeV1::Missing,
            (Some(_), Some(_)) => ContextScoutAddressResolveOutcomeV1::Ambiguous,
        }
    }

    /// Resolves only the full typed lifecycle tuple after rechecking the
    /// current request admission. A protected-session match is insufficient:
    /// one session may contain several threads, turns, agents, or messages.
    pub async fn resolve_current_exact(
        &self,
        hook: &AdmittedContextScoutHookV1,
        pin: &ContextScoutAuthorityPinV1,
        lifecycle: &ContextScoutLifecycleAddressV1,
        context: &RequestContext,
        observed_at: UtcMicros,
    ) -> ContextScoutAddressResolveOutcomeV1 {
        if !pin.matches_context(context, observed_at)
            || !lifecycle.validate()
            || lifecycle.project_id != self.project_id
            || lifecycle.project_id != pin.feedback_scope.project_id
            || lifecycle.worktree_id != pin.feedback_scope.worktree_id
        {
            return ContextScoutAddressResolveOutcomeV1::Unavailable;
        }
        let ledger = match self.read_ledger().await {
            Ok(Some(ledger)) => ledger,
            Ok(None) => return ContextScoutAddressResolveOutcomeV1::Missing,
            Err(()) => return ContextScoutAddressResolveOutcomeV1::Unavailable,
        };
        let mut matches = ledger
            .bindings
            .iter()
            .filter(|binding| binding.matches_exact_lifecycle(hook, pin, lifecycle))
            .map(|binding| binding.address);
        match (matches.next(), matches.next()) {
            (Some(address), None) => ContextScoutAddressResolveOutcomeV1::Resolved(address),
            (None, _) => ContextScoutAddressResolveOutcomeV1::Missing,
            (Some(_), Some(_)) => ContextScoutAddressResolveOutcomeV1::Ambiguous,
        }
    }

    /// Reauthorizes one opaque address against the current daemon-routed
    /// project/worktree scope and configuration. Possession of the address is
    /// never sufficient, and ambiguity fails closed.
    pub async fn authorize_current_exact_address(
        &self,
        address: ContextScoutAddressV1,
        configuration: &ContextScoutConfigurationPinV1,
        scope: &ResolvedScope,
    ) -> bool {
        if scope.project_id != self.project_id {
            return false;
        }
        let Ok(Some(ledger)) = self.read_ledger().await else {
            return false;
        };
        let mut matches = ledger.bindings.iter().filter(|binding| {
            binding.address == address
                && binding.lifecycle.project_id == scope.project_id
                && binding.lifecycle.worktree_id == scope.worktree_id
                && binding.scope_digest == scope.scope_digest
                && binding.configuration_revision == configuration.revision_id
                && binding.configuration_digest == configuration.configuration_digest
        });
        matches.next().is_some() && matches.next().is_none()
    }

    async fn read_ledger(&self) -> Result<Option<StoredContextScoutAddressLedgerV1>, ()> {
        let encoded = self
            .database
            .get_metadata(ADDRESS_LEDGER_KEY_V1)
            .await
            .map_err(|_| ())?;
        let Some(encoded) = encoded else {
            return Ok(None);
        };
        if encoded.len() > MAX_ADDRESS_LEDGER_BYTES_V1 {
            return Err(());
        }
        let ledger =
            serde_json::from_str::<StoredContextScoutAddressLedgerV1>(&encoded).map_err(|_| ())?;
        if !ledger.validate(&self.project_id) {
            return Err(());
        }
        Ok(Some(ledger))
    }
}

/// One complete daemon-owned input packet for asynchronous Scout execution.
#[derive(Clone, Debug)]
pub struct ContextScoutCanonicalInputV1 {
    pub address: ContextScoutAddressV1,
    pub control: ContextScoutControlV1,
    pub latest_publication: Option<FeedbackCompletedPublicationV1>,
    pub candidates: Vec<ContextScoutCandidateV1>,
}

impl ContextScoutCanonicalInputV1 {
    /// Builds the sole low-level selection packet from an authorized committed
    /// publication. The input watermark binds the publication's authoritative
    /// runtime watermark to the exact configuration revision.
    pub fn selection_input(
        &self,
        hook: &AdmittedContextScoutHookV1,
        observed_at: UtcMicros,
        delivery: ContextScoutDeliverySelectionInputV1,
    ) -> Option<ContextScoutSelectionInputV1> {
        let publication = self.latest_publication.as_ref()?;
        publication.validate().ok()?;
        if self.candidates.is_empty() {
            return None;
        }
        let input_watermark = canonical_sha256(&(
            "tracedecay.context-scout.selection-watermark.v1",
            &publication.runtime.authoritative.runtime_watermark,
            self.control.configuration_revision,
        ))
        .ok()
        .and_then(|digest| digest_bytes(&digest))?;
        Some(ContextScoutSelectionInputV1 {
            address: self.address,
            input_watermark,
            configuration_revision: self.control.configuration_revision,
            envelope_id: hook.envelope().event_id,
            now: observed_at,
            delivery_window: select_context_scout_delivery_window(&delivery),
            delivered_dedupe_keys: delivery.delivered_dedupe_keys,
            candidates: self.candidates.clone(),
        })
    }
}

pub struct ContextScoutCanonicalInputAssemblerV1<'a, P> {
    registry: &'a ProjectContextScoutAddressRegistryV1,
    publications: &'a P,
}

impl<'a, P> ContextScoutCanonicalInputAssemblerV1<'a, P>
where
    P: FeedbackCompletedPublicationReadPort,
{
    pub const fn new(
        registry: &'a ProjectContextScoutAddressRegistryV1,
        publications: &'a P,
    ) -> Self {
        Self {
            registry,
            publications,
        }
    }

    pub async fn assemble_registered(
        &self,
        hook: &AdmittedContextScoutHookV1,
        pin: &ContextScoutAuthorityPinV1,
        context: &RequestContext,
        observed_at: UtcMicros,
    ) -> Option<ContextScoutCanonicalInputV1> {
        if !pin.matches_context(context, observed_at) {
            return None;
        }
        let ContextScoutAddressResolveOutcomeV1::Resolved(address) =
            self.registry.resolve(hook, pin).await
        else {
            return None;
        };
        self.assemble(address, pin, context, observed_at).await
    }

    pub async fn assemble_registered_exact(
        &self,
        hook: &AdmittedContextScoutHookV1,
        pin: &ContextScoutAuthorityPinV1,
        lifecycle: &ContextScoutLifecycleAddressV1,
        context: &RequestContext,
        observed_at: UtcMicros,
    ) -> Option<ContextScoutCanonicalInputV1> {
        let ContextScoutAddressResolveOutcomeV1::Resolved(address) = self
            .registry
            .resolve_current_exact(hook, pin, lifecycle, context, observed_at)
            .await
        else {
            return None;
        };
        self.assemble(address, pin, context, observed_at).await
    }

    pub async fn bind_and_assemble(
        &self,
        hook: &AdmittedContextScoutHookV1,
        pin: &ContextScoutAuthorityPinV1,
        lifecycle: ContextScoutLifecycleAddressV1,
        context: &RequestContext,
        observed_at: UtcMicros,
    ) -> Option<ContextScoutCanonicalInputV1> {
        if !pin.matches_context(context, observed_at) {
            return None;
        }
        let address = match self.registry.bind(hook, pin, lifecycle).await {
            ContextScoutAddressBindOutcomeV1::Bound(address)
            | ContextScoutAddressBindOutcomeV1::Existing(address) => address,
            ContextScoutAddressBindOutcomeV1::Conflict
            | ContextScoutAddressBindOutcomeV1::Unavailable => return None,
        };
        self.assemble(address, pin, context, observed_at).await
    }

    async fn assemble(
        &self,
        address: ContextScoutAddressV1,
        pin: &ContextScoutAuthorityPinV1,
        context: &RequestContext,
        observed_at: UtcMicros,
    ) -> Option<ContextScoutCanonicalInputV1> {
        let latest_publication = self
            .publications
            .latest_committed(context, observed_at)
            .await
            .filter(|publication| publication_matches_pin(publication, pin));
        let candidates = latest_publication
            .as_ref()
            .map(|publication| {
                context_scout_candidates_from_publication(
                    publication,
                    pin.configuration.control(),
                    observed_at,
                )
            })
            .unwrap_or_default();
        Some(ContextScoutCanonicalInputV1 {
            address,
            control: pin.configuration.control(),
            latest_publication,
            candidates,
        })
    }
}

fn publication_matches_pin(
    publication: &FeedbackCompletedPublicationV1,
    pin: &ContextScoutAuthorityPinV1,
) -> bool {
    publication.validate().is_ok()
        && publication.result.scope == pin.feedback_scope
        && publication.authorized_scope.project_id == pin.feedback_scope.project_id
        && publication.authorized_scope.repository_id == pin.feedback_scope.repository_id
        && publication.authorized_scope.worktree_id == pin.feedback_scope.worktree_id
        && publication
            .authorized_scope
            .reference
            .as_ref()
            .map(tracedecay_domain::RefId::as_str)
            == Some(pin.feedback_scope.branch_ref.as_str())
        && publication.result.configuration_digest == pin.configuration.configuration_digest
}

/// Converts only anchored, bounded, durable findings from the authorized
/// committed publication. Fixed-size values are content/evidence locators;
/// canonical identity remains in `latest_publication`.
pub fn context_scout_candidates_from_publication(
    publication: &FeedbackCompletedPublicationV1,
    control: ContextScoutControlV1,
    observed_at: UtcMicros,
) -> Vec<ContextScoutCandidateV1> {
    if publication.validate().is_err() {
        return Vec::new();
    }
    let FeedbackContentIdentityV1::SavedContent {
        generation_digest, ..
    } = &publication.input.request.content
    else {
        return Vec::new();
    };
    let Some(content_identity) = digest_bytes(generation_digest) else {
        return Vec::new();
    };
    publication
        .result
        .findings
        .iter()
        .filter_map(|finding| {
            if finding.lifecycle != FeedbackFindingLifecycleV1::Active
                || finding.provider_state != ProviderEvaluationStateV1::SupportedCompletedComplete
            {
                return None;
            }
            let anchor = finding.retrieval_anchor_id.as_ref()?;
            finding.safe_bounded_preview.as_ref()?;
            let text = format!(
                "Inspect anchored finding {} before changing code.",
                finding.finding_id.as_str()
            );
            if text.len() > control.limits.max_text_bytes {
                return None;
            }
            let dedupe = canonical_sha256(&(
                "tracedecay.context-scout.publication-candidate.v1",
                &publication.result.result_id,
                &finding.finding_id,
                anchor,
            ))
            .ok()
            .and_then(|digest| digest_bytes(&digest))?;
            let anchor_digest =
                canonical_sha256(&("tracedecay.context-scout.anchor-locator.v1", anchor))
                    .ok()
                    .and_then(|digest| digest_bytes(&digest))?;
            let mut anchor_id = [0_u8; 16];
            anchor_id.copy_from_slice(&anchor_digest[..16]);
            Some(ContextScoutCandidateV1 {
                dedupe_key: dedupe,
                category: ContextScoutCategoryV1::Retrieval,
                relevance_score: u16::MAX,
                suggestion_text: text,
                evidence: vec![ContextScoutEvidenceBindingV1 {
                    anchor_id,
                    content_identity,
                    generation: ContextScoutEvidenceGenerationV1::SavedContent,
                }],
                expires_at: UtcMicros(observed_at.0.saturating_add(CANDIDATE_TTL_MICROS_V1)),
            })
        })
        .take(control.limits.max_candidates)
        .collect()
}

async fn load_address_ledger(
    transaction: &crate::db::DatabaseWriteTransaction<'_>,
    project_id: &ProjectId,
) -> Option<StoredContextScoutAddressLedgerV1> {
    let mut rows = transaction
        .query_engine(
            "SELECT value FROM metadata WHERE key = ?1",
            params![ADDRESS_LEDGER_KEY_V1],
        )
        .await
        .ok()?;
    let encoded = rows
        .next()
        .await
        .ok()?
        .map(|row| row.get::<String>(0))
        .transpose()
        .ok()?;
    drop(rows);
    let ledger = match encoded {
        Some(encoded) if encoded.len() <= MAX_ADDRESS_LEDGER_BYTES_V1 => {
            serde_json::from_str(&encoded).ok()?
        }
        Some(_) => return None,
        None => StoredContextScoutAddressLedgerV1::empty(project_id.clone()),
    };
    ledger.validate(project_id).then_some(ledger)
}

fn random_address(envelope: &HookEventEnvelopeV2) -> Option<ContextScoutAddressV1> {
    let mut random = [0_u8; 96];
    getrandom::getrandom(&mut random).ok()?;
    let part = |index: usize| {
        let mut value = [0_u8; 16];
        value.copy_from_slice(&random[index * 16..(index + 1) * 16]);
        value
    };
    let address = ContextScoutAddressV1 {
        profile_id: part(0),
        provider_id: part(1),
        protected_session_id: envelope.protected_session_id,
        thread_id: part(2),
        turn_id: part(3),
        agent_id: part(4),
        logical_message_id: part(5),
        project_id: envelope.project_id,
    };
    valid_opaque_address(address).then_some(address)
}

fn valid_opaque_address(address: ContextScoutAddressV1) -> bool {
    address.profile_id != [0; 16]
        && address.provider_id != [0; 16]
        && address.protected_session_id != [0; 32]
        && address.thread_id != [0; 16]
        && address.turn_id != [0; 16]
        && address.agent_id != [0; 16]
        && address.logical_message_id != [0; 16]
        && address.project_id != [0; 16]
}

fn digest_bytes(digest: &ManifestDigest) -> Option<[u8; 32]> {
    let encoded = digest.as_str().strip_prefix("sha256:")?;
    let mut bytes = [0_u8; 32];
    hex::decode_to_slice(encoded, &mut bytes).ok()?;
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use tempfile::TempDir;
    use tracedecay_domain::configuration::{
        CandidateDispositionV1, ConfigurationCandidateV1, ConfigurationLayerIdV1,
        ContextScoutConfigurationLimitsV1, ContextScoutSettingsV1,
    };
    use tracedecay_hooks::{
        HookCapabilityV1, HookEventFamily, HookHostV1, NativeEnvelopeMaterialV1,
        decode_bound_native_hook_event, stock_event_support,
    };

    use super::*;
    use crate::db::{DatabaseAuthority, TestDatabaseRuntimeMode};

    async fn database() -> (TempDir, Database) {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("graph.db");
        let authority = DatabaseAuthority::acquire_test(&path, "Scout address registry").unwrap();
        let database =
            Database::publish_test_runtime(&path, &authority, TestDatabaseRuntimeMode::Initialize)
                .await
                .unwrap()
                .0;
        (temporary, database)
    }

    fn id<T: TryFrom<String>>(value: &str) -> T
    where
        T::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).unwrap()
    }

    fn configuration(
        revision: &str,
        settings: ContextScoutSettingsV1,
    ) -> ConfigurationCurrentStateV1 {
        let key = SettingKey::new(CONTEXT_SCOUT_SETTINGS_SETTING_KEY).unwrap();
        let revision_id = ConfigurationRevisionId::new(revision).unwrap();
        let snapshot = tracedecay_domain::configuration::ConfigurationSnapshotV1::new(
            BTreeMap::from([(
                key.clone(),
                ConfigurationValueV1::ContextScoutSettings(settings),
            )]),
            BTreeMap::from([(
                key,
                vec![ConfigurationCandidateV1 {
                    layer: ConfigurationLayerIdV1::Project {
                        project_id: id("project.scout.fixture"),
                    },
                    revision_id: revision_id.clone(),
                    disposition: CandidateDispositionV1::Winning,
                    safe_reason: None,
                }],
            )]),
        )
        .unwrap();
        ConfigurationCurrentStateV1 {
            revision_id,
            snapshot,
        }
    }

    fn lifecycle() -> ContextScoutLifecycleAddressV1 {
        ContextScoutLifecycleAddressV1 {
            profile_id: id("profile.scout.fixture"),
            provider_id: id("provider.claude"),
            project_id: id("project.scout.fixture"),
            worktree_id: id("worktree.scout.fixture"),
            session_id: id("session.scout.fixture"),
            thread_id: id("thread.scout.fixture"),
            turn_id: id("turn.scout.fixture"),
            agent_id: id("agent.scout.fixture"),
            logical_message_id: id("message.scout.fixture"),
        }
    }

    fn pin(revision: &str) -> ContextScoutAuthorityPinV1 {
        let current = configuration(revision, ContextScoutSettingsV1::disabled());
        ContextScoutAuthorityPinV1 {
            feedback_scope: FeedbackScopeV1 {
                project_id: id("project.scout.fixture"),
                repository_id: id("repository.scout.fixture"),
                worktree_id: id("worktree.scout.fixture"),
                branch_ref: "refs/heads/main".to_owned(),
                head_commit_id: id("commit.scout.fixture"),
            },
            scope_digest: ManifestDigest::new(format!("sha256:{}", "1".repeat(64))).unwrap(),
            configuration: ContextScoutConfigurationPinV1::from_current(&current).unwrap(),
        }
    }

    fn binding() -> HookScopeBindingV1 {
        HookScopeBindingV1 {
            host: HookHostV1::ClaudeCode,
            project_id: [1; 16],
            repository_id: [2; 16],
            worktree_id: [3; 16],
            worktree_epoch: 1,
            binding_token: [4; 32],
            capabilities: [
                HookEventFamily::SessionBoundary,
                HookEventFamily::PromptBoundary,
                HookEventFamily::ToolLifecycle,
                HookEventFamily::SavedEdit,
                HookEventFamily::TestLifecycle,
            ]
            .into_iter()
            .map(|family| HookCapabilityV1 {
                family,
                support: stock_event_support(HookHostV1::ClaudeCode, family),
            })
            .collect(),
        }
    }

    fn admitted_hook() -> AdmittedContextScoutHookV1 {
        let binding = binding();
        let envelope = decode_bound_native_hook_event(
            HookHostV1::ClaudeCode,
            include_bytes!(
                "../../../../tests/fixtures/packaged_host_events/claude/post_tool_use_write.json"
            ),
            &binding,
            NativeEnvelopeMaterialV1 {
                event_id: [5; 16],
                protected_session_id: [6; 32],
                observed_at: UtcMicros(10),
                tool_id: Some([7; 16]),
                effect_receipt_id: Some([8; 16]),
                file_id: Some([9; 16]),
                changed_range_count: 1,
            },
        )
        .unwrap();
        AdmittedContextScoutHookV1::new(envelope, &binding).unwrap()
    }

    #[test]
    fn configuration_pin_preserves_disabled_and_explicit_model_states() {
        let disabled_current = configuration(
            "revision.scout.disabled",
            ContextScoutSettingsV1::disabled(),
        );
        let disabled = ContextScoutConfigurationPinV1::from_current(&disabled_current).unwrap();
        assert_eq!(
            disabled.control().state,
            ContextScoutServiceStateV1::Disabled
        );
        assert_eq!(disabled.control().model_path, None);
        assert!(disabled.matches_current(&disabled_current));

        let configured = ContextScoutSettingsV1 {
            schema_version: ContextScoutSettingsV1::SCHEMA_VERSION,
            state: ContextScoutConfigurationStateV1::Active,
            mode: ContextScoutConfigurationModeV1::ConfiguredModel,
            limits: ContextScoutConfigurationLimitsV1::bounded_defaults(),
            model_path: Some(ContextScoutConfiguredModelPathV1::CodexAppServer),
        };
        let configured = ContextScoutConfigurationPinV1::from_current(&configuration(
            "revision.scout.model",
            configured,
        ))
        .unwrap();
        assert_eq!(
            configured.control().model_path,
            Some(ContextScoutModelBackendV1::CodexAppServer)
        );
    }

    #[test]
    fn configuration_pin_rejects_inflight_authority_after_current_revision_changes() {
        let admitted = configuration(
            "revision.scout.admitted",
            ContextScoutSettingsV1::disabled(),
        );
        let pin = ContextScoutConfigurationPinV1::from_current(&admitted).unwrap();
        assert!(pin.matches_current(&admitted));
        assert!(!pin.matches_current(&configuration(
            "revision.scout.changed",
            ContextScoutSettingsV1::disabled(),
        )));
    }

    #[tokio::test]
    async fn native_fixture_binding_survives_restart_and_rotates_exact_revision_authority() {
        let (_temporary, database) = database().await;
        let registry = ProjectContextScoutAddressRegistryV1::new(
            database.clone(),
            id("project.scout.fixture"),
        )
        .unwrap();
        let hook = admitted_hook();
        let original_pin = pin("revision.scout.one");
        let original = match registry.bind(&hook, &original_pin, lifecycle()).await {
            ContextScoutAddressBindOutcomeV1::Bound(address) => address,
            other => panic!("expected bound address, got {other:?}"),
        };
        assert_eq!(original.project_id, hook.envelope().project_id);
        assert_eq!(
            original.protected_session_id,
            hook.envelope().protected_session_id
        );

        drop(registry);
        let restarted = ProjectContextScoutAddressRegistryV1::new(
            database.clone(),
            id("project.scout.fixture"),
        )
        .unwrap();
        assert_eq!(
            restarted.resolve(&hook, &original_pin).await,
            ContextScoutAddressResolveOutcomeV1::Resolved(original)
        );

        let next_revision = pin("revision.scout.two");
        let rotated = match restarted.bind(&hook, &next_revision, lifecycle()).await {
            ContextScoutAddressBindOutcomeV1::Bound(address) => address,
            other => panic!("expected rotated address, got {other:?}"),
        };
        assert_ne!(rotated, original);
        assert_eq!(
            restarted.resolve(&hook, &next_revision).await,
            ContextScoutAddressResolveOutcomeV1::Resolved(rotated)
        );
        assert_eq!(
            restarted.resolve(&hook, &original_pin).await,
            ContextScoutAddressResolveOutcomeV1::Missing
        );

        let mut mismatched = lifecycle();
        mismatched.logical_message_id = id("message.scout.other");
        let next_message = match restarted.bind(&hook, &next_revision, mismatched).await {
            ContextScoutAddressBindOutcomeV1::Bound(address) => address,
            other => panic!("expected later message binding, got {other:?}"),
        };
        assert_ne!(next_message, rotated);
        assert_eq!(
            restarted.resolve(&hook, &next_revision).await,
            ContextScoutAddressResolveOutcomeV1::Ambiguous
        );
        let mut mismatched_session = lifecycle();
        mismatched_session.session_id = id("session.scout.other");
        assert_eq!(
            restarted
                .bind(&hook, &next_revision, mismatched_session)
                .await,
            ContextScoutAddressBindOutcomeV1::Conflict
        );

        let encoded = database
            .get_metadata(ADDRESS_LEDGER_KEY_V1)
            .await
            .unwrap()
            .unwrap();
        let mut ledger: StoredContextScoutAddressLedgerV1 = serde_json::from_str(&encoded).unwrap();
        let mut duplicate = ledger.bindings[0].clone();
        duplicate.address = random_address(hook.envelope()).unwrap();
        assert_ne!(duplicate.address, original);
        ledger.bindings.push(duplicate);
        assert!(!ledger.validate(&id("project.scout.fixture")));
        let transaction = database
            .begin_write_transaction("inject ambiguous Scout address fixture")
            .await
            .unwrap();
        database
            .set_metadata_unguarded(
                &transaction,
                ADDRESS_LEDGER_KEY_V1,
                &serde_json::to_string(&ledger).unwrap(),
            )
            .await
            .unwrap();
        transaction.commit().await.unwrap();
        assert_eq!(
            restarted.resolve(&hook, &next_revision).await,
            ContextScoutAddressResolveOutcomeV1::Unavailable
        );
    }

    #[test]
    fn native_envelope_binding_mismatch_fails_closed() {
        let binding = binding();
        let mut envelope = admitted_hook().envelope;
        envelope.worktree_id = [99; 16];
        assert!(AdmittedContextScoutHookV1::new(envelope, &binding).is_none());
    }
}
