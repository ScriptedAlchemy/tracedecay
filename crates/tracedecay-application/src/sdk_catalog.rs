//! Canonical named SDK bindings for mounted application capabilities.
//!
//! This module does not introduce a router. It projects each executable
//! capability's already-mounted public transport into the stable SDK method
//! spelling the generator emits.

use tracedecay_tool_catalog::{
    CatalogValidationError, ExecutableBindingAvailabilityV1, ExecutableUnavailableDispositionV1,
    OperationId, RouteExposureV1, SdkExecutableBindingAvailabilityV1,
    SdkExecutableBindingRegistryV1, SdkExecutableBindingV1, SdkTransportBindingV1,
    SurfaceOperationName,
};

use crate::{work_executable_binding_registry, workflow_executable_binding_registry};

/// Canonical SDK bindings for every currently mounted typed application route.
///
/// The source registries remain authoritative for executable schemas and
/// lifecycle semantics. This projection only selects the actual public
/// transport and attaches a named SDK method to it.
pub fn sdk_executable_binding_registry()
-> Result<SdkExecutableBindingRegistryV1, CatalogValidationError> {
    let work = work_executable_binding_registry()?;
    let workflow = workflow_executable_binding_registry()?;
    let bindings = work
        .iter()
        .chain(workflow.iter())
        .map(project_http_binding)
        .collect::<Result<Vec<_>, _>>()?;
    SdkExecutableBindingRegistryV1::new(bindings)
}

fn project_http_binding(
    availability: &ExecutableBindingAvailabilityV1,
) -> Result<SdkExecutableBindingAvailabilityV1, CatalogValidationError> {
    let Some(executable) = availability.binding() else {
        return Ok(SdkExecutableBindingAvailabilityV1::Unavailable {
            operation_id: availability.operation_id().clone(),
            disposition: unavailable_disposition(availability),
        });
    };
    let RouteExposureV1::Public {
        binding_id,
        route_path,
    } = executable.exposure()
    else {
        return Ok(SdkExecutableBindingAvailabilityV1::Unavailable {
            operation_id: executable.operation_id().clone(),
            disposition: ExecutableUnavailableDispositionV1::RouteUnavailable,
        });
    };
    let sdk_method = SurfaceOperationName::new(sdk_method_name(executable.operation_id())?)?;
    let binding = SdkExecutableBindingV1::new(
        executable.clone(),
        binding_id.clone(),
        sdk_method,
        SdkTransportBindingV1::Http {
            route_path: route_path.clone(),
        },
    )?;
    Ok(SdkExecutableBindingAvailabilityV1::available(binding))
}

fn unavailable_disposition(
    availability: &ExecutableBindingAvailabilityV1,
) -> ExecutableUnavailableDispositionV1 {
    match availability {
        ExecutableBindingAvailabilityV1::Unavailable { disposition, .. } => *disposition,
        ExecutableBindingAvailabilityV1::Available { .. } => {
            ExecutableUnavailableDispositionV1::RouteUnavailable
        }
    }
}

fn sdk_method_name(operation_id: &OperationId) -> Result<String, CatalogValidationError> {
    let operation = operation_id.as_str().strip_prefix("operation.").ok_or(
        CatalogValidationError::InvalidValue {
            field: "SDK operation ID",
            reason: "must be rooted at operation.",
        },
    )?;
    if operation.split('.').count() != 2 {
        return Err(CatalogValidationError::InvalidValue {
            field: "SDK operation ID",
            reason: "must identify one product family and operation",
        });
    }
    Ok(operation.replace('.', "_"))
}

#[cfg(test)]
mod tests {
    use tracedecay_tool_catalog::{OperationId, SdkTransportBindingV1};

    use super::sdk_executable_binding_registry;

    #[test]
    fn sdk_registry_projects_mounted_routes_as_named_direct_methods() {
        let registry = sdk_executable_binding_registry().expect("SDK registry");
        let snapshot = registry
            .get(&OperationId::new("operation.work.snapshot").expect("operation ID"))
            .and_then(|availability| availability.binding())
            .expect("mounted work snapshot");

        assert_eq!(snapshot.sdk_method().as_str(), "work_snapshot");
        assert_eq!(snapshot.binding_id().as_str(), "binding.http.work.snapshot");
        assert!(matches!(
            snapshot.transport(),
            SdkTransportBindingV1::Http { route_path }
                if route_path == "/application/work/snapshot"
        ));

        let workflow = registry
            .get(&OperationId::new("operation.workflow.register_definition").expect("operation ID"))
            .and_then(|availability| availability.binding())
            .expect("mounted workflow register-definition");
        assert!(matches!(
            workflow.transport(),
            SdkTransportBindingV1::Http { route_path }
                if route_path == "/application/workflow/register-definition"
        ));
        assert_eq!(
            workflow.sdk_method().as_str(),
            "workflow_register_definition"
        );
    }
}
