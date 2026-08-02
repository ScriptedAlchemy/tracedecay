//! Typed provider registry and topology-pinned workflow placement.

use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::configuration::WorkTopologyPolicyV1;
use tracedecay_domain::{
    ManifestDigest, RunId, WorkProviderBackendV1, WorkProviderRouteV1, WorkflowPlacementReceiptV1,
    WorkflowStepId, canonical_sha256,
};

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowProviderRegistrationV1 {
    route: WorkProviderRouteV1,
    backend: WorkProviderBackendV1,
    model: String,
    priority: u32,
}

impl WorkflowProviderRegistrationV1 {
    pub fn new(
        route: WorkProviderRouteV1,
        backend: WorkProviderBackendV1,
        model: String,
        priority: u32,
    ) -> Result<Self, WorkflowProviderPlacementErrorV1> {
        if model.is_empty()
            || model.len() > 256
            || model.trim() != model
            || model.chars().any(char::is_control)
        {
            return Err(WorkflowProviderPlacementErrorV1::InvalidRegistry);
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
pub struct WorkflowProviderRegistryV1 {
    configuration_digest: ManifestDigest,
    registrations: Vec<WorkflowProviderRegistrationV1>,
    digest: ManifestDigest,
}

impl WorkflowProviderRegistryV1 {
    pub fn new(
        configuration_digest: ManifestDigest,
        mut registrations: Vec<WorkflowProviderRegistrationV1>,
    ) -> Result<Self, WorkflowProviderPlacementErrorV1> {
        if registrations.is_empty() {
            return Err(WorkflowProviderPlacementErrorV1::InvalidRegistry);
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
                return Err(WorkflowProviderPlacementErrorV1::InvalidRegistry);
            }
        }
        let digest = canonical_sha256(&(
            "tracedecay.application.workflow-provider-registry.v1",
            &configuration_digest,
            &registrations,
        ))
        .map_err(|_| WorkflowProviderPlacementErrorV1::InvalidRegistry)?;
        Ok(Self {
            configuration_digest,
            registrations,
            digest,
        })
    }

    pub fn configuration_digest(&self) -> &ManifestDigest {
        &self.configuration_digest
    }

    pub fn registrations(&self) -> &[WorkflowProviderRegistrationV1] {
        &self.registrations
    }

    pub fn digest(&self) -> &ManifestDigest {
        &self.digest
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowTopologyPlacementRequestV1 {
    pub run_id: RunId,
    pub step_id: WorkflowStepId,
    pub configuration_digest: ManifestDigest,
    pub topology_digest: ManifestDigest,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum WorkflowProviderPlacementErrorV1 {
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

pub struct WorkflowProviderPlacementServiceV1 {
    registry: WorkflowProviderRegistryV1,
}

impl WorkflowProviderPlacementServiceV1 {
    pub const fn new(registry: WorkflowProviderRegistryV1) -> Self {
        Self { registry }
    }

    pub fn place(
        &self,
        request: &WorkflowTopologyPlacementRequestV1,
        topology: &WorkTopologyPolicyV1,
    ) -> Result<WorkflowPlacementReceiptV1, WorkflowProviderPlacementErrorV1> {
        topology
            .validate()
            .map_err(|_| WorkflowProviderPlacementErrorV1::InvalidTopology)?;
        let topology_digest = topology
            .compute_digest()
            .map_err(|_| WorkflowProviderPlacementErrorV1::InvalidTopology)?
            .0;
        if &request.configuration_digest != self.registry.configuration_digest() {
            return Err(WorkflowProviderPlacementErrorV1::ConfigurationDigestMismatch);
        }
        if request.topology_digest != topology_digest {
            return Err(WorkflowProviderPlacementErrorV1::TopologyDigestMismatch);
        }
        let registration = self
            .registry
            .registrations()
            .first()
            .ok_or(WorkflowProviderPlacementErrorV1::Unavailable)?;
        WorkflowPlacementReceiptV1::new(
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
        .map_err(|_| WorkflowProviderPlacementErrorV1::InvalidRegistry)
    }
}
