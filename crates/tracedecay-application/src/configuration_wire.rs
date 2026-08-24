//! Concrete schema authority for mounted configuration wire bindings.
//!
//! The catalog owns operation identity and transport bindings. This module
//! verifies that each mounted configuration binding carries the request and
//! result schemas declared by its owning capability.

use std::collections::BTreeMap;

use serde::Serialize;
use tracedecay_tool_catalog::{
    BindingId, BindingSurface, CapabilityId, CapabilityManifestV1, CatalogValidationError,
    SchemaBodyAuthorityV1, SurfaceBindingV1,
};

/// Concrete request and result schema bodies for one configuration binding.
///
/// Executability is deliberately absent. The composition root joins schemas
/// with independently verified service and route availability.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ConfigurationWireSchemaV1 {
    capability_id: CapabilityId,
    binding_id: BindingId,
    surface: BindingSurface,
    request: SchemaBodyAuthorityV1,
    result: SchemaBodyAuthorityV1,
}

impl ConfigurationWireSchemaV1 {
    pub fn from_catalog(
        operation: &str,
        manifest: &CapabilityManifestV1,
        binding: &SurfaceBindingV1,
        request: SchemaBodyAuthorityV1,
        result: SchemaBodyAuthorityV1,
    ) -> Result<Self, CatalogValidationError> {
        if binding.capability_id() != manifest.capability_id()
            || manifest
                .binding_ids()
                .binary_search(binding.binding_id())
                .is_err()
            || binding.operation().as_str() != operation
            || request.schema_ref() != manifest.request_schema()
            || result.schema_ref() != manifest.result_schema()
        {
            return Err(CatalogValidationError::InvalidCapability {
                capability_id: manifest.capability_id().clone(),
                reason: "configuration wire schema authority does not match its catalog binding",
            });
        }
        Ok(Self {
            capability_id: manifest.capability_id().clone(),
            binding_id: binding.binding_id().clone(),
            surface: binding.surface(),
            request,
            result,
        })
    }

    pub fn capability_id(&self) -> &CapabilityId {
        &self.capability_id
    }

    pub fn binding_id(&self) -> &BindingId {
        &self.binding_id
    }

    pub const fn surface(&self) -> BindingSurface {
        self.surface
    }

    pub fn request(&self) -> &SchemaBodyAuthorityV1 {
        &self.request
    }

    pub fn result(&self) -> &SchemaBodyAuthorityV1 {
        &self.result
    }
}

/// Canonically ordered schema authority for mounted configuration bindings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigurationWireSchemaRegistryV1 {
    schemas: BTreeMap<BindingId, ConfigurationWireSchemaV1>,
}

impl ConfigurationWireSchemaRegistryV1 {
    pub fn new(schemas: Vec<ConfigurationWireSchemaV1>) -> Result<Self, CatalogValidationError> {
        let mut registry = BTreeMap::new();
        for schema in schemas {
            if registry
                .insert(schema.binding_id().clone(), schema)
                .is_some()
            {
                return Err(CatalogValidationError::DuplicateValue {
                    field: "configuration wire schema bindings",
                });
            }
        }
        Ok(Self { schemas: registry })
    }

    pub fn get(&self, binding_id: &BindingId) -> Option<&ConfigurationWireSchemaV1> {
        self.schemas.get(binding_id)
    }
}
