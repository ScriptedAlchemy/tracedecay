//! Registered Work and Workflow application-service composition.
//!
//! These types used to be constructed on `RegisteredGlobalDb`. Every input is
//! already a public accessor (`work_storage`, `workflow_storage`,
//! `project_graph_runtime`) plus a caller-supplied product binding, so the
//! composition lives here — above global-db — rather than inside it.

mod registered;
pub mod work_evidence_retrieval;
pub mod work_topology;
pub mod workflow_topology;

pub use registered::{
    RegisteredWorkApplicationServicesV1, RegisteredWorkProductServicesV1,
    RegisteredWorkflowApplicationServicesV1, RegisteredWorkflowTopologyV1,
    work_intelligence_service,
};
#[cfg(any(test, feature = "test-helpers"))]
pub use work_evidence_retrieval::tests::{StaticFederatedAuthority, federated_authority};
pub use work_evidence_retrieval::{
    WorkFederatedQueryAuthorityFutureV1, WorkFederatedQueryAuthorityPortV1,
    WorkTaskSessionAdmittedRetrievalFutureV1, WorkTaskSessionAdmittedRetrievalPortV1,
    WorkTaskSessionEvidenceRetrievalV1,
};
