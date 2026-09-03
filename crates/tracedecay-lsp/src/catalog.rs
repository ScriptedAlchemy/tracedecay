use std::collections::BTreeSet;
use std::sync::{Arc, LazyLock};

use thiserror::Error;
use tracedecay_application::{
    ApplicationContractError, ApplicationHandlerDescriptors, application_catalog_contributions,
    application_handler_descriptors,
};
use tracedecay_tool_catalog::{
    BindingId, BindingStatus, BindingSurface, CatalogContributionV1, SurfaceBindingV1,
};

use crate::dispatch::LspClientMethod;

static APPLICATION_LSP_CATALOG: LazyLock<Result<LspCatalogAdmission, LspCatalogAdmissionError>> =
    LazyLock::new(|| {
        let contributions = application_catalog_contributions()?;
        let handlers = application_handler_descriptors()?;
        LspCatalogAdmission::from_parts(&contributions, &handlers)
    });

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub(super) enum LspCatalogAdmissionError {
    #[error("application LSP catalog is unavailable: {0}")]
    Application(String),
    #[error("application LSP catalog declares duplicate operation {0}")]
    DuplicateOperation(String),
    #[error("application LSP catalog declares unroutable operation {0}")]
    UnroutableOperation(String),
    #[error("application LSP catalog has no executable handler for operation {0}")]
    BindingUnavailable(String),
}

impl From<ApplicationContractError> for LspCatalogAdmissionError {
    fn from(error: ApplicationContractError) -> Self {
        Self::Application(error.to_string())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum LspCatalogBindingRejection {
    Missing,
    Stale,
    CapabilityUnavailable,
    HandlerUnavailable,
}

#[derive(Clone, Debug)]
pub(super) struct LspCatalogAdmission {
    contributions: Arc<[CatalogContributionV1]>,
    handlers: Arc<ApplicationHandlerDescriptors>,
}

impl LspCatalogAdmission {
    pub(super) fn from_application_catalog() -> Result<Self, LspCatalogAdmissionError> {
        APPLICATION_LSP_CATALOG.clone()
    }

    pub(super) fn from_parts(
        contributions: &[CatalogContributionV1],
        handlers: &ApplicationHandlerDescriptors,
    ) -> Result<Self, LspCatalogAdmissionError> {
        handlers.validate_against(contributions)?;
        let mut operations = BTreeSet::new();
        for (contribution, binding) in contributions.iter().flat_map(|contribution| {
            contribution
                .bindings()
                .iter()
                .filter(|binding| binding.surface() == BindingSurface::Lsp)
                .map(move |binding| (contribution, binding))
        }) {
            let operation = binding.operation().as_str();
            if !LspClientMethod::is_catalog_routable_operation(operation) {
                return Err(LspCatalogAdmissionError::UnroutableOperation(
                    operation.to_owned(),
                ));
            }
            if !operations.insert(operation.to_owned()) {
                return Err(LspCatalogAdmissionError::DuplicateOperation(
                    operation.to_owned(),
                ));
            }
            if matches!(binding.status(), BindingStatus::Current)
                && !binding.is_alias()
                && binding.protocol_revisions().contains(1)
                && admit_binding(contribution, binding, handlers).is_err()
            {
                return Err(LspCatalogAdmissionError::BindingUnavailable(
                    operation.to_owned(),
                ));
            }
        }
        Ok(Self {
            contributions: Arc::<[CatalogContributionV1]>::from(contributions),
            handlers: Arc::new(handlers.clone()),
        })
    }

    pub(super) fn binding(
        &self,
        operation: &str,
    ) -> Result<&BindingId, LspCatalogBindingRejection> {
        let mut matches = self.contributions.iter().flat_map(|contribution| {
            contribution
                .bindings()
                .iter()
                .filter(move |binding| {
                    binding.surface() == BindingSurface::Lsp
                        && binding.operation().as_str() == operation
                })
                .map(move |binding| (contribution, binding))
        });
        let (contribution, binding) = matches.next().ok_or(LspCatalogBindingRejection::Missing)?;
        if matches.next().is_some() {
            return Err(LspCatalogBindingRejection::CapabilityUnavailable);
        }
        admit_binding(contribution, binding, &self.handlers).map(|()| binding.binding_id())
    }
}

fn admit_binding(
    contribution: &CatalogContributionV1,
    binding: &SurfaceBindingV1,
    handlers: &ApplicationHandlerDescriptors,
) -> Result<(), LspCatalogBindingRejection> {
    if !matches!(binding.status(), BindingStatus::Current)
        || binding.is_alias()
        || !binding.protocol_revisions().contains(1)
    {
        return Err(LspCatalogBindingRejection::Stale);
    }
    let capability = contribution
        .capabilities()
        .iter()
        .find(|capability| capability.capability_id() == binding.capability_id())
        .filter(|capability| {
            capability.availability().is_callable()
                && capability.binding_ids().contains(binding.binding_id())
        })
        .ok_or(LspCatalogBindingRejection::CapabilityUnavailable)?;
    handlers
        .get(capability.use_case_id())
        .filter(|handler| {
            handler.operation().capability_id() == capability.capability_id()
                && handler.operation().use_case_id() == capability.use_case_id()
                && handler.request_schema() == capability.request_schema()
                && handler.result_schema() == capability.result_schema()
        })
        .ok_or(LspCatalogBindingRejection::HandlerUnavailable)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use tracedecay_application::{
        application_catalog_contributions, application_handler_descriptors,
    };
    use tracedecay_tool_catalog::{
        BindingDeprecation, BindingStatus, BindingSurface, CatalogContributionInputV1,
        CatalogContributionV1, SurfaceBindingInputV1, SurfaceBindingV1,
    };

    use super::{LspCatalogAdmission, LspCatalogBindingRejection};

    const CONTEXT_METHOD: &str = "tracedecay/context";

    #[test]
    fn current_context_binding_is_admitted() {
        let admission = LspCatalogAdmission::from_application_catalog().unwrap();

        assert_eq!(
            admission.binding(CONTEXT_METHOD).unwrap().as_str(),
            "binding.lsp.context.v1"
        );
    }

    #[test]
    fn missing_context_binding_is_rejected() {
        let contributions = context_binding_fixture(None);
        let handlers = application_handler_descriptors().unwrap();
        let admission = LspCatalogAdmission::from_parts(&contributions, &handlers).unwrap();

        assert_eq!(
            admission.binding(CONTEXT_METHOD),
            Err(LspCatalogBindingRejection::Missing)
        );
    }

    #[test]
    fn stale_context_binding_is_rejected() {
        let contributions = context_binding_fixture(Some(BindingStatus::Deprecated {
            deprecation: BindingDeprecation::new(2).unwrap(),
        }));
        let handlers = application_handler_descriptors().unwrap();
        let admission = LspCatalogAdmission::from_parts(&contributions, &handlers).unwrap();

        assert_eq!(
            admission.binding(CONTEXT_METHOD),
            Err(LspCatalogBindingRejection::Stale)
        );
    }

    fn context_binding_fixture(status: Option<BindingStatus>) -> Vec<CatalogContributionV1> {
        let mut contributions = application_catalog_contributions().unwrap();
        let context = contributions
            .iter_mut()
            .find(|contribution| {
                contribution.contribution_id().as_str() == "contribution.application.lsp-context"
            })
            .unwrap();
        let mut bindings = context.bindings().to_vec();
        let index = bindings
            .iter()
            .position(|binding| binding.operation().as_str() == CONTEXT_METHOD)
            .unwrap();
        if let Some(status) = status {
            let binding = &bindings[index];
            bindings[index] = SurfaceBindingV1::new(SurfaceBindingInputV1 {
                binding_id: binding.binding_id().clone(),
                capability_id: binding.capability_id().clone(),
                surface: BindingSurface::Lsp,
                operation: binding.operation().clone(),
                protocol_revisions: binding.protocol_revisions().clone(),
                required_features: binding.required_features().to_vec(),
                status,
                alias_of: binding.alias_of().cloned(),
            })
            .unwrap();
        } else {
            bindings.remove(index);
        }
        *context = CatalogContributionV1::new(CatalogContributionInputV1 {
            contribution_id: context.contribution_id().clone(),
            depends_on: context.depends_on().to_vec(),
            capabilities: context.capabilities().to_vec(),
            retrieval_primitives: context.retrieval_primitives().to_vec(),
            bindings,
        })
        .unwrap();
        contributions
    }
}
