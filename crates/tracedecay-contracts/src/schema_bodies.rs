//! Whether a catalog contribution materializes reviewed JSON Schema bodies.
//!
//! Dispatch needs capability, binding, and schema-reference metadata. The
//! bodies are for SDK generation and MCP discovery. Building them walks every
//! request and result type through schemars, then canonicalizes and hashes
//! the document. A CLI process that only resolves a binding must not pay that.

use tracedecay_tool_catalog::{CatalogContributionV1, ExecutableSchemaAuthority};

use crate::ApplicationContractError;

/// Controls generation of executable JSON Schema bodies on one contribution.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SchemaBodyMaterialization {
    /// Generate the reviewed bodies owned by this contribution.
    Materialize,
    /// Leave `executable_schemas` empty. Schema references on the manifests stay.
    Omit,
}

/// Attach schema bodies only when `materialize` asks for them.
///
/// `schemas` runs only for [`SchemaBodyMaterialization::Materialize`], so an
/// omitted contribution never calls schemars.
pub(crate) fn attach_schema_bodies(
    contribution: CatalogContributionV1,
    materialize: SchemaBodyMaterialization,
    schemas: impl FnOnce(
        &CatalogContributionV1,
    ) -> Result<Vec<ExecutableSchemaAuthority>, ApplicationContractError>,
) -> Result<CatalogContributionV1, ApplicationContractError> {
    match materialize {
        SchemaBodyMaterialization::Omit => Ok(contribution),
        SchemaBodyMaterialization::Materialize => {
            let schemas = schemas(&contribution)?;
            Ok(contribution.with_executable_schemas(schemas)?)
        }
    }
}
