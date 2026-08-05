//! Runtime-store operations for the canonical external acquisition queue.

use std::sync::Arc;

use tracedecay_domain::{ManifestDigest, SourceBindingIdentityV1, UtcMicros, canonical_sha256};
use tracedecay_store::{
    ExternalSourceReadOperationV1, ExternalSourceReadResultV1, RepositoryReadResultV1,
    RepositoryWritePayloadV1, RuntimeReadCoverageV1, RuntimeReadResultV1, RuntimeSubmitOutcomeV1,
    SourceAcquisitionQueueCasV1, SourceAcquisitionQueueStateV1, StorageRuntimeReadPort,
};

use super::{
    ExternalSourceRuntimeProbe, RuntimeExternalSourceErrorV1, RuntimeExternalSourceStore, invalid,
    runtime_read_request, runtime_submit_request,
};

pub(crate) enum RuntimeAcquisitionCasOutcomeV1 {
    Committed,
    Conflict,
}

impl RuntimeExternalSourceStore {
    pub(crate) async fn read_acquisition_state(
        &self,
        binding: SourceBindingIdentityV1,
    ) -> Result<Option<SourceAcquisitionQueueStateV1>, RuntimeExternalSourceErrorV1> {
        self.read_acquisition_operation(ExternalSourceReadOperationV1::AcquisitionState { binding })
            .await
            .and_then(expect_acquisition_state)
    }

    pub(crate) async fn next_ready_acquisition(
        &self,
        now: UtcMicros,
    ) -> Result<Option<SourceAcquisitionQueueStateV1>, RuntimeExternalSourceErrorV1> {
        self.read_acquisition_operation(ExternalSourceReadOperationV1::NextReadyAcquisition { now })
            .await
            .and_then(expect_acquisition_state)
    }

    pub(crate) async fn acquisition_pending_count(
        &self,
    ) -> Result<usize, RuntimeExternalSourceErrorV1> {
        match self
            .read_acquisition_operation(ExternalSourceReadOperationV1::AcquisitionPendingCount)
            .await?
        {
            ExternalSourceReadResultV1::AcquisitionPendingCount(count) => {
                usize::try_from(count).map_err(invalid)
            }
            _ => Err(RuntimeExternalSourceErrorV1::Unavailable),
        }
    }

    pub(crate) async fn compare_and_swap_acquisition(
        &self,
        binding: SourceBindingIdentityV1,
        expected: Option<ManifestDigest>,
        next: SourceAcquisitionQueueStateV1,
    ) -> Result<RuntimeAcquisitionCasOutcomeV1, RuntimeExternalSourceErrorV1> {
        let command =
            SourceAcquisitionQueueCasV1::new(binding.clone(), expected.clone(), next.clone())
                .map_err(invalid)?;
        let idempotency_key = canonical_sha256(&(
            "tracedecay.external-source.acquisition-cas.v1",
            command.binding(),
            command.expected_state_digest(),
            command.next().state_digest(),
        ))
        .map_err(invalid)?;
        let payload =
            RepositoryWritePayloadV1::ExternalSourceAcquisition(Box::new(command.clone()));
        let request =
            runtime_submit_request(self.runtime.binding(), payload, &command, &idempotency_key)?;
        let probe = Arc::new(ExternalSourceRuntimeProbe::from_control(request.control()));
        let submitted = self
            .runtime
            .dispatch_submit_authorized(request, probe, self.authority.clone())
            .await;
        if matches!(
            submitted,
            Ok(RuntimeSubmitOutcomeV1::Committed { .. }
                | RuntimeSubmitOutcomeV1::CommittedAfterCancellation { .. }
                | RuntimeSubmitOutcomeV1::ExactReplay { .. })
        ) {
            return Ok(RuntimeAcquisitionCasOutcomeV1::Committed);
        }
        let current = self.read_acquisition_state(binding).await?;
        if current
            .as_ref()
            .map(SourceAcquisitionQueueStateV1::state_digest)
            != expected.as_ref()
        {
            Ok(RuntimeAcquisitionCasOutcomeV1::Conflict)
        } else {
            Err(RuntimeExternalSourceErrorV1::Unavailable)
        }
    }

    async fn read_acquisition_operation(
        &self,
        operation: ExternalSourceReadOperationV1,
    ) -> Result<ExternalSourceReadResultV1, RuntimeExternalSourceErrorV1> {
        let request = runtime_read_request(self.runtime.binding(), operation)?;
        let probe = ExternalSourceRuntimeProbe::from_control(request.control());
        let outcome = self
            .runtime
            .read(request, &probe)
            .await
            .map_err(|_| RuntimeExternalSourceErrorV1::Unavailable)?;
        if !matches!(
            outcome.coverage(),
            RuntimeReadCoverageV1::Latest { .. } | RuntimeReadCoverageV1::Complete { .. }
        ) {
            return Err(RuntimeExternalSourceErrorV1::Unavailable);
        }
        match outcome.value() {
            Some(RuntimeReadResultV1::Repository {
                result: RepositoryReadResultV1::ExternalSource(result),
            }) => Ok(result.clone()),
            _ => Err(RuntimeExternalSourceErrorV1::Unavailable),
        }
    }
}

fn expect_acquisition_state(
    result: ExternalSourceReadResultV1,
) -> Result<Option<SourceAcquisitionQueueStateV1>, RuntimeExternalSourceErrorV1> {
    match result {
        ExternalSourceReadResultV1::AcquisitionState(state) => {
            Ok(state.map(|state| state.as_ref().clone()))
        }
        _ => Err(RuntimeExternalSourceErrorV1::Unavailable),
    }
}
