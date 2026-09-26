use std::collections::BTreeSet;
use std::sync::{LazyLock, OnceLock};

use tracedecay_contracts::{
    APPLICATION_DEFAULT_PROFILE_ID, RetainedSurfaceOperation,
    retained_surface_application_operation,
};
use tracedecay_tool_catalog::{BindingSurface, ProfileId, SurfaceOperationName};

use tracedecay_contracts::catalog_composition::{
    ApplicationCatalogComposition, compose_application_catalog,
};
use tracedecay_domain::errors::{Result, TraceDecayError};

static RETAINED_MCP_COMPOSITION: OnceLock<
    std::result::Result<ApplicationCatalogComposition<()>, String>,
> = OnceLock::new();
const RETAINED_OPERATION_COUNT: usize = RetainedSurfaceOperation::ALL.len();
type RetainedMcpBindingCache =
    [OnceLock<std::result::Result<RetainedMcpBindingContract, String>>; RETAINED_OPERATION_COUNT];
static RETAINED_MCP_BINDINGS: LazyLock<RetainedMcpBindingCache> =
    LazyLock::new(|| std::array::from_fn(|_| OnceLock::new()));

pub(super) struct RetainedMcpBindingContract {
    maximum_millis: u64,
}

impl RetainedMcpBindingContract {
    pub(super) fn maximum_millis(&self) -> u64 {
        self.maximum_millis
    }
}

fn retained_catalog_error(error: impl std::fmt::Display) -> TraceDecayError {
    TraceDecayError::Config {
        message: format!("retained application catalog is unavailable: {error}"),
    }
}

fn retained_mcp_composition() -> Result<&'static ApplicationCatalogComposition<()>> {
    RETAINED_MCP_COMPOSITION
        .get_or_init(|| compose_application_catalog(()).map_err(|error| error.to_string()))
        .as_ref()
        .map_err(retained_catalog_error)
}

fn resolve_retained_mcp_binding(
    operation: RetainedSurfaceOperation,
) -> Result<RetainedMcpBindingContract> {
    let composition = retained_mcp_composition()?;
    let profile_id =
        ProfileId::new(APPLICATION_DEFAULT_PROFILE_ID).map_err(retained_catalog_error)?;
    let operation_name =
        SurfaceOperationName::new(operation.as_str()).map_err(retained_catalog_error)?;
    let capability = composition
        .snapshot()
        .resolve_binding(
            &profile_id,
            BindingSurface::Mcp,
            &operation_name,
            1,
            &BTreeSet::new(),
        )
        .ok_or_else(|| retained_catalog_error("retained MCP binding is not callable"))?;
    let expected =
        retained_surface_application_operation(operation).map_err(retained_catalog_error)?;
    if capability.capability_id() != expected.capability_id()
        || capability.use_case_id() != expected.use_case_id()
    {
        return Err(retained_catalog_error(
            "retained MCP binding resolves a different application operation",
        ));
    }
    Ok(RetainedMcpBindingContract {
        maximum_millis: capability.deadline().maximum_millis(),
    })
}

#[hotpath::measure(label = "mcp.retained.binding_resolve")]
pub(super) fn retained_mcp_binding(
    operation: RetainedSurfaceOperation,
) -> Result<&'static RetainedMcpBindingContract> {
    retained_mcp_binding_from_cache(
        &RETAINED_MCP_BINDINGS,
        operation,
        resolve_retained_mcp_binding,
    )
}

fn retained_mcp_binding_from_cache(
    bindings: &RetainedMcpBindingCache,
    operation: RetainedSurfaceOperation,
    resolve: impl FnOnce(RetainedSurfaceOperation) -> Result<RetainedMcpBindingContract>,
) -> Result<&RetainedMcpBindingContract> {
    let index = RetainedSurfaceOperation::ALL
        .iter()
        .position(|candidate| *candidate == operation)
        .ok_or_else(|| retained_catalog_error("retained MCP operation is not cataloged"))?;
    bindings[index]
        .get_or_init(|| resolve(operation).map_err(|error| error.to_string()))
        .as_ref()
        .map_err(retained_catalog_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_retained_binding_resolves_once_per_cache() {
        let bindings = std::array::from_fn(|_| OnceLock::new());
        let resolution_count = std::cell::Cell::new(0);
        let mut first_pass = Vec::with_capacity(RETAINED_OPERATION_COUNT);

        for operation in RetainedSurfaceOperation::ALL {
            let binding = retained_mcp_binding_from_cache(&bindings, operation, |operation| {
                resolution_count.set(resolution_count.get() + 1);
                resolve_retained_mcp_binding(operation)
            })
            .expect("first retained binding contract");
            assert!(binding.maximum_millis() > 0);
            first_pass.push((operation, std::ptr::from_ref(binding)));
        }
        assert_eq!(resolution_count.get(), RETAINED_OPERATION_COUNT);

        for (operation, first) in first_pass {
            let second = retained_mcp_binding_from_cache(&bindings, operation, |operation| {
                resolution_count.set(resolution_count.get() + 1);
                resolve_retained_mcp_binding(operation)
            })
            .expect("cached retained binding contract");
            assert_eq!(first, std::ptr::from_ref(second));
        }
        assert_eq!(
            resolution_count.get(),
            RETAINED_OPERATION_COUNT,
            "the second complete pass must resolve no binding again"
        );
    }
}
