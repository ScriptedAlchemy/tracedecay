use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::Serialize;
use serde_json::Value;

use crate::binding::SurfaceOperationName;
use crate::id::{BindingId, CapabilityId, CatalogDigest, CodecBindingKey, OperationId, ServiceId};
use crate::manifest::{
    CancellationContract, CapabilityManifestV1, DeadlineContract, EffectClass, IdempotencyContract,
    ReceiptContract, ReconciliationContract, SchemaRef,
};
use crate::validation::CatalogValidationError;

/// Reviewed JSON Schema body generated from the Rust type that owns the wire.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SchemaBodyAuthorityV1 {
    schema_ref: SchemaRef,
    body: Value,
    digest: CatalogDigest,
}

impl SchemaBodyAuthorityV1 {
    pub fn for_type<T: JsonSchema>(schema_ref: SchemaRef) -> Result<Self, CatalogValidationError> {
        let body = serde_json::to_value(schemars::schema_for!(T)).map_err(|_| {
            CatalogValidationError::InvalidValue {
                field: "schema body",
                reason: "Rust schema authority could not be serialized",
            }
        })?;
        let body = canonicalize_json(body);
        let bytes =
            serde_json::to_vec(&body).map_err(|_| CatalogValidationError::InvalidValue {
                field: "schema body",
                reason: "canonical schema body could not be encoded",
            })?;
        Ok(Self {
            schema_ref,
            body,
            digest: CatalogDigest::sha256(bytes),
        })
    }

    pub fn schema_ref(&self) -> &SchemaRef {
        &self.schema_ref
    }

    pub fn body(&self) -> &Value {
        &self.body
    }

    pub const fn digest(&self) -> CatalogDigest {
        self.digest
    }
}

fn canonicalize_json(value: Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.into_iter().map(canonicalize_json).collect()),
        Value::Object(values) => Value::Object(
            values
                .into_iter()
                .map(|(key, value)| (key, canonicalize_json(value)))
                .collect::<BTreeMap<_, _>>()
                .into_iter()
                .collect(),
        ),
        scalar => scalar,
    }
}

/// Runtime composition owner. Provider differences remain explicit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "mode")]
pub enum ExecutionOwnerV1 {
    Direct { service_id: ServiceId },
    DaemonOwned { service_id: ServiceId },
}

impl ExecutionOwnerV1 {
    pub fn service_id(&self) -> &ServiceId {
        match self {
            Self::Direct { service_id } | Self::DaemonOwned { service_id } => service_id,
        }
    }
}

/// Exact codec and adapter key used for request/result encoding.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "codec")]
pub enum ExecutableCodecV1 {
    Json { binding_key: CodecBindingKey },
}

impl ExecutableCodecV1 {
    pub fn binding_key(&self) -> &CodecBindingKey {
        match self {
            Self::Json { binding_key } => binding_key,
        }
    }
}

/// Whether the operation is private composition or exposed by a catalog route.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "visibility")]
pub enum RouteExposureV1 {
    Internal,
    Public {
        binding_id: BindingId,
        route_path: String,
    },
}

/// Fully executable metadata for one catalog capability.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ExecutableBindingV1 {
    capability_id: CapabilityId,
    operation_id: OperationId,
    owner: ExecutionOwnerV1,
    request_schema: SchemaBodyAuthorityV1,
    result_schema: SchemaBodyAuthorityV1,
    codec: ExecutableCodecV1,
    exposure: RouteExposureV1,
    effect: EffectClass,
    idempotency: IdempotencyContract,
    cancellation: CancellationContract,
    deadline: DeadlineContract,
    reconciliation: ReconciliationContract,
    receipt: ReceiptContract,
}

impl ExecutableBindingV1 {
    pub fn direct(
        manifest: &CapabilityManifestV1,
        operation_id: OperationId,
        service_id: ServiceId,
        request_schema: SchemaBodyAuthorityV1,
        result_schema: SchemaBodyAuthorityV1,
        binding_key: CodecBindingKey,
        exposure: RouteExposureV1,
    ) -> Result<Self, CatalogValidationError> {
        Self::from_manifest(
            manifest,
            operation_id,
            ExecutionOwnerV1::Direct { service_id },
            request_schema,
            result_schema,
            binding_key,
            exposure,
        )
    }

    pub fn daemon_owned(
        manifest: &CapabilityManifestV1,
        operation_id: OperationId,
        service_id: ServiceId,
        request_schema: SchemaBodyAuthorityV1,
        result_schema: SchemaBodyAuthorityV1,
        binding_key: CodecBindingKey,
        exposure: RouteExposureV1,
    ) -> Result<Self, CatalogValidationError> {
        Self::from_manifest(
            manifest,
            operation_id,
            ExecutionOwnerV1::DaemonOwned { service_id },
            request_schema,
            result_schema,
            binding_key,
            exposure,
        )
    }

    fn from_manifest(
        manifest: &CapabilityManifestV1,
        operation_id: OperationId,
        owner: ExecutionOwnerV1,
        request_schema: SchemaBodyAuthorityV1,
        result_schema: SchemaBodyAuthorityV1,
        binding_key: CodecBindingKey,
        exposure: RouteExposureV1,
    ) -> Result<Self, CatalogValidationError> {
        if request_schema.schema_ref() != manifest.request_schema()
            || result_schema.schema_ref() != manifest.result_schema()
        {
            return Err(CatalogValidationError::InvalidCapability {
                capability_id: manifest.capability_id().clone(),
                reason: "executable binding schema bodies do not match the manifest",
            });
        }
        if let RouteExposureV1::Public {
            binding_id,
            route_path,
        } = &exposure
        {
            if manifest.binding_ids().binary_search(binding_id).is_err() {
                return Err(CatalogValidationError::InvalidCapability {
                    capability_id: manifest.capability_id().clone(),
                    reason: "public executable route is not declared by the manifest",
                });
            }
            if !route_path.starts_with('/') || route_path.contains(['?', '#']) {
                return Err(CatalogValidationError::InvalidCapability {
                    capability_id: manifest.capability_id().clone(),
                    reason: "public executable route path must be canonical and absolute",
                });
            }
        }

        Ok(Self {
            capability_id: manifest.capability_id().clone(),
            operation_id,
            owner,
            request_schema,
            result_schema,
            codec: ExecutableCodecV1::Json { binding_key },
            exposure,
            effect: manifest.effect(),
            idempotency: manifest.idempotency(),
            cancellation: manifest.cancellation().clone(),
            deadline: manifest.deadline().clone(),
            reconciliation: manifest.reconciliation(),
            receipt: manifest.receipt(),
        })
    }

    pub fn capability_id(&self) -> &CapabilityId {
        &self.capability_id
    }

    pub fn operation_id(&self) -> &OperationId {
        &self.operation_id
    }

    pub fn owner(&self) -> &ExecutionOwnerV1 {
        &self.owner
    }

    pub fn request_schema(&self) -> &SchemaBodyAuthorityV1 {
        &self.request_schema
    }

    pub fn result_schema(&self) -> &SchemaBodyAuthorityV1 {
        &self.result_schema
    }

    pub fn codec(&self) -> &ExecutableCodecV1 {
        &self.codec
    }

    pub fn exposure(&self) -> &RouteExposureV1 {
        &self.exposure
    }

    pub const fn effect(&self) -> EffectClass {
        self.effect
    }

    pub const fn idempotency(&self) -> IdempotencyContract {
        self.idempotency
    }

    pub fn cancellation(&self) -> &CancellationContract {
        &self.cancellation
    }

    pub fn deadline(&self) -> &DeadlineContract {
        &self.deadline
    }

    pub const fn reconciliation(&self) -> ReconciliationContract {
        self.reconciliation
    }

    pub const fn receipt(&self) -> ReceiptContract {
        self.receipt
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutableUnavailableDispositionV1 {
    ServiceNotRegistered,
    SchemaUnavailable,
    CodecUnavailable,
    RouteUnavailable,
    CapabilityDisabled,
    HostUnsupported,
}

/// Truthful executable lookup state; unavailable records cannot carry a binding.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum ExecutableBindingAvailabilityV1 {
    Available {
        binding: Box<ExecutableBindingV1>,
    },
    Unavailable {
        operation_id: OperationId,
        disposition: ExecutableUnavailableDispositionV1,
    },
}

impl ExecutableBindingAvailabilityV1 {
    pub fn available(binding: ExecutableBindingV1) -> Self {
        Self::Available {
            binding: Box::new(binding),
        }
    }

    pub fn operation_id(&self) -> &OperationId {
        match self {
            Self::Available { binding } => binding.operation_id(),
            Self::Unavailable { operation_id, .. } => operation_id,
        }
    }

    pub fn binding(&self) -> Option<&ExecutableBindingV1> {
        match self {
            Self::Available { binding } => Some(binding),
            Self::Unavailable { .. } => None,
        }
    }
}

/// Canonically ordered executable lookup assembled by the application root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutableBindingRegistryV1 {
    bindings: BTreeMap<OperationId, ExecutableBindingAvailabilityV1>,
}

impl ExecutableBindingRegistryV1 {
    pub fn new(
        bindings: Vec<ExecutableBindingAvailabilityV1>,
    ) -> Result<Self, CatalogValidationError> {
        let mut registry = BTreeMap::new();
        for binding in bindings {
            if registry
                .insert(binding.operation_id().clone(), binding)
                .is_some()
            {
                return Err(CatalogValidationError::DuplicateValue {
                    field: "executable operation IDs",
                });
            }
        }
        Ok(Self { bindings: registry })
    }

    pub fn get(&self, operation_id: &OperationId) -> Option<&ExecutableBindingAvailabilityV1> {
        self.bindings.get(operation_id)
    }

    pub fn iter(&self) -> impl Iterator<Item = &ExecutableBindingAvailabilityV1> {
        self.bindings.values()
    }
}

/// The concrete transport a generated, named SDK method invokes.
///
/// This is deliberately distinct from [`RouteExposureV1`]. A capability can
/// be executable through MCP without acquiring a synthetic HTTP route merely
/// because an SDK also exposes it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum SdkTransportBindingV1 {
    Http { route_path: String },
    McpTool { tool_name: String },
}

impl SdkTransportBindingV1 {
    fn validate(&self) -> Result<(), CatalogValidationError> {
        match self {
            Self::Http { route_path } => {
                if !route_path.starts_with('/') || route_path.contains(['?', '#']) {
                    return Err(CatalogValidationError::InvalidValue {
                        field: "SDK HTTP route path",
                        reason: "must be canonical and absolute",
                    });
                }
            }
            Self::McpTool { tool_name } => {
                if tool_name.is_empty()
                    || tool_name.trim() != tool_name
                    || tool_name.len() > 192
                    || !tool_name.is_ascii()
                    || tool_name
                        .bytes()
                        .any(|byte| !(byte.is_ascii_alphanumeric() || byte == b'_'))
                {
                    return Err(CatalogValidationError::InvalidValue {
                        field: "SDK MCP tool name",
                        reason: "must be a bounded ASCII identifier",
                    });
                }
            }
        }
        Ok(())
    }
}

/// One public, named SDK method bound to a verified executable capability.
///
/// The embedded executable remains the authority for ownership, schemas, and
/// lifecycle semantics. This wrapper contributes only the SDK spelling and
/// concrete transport needed to invoke it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SdkExecutableBindingV1 {
    executable: ExecutableBindingV1,
    binding_id: BindingId,
    sdk_method: SurfaceOperationName,
    transport: SdkTransportBindingV1,
}

impl SdkExecutableBindingV1 {
    pub fn new(
        executable: ExecutableBindingV1,
        binding_id: BindingId,
        sdk_method: SurfaceOperationName,
        transport: SdkTransportBindingV1,
    ) -> Result<Self, CatalogValidationError> {
        transport.validate()?;
        match (&transport, executable.exposure()) {
            (
                SdkTransportBindingV1::Http { route_path },
                RouteExposureV1::Public {
                    binding_id: executable_binding_id,
                    route_path: executable_route_path,
                },
            ) if binding_id == *executable_binding_id && route_path == executable_route_path => {}
            (SdkTransportBindingV1::Http { .. }, _) => {
                return Err(CatalogValidationError::InvalidValue {
                    field: "SDK HTTP binding",
                    reason: "must exactly match the executable public route",
                });
            }
            (SdkTransportBindingV1::McpTool { .. }, RouteExposureV1::Internal) => {}
            (SdkTransportBindingV1::McpTool { .. }, RouteExposureV1::Public { .. }) => {
                return Err(CatalogValidationError::InvalidValue {
                    field: "SDK MCP binding",
                    reason: "must not alias an HTTP executable route",
                });
            }
        }
        Ok(Self {
            executable,
            binding_id,
            sdk_method,
            transport,
        })
    }

    pub fn executable(&self) -> &ExecutableBindingV1 {
        &self.executable
    }

    /// Canonical execution metadata retained by this SDK binding.
    pub fn binding(&self) -> &ExecutableBindingV1 {
        self.executable()
    }

    pub fn operation_id(&self) -> &OperationId {
        self.executable.operation_id()
    }

    pub fn binding_id(&self) -> &BindingId {
        &self.binding_id
    }

    pub fn sdk_method(&self) -> &SurfaceOperationName {
        &self.sdk_method
    }

    pub fn transport(&self) -> &SdkTransportBindingV1 {
        &self.transport
    }

    pub fn request_schema(&self) -> &SchemaBodyAuthorityV1 {
        self.executable.request_schema()
    }

    pub fn result_schema(&self) -> &SchemaBodyAuthorityV1 {
        self.executable.result_schema()
    }

    pub const fn effect(&self) -> EffectClass {
        self.executable.effect()
    }

    pub const fn idempotency(&self) -> IdempotencyContract {
        self.executable.idempotency()
    }

    pub fn cancellation(&self) -> &CancellationContract {
        self.executable.cancellation()
    }

    pub fn deadline(&self) -> &DeadlineContract {
        self.executable.deadline()
    }

    pub const fn reconciliation(&self) -> ReconciliationContract {
        self.executable.reconciliation()
    }

    pub const fn receipt(&self) -> ReceiptContract {
        self.executable.receipt()
    }
}

/// Truthful SDK lookup state. Unsupported product capabilities remain
/// explicit, while every available entry has a concrete named transport.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum SdkExecutableBindingAvailabilityV1 {
    Available {
        binding: Box<SdkExecutableBindingV1>,
    },
    Unavailable {
        operation_id: OperationId,
        disposition: ExecutableUnavailableDispositionV1,
    },
}

impl SdkExecutableBindingAvailabilityV1 {
    pub fn available(binding: SdkExecutableBindingV1) -> Self {
        Self::Available {
            binding: Box::new(binding),
        }
    }

    pub fn operation_id(&self) -> &OperationId {
        match self {
            Self::Available { binding } => binding.operation_id(),
            Self::Unavailable { operation_id, .. } => operation_id,
        }
    }

    pub fn binding(&self) -> Option<&SdkExecutableBindingV1> {
        match self {
            Self::Available { binding } => Some(binding),
            Self::Unavailable { .. } => None,
        }
    }
}

/// Canonically ordered SDK executable lookup assembled by application
/// composition from actual mounted surface bindings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SdkExecutableBindingRegistryV1 {
    bindings: BTreeMap<OperationId, SdkExecutableBindingAvailabilityV1>,
}

impl SdkExecutableBindingRegistryV1 {
    pub fn new(
        bindings: Vec<SdkExecutableBindingAvailabilityV1>,
    ) -> Result<Self, CatalogValidationError> {
        let mut registry = BTreeMap::new();
        for binding in bindings {
            if registry
                .insert(binding.operation_id().clone(), binding)
                .is_some()
            {
                return Err(CatalogValidationError::DuplicateValue {
                    field: "SDK executable operation IDs",
                });
            }
        }
        Ok(Self { bindings: registry })
    }

    pub fn get(&self, operation_id: &OperationId) -> Option<&SdkExecutableBindingAvailabilityV1> {
        self.bindings.get(operation_id)
    }

    pub fn iter(&self) -> impl Iterator<Item = &SdkExecutableBindingAvailabilityV1> {
        self.bindings.values()
    }
}
