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

/// How an LCM storage open treats the backing sessions.db.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum LcmOpenMode {
    /// Writable open: creates the store and ensures schema as needed.
    Writable,
    /// Read-only: a missing store is a hard error.
    ReadOnlyExisting,
    /// Read-only: a missing store is a distinguishable `not_ingested`
    /// result, without creating the file. Use this for every `readOnlyHint`
    /// LCM handler so "nothing ingested yet" never looks like "ok, 0 rows"
    /// (and the tool never ghost-creates an empty sessions.db).
    ReadOnlyOrMissing,
}

pub(super) async fn open_lcm_storage(
    context: LcmHandlerContext<'_>,
    args: &Value,
    _mode: LcmOpenMode,
) -> LcmStorageResolution {
    if let Some(db) = context.retained_session_db {
        return LcmStorageResolution::Available(Box::new(LcmStorage { db: Arc::clone(db) }));
    }
    LcmStorageResolution::Unavailable(lcm_unavailable(args))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn missing_retained_authority_never_opens_a_daemon_session_store() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("sessions.db");

        let resolution = open_lcm_storage(
            LcmHandlerContext::user(&db_path, None, None),
            &json!({}),
            LcmOpenMode::Writable,
        )
        .await;

        assert!(matches!(resolution, LcmStorageResolution::Unavailable(_)));
        assert!(!db_path.exists());
    }

    #[tokio::test]
    async fn explicit_direct_context_still_requires_registered_authority() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("sessions.db");

        let resolution = open_lcm_storage(
            LcmHandlerContext::user(&db_path, None, None),
            &json!({}),
            LcmOpenMode::Writable,
        )
        .await;

        assert!(matches!(resolution, LcmStorageResolution::Unavailable(_)));
        assert!(!db_path.exists());
    }
}
