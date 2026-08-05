//! Canonical store commit adapter for external-source acquisition.

use std::collections::BTreeSet;

use tracedecay_application::{SourceCanonicalRefetchAuthorityV1, SourceCaptureApplicationV1};
use tracedecay_domain::{
    ComponentVersion, SourceAggregateFrontierV1, SourceDefinitionV1, SourceEnvelopeKindV1,
    SourcePartitionFrontierV1, SourceWholeRootStageV1, canonical_sha256,
};

use crate::observation::ObservationCancellation;

use super::{
    ExternalSourceAcquisitionErrorV1, SourceAcquisitionFuture, SourceAcquisitionGrantV1,
    SourceCanonicalCommitOutcomeV1, SourceCanonicalCommitPortV1, SourceCanonicalRefetchPageV1,
    SourceScheduledRefetchV1,
};

impl SourceCanonicalCommitPortV1 for crate::external_source_store::RuntimeExternalSourceStore {
    fn commit<'a>(
        &'a self,
        task: &'a SourceScheduledRefetchV1,
        grant: &'a SourceAcquisitionGrantV1,
        page: SourceCanonicalRefetchPageV1,
        authority: &'a SourceCanonicalRefetchAuthorityV1,
        cancellation: &'a ObservationCancellation,
    ) -> SourceAcquisitionFuture<'a, SourceCanonicalCommitOutcomeV1> {
        Box::pin(async move {
            if cancellation.is_cancelled()
                || !authority.authorizes(task.refresh())
                || page.validate(task, grant).is_err()
            {
                return SourceCanonicalCommitOutcomeV1::Unavailable;
            }
            let binding_identity = match task.binding().immutable_identity() {
                Ok(binding) => binding,
                Err(_) => return SourceCanonicalCommitOutcomeV1::Unavailable,
            };
            let current = match self.read_state(binding_identity.clone()).await {
                Ok(current) => current,
                Err(_) => return SourceCanonicalCommitOutcomeV1::Unavailable,
            };
            let expected_frontier = current
                .as_ref()
                .map(|state| state.source_frontier().clone());
            let previous_partition = expected_frontier
                .as_ref()
                .and_then(|frontier| frontier.partition(page.envelope.partition()));
            let cursor = match page.envelope.kind() {
                SourceEnvelopeKindV1::Incremental => page.envelope.next_cursor().cloned(),
                SourceEnvelopeKindV1::WholeRoot
                | SourceEnvelopeKindV1::WholeRootFallback
                | SourceEnvelopeKindV1::Unavailable => None,
            };
            let next_partition = match SourcePartitionFrontierV1::new(
                binding_identity.clone(),
                page.envelope.partition().clone(),
                cursor,
                page.envelope.snapshot().cloned(),
                page.envelope.next_cursor().cloned(),
                page.envelope.coverage(),
                previous_partition.map_or(1, |frontier| frontier.sequence().saturating_add(1)),
                previous_partition.and_then(SourcePartitionFrontierV1::last_complete_snapshot),
                page.envelope.envelope_digest().clone(),
            ) {
                Ok(frontier) => frontier,
                Err(_) => return SourceCanonicalCommitOutcomeV1::Unavailable,
            };
            let _next_frontier = match SourceAggregateFrontierV1::with_updated_partition(
                binding_identity.clone(),
                expected_frontier.as_ref(),
                next_partition.clone(),
            ) {
                Ok(frontier) => frontier,
                Err(_) => return SourceCanonicalCommitOutcomeV1::Unavailable,
            };
            let page_objects = page
                .mutations
                .iter()
                .filter(|mutation| {
                    mutation.observation().content_state()
                        != tracedecay_domain::SourceContentStateV1::AuthoritativeDeleted
                })
                .map(|mutation| mutation.observation().native_object().clone())
                .collect::<BTreeSet<_>>();
            let whole_root_stage = match page.envelope.kind() {
                SourceEnvelopeKindV1::WholeRoot | SourceEnvelopeKindV1::WholeRootFallback => {
                    match SourceWholeRootStageV1::advance(
                        task.whole_root_stage(),
                        &page.envelope,
                        page_objects,
                    ) {
                        Ok(stage) => Some(stage),
                        Err(_) => return SourceCanonicalCommitOutcomeV1::Unavailable,
                    }
                }
                SourceEnvelopeKindV1::Incremental => None,
                SourceEnvelopeKindV1::Unavailable => {
                    return SourceCanonicalCommitOutcomeV1::Unavailable;
                }
            };
            let capture = match SourceCaptureApplicationV1::authorize(
                task.definition(),
                task.binding(),
                grant.configuration_revision,
                grant.configuration_digest.clone(),
                grant.sink_revision,
                grant.sink_digest.clone(),
                task.refresh(),
                &page.envelope,
            ) {
                Ok(capture) => capture,
                Err(_) => return SourceCanonicalCommitOutcomeV1::Unavailable,
            };
            let idempotency_key = match canonical_sha256(&(
                "tracedecay.external-source.canonical-refetch.commit.v1",
                task.event_receipt(),
                page.envelope.envelope_digest(),
            )) {
                Ok(digest) => digest,
                Err(_) => return SourceCanonicalCommitOutcomeV1::Unavailable,
            };
            let request_digest = match canonical_sha256(&(
                "tracedecay.external-source.canonical-refetch.request.v1",
                task.definition(),
                task.binding(),
                task.refresh(),
                &page.envelope,
                &page.mutations,
                grant.configuration_revision,
                &grant.configuration_digest,
                grant.sink_revision,
                &grant.sink_digest,
                &grant.source_authorization_digest,
            )) {
                Ok(digest) => digest,
                Err(_) => return SourceCanonicalCommitOutcomeV1::Unavailable,
            };
            let outcome = self
                .capture_and_commit_sanitized(
                    &capture,
                    crate::external_source_store::RuntimeSourceCaptureRequestV1 {
                        definition: task.definition().clone(),
                        binding: task.binding().clone(),
                        refresh: task.refresh().clone(),
                        provider_envelope: page.envelope.clone(),
                        authority:
                            crate::external_source_store::RuntimeSourceCaptureAuthorityV1::CanonicalRefetch(
                                authority,
                            ),
                        expected_frontier,
                        next_partition,
                        previous_whole_root_stage: task.whole_root_stage(),
                        mutations: page.mutations,
                        idempotency_key,
                        request_digest,
                    },
                )
                .await;
            if outcome.is_err() {
                return SourceCanonicalCommitOutcomeV1::Unavailable;
            }
            let projector = match canonical_projector(task.definition()) {
                Ok(projector) => projector,
                Err(_) => return SourceCanonicalCommitOutcomeV1::Unavailable,
            };
            if self
                .drain_projection_replay(Some(binding_identity), projector, 1, cancellation)
                .await
                .is_err()
            {
                return SourceCanonicalCommitOutcomeV1::Unavailable;
            }
            SourceCanonicalCommitOutcomeV1::Committed {
                coverage: page.envelope.coverage(),
                whole_root_stage,
            }
        })
    }
}

fn canonical_projector(
    definition: &SourceDefinitionV1,
) -> Result<ComponentVersion, ExternalSourceAcquisitionErrorV1> {
    let provider = definition
        .provider
        .as_str()
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '-'
            }
        })
        .collect::<String>();
    ComponentVersion::new(format!("projector.external-source.{provider}.v1"))
        .map_err(|_| ExternalSourceAcquisitionErrorV1::InvalidState)
}
