use std::{path::PathBuf, sync::Arc};

use tracedecay_application::{ApplicationContractError, ResolvedScope};

use crate::lsp_runtime::LspCodeIndexProjectionIdentityPort;

use super::{
    ManagedTestRunCurrentIdentity, ManagedTestRunCurrentIdentityFuture,
    ManagedTestRunCurrentScopePort,
};

#[derive(Clone)]
pub(super) struct ProductionManagedTestRunCurrentScope {
    project_root: PathBuf,
    scope: ResolvedScope,
    code_index: Arc<dyn LspCodeIndexProjectionIdentityPort>,
}

impl ProductionManagedTestRunCurrentScope {
    pub(super) fn new(
        project_root: PathBuf,
        scope: ResolvedScope,
        code_index: Arc<dyn LspCodeIndexProjectionIdentityPort>,
    ) -> Self {
        Self {
            project_root,
            scope,
            code_index,
        }
    }
}

impl ManagedTestRunCurrentScopePort for ProductionManagedTestRunCurrentScope {
    fn current_identity(&self) -> ManagedTestRunCurrentIdentityFuture<'_> {
        let project_root = self.project_root.clone();
        let scope = self.scope.clone();
        let code_index = Arc::clone(&self.code_index);
        Box::pin(async move {
            let current = code_index
                .current_identity(project_root, None)
                .await
                .map_err(|_| ApplicationContractError::Inconsistent {
                    field: "managed test result code generation",
                })?
                .admit_for_scope(&scope)
                .map_err(|_| ApplicationContractError::Inconsistent {
                    field: "managed test result sealed scope",
                })?;
            Ok(ManagedTestRunCurrentIdentity {
                head_commit_id: current.head_commit_id,
                code_generation_id: current.code_generation_id,
            })
        })
    }
}
