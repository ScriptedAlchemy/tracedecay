pub use tracedecay_sessions::runtime::workflow_index::{
    INGEST_WATERMARK_KEY, MAX_WORKFLOW_LIMIT, WORKFLOW_INDEX_SCHEMA_VERSION, WorkflowAgent,
    WorkflowIndexError, WorkflowRun, WorkflowStatus, agents_for_run, bump_ingest_watermark,
    read_ingest_watermark, run_for_id, runs_for_git_scope, runs_for_session, tables_present,
    upsert_agent, upsert_run,
};
pub(crate) use tracedecay_sessions::runtime::workflow_index::{
    ensure_workflow_index_schema, workflow_scope_exists_predicate,
};
