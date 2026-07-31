//! Provider-neutral external-source identities, frontiers, and safe snapshots.
//!
//! These contracts carry only typed owners and privacy-bound digests. Provider
//! locators, credentials, paths, and payloads remain outside this boundary.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::configuration::{SourceBindingId, UserProfileId};
use crate::research::{
    DomainError, LocatorDigest, ManifestDigest, PrivacyDomainId, ProjectId, ProviderId,
    SourceInstanceId, canonical_sha256,
};

pub const MAX_SOURCE_PARTITIONS_V1: u16 = 64;

/// Declare an external-source identity that is exactly one [`ManifestDigest`]
/// under a distinct type. The `@unordered` arm omits `PartialOrd`/`Ord`.
macro_rules! source_digest_id {
    ($($(#[$meta:meta])* $name:ident),+ $(,)?) => {$(
        $(#[$meta])*
        #[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[serde(transparent)]
        pub struct $name(ManifestDigest);

        source_digest_id!(@body $name);
    )+};

    (@unordered $($(#[$meta:meta])* $name:ident),+ $(,)?) => {$(
        $(#[$meta])*
        #[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
        #[serde(transparent)]
        pub struct $name(ManifestDigest);

        source_digest_id!(@body $name);
    )+};

    (@body $name:ident) => {
        impl $name {
            pub fn new(digest: ManifestDigest) -> Self {
                Self(digest)
            }

            pub fn digest(&self) -> &ManifestDigest {
                &self.0
            }

            pub fn validate(&self) -> Result<(), DomainError> {
                self.0.validate()
            }
        }
    };
}

source_digest_id!(
    SourcePartitionIdV1,
    SourceCursorV1,
    SourceSnapshotIdV1,
    SourceNativeObjectIdV1,
);

source_digest_id!(
    @unordered
    /// Stable provider revision identity for one native object.
    ///
    /// This intentionally does not derive an ordering relation: object revisions
    /// are comparable only by equality unless a provider-specific contract says
    /// otherwise.
    SourceObjectRevisionV1,
);

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum SourceCaptureModeV1 {
    Event,
    Poll,
    Hybrid,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourceRefreshCauseV1 {
    Event,
    Poll,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum SourceRefetchStrategyV1 {
    WholeRoot,
    IncrementalRevision,
    IncrementalWithWholeRootFallback,
}

impl SourceRefetchStrategyV1 {
    pub const fn supports_whole_root(self) -> bool {
        matches!(
            self,
            Self::WholeRoot | Self::IncrementalWithWholeRootFallback
        )
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourceEnvelopeKindV1 {
    WholeRoot,
    Incremental,
    WholeRootFallback,
    Unavailable,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum SourceDeletionSemanticsV1 {
    ExplicitOnly,
    CompleteSnapshotAbsence,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourceCoverageV1 {
    Complete,
    Partial,
    Unknown,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourceContentStateV1 {
    Live,
    AuthoritativeDeleted,
    Partial,
    TemporarilyUnavailable,
}

/// Provider capabilities decoded from one exact Plan 27 acquisition contract.
///
/// This is intentionally a closed capability set rather than a provider
/// descriptor or connector registry. The provider adapter owns acquisition;
/// the domain only pins the capabilities that admission may rely on.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceAcquisitionCapabilitiesV1 {
    pub capture_modes: BTreeSet<SourceCaptureModeV1>,
    pub refetch_strategies: BTreeSet<SourceRefetchStrategyV1>,
    pub deletion_semantics: BTreeSet<SourceDeletionSemanticsV1>,
}

impl SourceAcquisitionCapabilitiesV1 {
    pub fn new(
        capture_modes: BTreeSet<SourceCaptureModeV1>,
        refetch_strategies: BTreeSet<SourceRefetchStrategyV1>,
        deletion_semantics: BTreeSet<SourceDeletionSemanticsV1>,
    ) -> Result<Self, DomainError> {
        let capabilities = Self {
            capture_modes,
            refetch_strategies,
            deletion_semantics,
        };
        capabilities.validate()?;
        Ok(capabilities)
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        if self.capture_modes.is_empty()
            || self.refetch_strategies.is_empty()
            || self.deletion_semantics.is_empty()
            || (self
                .deletion_semantics
                .contains(&SourceDeletionSemanticsV1::CompleteSnapshotAbsence)
                && !self
                    .refetch_strategies
                    .iter()
                    .copied()
                    .any(SourceRefetchStrategyV1::supports_whole_root))
        {
            return Err(DomainError::NonCanonical {
                field: "external source acquisition capabilities",
            });
        }
        Ok(())
    }

    pub fn supports(
        &self,
        capture_mode: SourceCaptureModeV1,
        refetch_strategy: SourceRefetchStrategyV1,
        deletion_semantics: SourceDeletionSemanticsV1,
    ) -> bool {
        self.capture_modes.contains(&capture_mode)
            && self.refetch_strategies.contains(&refetch_strategy)
            && self.deletion_semantics.contains(&deletion_semantics)
    }
}

/// Exact provider/capability contract emitted by the Plan 27 adapter.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceAcquisitionContractV1 {
    pub provider: ProviderId,
    pub capabilities: SourceAcquisitionCapabilitiesV1,
    pub contract_digest: ManifestDigest,
}

impl SourceAcquisitionContractV1 {
    pub fn new(
        provider: ProviderId,
        capabilities: SourceAcquisitionCapabilitiesV1,
    ) -> Result<Self, DomainError> {
        provider.validate()?;
        capabilities.validate()?;
        let contract_digest = Self::compute_digest(&provider, &capabilities)?;
        Ok(Self {
            provider,
            capabilities,
            contract_digest,
        })
    }

    fn compute_digest(
        provider: &ProviderId,
        capabilities: &SourceAcquisitionCapabilitiesV1,
    ) -> Result<ManifestDigest, DomainError> {
        canonical_sha256(&(
            "tracedecay.plan27.source-acquisition-contract.v1",
            provider,
            capabilities,
        ))
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.provider.validate()?;
        self.capabilities.validate()?;
        self.contract_digest.validate()?;
        if Self::compute_digest(&self.provider, &self.capabilities)? != self.contract_digest {
            return Err(DomainError::DigestMismatch);
        }
        Ok(())
    }
}

/// Immutable provider-neutral source definition.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceDefinitionV1 {
    pub source_id: SourceInstanceId,
    pub provider: ProviderId,
    pub revision: u64,
    pub capture_mode: SourceCaptureModeV1,
    pub refetch_strategy: SourceRefetchStrategyV1,
    pub deletion_semantics: SourceDeletionSemanticsV1,
    pub max_partitions: u16,
    pub acquisition_contract_digest: ManifestDigest,
    pub acquisition_capabilities: SourceAcquisitionCapabilitiesV1,
    pub definition_digest: ManifestDigest,
}

impl SourceDefinitionV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        source_id: SourceInstanceId,
        revision: u64,
        acquisition_contract: SourceAcquisitionContractV1,
        capture_mode: SourceCaptureModeV1,
        refetch_strategy: SourceRefetchStrategyV1,
        deletion_semantics: SourceDeletionSemanticsV1,
        max_partitions: u16,
    ) -> Result<Self, DomainError> {
        acquisition_contract.validate()?;
        let provider = acquisition_contract.provider;
        let acquisition_contract_digest = acquisition_contract.contract_digest;
        let acquisition_capabilities = acquisition_contract.capabilities;
        let definition_digest = Self::compute_digest(
            &source_id,
            &provider,
            revision,
            capture_mode,
            refetch_strategy,
            deletion_semantics,
            max_partitions,
            &acquisition_contract_digest,
            &acquisition_capabilities,
        )?;
        let definition = Self {
            source_id,
            provider,
            revision,
            capture_mode,
            refetch_strategy,
            deletion_semantics,
            max_partitions,
            acquisition_contract_digest,
            acquisition_capabilities,
            definition_digest,
        };
        definition.validate()?;
        Ok(definition)
    }

    #[allow(clippy::too_many_arguments)]
    fn compute_digest(
        source_id: &SourceInstanceId,
        provider: &ProviderId,
        revision: u64,
        capture_mode: SourceCaptureModeV1,
        refetch_strategy: SourceRefetchStrategyV1,
        deletion_semantics: SourceDeletionSemanticsV1,
        max_partitions: u16,
        acquisition_contract_digest: &ManifestDigest,
        acquisition_capabilities: &SourceAcquisitionCapabilitiesV1,
    ) -> Result<ManifestDigest, DomainError> {
        canonical_sha256(&(
            "tracedecay.external-source.definition.v1",
            source_id,
            provider,
            revision,
            capture_mode,
            refetch_strategy,
            deletion_semantics,
            max_partitions,
            acquisition_contract_digest,
            acquisition_capabilities,
        ))
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.source_id.validate()?;
        self.provider.validate()?;
        self.acquisition_contract_digest.validate()?;
        self.acquisition_capabilities.validate()?;
        self.definition_digest.validate()?;
        if self.revision == 0
            || self.max_partitions == 0
            || self.max_partitions > MAX_SOURCE_PARTITIONS_V1
            || (self.deletion_semantics == SourceDeletionSemanticsV1::CompleteSnapshotAbsence
                && !self.refetch_strategy.supports_whole_root())
            || !self.acquisition_capabilities.supports(
                self.capture_mode,
                self.refetch_strategy,
                self.deletion_semantics,
            )
        {
            return Err(DomainError::NonCanonical {
                field: "external source definition",
            });
        }
        if SourceAcquisitionContractV1::compute_digest(
            &self.provider,
            &self.acquisition_capabilities,
        )? != self.acquisition_contract_digest
        {
            return Err(DomainError::DigestMismatch);
        }
        if Self::compute_digest(
            &self.source_id,
            &self.provider,
            self.revision,
            self.capture_mode,
            self.refetch_strategy,
            self.deletion_semantics,
            self.max_partitions,
            &self.acquisition_contract_digest,
            &self.acquisition_capabilities,
        )? != self.definition_digest
        {
            return Err(DomainError::DigestMismatch);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case", tag = "kind", content = "id")]
pub enum SourceBindingOwnerV1 {
    Project(ProjectId),
    Profile(UserProfileId),
}

impl SourceBindingOwnerV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        match self {
            Self::Project(project_id) => project_id.validate(),
            Self::Profile(profile_id) => profile_id.validate(),
        }
    }
}

/// The immutable dimensions that prevent sources from crossing owners or
/// privacy domains.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(deny_unknown_fields)]
pub struct SourceBindingIdentityV1 {
    pub binding_id: SourceBindingId,
    pub source_id: SourceInstanceId,
    pub owner: SourceBindingOwnerV1,
    pub privacy_domain: PrivacyDomainId,
    pub native_root: LocatorDigest,
}

impl SourceBindingIdentityV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.binding_id.validate()?;
        self.source_id.validate()?;
        self.owner.validate()?;
        self.privacy_domain.validate()?;
        self.native_root.validate()
    }
}

/// Immutable source-to-owner binding snapshot.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceBindingV1 {
    pub binding_id: SourceBindingId,
    pub source_id: SourceInstanceId,
    pub definition_revision: u64,
    pub definition_digest: ManifestDigest,
    pub binding_revision: u64,
    pub owner: SourceBindingOwnerV1,
    pub privacy_domain: PrivacyDomainId,
    pub native_root: LocatorDigest,
    pub binding_digest: ManifestDigest,
}

impl SourceBindingV1 {
    pub fn new(
        definition: &SourceDefinitionV1,
        owner: SourceBindingOwnerV1,
        privacy_domain: PrivacyDomainId,
        native_root: LocatorDigest,
        binding_revision: u64,
    ) -> Result<Self, DomainError> {
        definition.validate()?;
        owner.validate()?;
        privacy_domain.validate()?;
        native_root.validate()?;
        if binding_revision == 0 {
            return Err(DomainError::NonCanonical {
                field: "external source binding revision",
            });
        }
        let binding_id =
            Self::derive_binding_id(&definition.source_id, &owner, &privacy_domain, &native_root)?;
        let binding_digest = Self::compute_digest(
            &binding_id,
            &definition.source_id,
            definition.revision,
            &definition.definition_digest,
            binding_revision,
            &owner,
            &privacy_domain,
            &native_root,
        )?;
        let binding = Self {
            binding_id,
            source_id: definition.source_id.clone(),
            definition_revision: definition.revision,
            definition_digest: definition.definition_digest.clone(),
            binding_revision,
            owner,
            privacy_domain,
            native_root,
            binding_digest,
        };
        binding.validate_against(definition)?;
        Ok(binding)
    }

    pub fn immutable_identity(&self) -> Result<SourceBindingIdentityV1, DomainError> {
        let identity = SourceBindingIdentityV1 {
            binding_id: self.binding_id.clone(),
            source_id: self.source_id.clone(),
            owner: self.owner.clone(),
            privacy_domain: self.privacy_domain.clone(),
            native_root: self.native_root.clone(),
        };
        identity.validate()?;
        Ok(identity)
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.immutable_identity()?.validate()?;
        self.definition_digest.validate()?;
        self.binding_digest.validate()?;
        if self.definition_revision == 0 || self.binding_revision == 0 {
            return Err(DomainError::NonCanonical {
                field: "external source binding revision",
            });
        }
        let expected_id = Self::derive_binding_id(
            &self.source_id,
            &self.owner,
            &self.privacy_domain,
            &self.native_root,
        )?;
        if self.binding_id != expected_id {
            return Err(DomainError::NonCanonical {
                field: "external source binding identity",
            });
        }
        let expected_digest = Self::compute_digest(
            &self.binding_id,
            &self.source_id,
            self.definition_revision,
            &self.definition_digest,
            self.binding_revision,
            &self.owner,
            &self.privacy_domain,
            &self.native_root,
        )?;
        if expected_digest != self.binding_digest {
            return Err(DomainError::DigestMismatch);
        }
        Ok(())
    }

    pub fn validate_against(&self, definition: &SourceDefinitionV1) -> Result<(), DomainError> {
        self.validate()?;
        definition.validate()?;
        if self.source_id != definition.source_id
            || self.definition_revision != definition.revision
            || self.definition_digest != definition.definition_digest
        {
            return Err(DomainError::SnapshotMismatch {
                field: "external source binding definition",
            });
        }
        Ok(())
    }

    fn derive_binding_id(
        source_id: &SourceInstanceId,
        owner: &SourceBindingOwnerV1,
        privacy_domain: &PrivacyDomainId,
        native_root: &LocatorDigest,
    ) -> Result<SourceBindingId, DomainError> {
        let digest = canonical_sha256(&(
            "tracedecay.external-source.binding-id.v1",
            source_id,
            owner,
            privacy_domain,
            native_root,
        ))?;
        SourceBindingId::new(format!(
            "external-source.{}",
            digest.as_str().trim_start_matches("sha256:")
        ))
    }

    #[allow(clippy::too_many_arguments)]
    fn compute_digest(
        binding_id: &SourceBindingId,
        source_id: &SourceInstanceId,
        definition_revision: u64,
        definition_digest: &ManifestDigest,
        binding_revision: u64,
        owner: &SourceBindingOwnerV1,
        privacy_domain: &PrivacyDomainId,
        native_root: &LocatorDigest,
    ) -> Result<ManifestDigest, DomainError> {
        canonical_sha256(&(
            "tracedecay.external-source.binding.v1",
            binding_id,
            source_id,
            definition_revision,
            definition_digest,
            binding_revision,
            owner,
            privacy_domain,
            native_root,
        ))
    }
}

/// Stable, content-free identity for one external wake-up signal.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct SourceEventKeyV1(ManifestDigest);

impl SourceEventKeyV1 {
    pub fn derive(
        binding: &SourceBindingIdentityV1,
        stable_signal_digest: &ManifestDigest,
    ) -> Result<Self, DomainError> {
        binding.validate()?;
        stable_signal_digest.validate()?;
        Ok(Self(canonical_sha256(&(
            "tracedecay.external-source.event-key.v1",
            binding,
            stable_signal_digest,
        ))?))
    }

    pub fn digest(&self) -> &ManifestDigest {
        &self.0
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.0.validate()
    }
}

/// Content-free wake-up evidence. Native payload, paths, URLs, and rendered
/// provider fields cannot cross this boundary.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceEventV1 {
    binding: SourceBindingIdentityV1,
    stable_signal_digest: ManifestDigest,
    event_key: SourceEventKeyV1,
}

impl SourceEventV1 {
    pub fn new(
        binding: SourceBindingIdentityV1,
        stable_signal_digest: ManifestDigest,
    ) -> Result<Self, DomainError> {
        let event_key = SourceEventKeyV1::derive(&binding, &stable_signal_digest)?;
        let event = Self {
            binding,
            stable_signal_digest,
            event_key,
        };
        event.validate()?;
        Ok(event)
    }

    pub fn binding(&self) -> &SourceBindingIdentityV1 {
        &self.binding
    }

    pub fn stable_signal_digest(&self) -> &ManifestDigest {
        &self.stable_signal_digest
    }

    pub fn event_key(&self) -> &SourceEventKeyV1 {
        &self.event_key
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.binding.validate()?;
        self.stable_signal_digest.validate()?;
        self.event_key.validate()?;
        if self.event_key != SourceEventKeyV1::derive(&self.binding, &self.stable_signal_digest)? {
            return Err(DomainError::DigestMismatch);
        }
        Ok(())
    }
}

/// Acquisition-owned durable evidence for one canonical provider refresh.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceRefreshReceiptV1 {
    binding: SourceBindingIdentityV1,
    provider: ProviderId,
    refresh_id: ManifestDigest,
    cause: SourceRefreshCauseV1,
    capture_mode: SourceCaptureModeV1,
    refetch_strategy: SourceRefetchStrategyV1,
    receipt_digest: ManifestDigest,
}

impl SourceRefreshReceiptV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        binding: SourceBindingIdentityV1,
        provider: ProviderId,
        refresh_id: ManifestDigest,
        cause: SourceRefreshCauseV1,
        capture_mode: SourceCaptureModeV1,
        refetch_strategy: SourceRefetchStrategyV1,
    ) -> Result<Self, DomainError> {
        binding.validate()?;
        provider.validate()?;
        refresh_id.validate()?;
        let receipt_digest = canonical_sha256(&(
            "tracedecay.external-source.refresh-receipt.v1",
            &binding,
            &provider,
            &refresh_id,
            cause,
            capture_mode,
            refetch_strategy,
        ))?;
        let receipt = Self {
            binding,
            provider,
            refresh_id,
            cause,
            capture_mode,
            refetch_strategy,
            receipt_digest,
        };
        receipt.validate()?;
        Ok(receipt)
    }

    pub fn binding(&self) -> &SourceBindingIdentityV1 {
        &self.binding
    }

    pub fn provider(&self) -> &ProviderId {
        &self.provider
    }

    pub fn refresh_id(&self) -> &ManifestDigest {
        &self.refresh_id
    }

    pub fn cause(&self) -> SourceRefreshCauseV1 {
        self.cause
    }

    pub fn capture_mode(&self) -> SourceCaptureModeV1 {
        self.capture_mode
    }

    pub fn refetch_strategy(&self) -> SourceRefetchStrategyV1 {
        self.refetch_strategy
    }

    pub fn receipt_digest(&self) -> &ManifestDigest {
        &self.receipt_digest
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.binding.validate()?;
        self.provider.validate()?;
        self.refresh_id.validate()?;
        self.receipt_digest.validate()?;
        let digest = canonical_sha256(&(
            "tracedecay.external-source.refresh-receipt.v1",
            &self.binding,
            &self.provider,
            &self.refresh_id,
            self.cause,
            self.capture_mode,
            self.refetch_strategy,
        ))?;
        if digest != self.receipt_digest {
            return Err(DomainError::DigestMismatch);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourceEventAdmissionDispositionV1 {
    Enqueued,
    Coalesced,
    Duplicate,
}

/// Stable content-free event receipt. Coalesced and duplicate deliveries retain
/// the first event and refresh rather than manufacturing another refresh.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceEventAdmissionReceiptV1 {
    binding: SourceBindingIdentityV1,
    event_key: SourceEventKeyV1,
    original_event_key: SourceEventKeyV1,
    original_refresh: SourceRefreshReceiptV1,
    disposition: SourceEventAdmissionDispositionV1,
    receipt_digest: ManifestDigest,
}

impl SourceEventAdmissionReceiptV1 {
    pub fn new(
        event: &SourceEventV1,
        original_event_key: SourceEventKeyV1,
        original_refresh: SourceRefreshReceiptV1,
        disposition: SourceEventAdmissionDispositionV1,
    ) -> Result<Self, DomainError> {
        event.validate()?;
        original_event_key.validate()?;
        original_refresh.validate()?;
        if original_refresh.binding() != event.binding() {
            return Err(DomainError::SnapshotMismatch {
                field: "external source event refresh binding",
            });
        }
        if disposition == SourceEventAdmissionDispositionV1::Enqueued
            && original_event_key != *event.event_key()
        {
            return Err(DomainError::NonCanonical {
                field: "external source original event",
            });
        }
        let receipt_digest = canonical_sha256(&(
            "tracedecay.external-source.event-admission-receipt.v1",
            event.binding(),
            event.event_key(),
            &original_event_key,
            &original_refresh,
            disposition,
        ))?;
        let receipt = Self {
            binding: event.binding().clone(),
            event_key: event.event_key().clone(),
            original_event_key,
            original_refresh,
            disposition,
            receipt_digest,
        };
        receipt.validate()?;
        Ok(receipt)
    }

    pub fn binding(&self) -> &SourceBindingIdentityV1 {
        &self.binding
    }

    pub fn event_key(&self) -> &SourceEventKeyV1 {
        &self.event_key
    }

    pub fn original_event_key(&self) -> &SourceEventKeyV1 {
        &self.original_event_key
    }

    pub fn original_refresh(&self) -> &SourceRefreshReceiptV1 {
        &self.original_refresh
    }

    pub fn disposition(&self) -> SourceEventAdmissionDispositionV1 {
        self.disposition
    }

    pub fn receipt_digest(&self) -> &ManifestDigest {
        &self.receipt_digest
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.binding.validate()?;
        self.event_key.validate()?;
        self.original_event_key.validate()?;
        self.original_refresh.validate()?;
        self.receipt_digest.validate()?;
        if self.original_refresh.binding() != &self.binding
            || (self.disposition == SourceEventAdmissionDispositionV1::Enqueued
                && self.original_event_key != self.event_key)
        {
            return Err(DomainError::NonCanonical {
                field: "external source event admission receipt",
            });
        }
        let digest = canonical_sha256(&(
            "tracedecay.external-source.event-admission-receipt.v1",
            &self.binding,
            &self.event_key,
            &self.original_event_key,
            &self.original_refresh,
            self.disposition,
        ))?;
        if digest != self.receipt_digest {
            return Err(DomainError::DigestMismatch);
        }
        Ok(())
    }
}

/// Sanitized provider-page metadata. The native provider payload remains
/// transient and is represented only by its privacy-safe digest.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceProviderEnvelopeV1 {
    binding: SourceBindingIdentityV1,
    provider: ProviderId,
    refresh_id: ManifestDigest,
    cause: SourceRefreshCauseV1,
    capture_mode: SourceCaptureModeV1,
    refetch_strategy: SourceRefetchStrategyV1,
    kind: SourceEnvelopeKindV1,
    partition: SourcePartitionIdV1,
    page_sequence: u32,
    expected_cursor: Option<SourceCursorV1>,
    next_cursor: Option<SourceCursorV1>,
    snapshot: Option<SourceSnapshotIdV1>,
    coverage: SourceCoverageV1,
    sanitized_envelope_digest: ManifestDigest,
    envelope_digest: ManifestDigest,
}

impl SourceProviderEnvelopeV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        binding: SourceBindingIdentityV1,
        provider: ProviderId,
        refresh_id: ManifestDigest,
        cause: SourceRefreshCauseV1,
        capture_mode: SourceCaptureModeV1,
        refetch_strategy: SourceRefetchStrategyV1,
        kind: SourceEnvelopeKindV1,
        partition: SourcePartitionIdV1,
        page_sequence: u32,
        expected_cursor: Option<SourceCursorV1>,
        next_cursor: Option<SourceCursorV1>,
        snapshot: Option<SourceSnapshotIdV1>,
        coverage: SourceCoverageV1,
        sanitized_envelope_digest: ManifestDigest,
    ) -> Result<Self, DomainError> {
        let envelope_digest = Self::compute_digest(
            &binding,
            &provider,
            &refresh_id,
            cause,
            capture_mode,
            refetch_strategy,
            kind,
            &partition,
            page_sequence,
            expected_cursor.as_ref(),
            next_cursor.as_ref(),
            snapshot.as_ref(),
            coverage,
            &sanitized_envelope_digest,
        )?;
        let envelope = Self {
            binding,
            provider,
            refresh_id,
            cause,
            capture_mode,
            refetch_strategy,
            kind,
            partition,
            page_sequence,
            expected_cursor,
            next_cursor,
            snapshot,
            coverage,
            sanitized_envelope_digest,
            envelope_digest,
        };
        envelope.validate()?;
        Ok(envelope)
    }

    #[allow(clippy::too_many_arguments)]
    fn compute_digest(
        binding: &SourceBindingIdentityV1,
        provider: &ProviderId,
        refresh_id: &ManifestDigest,
        cause: SourceRefreshCauseV1,
        capture_mode: SourceCaptureModeV1,
        refetch_strategy: SourceRefetchStrategyV1,
        kind: SourceEnvelopeKindV1,
        partition: &SourcePartitionIdV1,
        page_sequence: u32,
        expected_cursor: Option<&SourceCursorV1>,
        next_cursor: Option<&SourceCursorV1>,
        snapshot: Option<&SourceSnapshotIdV1>,
        coverage: SourceCoverageV1,
        sanitized_envelope_digest: &ManifestDigest,
    ) -> Result<ManifestDigest, DomainError> {
        canonical_sha256(&(
            "tracedecay.external-source.provider-envelope.v1",
            binding,
            provider,
            refresh_id,
            cause,
            capture_mode,
            refetch_strategy,
            kind,
            partition,
            page_sequence,
            expected_cursor,
            next_cursor,
            snapshot,
            coverage,
            sanitized_envelope_digest,
        ))
    }

    pub fn binding(&self) -> &SourceBindingIdentityV1 {
        &self.binding
    }

    pub fn provider(&self) -> &ProviderId {
        &self.provider
    }

    pub fn refresh_id(&self) -> &ManifestDigest {
        &self.refresh_id
    }

    pub fn cause(&self) -> SourceRefreshCauseV1 {
        self.cause
    }

    pub fn capture_mode(&self) -> SourceCaptureModeV1 {
        self.capture_mode
    }

    pub fn refetch_strategy(&self) -> SourceRefetchStrategyV1 {
        self.refetch_strategy
    }

    pub fn kind(&self) -> SourceEnvelopeKindV1 {
        self.kind
    }

    pub fn partition(&self) -> &SourcePartitionIdV1 {
        &self.partition
    }

    pub fn page_sequence(&self) -> u32 {
        self.page_sequence
    }

    pub fn expected_cursor(&self) -> Option<&SourceCursorV1> {
        self.expected_cursor.as_ref()
    }

    pub fn next_cursor(&self) -> Option<&SourceCursorV1> {
        self.next_cursor.as_ref()
    }

    pub fn snapshot(&self) -> Option<&SourceSnapshotIdV1> {
        self.snapshot.as_ref()
    }

    pub fn coverage(&self) -> SourceCoverageV1 {
        self.coverage
    }

    pub fn sanitized_envelope_digest(&self) -> &ManifestDigest {
        &self.sanitized_envelope_digest
    }

    pub fn envelope_digest(&self) -> &ManifestDigest {
        &self.envelope_digest
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.binding.validate()?;
        self.provider.validate()?;
        self.refresh_id.validate()?;
        self.partition.validate()?;
        self.expected_cursor
            .as_ref()
            .map_or(Ok(()), SourceCursorV1::validate)?;
        self.next_cursor
            .as_ref()
            .map_or(Ok(()), SourceCursorV1::validate)?;
        self.snapshot
            .as_ref()
            .map_or(Ok(()), SourceSnapshotIdV1::validate)?;
        self.sanitized_envelope_digest.validate()?;
        self.envelope_digest.validate()?;
        if self.page_sequence == 0 {
            return Err(DomainError::NonCanonical {
                field: "external source provider page sequence",
            });
        }
        match self.kind {
            SourceEnvelopeKindV1::Incremental => {
                if self.snapshot.is_some()
                    || self.expected_cursor.is_none()
                    || self.next_cursor.is_none()
                    || self.expected_cursor == self.next_cursor
                    || self.coverage == SourceCoverageV1::Complete
                {
                    return Err(DomainError::NonCanonical {
                        field: "incremental external source envelope",
                    });
                }
            }
            SourceEnvelopeKindV1::WholeRoot | SourceEnvelopeKindV1::WholeRootFallback => {
                if self.expected_cursor.is_some()
                    || self.snapshot.is_none()
                    || self.coverage == SourceCoverageV1::Unknown
                    || (self.coverage == SourceCoverageV1::Partial && self.next_cursor.is_none())
                    || (self.coverage == SourceCoverageV1::Complete && self.next_cursor.is_some())
                {
                    return Err(DomainError::NonCanonical {
                        field: "whole-root external source envelope",
                    });
                }
            }
            SourceEnvelopeKindV1::Unavailable => {
                if self.expected_cursor.is_some()
                    || self.next_cursor.is_some()
                    || self.snapshot.is_some()
                    || self.coverage != SourceCoverageV1::Unknown
                {
                    return Err(DomainError::NonCanonical {
                        field: "unavailable external source envelope",
                    });
                }
            }
        }
        let digest = Self::compute_digest(
            &self.binding,
            &self.provider,
            &self.refresh_id,
            self.cause,
            self.capture_mode,
            self.refetch_strategy,
            self.kind,
            &self.partition,
            self.page_sequence,
            self.expected_cursor.as_ref(),
            self.next_cursor.as_ref(),
            self.snapshot.as_ref(),
            self.coverage,
            &self.sanitized_envelope_digest,
        )?;
        if digest != self.envelope_digest {
            return Err(DomainError::DigestMismatch);
        }
        Ok(())
    }
}

/// Payload-free whole-root staging state accumulated across provider pages.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceWholeRootStageV1 {
    binding: SourceBindingIdentityV1,
    refresh_id: ManifestDigest,
    partition: SourcePartitionIdV1,
    snapshot: SourceSnapshotIdV1,
    last_page_sequence: u32,
    complete: bool,
    present_objects: BTreeSet<SourceNativeObjectIdV1>,
    stage_digest: ManifestDigest,
}

impl SourceWholeRootStageV1 {
    pub fn advance(
        previous: Option<&Self>,
        envelope: &SourceProviderEnvelopeV1,
        page_objects: BTreeSet<SourceNativeObjectIdV1>,
    ) -> Result<Self, DomainError> {
        envelope.validate()?;
        if !matches!(
            envelope.kind(),
            SourceEnvelopeKindV1::WholeRoot | SourceEnvelopeKindV1::WholeRootFallback
        ) {
            return Err(DomainError::NonCanonical {
                field: "external source whole-root staging envelope",
            });
        }
        for object in &page_objects {
            object.validate()?;
        }
        let snapshot = envelope
            .snapshot()
            .cloned()
            .ok_or(DomainError::NonCanonical {
                field: "external source whole-root staging snapshot",
            })?;
        let mut present_objects = page_objects;
        if let Some(previous) = previous {
            previous.validate()?;
            if previous.complete {
                return Err(DomainError::NonCanonical {
                    field: "completed external source whole-root stage",
                });
            }
            if previous.binding != *envelope.binding()
                || previous.refresh_id != *envelope.refresh_id()
                || previous.partition != *envelope.partition()
                || previous.snapshot != snapshot
            {
                return Err(DomainError::SnapshotMismatch {
                    field: "external source whole-root staging",
                });
            }
            if envelope.page_sequence() != previous.last_page_sequence + 1 {
                return Err(DomainError::NonCanonical {
                    field: "external source whole-root page gap",
                });
            }
            present_objects.extend(previous.present_objects.iter().cloned());
        } else if envelope.page_sequence() != 1 {
            return Err(DomainError::NonCanonical {
                field: "external source whole-root first page",
            });
        }
        let stage_digest = canonical_sha256(&(
            "tracedecay.external-source.whole-root-stage.v1",
            envelope.binding(),
            envelope.refresh_id(),
            envelope.partition(),
            &snapshot,
            envelope.page_sequence(),
            envelope.coverage() == SourceCoverageV1::Complete,
            &present_objects,
        ))?;
        let stage = Self {
            binding: envelope.binding().clone(),
            refresh_id: envelope.refresh_id().clone(),
            partition: envelope.partition().clone(),
            snapshot,
            last_page_sequence: envelope.page_sequence(),
            complete: envelope.coverage() == SourceCoverageV1::Complete,
            present_objects,
            stage_digest,
        };
        stage.validate()?;
        Ok(stage)
    }

    pub fn present_objects(&self) -> &BTreeSet<SourceNativeObjectIdV1> {
        &self.present_objects
    }

    pub fn is_complete(&self) -> bool {
        self.complete
    }

    pub fn completion(&self) -> Result<SourceSnapshotCompletionV1, DomainError> {
        if !self.complete {
            return Err(DomainError::NonCanonical {
                field: "incomplete external source whole-root stage",
            });
        }
        SourceSnapshotCompletionV1::new(
            self.partition.clone(),
            self.snapshot.clone(),
            self.present_objects.clone(),
        )
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.binding.validate()?;
        self.refresh_id.validate()?;
        self.partition.validate()?;
        self.snapshot.validate()?;
        self.stage_digest.validate()?;
        if self.last_page_sequence == 0 {
            return Err(DomainError::NonCanonical {
                field: "external source whole-root stage sequence",
            });
        }
        for object in &self.present_objects {
            object.validate()?;
        }
        let digest = canonical_sha256(&(
            "tracedecay.external-source.whole-root-stage.v1",
            &self.binding,
            &self.refresh_id,
            &self.partition,
            &self.snapshot,
            self.last_page_sequence,
            self.complete,
            &self.present_objects,
        ))?;
        if digest != self.stage_digest {
            return Err(DomainError::DigestMismatch);
        }
        Ok(())
    }
}

/// One partition's committed source frontier. Cursor and snapshot identities
/// are opaque provider-bound digests, never raw provider cursors or URLs.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourcePartitionFrontierV1 {
    binding: SourceBindingIdentityV1,
    partition: SourcePartitionIdV1,
    cursor: Option<SourceCursorV1>,
    snapshot: Option<SourceSnapshotIdV1>,
    continuation: Option<SourceCursorV1>,
    coverage: SourceCoverageV1,
    sequence: u64,
    last_complete_snapshot: Option<SourceSnapshotIdV1>,
    input_digest: ManifestDigest,
}

impl SourcePartitionFrontierV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        binding: SourceBindingIdentityV1,
        partition: SourcePartitionIdV1,
        cursor: Option<SourceCursorV1>,
        snapshot: Option<SourceSnapshotIdV1>,
        continuation: Option<SourceCursorV1>,
        coverage: SourceCoverageV1,
        sequence: u64,
        previous_complete_snapshot: Option<SourceSnapshotIdV1>,
        input_digest: ManifestDigest,
    ) -> Result<Self, DomainError> {
        binding.validate()?;
        partition.validate()?;
        cursor.as_ref().map_or(Ok(()), SourceCursorV1::validate)?;
        snapshot
            .as_ref()
            .map_or(Ok(()), SourceSnapshotIdV1::validate)?;
        continuation
            .as_ref()
            .map_or(Ok(()), SourceCursorV1::validate)?;
        input_digest.validate()?;
        if sequence == 0 {
            return Err(DomainError::NonCanonical {
                field: "external source partition sequence",
            });
        }
        let last_complete_snapshot = match coverage {
            SourceCoverageV1::Complete => {
                if continuation.is_some() {
                    return Err(DomainError::NonCanonical {
                        field: "complete external source continuation",
                    });
                }
                Some(snapshot.clone().ok_or(DomainError::NonCanonical {
                    field: "complete external source snapshot",
                })?)
            }
            SourceCoverageV1::Partial => {
                if continuation.is_none() {
                    return Err(DomainError::NonCanonical {
                        field: "partial external source continuation",
                    });
                }
                previous_complete_snapshot
            }
            SourceCoverageV1::Unknown => {
                if snapshot.is_some() || continuation.is_some() {
                    return Err(DomainError::NonCanonical {
                        field: "unknown external source frontier",
                    });
                }
                previous_complete_snapshot
            }
        };
        let frontier = Self {
            binding,
            partition,
            cursor,
            snapshot,
            continuation,
            coverage,
            sequence,
            last_complete_snapshot,
            input_digest,
        };
        frontier.validate()?;
        Ok(frontier)
    }

    pub fn binding(&self) -> &SourceBindingIdentityV1 {
        &self.binding
    }

    pub fn partition(&self) -> &SourcePartitionIdV1 {
        &self.partition
    }

    pub fn cursor(&self) -> Option<&SourceCursorV1> {
        self.cursor.as_ref()
    }

    pub fn snapshot(&self) -> Option<&SourceSnapshotIdV1> {
        self.snapshot.as_ref()
    }

    pub fn continuation(&self) -> Option<&SourceCursorV1> {
        self.continuation.as_ref()
    }

    pub fn coverage(&self) -> SourceCoverageV1 {
        self.coverage
    }

    pub fn sequence(&self) -> u64 {
        self.sequence
    }

    pub fn last_complete_snapshot(&self) -> Option<SourceSnapshotIdV1> {
        self.last_complete_snapshot.clone()
    }

    pub fn input_digest(&self) -> &ManifestDigest {
        &self.input_digest
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.binding.validate()?;
        self.partition.validate()?;
        self.cursor
            .as_ref()
            .map_or(Ok(()), SourceCursorV1::validate)?;
        self.snapshot
            .as_ref()
            .map_or(Ok(()), SourceSnapshotIdV1::validate)?;
        self.continuation
            .as_ref()
            .map_or(Ok(()), SourceCursorV1::validate)?;
        self.last_complete_snapshot
            .as_ref()
            .map_or(Ok(()), SourceSnapshotIdV1::validate)?;
        self.input_digest.validate()?;
        if self.sequence == 0 {
            return Err(DomainError::NonCanonical {
                field: "external source partition sequence",
            });
        }
        match self.coverage {
            SourceCoverageV1::Complete => {
                if self.continuation.is_some()
                    || self.snapshot.is_none()
                    || self.last_complete_snapshot != self.snapshot
                {
                    return Err(DomainError::NonCanonical {
                        field: "complete external source frontier",
                    });
                }
            }
            SourceCoverageV1::Partial if self.continuation.is_none() => {
                return Err(DomainError::NonCanonical {
                    field: "partial external source continuation",
                });
            }
            SourceCoverageV1::Unknown if self.snapshot.is_some() || self.continuation.is_some() => {
                return Err(DomainError::NonCanonical {
                    field: "unknown external source frontier",
                });
            }
            SourceCoverageV1::Partial | SourceCoverageV1::Unknown => {}
        }
        Ok(())
    }
}

/// Domain-separated aggregate over the sorted current partition heads.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceAggregateFrontierV1 {
    binding: SourceBindingIdentityV1,
    partitions: BTreeMap<SourcePartitionIdV1, SourcePartitionFrontierV1>,
    digest: ManifestDigest,
}

impl SourceAggregateFrontierV1 {
    pub fn with_updated_partition(
        binding: SourceBindingIdentityV1,
        previous: Option<&Self>,
        next: SourcePartitionFrontierV1,
    ) -> Result<Self, DomainError> {
        binding.validate()?;
        next.validate()?;
        if next.binding() != &binding {
            return Err(DomainError::SnapshotMismatch {
                field: "external source partition binding",
            });
        }
        let mut partitions =
            previous.map_or_else(BTreeMap::new, |frontier| frontier.partitions.clone());
        if let Some(previous) = previous {
            previous.validate()?;
            if previous.binding != binding {
                return Err(DomainError::SnapshotMismatch {
                    field: "external source aggregate binding",
                });
            }
        }
        partitions.insert(next.partition().clone(), next);
        Self::new(binding, partitions)
    }

    pub fn new(
        binding: SourceBindingIdentityV1,
        partitions: BTreeMap<SourcePartitionIdV1, SourcePartitionFrontierV1>,
    ) -> Result<Self, DomainError> {
        binding.validate()?;
        if partitions.is_empty() || partitions.len() > usize::from(MAX_SOURCE_PARTITIONS_V1) {
            return Err(DomainError::NonCanonical {
                field: "external source aggregate partitions",
            });
        }
        for (partition, frontier) in &partitions {
            partition.validate()?;
            frontier.validate()?;
            if partition != frontier.partition() || frontier.binding() != &binding {
                return Err(DomainError::SnapshotMismatch {
                    field: "external source aggregate partition",
                });
            }
        }
        let digest = canonical_sha256(&(
            "tracedecay.external-source.aggregate-frontier.v1",
            &binding,
            &partitions,
        ))?;
        let frontier = Self {
            binding,
            partitions,
            digest,
        };
        frontier.validate()?;
        Ok(frontier)
    }

    pub fn binding(&self) -> &SourceBindingIdentityV1 {
        &self.binding
    }

    pub fn partition(&self, partition: &SourcePartitionIdV1) -> Option<&SourcePartitionFrontierV1> {
        self.partitions.get(partition)
    }

    pub fn partitions(&self) -> &BTreeMap<SourcePartitionIdV1, SourcePartitionFrontierV1> {
        &self.partitions
    }

    pub fn digest(&self) -> &ManifestDigest {
        &self.digest
    }

    pub fn coverage(&self) -> SourceCoverageV1 {
        if self
            .partitions
            .values()
            .all(|frontier| frontier.coverage() == SourceCoverageV1::Complete)
        {
            SourceCoverageV1::Complete
        } else if self
            .partitions
            .values()
            .any(|frontier| frontier.coverage() == SourceCoverageV1::Unknown)
        {
            SourceCoverageV1::Unknown
        } else {
            SourceCoverageV1::Partial
        }
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.binding.validate()?;
        if self.partitions.is_empty()
            || self.partitions.len() > usize::from(MAX_SOURCE_PARTITIONS_V1)
        {
            return Err(DomainError::NonCanonical {
                field: "external source aggregate partitions",
            });
        }
        for (partition, frontier) in &self.partitions {
            partition.validate()?;
            frontier.validate()?;
            if partition != frontier.partition() || frontier.binding() != &self.binding {
                return Err(DomainError::SnapshotMismatch {
                    field: "external source aggregate partition",
                });
            }
        }
        let digest = canonical_sha256(&(
            "tracedecay.external-source.aggregate-frontier.v1",
            &self.binding,
            &self.partitions,
        ))?;
        if digest != self.digest {
            return Err(DomainError::DigestMismatch);
        }
        Ok(())
    }
}

/// Immutable sanitized evidence for one provider-native object revision.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceObjectObservationV1 {
    native_object: SourceNativeObjectIdV1,
    revision: SourceObjectRevisionV1,
    sanitized_digest: ManifestDigest,
    content_state: SourceContentStateV1,
}

impl SourceObjectObservationV1 {
    pub fn new(
        native_object: SourceNativeObjectIdV1,
        revision: SourceObjectRevisionV1,
        sanitized_digest: ManifestDigest,
        content_state: SourceContentStateV1,
    ) -> Result<Self, DomainError> {
        let observation = Self {
            native_object,
            revision,
            sanitized_digest,
            content_state,
        };
        observation.validate()?;
        Ok(observation)
    }

    pub fn native_object(&self) -> &SourceNativeObjectIdV1 {
        &self.native_object
    }

    pub fn revision(&self) -> &SourceObjectRevisionV1 {
        &self.revision
    }

    pub fn sanitized_digest(&self) -> &ManifestDigest {
        &self.sanitized_digest
    }

    pub fn content_state(&self) -> SourceContentStateV1 {
        self.content_state
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.native_object.validate()?;
        self.revision.validate()?;
        self.sanitized_digest.validate()
    }
}

/// Payload-free evidence that one whole-root snapshot is complete.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceSnapshotCompletionV1 {
    partition: SourcePartitionIdV1,
    snapshot: SourceSnapshotIdV1,
    present_objects: BTreeSet<SourceNativeObjectIdV1>,
    completion_digest: ManifestDigest,
}

impl SourceSnapshotCompletionV1 {
    pub fn new(
        partition: SourcePartitionIdV1,
        snapshot: SourceSnapshotIdV1,
        present_objects: BTreeSet<SourceNativeObjectIdV1>,
    ) -> Result<Self, DomainError> {
        partition.validate()?;
        snapshot.validate()?;
        for object in &present_objects {
            object.validate()?;
        }
        let completion_digest = canonical_sha256(&(
            "tracedecay.external-source.snapshot-completion.v1",
            &partition,
            &snapshot,
            &present_objects,
        ))?;
        let completion = Self {
            partition,
            snapshot,
            present_objects,
            completion_digest,
        };
        completion.validate()?;
        Ok(completion)
    }

    pub fn partition(&self) -> &SourcePartitionIdV1 {
        &self.partition
    }

    pub fn snapshot(&self) -> &SourceSnapshotIdV1 {
        &self.snapshot
    }

    pub fn present_objects(&self) -> &BTreeSet<SourceNativeObjectIdV1> {
        &self.present_objects
    }

    pub fn completion_digest(&self) -> &ManifestDigest {
        &self.completion_digest
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.partition.validate()?;
        self.snapshot.validate()?;
        self.completion_digest.validate()?;
        for object in &self.present_objects {
            object.validate()?;
        }
        let digest = canonical_sha256(&(
            "tracedecay.external-source.snapshot-completion.v1",
            &self.partition,
            &self.snapshot,
            &self.present_objects,
        ))?;
        if digest != self.completion_digest {
            return Err(DomainError::DigestMismatch);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(seed: char) -> ManifestDigest {
        ManifestDigest::new(format!("sha256:{}", seed.to_string().repeat(64))).unwrap()
    }

    fn binding_identity() -> SourceBindingIdentityV1 {
        SourceBindingIdentityV1 {
            binding_id: SourceBindingId::new("external-source.fixture").unwrap(),
            source_id: SourceInstanceId::new("source.fixture").unwrap(),
            owner: SourceBindingOwnerV1::Project(ProjectId::new("project.fixture").unwrap()),
            privacy_domain: PrivacyDomainId::new("privacy.fixture").unwrap(),
            native_root: LocatorDigest::new(digest('a').as_str()).unwrap(),
        }
    }

    fn acquisition_contract() -> SourceAcquisitionContractV1 {
        SourceAcquisitionContractV1::new(
            ProviderId::new("fixture-provider").unwrap(),
            SourceAcquisitionCapabilitiesV1::new(
                BTreeSet::from([SourceCaptureModeV1::Poll]),
                BTreeSet::from([SourceRefetchStrategyV1::WholeRoot]),
                BTreeSet::from([SourceDeletionSemanticsV1::CompleteSnapshotAbsence]),
            )
            .unwrap(),
        )
        .unwrap()
    }

    fn envelope(
        page_sequence: u32,
        coverage: SourceCoverageV1,
        continuation: Option<SourceCursorV1>,
    ) -> SourceProviderEnvelopeV1 {
        SourceProviderEnvelopeV1::new(
            binding_identity(),
            ProviderId::new("fixture-provider").unwrap(),
            digest('b'),
            SourceRefreshCauseV1::Poll,
            SourceCaptureModeV1::Poll,
            SourceRefetchStrategyV1::WholeRoot,
            SourceEnvelopeKindV1::WholeRoot,
            SourcePartitionIdV1::new(digest('c')),
            page_sequence,
            None,
            continuation,
            Some(SourceSnapshotIdV1::new(digest('d'))),
            coverage,
            digest(char::from_digit(page_sequence, 10).unwrap()),
        )
        .unwrap()
    }

    #[test]
    fn event_wire_shape_is_content_free_and_key_is_stable() {
        let first = SourceEventV1::new(binding_identity(), digest('e')).unwrap();
        let replay = SourceEventV1::new(binding_identity(), digest('e')).unwrap();
        let json = serde_json::to_value(&first).unwrap();
        let fields = json
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>();

        assert_eq!(first.event_key(), replay.event_key());
        assert_eq!(
            fields,
            BTreeSet::from([
                "binding".to_owned(),
                "event_key".to_owned(),
                "stable_signal_digest".to_owned(),
            ])
        );
    }

    #[test]
    fn definition_pins_and_enforces_the_acquisition_contract() {
        let definition = SourceDefinitionV1::new(
            SourceInstanceId::new("source.fixture").unwrap(),
            1,
            acquisition_contract(),
            SourceCaptureModeV1::Poll,
            SourceRefetchStrategyV1::WholeRoot,
            SourceDeletionSemanticsV1::CompleteSnapshotAbsence,
            1,
        )
        .unwrap();
        assert_eq!(
            definition.acquisition_contract_digest,
            acquisition_contract().contract_digest
        );

        let unsupported = SourceDefinitionV1::new(
            SourceInstanceId::new("source.fixture").unwrap(),
            1,
            acquisition_contract(),
            SourceCaptureModeV1::Event,
            SourceRefetchStrategyV1::WholeRoot,
            SourceDeletionSemanticsV1::CompleteSnapshotAbsence,
            1,
        );
        assert!(matches!(
            unsupported,
            Err(DomainError::NonCanonical {
                field: "external source definition"
            })
        ));

        let mut tampered = definition;
        tampered.acquisition_contract_digest = digest('9');
        assert_eq!(tampered.validate(), Err(DomainError::DigestMismatch));
    }

    #[test]
    fn whole_root_stage_accumulates_pages_before_payload_free_completion() {
        let first_object = SourceNativeObjectIdV1::new(digest('f'));
        let second_object = SourceNativeObjectIdV1::new(digest('1'));
        let first = SourceWholeRootStageV1::advance(
            None,
            &envelope(
                1,
                SourceCoverageV1::Partial,
                Some(SourceCursorV1::new(digest('2'))),
            ),
            BTreeSet::from([first_object.clone()]),
        )
        .unwrap();
        let second = SourceWholeRootStageV1::advance(
            Some(&first),
            &envelope(2, SourceCoverageV1::Complete, None),
            BTreeSet::from([second_object.clone()]),
        )
        .unwrap();
        let completion = second.completion().unwrap();

        assert_eq!(
            completion.present_objects(),
            &BTreeSet::from([first_object, second_object])
        );
        let completion_json = serde_json::to_value(completion).unwrap();
        assert_eq!(
            completion_json
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([
                "completion_digest".to_owned(),
                "partition".to_owned(),
                "present_objects".to_owned(),
                "snapshot".to_owned(),
            ])
        );
    }
}
