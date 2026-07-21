use std::collections::BTreeMap;

use tracedecay_tool_catalog::{
    ApplicationHandlerDescriptorV1 as CatalogHandlerDescriptor, CapabilityId,
    CatalogContributionV1, SchemaRef, UseCaseId,
};

use crate::error::ApplicationContractError;
use crate::result::ResultContractRef;

/// Closed application operation identity. It is validation metadata only and
/// contains no invocation callback, registry, or transport dispatch.
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

    pub const fn resource_addressed(&self) -> bool {
        self.resource_addressed
    }
}

/// Validation-only proof that one concrete application use case owns a
/// request/result schema pair.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApplicationHandlerDescriptor {
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
            operation,
            request_schema,
            result_schema,
        })
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

    pub fn catalog_descriptor(&self) -> Result<CatalogHandlerDescriptor, ApplicationContractError> {
        Ok(CatalogHandlerDescriptor::new(
            self.operation.use_case_id().clone(),
            self.request_schema.clone(),
            self.result_schema.clone(),
        ))
    }
}

/// Closed set of validation-only handler descriptors supplied to future root
/// catalog composition.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ApplicationHandlerDescriptors {
    descriptors: BTreeMap<UseCaseId, ApplicationHandlerDescriptor>,
}

impl ApplicationHandlerDescriptors {
    pub fn new(
        descriptors: impl IntoIterator<Item = ApplicationHandlerDescriptor>,
    ) -> Result<Self, ApplicationContractError> {
        let mut indexed = BTreeMap::new();
        for descriptor in descriptors {
            let use_case_id = descriptor.operation.use_case_id().clone();
            if indexed.insert(use_case_id, descriptor).is_some() {
                return Err(ApplicationContractError::Duplicate {
                    field: "application handler use case",
                });
            }
        }
        Ok(Self {
            descriptors: indexed,
        })
    }

    pub fn get(&self, use_case_id: &UseCaseId) -> Option<&ApplicationHandlerDescriptor> {
        self.descriptors.get(use_case_id)
    }

    pub fn iter(&self) -> impl Iterator<Item = &ApplicationHandlerDescriptor> {
        self.descriptors.values()
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
    if operation.use_case_id() != capability.use_case_id() {
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

/// Application-owned descriptor source. Root catalog composition remains
/// intentionally outside this crate and is introduced by its owning packet.
pub fn application_handler_descriptors()
-> Result<ApplicationHandlerDescriptors, ApplicationContractError> {
    let mut descriptors = vec![crate::retrieval::catalog::symbol_search_handler_descriptor()?];
    descriptors.extend(crate::git::git_index_handler_descriptors()?);
    descriptors.extend(crate::git::git_surface_handler_descriptors()?);
    descriptors.extend(crate::feedback::feedback_surface_handler_descriptors()?);
    ApplicationHandlerDescriptors::new(descriptors)
}
