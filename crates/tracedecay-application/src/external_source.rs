//! Application admission for one sanitized external-source page.
//!
//! This layer owns no connector, scheduler, or store implementation. It turns
//! pinned source authority and an admitted page into a bounded commit-ready
//! value for the authoritative store adapter.

use std::collections::BTreeSet;

use thiserror::Error;
use tracedecay_domain::{
    DomainError, ManifestDigest, SourceAggregateFrontierV1, SourceBindingIdentityV1,
    SourceBindingV1, SourceCaptureModeV1, SourceContentStateV1, SourceDefinitionV1,
    SourceEnvelopeKindV1, SourceEventAdmissionDispositionV1, SourceEventAdmissionReceiptV1,
    SourceEventV1, SourceObjectObservationV1, SourcePartitionFrontierV1, SourceProviderEnvelopeV1,
    SourceRefetchStrategyV1, SourceRefreshCauseV1, SourceRefreshReceiptV1,
    SourceSnapshotCompletionV1, SourceWholeRootStageV1, canonical_sha256,
};

pub const MAX_SOURCE_OBSERVATIONS_PER_ADMISSION_V1: usize = 10_000;

#[derive(Debug, Error)]
pub enum SourceCaptureAdmissionErrorV1 {
    #[error("external source domain contract is invalid")]
    Domain(#[from] DomainError),
    #[error("external source event capture mode does not admit events")]
    EventModeMismatch,
    #[error("external source refresh does not match pinned definition or binding")]
    RefreshAuthorityMismatch,
    #[error("external source provider envelope does not match its owning refresh")]
    ProviderEnvelopeMismatch,
    #[error("external source provider envelope mode or strategy is not pinned")]
    ModeStrategyMismatch,
    #[error("external source incremental cursor or sequence is not gap-free")]
    CursorGap,
    #[error("external source whole-root staging is not contiguous")]
    WholeRootStageMismatch,
    #[error("event-triggered source content requires canonical refetch authority")]
    MissingCanonicalRefetchAuthority,
    #[error("external source admission authority is missing, stale, or mismatched")]
    AdmissionAuthority,
    #[error("external source sanitization authority is missing, stale, or mismatched")]
    SanitizationAuthority,
    #[error("source admission contains duplicate native objects")]
    DuplicateNativeObject,
    #[error("source snapshot completion does not match its complete partition frontier")]
    SnapshotCompletionMismatch,
    #[error("source admission exceeds the bounded object limit")]
    TooManyObjects,
}

/// Exact revisions checked by the application immediately before source
/// admission. Paths and mutable labels are deliberately absent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceAuthorityContextV1 {
    binding: SourceBindingIdentityV1,
    definition_revision: u64,
    definition_digest: ManifestDigest,
    binding_revision: u64,
    binding_digest: ManifestDigest,
    configuration_revision: u64,
    configuration_digest: ManifestDigest,
    sink_revision: u64,
    sink_digest: ManifestDigest,
    refresh_receipt_digest: ManifestDigest,
    provider_envelope_digest: ManifestDigest,
}

impl SourceAuthorityContextV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        definition: &SourceDefinitionV1,
        binding: &SourceBindingV1,
        configuration_revision: u64,
        configuration_digest: ManifestDigest,
        sink_revision: u64,
        sink_digest: ManifestDigest,
        refresh: &SourceRefreshReceiptV1,
        envelope: &SourceProviderEnvelopeV1,
    ) -> Result<Self, SourceCaptureAdmissionErrorV1> {
        definition.validate()?;
        binding.validate_against(definition)?;
        refresh.validate()?;
        envelope.validate()?;
        configuration_digest.validate()?;
        sink_digest.validate()?;
        if configuration_revision == 0 || sink_revision == 0 {
            return Err(SourceCaptureAdmissionErrorV1::AdmissionAuthority);
        }
        let context = Self {
            binding: binding.immutable_identity()?,
            definition_revision: definition.revision,
            definition_digest: definition.definition_digest.clone(),
            binding_revision: binding.binding_revision,
            binding_digest: binding.binding_digest.clone(),
            configuration_revision,
            configuration_digest,
            sink_revision,
            sink_digest,
            refresh_receipt_digest: refresh.receipt_digest().clone(),
            provider_envelope_digest: envelope.envelope_digest().clone(),
        };
        Ok(context)
    }

    pub fn binding(&self) -> &SourceBindingIdentityV1 {
        &self.binding
    }

    pub fn definition_revision(&self) -> u64 {
        self.definition_revision
    }

    pub fn definition_digest(&self) -> &ManifestDigest {
        &self.definition_digest
    }

    pub fn binding_revision(&self) -> u64 {
        self.binding_revision
    }

    pub fn binding_digest(&self) -> &ManifestDigest {
        &self.binding_digest
    }

    pub fn configuration_revision(&self) -> u64 {
        self.configuration_revision
    }

    pub fn configuration_digest(&self) -> &ManifestDigest {
        &self.configuration_digest
    }

    pub fn sink_revision(&self) -> u64 {
        self.sink_revision
    }

    pub fn sink_digest(&self) -> &ManifestDigest {
        &self.sink_digest
    }

    pub fn refresh_receipt_digest(&self) -> &ManifestDigest {
        &self.refresh_receipt_digest
    }

    pub fn provider_envelope_digest(&self) -> &ManifestDigest {
        &self.provider_envelope_digest
    }
}

/// Opaque, non-serializable proof that the exact authority snapshot was
/// admitted. Callers cannot construct one from DTO fields.
#[derive(Clone, Debug)]
pub struct SourceAdmissionAuthorityV1 {
    context: SourceAuthorityContextV1,
}

impl SourceAdmissionAuthorityV1 {
    /// Minted only by the application owner after its authorization rechecks.
    pub(crate) fn issue(context: SourceAuthorityContextV1) -> Self {
        Self { context }
    }
}

/// Opaque, non-serializable proof over the exact sanitized observation set.
#[derive(Clone, Debug)]
pub struct SourceSanitizationAuthorityV1 {
    context: SourceAuthorityContextV1,
    observations_digest: ManifestDigest,
}

impl SourceSanitizationAuthorityV1 {
    /// Minted only by the capture owner after canonical sanitization.
    pub(crate) fn issue(
        context: SourceAuthorityContextV1,
        observations: &[SourceObjectObservationV1],
    ) -> Result<Self, SourceCaptureAdmissionErrorV1> {
        let observations_digest = canonical_sha256(&(
            "tracedecay.external-source.sanitized-observations.v1",
            observations,
        ))?;
        Ok(Self {
            context,
            observations_digest,
        })
    }
}

/// Application owner for one exact authorized source refresh.
///
/// Authorization and sanitization remain separate stages: admission authority
/// is fixed after the application rechecks pinned revisions, while
/// sanitization authority is minted only over the final canonical observation
/// set passed to [`Self::capture_sanitized`].
#[derive(Clone, Debug)]
pub struct SourceCaptureApplicationV1 {
    context: SourceAuthorityContextV1,
    admission_authority: SourceAdmissionAuthorityV1,
}

impl SourceCaptureApplicationV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn authorize(
        definition: &SourceDefinitionV1,
        binding: &SourceBindingV1,
        configuration_revision: u64,
        configuration_digest: ManifestDigest,
        sink_revision: u64,
        sink_digest: ManifestDigest,
        refresh: &SourceRefreshReceiptV1,
        provider_envelope: &SourceProviderEnvelopeV1,
    ) -> Result<Self, SourceCaptureAdmissionErrorV1> {
        let context = SourceAuthorityContextV1::new(
            definition,
            binding,
            configuration_revision,
            configuration_digest,
            sink_revision,
            sink_digest,
            refresh,
            provider_envelope,
        )?;
        let admission_authority = SourceAdmissionAuthorityV1::issue(context.clone());
        Ok(Self {
            context,
            admission_authority,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn capture_sanitized(
        &self,
        definition: SourceDefinitionV1,
        binding: SourceBindingV1,
        refresh: SourceRefreshReceiptV1,
        provider_envelope: SourceProviderEnvelopeV1,
        canonical_refetch: Option<&SourceCanonicalRefetchAuthorityV1>,
        expected_frontier: Option<SourceAggregateFrontierV1>,
        next_partition: SourcePartitionFrontierV1,
        previous_whole_root_stage: Option<&SourceWholeRootStageV1>,
        observations: Vec<SourceObjectObservationV1>,
        idempotency_key: ManifestDigest,
        request_digest: ManifestDigest,
    ) -> Result<SourceCaptureAdmissionV1, SourceCaptureAdmissionErrorV1> {
        let sanitization_authority =
            SourceSanitizationAuthorityV1::issue(self.context.clone(), &observations)?;
        SourceCaptureAdmissionV1::from_authorities(
            definition,
            binding,
            refresh,
            provider_envelope,
            &self.admission_authority,
            &sanitization_authority,
            canonical_refetch,
            expected_frontier,
            next_partition,
            previous_whole_root_stage,
            observations,
            idempotency_key,
            request_digest,
        )
    }
}

/// Opaque capability to consume only the acquisition refresh named by an
/// admitted content-free event.
#[derive(Clone, Debug)]
pub struct SourceCanonicalRefetchAuthorityV1 {
    binding: SourceBindingIdentityV1,
    original_refresh_digest: ManifestDigest,
}

impl SourceCanonicalRefetchAuthorityV1 {
    fn matches(&self, refresh: &SourceRefreshReceiptV1) -> bool {
        self.binding == *refresh.binding()
            && self.original_refresh_digest == *refresh.receipt_digest()
    }

    /// Reports whether this opaque capability names the exact refresh.
    ///
    /// The capability still exposes no binding fields or constructor, so a
    /// provider or transport cannot mint or retarget it.
    pub fn authorizes(&self, refresh: &SourceRefreshReceiptV1) -> bool {
        self.matches(refresh)
    }
}

#[derive(Clone, Debug)]
pub enum SourceEventAdmissionContextV1 {
    Enqueue(SourceRefreshReceiptV1),
    Coalesce(SourceEventAdmissionReceiptV1),
    Duplicate(SourceEventAdmissionReceiptV1),
}

/// Pure content-free event admission. Acquisition owns refresh scheduling; the
/// boolean only tells it whether this admission created the original refresh.
#[derive(Clone, Debug)]
pub struct SourceEventAdmissionV1 {
    receipt: SourceEventAdmissionReceiptV1,
    canonical_refetch: SourceCanonicalRefetchAuthorityV1,
    schedules_refresh: bool,
}

impl SourceEventAdmissionV1 {
    pub fn admit(
        definition: &SourceDefinitionV1,
        binding: &SourceBindingV1,
        event: SourceEventV1,
        context: SourceEventAdmissionContextV1,
    ) -> Result<Self, SourceCaptureAdmissionErrorV1> {
        definition.validate()?;
        binding.validate_against(definition)?;
        event.validate()?;
        let binding_identity = binding.immutable_identity()?;
        if definition.capture_mode == SourceCaptureModeV1::Poll
            || event.binding() != &binding_identity
        {
            return Err(SourceCaptureAdmissionErrorV1::EventModeMismatch);
        }

        let (original_event_key, original_refresh, disposition, schedules_refresh) = match context {
            SourceEventAdmissionContextV1::Enqueue(refresh) => (
                event.event_key().clone(),
                refresh,
                SourceEventAdmissionDispositionV1::Enqueued,
                true,
            ),
            SourceEventAdmissionContextV1::Coalesce(original) => {
                original.validate()?;
                (
                    original.original_event_key().clone(),
                    original.original_refresh().clone(),
                    SourceEventAdmissionDispositionV1::Coalesced,
                    false,
                )
            }
            SourceEventAdmissionContextV1::Duplicate(original) => {
                original.validate()?;
                if original.event_key() != event.event_key() {
                    return Err(SourceCaptureAdmissionErrorV1::RefreshAuthorityMismatch);
                }
                (
                    original.original_event_key().clone(),
                    original.original_refresh().clone(),
                    SourceEventAdmissionDispositionV1::Duplicate,
                    false,
                )
            }
        };
        validate_refresh(definition, &binding_identity, &original_refresh)?;
        if original_refresh.cause() != SourceRefreshCauseV1::Event {
            return Err(SourceCaptureAdmissionErrorV1::RefreshAuthorityMismatch);
        }
        let receipt = SourceEventAdmissionReceiptV1::new(
            &event,
            original_event_key,
            original_refresh,
            disposition,
        )?;
        let canonical_refetch = SourceCanonicalRefetchAuthorityV1 {
            binding: binding_identity,
            original_refresh_digest: receipt.original_refresh().receipt_digest().clone(),
        };
        Ok(Self {
            receipt,
            canonical_refetch,
            schedules_refresh,
        })
    }

    /// Reissue the opaque refetch capability after restart from the persisted
    /// content-free event receipt.
    ///
    /// The receipt is not a provider payload and cannot authorize any refresh
    /// other than the exact original event refresh it records.
    pub fn resume(
        definition: &SourceDefinitionV1,
        binding: &SourceBindingV1,
        receipt: SourceEventAdmissionReceiptV1,
    ) -> Result<Self, SourceCaptureAdmissionErrorV1> {
        definition.validate()?;
        binding.validate_against(definition)?;
        receipt.validate()?;
        let binding_identity = binding.immutable_identity()?;
        if definition.capture_mode == SourceCaptureModeV1::Poll
            || receipt.binding() != &binding_identity
        {
            return Err(SourceCaptureAdmissionErrorV1::EventModeMismatch);
        }
        validate_refresh(definition, &binding_identity, receipt.original_refresh())?;
        if receipt.original_refresh().cause() != SourceRefreshCauseV1::Event {
            return Err(SourceCaptureAdmissionErrorV1::RefreshAuthorityMismatch);
        }
        let canonical_refetch = SourceCanonicalRefetchAuthorityV1 {
            binding: binding_identity,
            original_refresh_digest: receipt.original_refresh().receipt_digest().clone(),
        };
        Ok(Self {
            receipt,
            canonical_refetch,
            schedules_refresh: false,
        })
    }

    pub fn receipt(&self) -> &SourceEventAdmissionReceiptV1 {
        &self.receipt
    }

    pub fn canonical_refetch(&self) -> &SourceCanonicalRefetchAuthorityV1 {
        &self.canonical_refetch
    }

    pub fn schedules_refresh(&self) -> bool {
        self.schedules_refresh
    }
}

/// One already-sanitized provider page, pinned to one definition/binding and
/// ready for an atomic source commit. Capture does not persist it itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceCaptureAdmissionV1 {
    definition: SourceDefinitionV1,
    binding: SourceBindingV1,
    refresh: SourceRefreshReceiptV1,
    provider_envelope: SourceProviderEnvelopeV1,
    expected_frontier: Option<SourceAggregateFrontierV1>,
    next_frontier: SourceAggregateFrontierV1,
    observations: Vec<SourceObjectObservationV1>,
    whole_root_stage: Option<SourceWholeRootStageV1>,
    snapshot_completion: Option<SourceSnapshotCompletionV1>,
    idempotency_key: ManifestDigest,
    request_digest: ManifestDigest,
}

impl SourceCaptureAdmissionV1 {
    #[allow(clippy::too_many_arguments)]
    fn from_authorities(
        definition: SourceDefinitionV1,
        binding: SourceBindingV1,
        refresh: SourceRefreshReceiptV1,
        provider_envelope: SourceProviderEnvelopeV1,
        admission_authority: &SourceAdmissionAuthorityV1,
        sanitization_authority: &SourceSanitizationAuthorityV1,
        canonical_refetch: Option<&SourceCanonicalRefetchAuthorityV1>,
        expected_frontier: Option<SourceAggregateFrontierV1>,
        next_partition: SourcePartitionFrontierV1,
        previous_whole_root_stage: Option<&SourceWholeRootStageV1>,
        observations: Vec<SourceObjectObservationV1>,
        idempotency_key: ManifestDigest,
        request_digest: ManifestDigest,
    ) -> Result<Self, SourceCaptureAdmissionErrorV1> {
        definition.validate()?;
        binding.validate_against(&definition)?;
        refresh.validate()?;
        provider_envelope.validate()?;
        if observations.len() > MAX_SOURCE_OBSERVATIONS_PER_ADMISSION_V1 {
            return Err(SourceCaptureAdmissionErrorV1::TooManyObjects);
        }
        let binding_identity = binding.immutable_identity()?;
        validate_refresh(&definition, &binding_identity, &refresh)?;
        validate_provider_envelope(&definition, &refresh, &provider_envelope)?;
        if refresh.cause() == SourceRefreshCauseV1::Event
            && !canonical_refetch.is_some_and(|authority| authority.matches(&refresh))
        {
            return Err(SourceCaptureAdmissionErrorV1::MissingCanonicalRefetchAuthority);
        }
        if admission_authority.context.binding != binding_identity
            || admission_authority.context.definition_revision != definition.revision
            || admission_authority.context.definition_digest != definition.definition_digest
            || admission_authority.context.binding_revision != binding.binding_revision
            || admission_authority.context.binding_digest != binding.binding_digest
            || admission_authority.context.refresh_receipt_digest != *refresh.receipt_digest()
            || admission_authority.context.provider_envelope_digest
                != *provider_envelope.envelope_digest()
        {
            return Err(SourceCaptureAdmissionErrorV1::AdmissionAuthority);
        }
        if sanitization_authority.context != admission_authority.context
            || sanitization_authority.observations_digest
                != canonical_sha256(&(
                    "tracedecay.external-source.sanitized-observations.v1",
                    &observations,
                ))?
        {
            return Err(SourceCaptureAdmissionErrorV1::SanitizationAuthority);
        }
        if next_partition.binding() != &binding_identity {
            return Err(SourceCaptureAdmissionErrorV1::SnapshotCompletionMismatch);
        }
        if next_partition.partition() != provider_envelope.partition()
            || next_partition.input_digest() != provider_envelope.envelope_digest()
            || next_partition.coverage() != provider_envelope.coverage()
        {
            return Err(SourceCaptureAdmissionErrorV1::ProviderEnvelopeMismatch);
        }
        if let Some(expected) = &expected_frontier
            && expected.binding() != &binding_identity
        {
            return Err(SourceCaptureAdmissionErrorV1::SnapshotCompletionMismatch);
        }
        let previous_partition = expected_frontier
            .as_ref()
            .and_then(|frontier| frontier.partition(provider_envelope.partition()));
        let expected_sequence = previous_partition.map_or(1, |frontier| frontier.sequence() + 1);
        if next_partition.sequence() != expected_sequence {
            return Err(SourceCaptureAdmissionErrorV1::CursorGap);
        }
        let mut native_objects = BTreeSet::new();
        for observation in &observations {
            observation.validate()?;
            if !native_objects.insert(observation.native_object().clone()) {
                return Err(SourceCaptureAdmissionErrorV1::DuplicateNativeObject);
            }
        }
        let (whole_root_stage, snapshot_completion) = match provider_envelope.kind() {
            SourceEnvelopeKindV1::Incremental => {
                if previous_whole_root_stage.is_some()
                    || provider_envelope.expected_cursor()
                        != previous_partition.and_then(SourcePartitionFrontierV1::cursor)
                    || next_partition.cursor() != provider_envelope.next_cursor()
                    || next_partition.continuation() != provider_envelope.next_cursor()
                    || next_partition.snapshot().is_some()
                {
                    return Err(SourceCaptureAdmissionErrorV1::CursorGap);
                }
                (None, None)
            }
            SourceEnvelopeKindV1::WholeRoot | SourceEnvelopeKindV1::WholeRootFallback => {
                if next_partition.snapshot() != provider_envelope.snapshot()
                    || next_partition.continuation() != provider_envelope.next_cursor()
                {
                    return Err(SourceCaptureAdmissionErrorV1::WholeRootStageMismatch);
                }
                let page_objects = observations
                    .iter()
                    .filter(|observation| {
                        observation.content_state() != SourceContentStateV1::AuthoritativeDeleted
                    })
                    .map(|observation| observation.native_object().clone())
                    .collect();
                let stage = SourceWholeRootStageV1::advance(
                    previous_whole_root_stage,
                    &provider_envelope,
                    page_objects,
                )
                .map_err(|_| SourceCaptureAdmissionErrorV1::WholeRootStageMismatch)?;
                let completion = (provider_envelope.coverage()
                    == tracedecay_domain::SourceCoverageV1::Complete)
                    .then(|| stage.completion())
                    .transpose()?;
                (Some(stage), completion)
            }
            SourceEnvelopeKindV1::Unavailable => {
                if previous_whole_root_stage.is_some()
                    || !observations.is_empty()
                    || next_partition.cursor().is_some()
                    || next_partition.snapshot().is_some()
                    || next_partition.continuation().is_some()
                {
                    return Err(SourceCaptureAdmissionErrorV1::ProviderEnvelopeMismatch);
                }
                (None, None)
            }
        };
        idempotency_key.validate()?;
        request_digest.validate()?;
        let next_frontier = SourceAggregateFrontierV1::with_updated_partition(
            binding_identity,
            expected_frontier.as_ref(),
            next_partition,
        )?;
        Ok(Self {
            definition,
            binding,
            refresh,
            provider_envelope,
            expected_frontier,
            next_frontier,
            observations,
            whole_root_stage,
            snapshot_completion,
            idempotency_key,
            request_digest,
        })
    }

    #[allow(clippy::type_complexity)]
    pub fn into_parts(
        self,
    ) -> (
        SourceDefinitionV1,
        SourceBindingV1,
        SourceRefreshReceiptV1,
        SourceProviderEnvelopeV1,
        Option<SourceAggregateFrontierV1>,
        SourceAggregateFrontierV1,
        Vec<SourceObjectObservationV1>,
        Option<SourceWholeRootStageV1>,
        Option<SourceSnapshotCompletionV1>,
        ManifestDigest,
        ManifestDigest,
    ) {
        (
            self.definition,
            self.binding,
            self.refresh,
            self.provider_envelope,
            self.expected_frontier,
            self.next_frontier,
            self.observations,
            self.whole_root_stage,
            self.snapshot_completion,
            self.idempotency_key,
            self.request_digest,
        )
    }

    pub fn next_frontier(&self) -> &SourceAggregateFrontierV1 {
        &self.next_frontier
    }

    pub fn whole_root_stage(&self) -> Option<&SourceWholeRootStageV1> {
        self.whole_root_stage.as_ref()
    }

    pub fn snapshot_completion(&self) -> Option<&SourceSnapshotCompletionV1> {
        self.snapshot_completion.as_ref()
    }
}

fn validate_refresh(
    definition: &SourceDefinitionV1,
    binding: &SourceBindingIdentityV1,
    refresh: &SourceRefreshReceiptV1,
) -> Result<(), SourceCaptureAdmissionErrorV1> {
    if refresh.binding() != binding
        || refresh.provider() != &definition.provider
        || refresh.capture_mode() != definition.capture_mode
        || refresh.refetch_strategy() != definition.refetch_strategy
        || !matches!(
            (definition.capture_mode, refresh.cause()),
            (SourceCaptureModeV1::Event, SourceRefreshCauseV1::Event)
                | (SourceCaptureModeV1::Poll, SourceRefreshCauseV1::Poll)
                | (SourceCaptureModeV1::Hybrid, _)
        )
    {
        return Err(SourceCaptureAdmissionErrorV1::RefreshAuthorityMismatch);
    }
    Ok(())
}

fn validate_provider_envelope(
    definition: &SourceDefinitionV1,
    refresh: &SourceRefreshReceiptV1,
    envelope: &SourceProviderEnvelopeV1,
) -> Result<(), SourceCaptureAdmissionErrorV1> {
    if envelope.binding() != refresh.binding()
        || envelope.provider() != refresh.provider()
        || envelope.refresh_id() != refresh.refresh_id()
        || envelope.cause() != refresh.cause()
        || envelope.capture_mode() != refresh.capture_mode()
        || envelope.refetch_strategy() != refresh.refetch_strategy()
    {
        return Err(SourceCaptureAdmissionErrorV1::ProviderEnvelopeMismatch);
    }
    let compatible = matches!(
        (definition.refetch_strategy, envelope.kind()),
        (
            SourceRefetchStrategyV1::WholeRoot,
            SourceEnvelopeKindV1::WholeRoot
        ) | (
            SourceRefetchStrategyV1::IncrementalRevision,
            SourceEnvelopeKindV1::Incremental
        ) | (
            SourceRefetchStrategyV1::IncrementalWithWholeRootFallback,
            SourceEnvelopeKindV1::Incremental | SourceEnvelopeKindV1::WholeRootFallback
        ) | (_, SourceEnvelopeKindV1::Unavailable)
    );
    if !compatible {
        return Err(SourceCaptureAdmissionErrorV1::ModeStrategyMismatch);
    }
    Ok(())
}

#[cfg(test)]
#[path = "external_source_tests.rs"]
mod tests;
