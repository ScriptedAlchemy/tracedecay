use std::collections::BTreeMap;

use tracedecay_domain::{CapabilityId as DomainCapabilityId, UtcMicros};
use tracedecay_policy::routing::{
    CapabilityAvailabilityV1, CapabilityEffectClassV1, ScopeMatchV1, TruthFreshnessRequirementV1,
    TruthSourceStateV1,
};
use tracedecay_tool_catalog::{
    ApplicationHandlerDescriptorV1 as CatalogHandlerDescriptor, ApplicationSurfaceOperation,
    CapabilityId, CatalogContributionV1, SchemaRef, ServiceId, UseCaseId,
};

use crate::error::ApplicationContractError;
use crate::policy::{
    PolicyEvaluationContextV1, PolicyEvaluationV1, PolicyEvaluatorCompositionV1,
    PolicyEvidenceHorizonV1,
};
use crate::result::ResultContractRef;

/// Closed application operation identity passed intact to the retained
/// canonical dispatcher after catalog resolution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApplicationOperation {
    capability_id: CapabilityId,
    use_case_id: UseCaseId,
    result_contract: ResultContractRef,
    resource_addressed: bool,
}

impl ApplicationOperation {
    pub fn new(
        capability_id: CapabilityId,
        use_case_id: UseCaseId,
        result_contract: ResultContractRef,
        resource_addressed: bool,
    ) -> Self {
        Self {
            capability_id,
            use_case_id,
            result_contract,
            resource_addressed,
        }
    }

    pub fn capability_id(&self) -> &CapabilityId {
        &self.capability_id
    }

    pub fn use_case_id(&self) -> &UseCaseId {
        &self.use_case_id
    }

    pub fn result_contract(&self) -> &ResultContractRef {
        &self.result_contract
    }

    #[hotpath::skip]
    pub const fn resource_addressed(&self) -> bool {
        self.resource_addressed
    }

    /// Evaluates this exact callable catalog/application operation through the
    /// retained capability-routing evaluator.
    ///
    /// `scope_match` and `required_effect_class` are supplied by the caller on
    /// purpose. Deriving them here — asserting `ScopeMatchV1::Match` and reading
    /// the effect class off the candidate being tested — makes the evaluator's
    /// scope-mismatch and effect-class gates compare a value against itself, so
    /// they can never reject anything.
    #[allow(clippy::too_many_arguments)]
    pub fn evaluate_local_live_policy(
        &self,
        composition: &PolicyEvaluatorCompositionV1,
        context: &PolicyEvaluationContextV1,
        runtime_availability: CapabilityAvailabilityV1,
        scope_match: ScopeMatchV1,
        truth_source_state: TruthSourceStateV1,
        required_effect_class: CapabilityEffectClassV1,
        required_freshness: TruthFreshnessRequirementV1,
        evidence_horizon: PolicyEvidenceHorizonV1,
        evaluated_at: UtcMicros,
    ) -> Result<
        PolicyEvaluationV1<tracedecay_policy::routing::CapabilityRoutingDecisionV1>,
        ApplicationContractError,
    > {
        let candidate = composition.candidate(
            self.capability_id.as_str(),
            runtime_availability,
            scope_match,
            truth_source_state,
        )?;
        let capability_id = DomainCapabilityId::new(self.capability_id.as_str().to_owned())?;
        let request = composition.routing_request(
            context,
            &self.use_case_id,
            vec![capability_id],
            vec![candidate],
            required_effect_class,
            required_freshness,
            evaluated_at,
        )?;
        composition.route_local_live(context, &request, evidence_horizon)
    }
}

/// One canonical dispatcher can implement this trait for each typed request it
/// accepts. The catalog never erases requests through JSON or `Any`.
pub trait CanonicalApplicationDispatcher<Request> {
    type Output;

    fn invoke(&self, operation: &ApplicationOperation, request: Request) -> Self::Output;
}

/// A resolved application handler bound to the one canonical dispatcher
/// retained by `tracedecay-daemon-service`.
pub struct BoundApplicationHandler<'a, Dispatcher> {
    descriptor: &'a ApplicationHandlerDescriptor,
    dispatcher: &'a Dispatcher,
}

impl<'a, Dispatcher> BoundApplicationHandler<'a, Dispatcher> {
    fn new(descriptor: &'a ApplicationHandlerDescriptor, dispatcher: &'a Dispatcher) -> Self {
        Self {
            descriptor,
            dispatcher,
        }
    }

    pub fn operation(&self) -> &ApplicationOperation {
        self.descriptor.operation()
    }

    pub fn request_schema(&self) -> &SchemaRef {
        self.descriptor.request_schema()
    }

    pub fn result_schema(&self) -> &SchemaRef {
        self.descriptor.result_schema()
    }

    pub fn invoke<Request>(
        &self,
        request: Request,
    ) -> <Dispatcher as CanonicalApplicationDispatcher<Request>>::Output
    where
        Dispatcher: CanonicalApplicationDispatcher<Request>,
    {
        self.dispatcher.invoke(self.descriptor.operation(), request)
    }
}

/// Proof that one concrete application use case owns a request/result schema
/// pair and can be bound to the canonical dispatcher that
/// `tracedecay-daemon-service` binds and the composition root mounts.
///
/// Canonical public operations also retain their typed surface identity and
/// execution service here so MCP, HTTP, SDK, and dispatch projections do not
/// restate those facts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApplicationHandlerDescriptor {
    surface_operation: Option<ApplicationSurfaceOperation>,
    service_id: Option<ServiceId>,
    operation: ApplicationOperation,
    request_schema: SchemaRef,
    result_schema: SchemaRef,
}

impl ApplicationHandlerDescriptor {
    pub fn new(
        operation: ApplicationOperation,
        request_schema: SchemaRef,
        result_schema: SchemaRef,
    ) -> Result<Self, ApplicationContractError> {
        if ResultContractRef::from_schema(&result_schema) != operation.result_contract().clone() {
            return Err(ApplicationContractError::Inconsistent {
                field: "application handler result schema",
            });
        }
        Ok(Self {
            surface_operation: None,
            service_id: None,
            operation,
            request_schema,
            result_schema,
        })
    }

    pub fn for_catalog_operation(
        catalog_operation: &str,
        service_id: &str,
        operation: ApplicationOperation,
        request_schema: SchemaRef,
        result_schema: SchemaRef,
    ) -> Result<Self, ApplicationContractError> {
        let mut descriptor = Self::new(operation, request_schema, result_schema)?;
        descriptor.surface_operation =
            ApplicationSurfaceOperation::from_catalog_name(catalog_operation);
        if descriptor.surface_operation.is_some() {
            descriptor.service_id = Some(ServiceId::new(service_id)?);
        }
        Ok(descriptor)
    }

    pub const fn surface_operation(&self) -> Option<ApplicationSurfaceOperation> {
        self.surface_operation
    }

    pub fn service_id(&self) -> Option<&ServiceId> {
        self.service_id.as_ref()
    }

    pub fn operation(&self) -> &ApplicationOperation {
        &self.operation
    }

    pub fn request_schema(&self) -> &SchemaRef {
        &self.request_schema
    }

    pub fn result_schema(&self) -> &SchemaRef {
        &self.result_schema
    }

    pub fn bind<'a, Dispatcher>(
        &'a self,
        dispatcher: &'a Dispatcher,
    ) -> BoundApplicationHandler<'a, Dispatcher> {
        BoundApplicationHandler::new(self, dispatcher)
    }

    pub fn catalog_descriptor(&self) -> Result<CatalogHandlerDescriptor, ApplicationContractError> {
        Ok(CatalogHandlerDescriptor::new(
            self.operation.capability_id().clone(),
            self.operation.use_case_id().clone(),
            self.request_schema.clone(),
            self.result_schema.clone(),
        ))
    }
}

/// Closed set of handler descriptors supplied to [`crate::catalog_composition`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ApplicationHandlerDescriptors {
    descriptors: BTreeMap<UseCaseId, ApplicationHandlerDescriptor>,
    surface_operations: BTreeMap<ApplicationSurfaceOperation, UseCaseId>,
}

impl ApplicationHandlerDescriptors {
    pub fn new(
        descriptors: impl IntoIterator<Item = ApplicationHandlerDescriptor>,
    ) -> Result<Self, ApplicationContractError> {
        let mut indexed = BTreeMap::new();
        let mut surface_operations = BTreeMap::new();
        for descriptor in descriptors {
            let use_case_id = descriptor.operation.use_case_id().clone();
            if let Some(surface_operation) = descriptor.surface_operation()
                && surface_operations
                    .insert(surface_operation, use_case_id.clone())
                    .is_some()
            {
                return Err(ApplicationContractError::Duplicate {
                    field: "application surface operation",
                });
            }
            if indexed.insert(use_case_id, descriptor).is_some() {
                return Err(ApplicationContractError::Duplicate {
                    field: "application handler use case",
                });
            }
        }
        Ok(Self {
            descriptors: indexed,
            surface_operations,
        })
    }

    pub fn get(&self, use_case_id: &UseCaseId) -> Option<&ApplicationHandlerDescriptor> {
        self.descriptors.get(use_case_id)
    }

    pub fn iter(&self) -> impl Iterator<Item = &ApplicationHandlerDescriptor> {
        self.descriptors.values()
    }

    pub fn for_surface_operation(
        &self,
        operation: ApplicationSurfaceOperation,
    ) -> Option<&ApplicationHandlerDescriptor> {
        self.surface_operations
            .get(&operation)
            .and_then(|use_case_id| self.descriptors.get(use_case_id))
    }

    pub fn surface_operations(
        &self,
    ) -> impl Iterator<Item = (ApplicationSurfaceOperation, &ApplicationHandlerDescriptor)> {
        self.surface_operations
            .iter()
            .filter_map(|(operation, use_case_id)| {
                self.descriptors
                    .get(use_case_id)
                    .map(|descriptor| (*operation, descriptor))
            })
    }

    pub fn catalog_descriptors(
        &self,
    ) -> Result<Vec<CatalogHandlerDescriptor>, ApplicationContractError> {
        self.descriptors
            .values()
            .map(ApplicationHandlerDescriptor::catalog_descriptor)
            .collect()
    }

    /// Verifies the application-owned, bidirectional use-case/schema mapping.
    /// Capability, effect, scope, privacy, and availability remain catalog-owned
    /// metadata; copying them into these descriptors would make validation
    /// circular.
    pub fn validate_against(
        &self,
        contributions: &[CatalogContributionV1],
    ) -> Result<(), ApplicationContractError> {
        let mut capabilities = BTreeMap::new();
        for capability in contributions
            .iter()
            .flat_map(|contribution| contribution.capabilities())
        {
            if capabilities
                .insert(capability.use_case_id().clone(), capability)
                .is_some()
            {
                return Err(ApplicationContractError::Duplicate {
                    field: "application catalog use case",
                });
            }
        }

        for descriptor in self.iter() {
            let operation = descriptor.operation();
            let Some(capability) = capabilities.get(operation.use_case_id()) else {
                return Err(ApplicationContractError::Inconsistent {
                    field: "application handler use case",
                });
            };
            validate_descriptor_mapping(descriptor, capability)?;
        }

        for capability in capabilities.values() {
            let Some(descriptor) = self.get(capability.use_case_id()) else {
                return Err(ApplicationContractError::Inconsistent {
                    field: "application capability handler mapping",
                });
            };
            validate_descriptor_mapping(descriptor, capability)?;
        }

        Ok(())
    }
}

fn validate_descriptor_mapping(
    descriptor: &ApplicationHandlerDescriptor,
    capability: &tracedecay_tool_catalog::CapabilityManifestV1,
) -> Result<(), ApplicationContractError> {
    let operation = descriptor.operation();
    if operation.capability_id() != capability.capability_id()
        || operation.use_case_id() != capability.use_case_id()
    {
        return Err(ApplicationContractError::Inconsistent {
            field: "application capability/use-case mapping",
        });
    }
    if descriptor.request_schema() != capability.request_schema()
        || descriptor.result_schema() != capability.result_schema()
        || operation.result_contract()
            != &ResultContractRef::from_schema(capability.result_schema())
    {
        return Err(ApplicationContractError::Inconsistent {
            field: "application capability schema mapping",
        });
    }
    Ok(())
}

/// Application-owned descriptor source. [`crate::catalog_composition`]
/// validates these descriptors against the catalog contributions;
/// `tracedecay-daemon-service` binds the canonical dispatcher and the
/// composition root mounts the result.
pub fn application_handler_descriptors()
-> Result<ApplicationHandlerDescriptors, ApplicationContractError> {
    let mut descriptors = vec![crate::retrieval::catalog::symbol_search_handler_descriptor()?];
    descriptors.extend(crate::retrieval::catalog::primitive_read_handler_descriptors()?);
    descriptors.extend(crate::retrieval::callable_code_handler_descriptors()?);
    descriptors.extend(crate::git::git_index_handler_descriptors()?);
    descriptors.extend(crate::git::git_surface_handler_descriptors()?);
    descriptors.extend(crate::git::native_integration_surface_handler_descriptors()?);
    descriptors.extend(crate::configuration::configuration_surface_handler_descriptors()?);
    descriptors.extend(crate::context_scout::context_scout_surface_handler_descriptors()?);
    descriptors.extend(crate::feedback::feedback_surface_handler_descriptors()?);
    descriptors.extend(crate::lsp_context_catalog::lsp_context_handler_descriptors()?);
    descriptors.push(crate::observatory_surface::observatory_read_handler_descriptor()?);
    descriptors.extend(crate::retained_surfaces::retained_surface_handler_descriptors()?);
    descriptors.extend(crate::source_edit::source_edit_handler_descriptors()?);
    let descriptors = ApplicationHandlerDescriptors::new(descriptors)?;
    if ApplicationSurfaceOperation::ALL
        .into_iter()
        .any(|operation| descriptors.for_surface_operation(operation).is_none())
    {
        return Err(ApplicationContractError::Inconsistent {
            field: "application surface handler set",
        });
    }
    Ok(descriptors)
}
