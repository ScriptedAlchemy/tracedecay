use super::message_search::{SessionRetrievalServicePort, SessionRetrievalStoreScope};
use super::*;

#[derive(Clone, Copy)]
pub(in super::super) struct LcmHandlerContext<'a> {
    pub(super) project_root: Option<&'a Path>,
    retained_session_db: Option<&'a Arc<RegisteredGlobalDb>>,
    pub(super) retrieval_service: Option<&'a dyn SessionRetrievalServicePort>,
    pub(super) retrieval_store_scope: SessionRetrievalStoreScope,
}

impl<'a> LcmHandlerContext<'a> {
    pub(in super::super) fn active(
        cg: &'a TraceDecay,
        retained_session_db: Option<&'a Arc<RegisteredGlobalDb>>,
        retrieval_service: Option<&'a dyn SessionRetrievalServicePort>,
    ) -> Self {
        Self {
            project_root: Some(cg.project_root()),
            retained_session_db,
            retrieval_service,
            retrieval_store_scope: SessionRetrievalStoreScope::Project,
        }
    }

    pub(in super::super) fn user(
        _sessions_db_path: &'a Path,
        retained_session_db: Option<&'a Arc<RegisteredGlobalDb>>,
        retrieval_service: Option<&'a dyn SessionRetrievalServicePort>,
    ) -> Self {
        Self {
            project_root: None,
            retained_session_db,
            retrieval_service,
            retrieval_store_scope: SessionRetrievalStoreScope::Profile,
        }
    }

    #[cfg(test)]
    pub(super) fn project_for_test(
        project_root: &'a Path,
        _sessions_db_path: &'a Path,
        retrieval_service: Option<&'a dyn SessionRetrievalServicePort>,
    ) -> Self {
        Self {
            project_root: Some(project_root),
            retained_session_db: None,
            retrieval_service,
            retrieval_store_scope: SessionRetrievalStoreScope::Project,
        }
    }
}

fn lcm_unavailable(args: &Value) -> ToolResult {
    tool_json(
        None,
        args,
        &json!({
            "status": "unavailable",
            "message": "could not open active project tracedecay session database",
        }),
    )
}

pub(super) struct LcmStorage {
    pub(super) db: Arc<RegisteredGlobalDb>,
}

pub(super) enum LcmStorageResolution {
    Available(Box<LcmStorage>),
    Unavailable(ToolResult),
}

pub(super) fn resolve_lcm_storage(
    context: LcmHandlerContext<'_>,
    args: &Value,
) -> LcmStorageResolution {
    if let Some(db) = context.retained_session_db {
        return LcmStorageResolution::Available(Box::new(LcmStorage { db: Arc::clone(db) }));
    }
    LcmStorageResolution::Unavailable(lcm_unavailable(args))
}
