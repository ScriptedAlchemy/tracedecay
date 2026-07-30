use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::Serialize;
use serde_json::Value;

use crate::id::{BindingId, CapabilityId, CatalogDigest, CodecBindingKey, OperationId, ServiceId};
use crate::manifest::{CancellationContract, CapabilityManifestV1, SchemaRef};
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
    Public { binding_id: BindingId },
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
    cancellation: CancellationContract,
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
        if let RouteExposureV1::Public { binding_id } = &exposure
            && manifest.binding_ids().binary_search(binding_id).is_err()
        {
            return Err(CatalogValidationError::InvalidCapability {
                capability_id: manifest.capability_id().clone(),
                reason: "public executable route is not declared by the manifest",
            });
        }

        Ok(Self {
            capability_id: manifest.capability_id().clone(),
            operation_id,
            owner,
            request_schema,
            result_schema,
            codec: ExecutableCodecV1::Json { binding_key },
            exposure,
            cancellation: manifest.cancellation().clone(),
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

    pub fn cancellation(&self) -> &CancellationContract {
        &self.cancellation
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
        binding: ExecutableBindingV1,
    },
    Unavailable {
        operation_id: OperationId,
        disposition: ExecutableUnavailableDispositionV1,
    },
}

impl ExecutableBindingAvailabilityV1 {
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
