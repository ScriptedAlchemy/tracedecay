use serde_json::{Value, json};
use tracedecay_tool_catalog::{
    ExecutableBindingRegistryV1, OperationId, RouteExposureV1, SchemaBodyAuthorityV1,
};

use super::OpenApiDocumentError;

/// The HTTP-owned request representation for one mounted operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpenApiRequestV1 {
    Json(SchemaBodyAuthorityV1),
    Query(SchemaBodyAuthorityV1),
    Empty,
}

/// The HTTP-owned success representation for one mounted operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpenApiSuccessV1 {
    ApplicationJson(SchemaBodyAuthorityV1),
    Json(SchemaBodyAuthorityV1),
    ServerSentEvents(SchemaBodyAuthorityV1),
}

/// One mounted route bound to its concrete schema and lifecycle authorities.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenApiRouteDocumentV1 {
    pub(crate) method: &'static str,
    pub(crate) path: String,
    pub(crate) operation_id: String,
    pub(crate) capability_id: Option<String>,
    pub(crate) binding_id: Option<String>,
    pub(crate) request: OpenApiRequestV1,
    pub(crate) success: OpenApiSuccessV1,
    pub(crate) success_statuses: Vec<&'static str>,
    pub(crate) lifecycle: Value,
}

impl OpenApiRouteDocumentV1 {
    pub fn method(&self) -> &'static str {
        self.method
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn operation_id(&self) -> &str {
        &self.operation_id
    }

    pub fn request(&self) -> &OpenApiRequestV1 {
        &self.request
    }

    pub fn success(&self) -> &OpenApiSuccessV1 {
        &self.success
    }

    pub fn success_statuses(&self) -> &[&'static str] {
        &self.success_statuses
    }

    pub fn executable_json(
        registry: &ExecutableBindingRegistryV1,
        operation_id: &str,
        mounted_path: &str,
        advertised_path: &str,
    ) -> Result<Self, OpenApiDocumentError> {
        let operation = OperationId::new(operation_id.to_owned()).map_err(|_| {
            OpenApiDocumentError::RouteAuthority {
                operation_id: operation_id.to_owned(),
                reason: "operation ID is invalid",
            }
        })?;
        let availability =
            registry
                .get(&operation)
                .ok_or_else(|| OpenApiDocumentError::RouteAuthority {
                    operation_id: operation_id.to_owned(),
                    reason: "executable binding is missing",
                })?;
        let binding =
            availability
                .binding()
                .ok_or_else(|| OpenApiDocumentError::RouteAuthority {
                    operation_id: operation_id.to_owned(),
                    reason: "executable binding is unavailable",
                })?;
        let (binding_id, route_path) = match binding.exposure() {
            RouteExposureV1::Public {
                binding_id,
                route_path,
            } => (binding_id, route_path),
            RouteExposureV1::Internal => {
                return Err(OpenApiDocumentError::RouteAuthority {
                    operation_id: operation_id.to_owned(),
                    reason: "executable binding is not publicly mounted",
                });
            }
        };
        if binding.operation_id().as_str() != operation_id || route_path != advertised_path {
            return Err(OpenApiDocumentError::RouteAuthority {
                operation_id: operation_id.to_owned(),
                reason: "mounted route drifted from its executable binding",
            });
        }
        Ok(Self {
            method: "POST",
            path: mounted_path.to_owned(),
            operation_id: operation_id.to_owned(),
            capability_id: Some(binding.capability_id().as_str().to_owned()),
            binding_id: Some(binding_id.as_str().to_owned()),
            request: OpenApiRequestV1::Json(binding.request_schema().clone()),
            success: OpenApiSuccessV1::ApplicationJson(binding.result_schema().clone()),
            success_statuses: vec!["200"],
            lifecycle: json!({
                "cancellation": binding.cancellation(),
                "deadline": binding.deadline(),
                "effect": binding.effect(),
                "idempotency": binding.idempotency(),
                "receipt": binding.receipt(),
                "reconciliation": binding.reconciliation(),
            }),
        })
    }

    pub(crate) fn adapter(
        method: &'static str,
        path: &'static str,
        operation_id: &'static str,
        request: OpenApiRequestV1,
        success: OpenApiSuccessV1,
        success_statuses: Vec<&'static str>,
        lifecycle: Value,
    ) -> Self {
        Self {
            method,
            path: path.to_owned(),
            operation_id: operation_id.to_owned(),
            capability_id: None,
            binding_id: None,
            request,
            success,
            success_statuses,
            lifecycle,
        }
    }

    pub(crate) fn component_stem(&self) -> &str {
        self.binding_id
            .as_deref()
            .unwrap_or(self.operation_id.as_str())
    }
}
