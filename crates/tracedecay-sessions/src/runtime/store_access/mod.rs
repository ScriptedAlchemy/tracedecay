//! Session-store operations hosted on [`SessionStoreAccess`].
//!
//! These are the former `RegisteredGlobalDb` SQL/LCM adapter bodies. Global-db
//! keeps thin inherent wrappers so existing lease call sites keep working.

mod codex_goal_reconciliation;
mod lcm;
mod search;
mod session_sync;
mod sessions;
mod transcript;
mod types;

pub use codex_goal_reconciliation::find_preceding_codex_goal_response;
pub use search::{
    SESSION_MESSAGE_SEARCH_MAX_FETCH, downrank_inventory_messages,
    interleave_workflow_search_results, session_fts_query,
};
pub use sessions::SESSION_MESSAGES_AFTER_SQL;
pub use transcript::{get_parse_offset, require_expected_offset, set_parse_offset};
pub use types::{
    SessionActivityRow, SessionIngestHealth, SessionProviderCoverage, SessionProviderCoverageState,
    TranscriptBatch, TranscriptPersistenceError, UNIX_TIMESTAMP_MILLIS_THRESHOLD,
};
