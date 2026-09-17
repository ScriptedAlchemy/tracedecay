use tracedecay_tool_catalog::{
    BindingStatus, BindingSurface, CatalogContributionV1, CodecBindingKey,
    ExecutableBindingAvailabilityV1, ExecutableBindingRegistryV1, ExecutableBindingV1,
    ExecutableUnavailableDispositionV1, OperationId, RouteExposureV1, ServiceId, SurfaceBindingV1,
};

use crate::{
    ApplicationContractError, ApplicationHandlerDescriptors, application_catalog_contributions,
    application_handler_descriptors,
};

#[derive(Clone, Copy)]
pub(crate) enum ApplicationCatalogProjection {
    Mcp,
    Http,
}

impl ApplicationCatalogProjection {
    const fn surface(self) -> BindingSurface {
        match self {
            Self::Mcp => BindingSurface::Mcp,
            Self::Http => BindingSurface::Http,
        }
    }
}

pub(crate) fn project_application_executable_bindings(
    projection: ApplicationCatalogProjection,
    exposure: impl Fn(&SurfaceBindingV1, &str) -> Result<RouteExposureV1, ApplicationContractError>,
) -> Result<ExecutableBindingRegistryV1, ApplicationContractError> {
    let handlers = application_handler_descriptors()?;
    let mut bindings = Vec::new();
    for contribution in application_catalog_contributions()? {
        for surface in contribution.bindings().iter().filter(|binding| {
            binding.surface() == projection.surface()
                && matches!(binding.status(), BindingStatus::Current)
                && !binding.is_alias()
        }) {
            if let Some(binding) =
                project_availability(projection, &contribution, &handlers, surface, &exposure)?
            {
                bindings.push(binding);
            }
        }
    }
    Ok(ExecutableBindingRegistryV1::new(bindings)?)
}

fn project_availability(
    projection: ApplicationCatalogProjection,
    contribution: &CatalogContributionV1,
    handlers: &ApplicationHandlerDescriptors,
    surface: &SurfaceBindingV1,
    exposure: &impl Fn(&SurfaceBindingV1, &str) -> Result<RouteExposureV1, ApplicationContractError>,
) -> Result<Option<ExecutableBindingAvailabilityV1>, ApplicationContractError> {
    let manifest = contribution
        .capabilities()
        .iter()
        .find(|manifest| manifest.capability_id() == surface.capability_id())
        .ok_or(ApplicationContractError::Inconsistent {
            field: "application surface binding manifest",
        })?;
    let descriptor =
        handlers
            .get(manifest.use_case_id())
            .ok_or(ApplicationContractError::Inconsistent {
                field: "application surface binding handler",
            })?;
    if matches!(projection, ApplicationCatalogProjection::Http)
        && descriptor.surface_operation().is_none()
    {
        return Ok(None);
    }
    if descriptor.surface_operation().is_some_and(|operation| {
        operation.name_for_surface(projection.surface()) != surface.operation().as_str()
    }) {
        return Err(ApplicationContractError::Inconsistent {
            field: "application surface operation spelling",
        });
    }
    let operation = descriptor.surface_operation().map_or_else(
        || surface.operation().as_str(),
        |operation| operation.as_str(),
    );
    let operation_id = OperationId::new(format!("operation.application.{operation}"))?;
    if !manifest.availability().is_callable() {
        return unavailable_or_error(
            projection,
            operation_id,
            ExecutableUnavailableDispositionV1::CapabilityDisabled,
            "application HTTP capability",
        );
    }
    let Some(schema) = contribution.executable_schema(surface.capability_id()) else {
        return unavailable_or_error(
            projection,
            operation_id,
            ExecutableUnavailableDispositionV1::SchemaUnavailable,
            "application HTTP schema",
        );
    };
    let service_id = match descriptor.service_id() {
        Some(service_id) => service_id.clone(),
        None if matches!(projection, ApplicationCatalogProjection::Mcp) => service_id(surface)?,
        None => {
            return Err(ApplicationContractError::Inconsistent {
                field: "application HTTP service",
            });
        }
    };
    Ok(Some(ExecutableBindingAvailabilityV1::available(
        ExecutableBindingV1::daemon_owned(
            manifest,
            operation_id,
            service_id,
            schema.request_schema().clone(),
            schema.result_schema().clone(),
            CodecBindingKey::new(format!("codec.application.{operation}.json.v1"))?,
            exposure(surface, operation)?,
        )?,
    )))
}

fn unavailable_or_error(
    projection: ApplicationCatalogProjection,
    operation_id: OperationId,
    disposition: ExecutableUnavailableDispositionV1,
    http_error_field: &'static str,
) -> Result<Option<ExecutableBindingAvailabilityV1>, ApplicationContractError> {
    match projection {
        ApplicationCatalogProjection::Mcp => {
            Ok(Some(ExecutableBindingAvailabilityV1::Unavailable {
                operation_id,
                disposition,
            }))
        }
        ApplicationCatalogProjection::Http => Err(ApplicationContractError::Inconsistent {
            field: http_error_field,
        }),
    }
}

fn service_id(surface: &SurfaceBindingV1) -> Result<ServiceId, ApplicationContractError> {
    let family = surface
        .capability_id()
        .as_str()
        .strip_prefix("capability.application.")
        .and_then(|value| value.split('.').next())
        .filter(|value| !value.is_empty())
        .ok_or(ApplicationContractError::Inconsistent {
            field: "MCP service family",
        })?;
    Ok(ServiceId::new(format!("service.application.{family}"))?)
}
