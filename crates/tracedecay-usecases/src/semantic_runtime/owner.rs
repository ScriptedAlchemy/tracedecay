use super::ports::{
    SemanticActivationCommandV1, SemanticActivationReceiptV1, SemanticActivationRequestV1,
    SemanticConfigurationPinV1, SemanticConfigurationSnapshotSourceV1, SemanticFallbackReasonV1,
    SemanticRollbackCommandV1, SemanticRollbackReceiptV1, SemanticRollbackRequestV1,
    SemanticRuntimeBackendErrorV1, SemanticRuntimeBackendV1, SemanticRuntimeControlErrorV1,
    SemanticRuntimeFuture, SemanticRuntimeIntegrationPortV1, SemanticRuntimeStateV1,
    SemanticRuntimeStatusV1,
};

/// Application owner for semantic activation, status, and rollback.
///
/// Configuration remains authoritative through its existing snapshot port,
/// while the backend remains authoritative for artifact checks, indexing, and
/// active/rollback pointer CAS. This owner only publishes semantic routing
/// after it observes the exact activation receipt as current.
pub struct SemanticRuntimeOwnerV1<C, R> {
    configuration: C,
    runtime: R,
}

impl<C, R> SemanticRuntimeOwnerV1<C, R> {
    pub fn new(configuration: C, runtime: R) -> Self {
        Self {
            configuration,
            runtime,
        }
    }

    pub fn configuration(&self) -> &C {
        &self.configuration
    }

    pub fn runtime(&self) -> &R {
        &self.runtime
    }
}

impl<C, R> SemanticRuntimeOwnerV1<C, R>
where
    C: SemanticConfigurationSnapshotSourceV1,
    R: SemanticRuntimeBackendV1,
{
    pub async fn status(&self) -> SemanticRuntimeStatusV1 {
        let Ok(configuration) = self.configuration_pin().await else {
            return unavailable_status(SemanticFallbackReasonV1::ConfigurationUnavailable);
        };
        let Ok(state) = self.runtime.status(&configuration).await else {
            return SemanticRuntimeStatusV1::new(
                Some(configuration),
                SemanticRuntimeStateV1::Unavailable {
                    reason: SemanticFallbackReasonV1::RuntimeUnavailable,
                },
            );
        };
        let status = SemanticRuntimeStatusV1::new(Some(configuration.clone()), state);
        if status.validate().is_err() {
            return SemanticRuntimeStatusV1::new(
                Some(configuration),
                SemanticRuntimeStateV1::Degraded {
                    active_generation: None,
                    reason: SemanticFallbackReasonV1::InvalidRuntimeStatus,
                },
            );
        }
        status
    }

    pub async fn activate(
        &self,
        request: SemanticActivationRequestV1,
    ) -> Result<SemanticActivationReceiptV1, SemanticRuntimeControlErrorV1> {
        request
            .validate()
            .map_err(|_| SemanticRuntimeControlErrorV1::InvalidRequest)?;
        let configuration = self.configuration_pin().await?;
        let command = SemanticActivationCommandV1::new(configuration.clone(), request)
            .map_err(|_| SemanticRuntimeControlErrorV1::InvalidRequest)?;
        let receipt = self
            .runtime
            .activate(&command)
            .await
            .map_err(map_backend_error)?;
        receipt
            .validate_for(&command)
            .map_err(|_| SemanticRuntimeControlErrorV1::InvalidReceipt)?;
        let observed = self
            .runtime
            .status(&receipt.configuration)
            .await
            .map_err(map_backend_error)?;
        let current_configuration = self.configuration_pin().await?;
        if current_configuration != receipt.configuration
            || !matches!(
                observed,
                SemanticRuntimeStateV1::Current {
                    receipt: ref current
                } if current == &receipt
            )
        {
            return Err(SemanticRuntimeControlErrorV1::PromotionNotObserved);
        }
        Ok(receipt)
    }

    pub async fn rollback(
        &self,
        request: SemanticRollbackRequestV1,
    ) -> Result<SemanticRollbackReceiptV1, SemanticRuntimeControlErrorV1> {
        request
            .validate()
            .map_err(|_| SemanticRuntimeControlErrorV1::InvalidRequest)?;
        let configuration = self.configuration_pin().await?;
        let command = SemanticRollbackCommandV1::new(configuration.clone(), request)
            .map_err(|_| SemanticRuntimeControlErrorV1::InvalidRequest)?;
        let receipt = self
            .runtime
            .rollback(&command)
            .await
            .map_err(map_backend_error)?;
        receipt
            .validate_for(&command)
            .map_err(|_| SemanticRuntimeControlErrorV1::InvalidReceipt)?;
        let observed = self
            .runtime
            .status(&receipt.configuration)
            .await
            .map_err(map_backend_error)?;
        let current_configuration = self.configuration_pin().await?;
        let promotion_observed = match (&receipt.restored_activation, &observed) {
            (Some(restored), SemanticRuntimeStateV1::Current { receipt: current }) => {
                current == restored
            }
            (
                None,
                SemanticRuntimeStateV1::Unavailable {
                    reason: SemanticFallbackReasonV1::ArtifactUnavailable,
                },
            ) => true,
            _ => false,
        };
        if current_configuration != receipt.configuration || !promotion_observed {
            return Err(SemanticRuntimeControlErrorV1::PromotionNotObserved);
        }
        Ok(receipt)
    }

    async fn configuration_pin(
        &self,
    ) -> Result<SemanticConfigurationPinV1, SemanticRuntimeControlErrorV1> {
        let current = self
            .configuration
            .current_configuration()
            .await
            .map_err(|_| SemanticRuntimeControlErrorV1::ConfigurationUnavailable)?;
        SemanticConfigurationPinV1::from_current(&current)
            .map_err(|_| SemanticRuntimeControlErrorV1::ConfigurationUnavailable)
    }
}

impl<C, R> SemanticRuntimeIntegrationPortV1 for SemanticRuntimeOwnerV1<C, R>
where
    C: SemanticConfigurationSnapshotSourceV1,
    R: SemanticRuntimeBackendV1,
{
    fn status(&self) -> SemanticRuntimeFuture<'_, SemanticRuntimeStatusV1> {
        Box::pin(async move { SemanticRuntimeOwnerV1::status(self).await })
    }

    fn activate(
        &self,
        request: SemanticActivationRequestV1,
    ) -> SemanticRuntimeFuture<'_, Result<SemanticActivationReceiptV1, SemanticRuntimeControlErrorV1>>
    {
        Box::pin(async move { SemanticRuntimeOwnerV1::activate(self, request).await })
    }

    fn rollback(
        &self,
        request: SemanticRollbackRequestV1,
    ) -> SemanticRuntimeFuture<'_, Result<SemanticRollbackReceiptV1, SemanticRuntimeControlErrorV1>>
    {
        Box::pin(async move { SemanticRuntimeOwnerV1::rollback(self, request).await })
    }
}

fn unavailable_status(reason: SemanticFallbackReasonV1) -> SemanticRuntimeStatusV1 {
    SemanticRuntimeStatusV1::new(None, SemanticRuntimeStateV1::Unavailable { reason })
}

fn map_backend_error(error: SemanticRuntimeBackendErrorV1) -> SemanticRuntimeControlErrorV1 {
    match error {
        SemanticRuntimeBackendErrorV1::Unavailable => {
            SemanticRuntimeControlErrorV1::RuntimeUnavailable
        }
        SemanticRuntimeBackendErrorV1::Rejected => SemanticRuntimeControlErrorV1::Rejected,
        SemanticRuntimeBackendErrorV1::Conflict => SemanticRuntimeControlErrorV1::Conflict,
    }
}
