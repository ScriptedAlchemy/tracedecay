//! Typed provider registry and topology-pinned workflow placement.

use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::configuration::WorkTopologyPolicyV1;
use tracedecay_domain::{
    ManifestDigest, RunId, WorkProviderBackendV1, WorkProviderRouteV1, WorkflowPlacementReceipt,
    WorkflowStepId, canonical_sha256,
};

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowProviderRegistration {
    route: WorkProviderRouteV1,
    backend: WorkProviderBackendV1,
    model: String,
    priority: u32,
}

impl WorkflowProviderRegistration {
    pub fn new(
        route: WorkProviderRouteV1,
        backend: WorkProviderBackendV1,
        model: String,
        priority: u32,
    ) -> Result<Self, WorkflowProviderPlacementError> {
        if model.is_empty()
            || model.len() > 256
            || model.trim() != model
            || model.chars().any(char::is_control)
        {
            return Err(WorkflowProviderPlacementError::InvalidRegistry);
        }
        Ok(Self {
            route,
            backend,
            model,
            priority,
        })
    }

    pub fn route(&self) -> &WorkProviderRouteV1 {
        &self.route
    }

    pub const fn backend(&self) -> WorkProviderBackendV1 {
        self.backend
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub const fn priority(&self) -> u32 {
        self.priority
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkflowProviderRegistry {
    configuration_digest: ManifestDigest,
    registrations: Vec<WorkflowProviderRegistration>,
    digest: ManifestDigest,
}

impl WorkflowProviderRegistry {
    pub fn new(
        configuration_digest: ManifestDigest,
        mut registrations: Vec<WorkflowProviderRegistration>,
    ) -> Result<Self, WorkflowProviderPlacementError> {
        if registrations.is_empty() {
            return Err(WorkflowProviderPlacementError::InvalidRegistry);
        }
        registrations.sort_by(|left, right| {
            (
                left.priority,
                left.route.provider_id().as_str(),
                left.route.route_id().as_str(),
            )
                .cmp(&(
                    right.priority,
                    right.route.provider_id().as_str(),
                    right.route.route_id().as_str(),
                ))
        });
        let mut routes = BTreeSet::new();
        for registration in &registrations {
            if !routes.insert((
                registration.route.provider_id().as_str(),
                registration.route.route_id().as_str(),
            )) {
                return Err(WorkflowProviderPlacementError::InvalidRegistry);
            }
        }
        let digest = canonical_sha256(&(
            "tracedecay.application.workflow-provider-registry.v1",
            &configuration_digest,
            &registrations,
        ))
        .map_err(|_| WorkflowProviderPlacementError::InvalidRegistry)?;
        Ok(Self {
            configuration_digest,
            registrations,
            digest,
        })
    }

    pub fn configuration_digest(&self) -> &ManifestDigest {
        &self.configuration_digest
    }

    pub fn registrations(&self) -> &[WorkflowProviderRegistration] {
        &self.registrations
    }

    pub fn digest(&self) -> &ManifestDigest {
        &self.digest
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowTopologyPlacementRequest {
    pub run_id: RunId,
    pub step_id: WorkflowStepId,
    pub configuration_digest: ManifestDigest,
    pub topology_digest: ManifestDigest,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum WorkflowProviderPlacementError {
    #[error("workflow provider registry is invalid")]
    InvalidRegistry,
    #[error("workflow provider configuration digest is stale")]
    ConfigurationDigestMismatch,
    #[error("workflow topology digest is stale")]
    TopologyDigestMismatch,
    #[error("workflow topology policy is invalid")]
    InvalidTopology,
    #[error("no workflow provider is registered")]
    Unavailable,
}

pub struct WorkflowProviderPlacementService {
    registry: WorkflowProviderRegistry,
}

impl WorkflowProviderPlacementService {
    pub const fn new(registry: WorkflowProviderRegistry) -> Self {
        Self { registry }
    }

    pub fn place(
        &self,
        request: &WorkflowTopologyPlacementRequest,
        topology: &WorkTopologyPolicyV1,
    ) -> Result<WorkflowPlacementReceipt, WorkflowProviderPlacementError> {
        topology
            .validate()
            .map_err(|_| WorkflowProviderPlacementError::InvalidTopology)?;
        let topology_digest = topology
            .compute_digest()
            .map_err(|_| WorkflowProviderPlacementError::InvalidTopology)?
            .0;
        if &request.configuration_digest != self.registry.configuration_digest() {
            return Err(WorkflowProviderPlacementError::ConfigurationDigestMismatch);
        }
        if request.topology_digest != topology_digest {
            return Err(WorkflowProviderPlacementError::TopologyDigestMismatch);
        }
        let registration = self
            .registry
            .registrations()
            .first()
            .ok_or(WorkflowProviderPlacementError::Unavailable)?;
        WorkflowPlacementReceipt::new(
            request.run_id.clone(),
            request.step_id.clone(),
            registration.route.clone(),
            registration.backend,
            registration.model.clone(),
            request.configuration_digest.clone(),
            topology_digest,
            self.registry.digest().clone(),
            topology.placement.clone(),
        )
        .map_err(|_| WorkflowProviderPlacementError::InvalidRegistry)
    }
}
