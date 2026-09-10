//! Daemon-service policy and adapters for project-owner registration.

use std::collections::BTreeSet;

use tracedecay_contracts::ApplicationContractError;
use tracedecay_contracts::feedback::{
    CI_FAILURE_LOCALIZE_CAPABILITY_ID_V1, FEEDBACK_DIAGNOSTICS_CAPABILITY_ID_V1,
    FEEDBACK_EXPAND_CAPABILITY_ID_V1, FEEDBACK_GET_CAPABILITY_ID_V1,
    FEEDBACK_LIST_CAPABILITY_ID_V1, GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1,
    PROXIMITY_CAPABILITY_ID_V1,
};
use tracedecay_tool_catalog::CapabilityId;

mod lsp;
mod source_edit;
pub use lsp::{production_lsp_registration, project_open_lsp_scope_grant};
pub use source_edit::{
    ProjectSourceEditAuthorizationV1, ProjectSourceEditOwnerV1, SourceEditMutationGate,
    SourceEditOwnerError,
};

pub fn project_owner_capabilities() -> Result<BTreeSet<CapabilityId>, ApplicationContractError> {
    let mut capabilities = BTreeSet::new();
    for capability in [
        "capability.diagnostics.current",
        FEEDBACK_DIAGNOSTICS_CAPABILITY_ID_V1,
        FEEDBACK_GET_CAPABILITY_ID_V1,
        FEEDBACK_EXPAND_CAPABILITY_ID_V1,
        FEEDBACK_LIST_CAPABILITY_ID_V1,
        "capability.application.feedback.impact",
        "capability.application.feedback.affected-tests",
        "capability.application.feedback.test-results",
        "capability.application.code-query.exact-occurrence",
        "capability.application.code-query.phrase-search",
        "capability.application.code-query.callees",
        "capability.application.code-query.facets",
        "capability.application.code-query.timeline",
        "capability.application.code-query.declaration",
        "capability.application.code-query.definition",
        "capability.application.code-query.type-definition",
        "capability.application.code-query.references",
        "capability.application.symbol-search",
        GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1,
        CI_FAILURE_LOCALIZE_CAPABILITY_ID_V1,
        PROXIMITY_CAPABILITY_ID_V1,
        "capability.application.git.status",
        "capability.application.git.diff",
        "capability.application.git.history",
        "capability.application.git.blame",
        "capability.application.git.hunks",
        crate::LSP_WORKSPACE_CAPABILITY_ID_V1,
        "capability.application.source-edit.ast-grep-rewrite",
        "capability.application.source-edit.insert-at",
        "capability.application.source-edit.insert-at-symbol",
        "capability.application.source-edit.move-symbol",
        "capability.application.source-edit.multi-str-replace",
        "capability.application.source-edit.rename-symbol",
        "capability.application.source-edit.replace-symbol",
        "capability.application.source-edit.reconcile",
        "capability.application.source-edit.rollback",
        "capability.application.source-edit.str-replace",
        "capability.git.stage-hunks",
        "capability.git.unstage-hunks",
        "capability.git.commit-index",
    ] {
        capabilities.insert(CapabilityId::new(capability.to_owned()).map_err(|_| {
            ApplicationContractError::Inconsistent {
                field: "project-open capability",
            }
        })?);
    }
    for descriptor in
        tracedecay_contracts::retrieval::catalog::primitive_read_handler_descriptors()?
    {
        capabilities.insert(descriptor.operation().capability_id().clone());
    }
    capabilities.insert(
        tracedecay_contracts::observatory_read_operation()?
            .capability_id()
            .clone(),
    );
    capabilities.extend(
        tracedecay_application::project_open_authorization::project_open_work_capabilities()?,
    );
    for operation in tracedecay_contracts::RetainedSurfaceOperation::CALLABLE {
        let operation = tracedecay_contracts::retained_surface_application_operation(operation)?;
        capabilities.insert(operation.capability_id().clone());
    }
    Ok(capabilities)
}

#[cfg(test)]
mod tests {
    use tracedecay_tool_catalog::CapabilityId;

    use super::project_owner_capabilities;

    #[test]
    fn project_owner_capabilities_cover_git_reads() {
        let capabilities = project_owner_capabilities().expect("project-owner capabilities");
        for capability in [
            "capability.application.git.status",
            "capability.application.git.diff",
            "capability.application.git.history",
            "capability.application.git.blame",
            "capability.application.git.hunks",
        ] {
            assert!(capabilities.contains(&CapabilityId::new(capability).expect("capability")));
        }
    }

    #[test]
    fn project_owner_capabilities_cover_every_primitive_read() {
        let capabilities = project_owner_capabilities().expect("project-owner capabilities");
        for descriptor in
            tracedecay_contracts::retrieval::catalog::primitive_read_handler_descriptors()
                .expect("primitive descriptors")
        {
            assert!(capabilities.contains(descriptor.operation().capability_id()));
        }
    }

    #[test]
    fn project_owner_capabilities_include_observatory_read() {
        let capabilities = project_owner_capabilities().expect("project-owner capabilities");
        let operation =
            tracedecay_contracts::observatory_read_operation().expect("Observatory read operation");
        assert!(capabilities.contains(operation.capability_id()));
    }

    #[test]
    fn project_owner_capabilities_cover_every_retained_operation() {
        let capabilities = project_owner_capabilities().expect("project-owner capabilities");
        for operation in tracedecay_contracts::RetainedSurfaceOperation::CALLABLE {
            let operation = tracedecay_contracts::retained_surface_application_operation(operation)
                .expect("retained operation");
            assert!(capabilities.contains(operation.capability_id()));
        }
    }
}
