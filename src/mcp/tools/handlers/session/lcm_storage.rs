use super::message_search::{SessionRetrievalServicePort, SessionRetrievalStoreScope};
use super::*;

#[derive(Clone, Copy)]
pub(in super::super) struct LcmHandlerContext<'a> {
    pub(super) project_root: Option<&'a Path>,
    pub(super) lcm_authority: Option<&'a dyn crate::daemon::lcm_authority::MountedLcmAuthorityPort>,
    pub(super) retrieval_service: Option<&'a dyn SessionRetrievalServicePort>,
    pub(super) retrieval_store_scope: SessionRetrievalStoreScope,
}

impl<'a> LcmHandlerContext<'a> {
    pub(in super::super) fn active(
        cg: &'a TraceDecay,
        retrieval_service: Option<&'a dyn SessionRetrievalServicePort>,
    ) -> Self {
        Self {
            project_root: Some(cg.project_root()),
            lcm_authority: None,
            retrieval_service,
            retrieval_store_scope: SessionRetrievalStoreScope::Project,
        }
    }

    pub(in super::super) fn user(
        retrieval_service: Option<&'a dyn SessionRetrievalServicePort>,
    ) -> Self {
        Self {
            project_root: None,
            lcm_authority: None,
            retrieval_service,
            retrieval_store_scope: SessionRetrievalStoreScope::Profile,
        }
    }

    #[cfg(test)]
    pub(super) fn project_for_test(
        project_root: &'a Path,
        retrieval_service: Option<&'a dyn SessionRetrievalServicePort>,
    ) -> Self {
        Self {
            project_root: Some(project_root),
            lcm_authority: None,
            retrieval_service,
            retrieval_store_scope: SessionRetrievalStoreScope::Project,
        }
    }

    pub(in super::super) const fn with_lcm_authority(
        mut self,
        authority: Option<&'a dyn crate::daemon::lcm_authority::MountedLcmAuthorityPort>,
    ) -> Self {
        self.lcm_authority = authority;
        self
    }
}

pub(super) fn lcm_unavailable(args: &Value) -> ToolResult {
    tool_json(
        None,
        args,
        &json!({
            "status": "unavailable",
            "reason": "lcm_daemon_authority_unavailable",
            "message": "the daemon did not mount an LCM authority for this exact session store",
        }),
    )
    .with_semantic_error(true)
}
