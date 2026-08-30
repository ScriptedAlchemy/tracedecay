//! Registered Work and Workflow application-service composition.
//!
//! These types used to be constructed on `RegisteredGlobalDb`. Every input is
//! already a public accessor (`work_storage`, `workflow_storage`,
//! `project_graph_runtime`) plus a caller-supplied product binding, so the
//! composition lives here — above global-db — rather than inside it.

mod registered;

pub use registered::{
    RegisteredWorkApplicationServicesV1, RegisteredWorkProductServicesV1, RegisteredWorkTopologyV1,
    RegisteredWorkflowApplicationServicesV1, RegisteredWorkflowTopologyV1,
    work_intelligence_service,
};
