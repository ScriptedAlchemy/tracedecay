//! Daemon-owned `SemanticRuntimeBackendV1` over the production runtime handle.

use std::sync::Mutex;

use tracedecay_query::retrieval::semantic::SemanticIndexStateV1;
use tracedecay_semantic::DaemonSemanticRuntimeHandleV1;
use tracedecay_semantic_contracts::SemanticRuntimeScheduleStatusV1;

use super::super::ports::{
    SemanticActivationCommandV1, SemanticActivationReceiptV1, SemanticConfigurationPinV1,
    SemanticRollbackCommandV1, SemanticRollbackReceiptV1, SemanticRuntimeBackendErrorV1,
    SemanticRuntimeBackendV1, SemanticRuntimeFuture, SemanticRuntimeStateV1,
    SemanticRuntimeStatusV1,
};
use super::ProductionSemanticRuntimeV1;
use super::application_status::application_status_from_projection;

/// Daemon backend that surfaces schedule projection through the application port.
pub struct DaemonSemanticRuntimeBackendV1 {
    handle: DaemonSemanticRuntimeHandleV1,
    configuration: Mutex<Option<SemanticConfigurationPinV1>>,
}

impl DaemonSemanticRuntimeBackendV1 {
    #[cfg(test)]
    pub fn new(handle: DaemonSemanticRuntimeHandleV1) -> Self {
        Self {
            handle,
            configuration: Mutex::new(None),
        }
    }

    pub fn from_production(runtime: ProductionSemanticRuntimeV1) -> Self {
        Self {
            handle: runtime.handle.clone(),
            configuration: Mutex::new(None),
        }
    }

    pub fn bind_configuration(&self, pin: SemanticConfigurationPinV1) {
        *self
            .configuration
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(pin);
    }

    pub fn application_status(&self) -> SemanticRuntimeStatusV1 {
        self.application_status_with_receipt(None)
    }

    pub(super) fn application_status_with_receipt(
        &self,
        activation_receipt: Option<SemanticActivationReceiptV1>,
    ) -> SemanticRuntimeStatusV1 {
        let configuration = self
            .configuration
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        application_status_from_projection(
            &self.handle.status_projection(),
            configuration,
            activation_receipt,
        )
    }
}

impl SemanticRuntimeBackendV1 for DaemonSemanticRuntimeBackendV1 {
    fn status<'a>(
        &'a self,
        configuration: &'a SemanticConfigurationPinV1,
    ) -> SemanticRuntimeFuture<'a, Result<SemanticRuntimeStateV1, SemanticRuntimeBackendErrorV1>>
    {
        Box::pin(async move {
            self.bind_configuration(configuration.clone());
            Ok(self.application_status().state)
        })
    }

    fn activate<'a>(
        &'a self,
        command: &'a SemanticActivationCommandV1,
    ) -> SemanticRuntimeFuture<'a, Result<SemanticActivationReceiptV1, SemanticRuntimeBackendErrorV1>>
    {
        Box::pin(async move {
            self.bind_configuration(command.configuration.clone());
            Err(SemanticRuntimeBackendErrorV1::Unavailable)
        })
    }

    fn rollback<'a>(
        &'a self,
        command: &'a SemanticRollbackCommandV1,
    ) -> SemanticRuntimeFuture<'a, Result<SemanticRollbackReceiptV1, SemanticRuntimeBackendErrorV1>>
    {
        Box::pin(async move {
            self.bind_configuration(command.configuration.clone());
            Err(SemanticRuntimeBackendErrorV1::Unavailable)
        })
    }
}

pub(super) fn index_state_from_status(
    status: SemanticRuntimeScheduleStatusV1,
) -> SemanticIndexStateV1 {
    match status {
        SemanticRuntimeScheduleStatusV1::Unavailable => SemanticIndexStateV1::Unavailable,
        SemanticRuntimeScheduleStatusV1::Indexing { .. } => SemanticIndexStateV1::Indexing,
        SemanticRuntimeScheduleStatusV1::Failed { .. } => SemanticIndexStateV1::Failed,
        SemanticRuntimeScheduleStatusV1::Current { .. } => SemanticIndexStateV1::Incompatible,
    }
}
