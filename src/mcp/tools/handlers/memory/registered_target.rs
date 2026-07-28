//! Authorized opening of a *selected* registered project's project-wide memory
//! store.
//!
//! A memory selector names a registered project, not a mounted graph. The
//! served graph is irrelevant to where that project's durable facts live: they
//! live in the profile's project shard for the selected `ProjectId`, which is
//! project-wide and branch-independent. This module resolves that shard through
//! the same registered-store authorities the active project uses — the profile
//! registry for identity and the daemon store-runtime registry for the mount —
//! so no second registry, ambient default, or synthetic enrollment can stand in
//! for a project the profile has not enrolled.

use std::fmt;

use tracedecay_domain::ProjectId;

use crate::db::Database;
use crate::errors::{Result, TraceDecayError};
use crate::global_db::ProjectRegistryContext;
use crate::tracedecay::TraceDecay;

/// Why a selected registered project's memory store cannot be served.
#[derive(Debug)]
pub(super) enum RegisteredMemoryDenial {
    /// The registry named a project identity the store runtime cannot key on.
    UnusableProjectIdentity { project_id: String, reason: String },
    /// The project is registered, but no root it claims carries a
    /// profile-sharded enrollment marker for it, so this profile holds no
    /// project-wide memory store to read.
    NotEnrolledInProfile { project_id: String },
    /// The enrollment exists but its store could not be mounted read-only.
    StoreUnavailable { project_id: String, reason: String },
}

impl fmt::Display for RegisteredMemoryDenial {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnusableProjectIdentity { project_id, reason } => write!(
                formatter,
                "registered project '{project_id}' has an unusable project identity: {reason}"
            ),
            Self::NotEnrolledInProfile { project_id } => write!(
                formatter,
                "registered project '{project_id}' is not enrolled in this TraceDecay profile, \
                 so it has no project-wide memory store here; open the project once to enroll it"
            ),
            Self::StoreUnavailable { project_id, reason } => write!(
                formatter,
                "project-wide memory store for registered project '{project_id}' is unavailable: \
                 {reason}"
            ),
        }
    }
}

impl From<RegisteredMemoryDenial> for TraceDecayError {
    fn from(denial: RegisteredMemoryDenial) -> Self {
        Self::Config {
            message: denial.to_string(),
        }
    }
}

/// Opens the selected registered project's project-wide memory store read-only.
///
/// Read-only is the whole authority this grants. Every cross-project memory
/// operation the tool layer admits is a query — write actions are refused
/// before resolution — so a mount that cannot write matches what the caller is
/// allowed to do.
pub(super) async fn open_registered_project_memory_read_only(
    cg: &TraceDecay,
    context: &ProjectRegistryContext,
) -> Result<Database> {
    let project_id = context.project.project_id.as_str();
    let selected = ProjectId::new(project_id.to_owned()).map_err(|error| {
        RegisteredMemoryDenial::UnusableProjectIdentity {
            project_id: project_id.to_owned(),
            reason: error.to_string(),
        }
    })?;
    let enrollment_roots = TraceDecay::enrolled_project_roots(
        TraceDecay::registry_context_candidate_roots(context),
        &selected,
    )?;
    if enrollment_roots.is_empty() {
        return Err(RegisteredMemoryDenial::NotEnrolledInProfile {
            project_id: project_id.to_owned(),
        }
        .into());
    }
    cg.store_runtime_registry()
        .project_memory_read_only(selected, enrollment_roots)
        .await
        .map_err(|error| {
            RegisteredMemoryDenial::StoreUnavailable {
                project_id: project_id.to_owned(),
                reason: error.to_string(),
            }
            .into()
        })
}
