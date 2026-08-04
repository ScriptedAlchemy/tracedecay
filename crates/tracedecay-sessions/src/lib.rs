#![deny(clippy::all)]
#![warn(clippy::pedantic)]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![allow(clippy::module_name_repetitions)]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::cast_precision_loss)]
#![allow(clippy::cast_possible_wrap)]
#![allow(clippy::too_many_lines)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::struct_excessive_bools)]
#![allow(clippy::similar_names)]
#![allow(clippy::wildcard_imports)]
#![allow(clippy::collapsible_if)]
#![allow(clippy::unnecessary_wraps)]
#![allow(clippy::single_match)]
#![allow(clippy::needless_borrow)]
#![allow(clippy::map_unwrap_or)]
#![allow(clippy::redundant_closure)]
#![allow(clippy::redundant_closure_for_method_calls)]
#![allow(clippy::format_push_string)]

//! Provider-neutral session parsing, correlation, and LCM contracts.

use serde::{Deserialize, Serialize};

pub mod compatibility;
pub mod provider;
pub mod runtime;

#[cfg(test)]
mod worktree {
    pub use tracedecay_runtime_core::worktree::*;
}

pub mod git_correlation {
    pub use crate::runtime::git_correlation::*;
}

pub mod lcm {
    pub use crate::runtime::lcm::*;
}

pub mod workflow_index {
    pub use crate::runtime::workflow_index::*;
}

pub mod codex_app_server {
    pub use crate::runtime::codex_app_server::*;
}

pub const USER_SESSIONS_DB_FILENAME: &str = "user-sessions.db";

pub fn user_sessions_db_path(profile_root: &std::path::Path) -> std::path::PathBuf {
    profile_root.join(USER_SESSIONS_DB_FILENAME)
}

pub struct SessionQueryDb {
    database: tracedecay_runtime_core::db::Database,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionToolUsageRow {
    pub tool_names: String,
    pub text: String,
    pub metadata_json: String,
}

impl SessionQueryDb {
    pub async fn open_read_only_at(path: &std::path::Path) -> Option<Self> {
        if !path.is_file() {
            return None;
        }
        let authority = tracedecay_runtime_core::db::DatabaseAuthority::for_runtime(
            path,
            "open session query database",
        )
        .ok()?;
        let (database, _) = tracedecay_runtime_core::db::Database::open_read_only(path, &authority)
            .await
            .ok()?;
        Some(Self { database })
    }

    pub async fn lcm_grep(
        &self,
        request: runtime::lcm::LcmGrepRequest,
    ) -> Result<runtime::lcm::LcmGrepOutcome, runtime::lcm::LcmError> {
        runtime::lcm::query::grep(
            self.database.conn(),
            request,
            runtime::lcm::LcmGrepFilters::default(),
        )
        .await
    }

    pub async fn lcm_recent_sessions(
        &self,
        provider: Option<&str>,
        limit: usize,
    ) -> Result<Vec<runtime::lcm::LcmRecentSession>, runtime::lcm::LcmError> {
        runtime::lcm::query::recent_sessions(self.database.conn(), provider, limit).await
    }

    pub async fn lcm_session_providers(
        &self,
        session_id: &str,
    ) -> Result<Vec<String>, runtime::lcm::LcmError> {
        runtime::lcm::query::session_providers(self.database.conn(), session_id).await
    }

    pub async fn lcm_session_replay_slice(
        &self,
        request: &runtime::lcm::LcmSessionReplayRequest,
    ) -> Result<runtime::lcm::LcmSessionReplaySlice, runtime::lcm::LcmError> {
        runtime::lcm::query::session_replay_slice(self.database.conn(), request).await
    }

    pub async fn session_tool_usage_rows(
        &self,
        limit: usize,
    ) -> Result<Vec<SessionToolUsageRow>, String> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let mut rows = self
            .database
            .conn()
            .query(
                "SELECT COALESCE(tool_names, '') AS tool_names,
                        COALESCE(text, '') AS text,
                        COALESCE(metadata_json, '') AS metadata_json
                 FROM session_messages
                 ORDER BY timestamp, ordinal
                 LIMIT ?1",
                [i64::try_from(limit).unwrap_or(i64::MAX)],
            )
            .await
            .map_err(|error| format!("failed to query session tool usage rows: {error}"))?;
        let mut result = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| format!("failed to read session tool usage rows: {error}"))?
        {
            result.push(SessionToolUsageRow {
                tool_names: row.get::<String>(0).map_err(|error| {
                    format!("failed to decode session tool usage tool_names: {error}")
                })?,
                text: row.get::<String>(1).map_err(|error| {
                    format!("failed to decode session tool usage text: {error}")
                })?,
                metadata_json: row.get::<String>(2).map_err(|error| {
                    format!("failed to decode session tool usage metadata_json: {error}")
                })?,
            });
        }
        Ok(result)
    }
}

pub use provider::{ProviderScope, SessionProvider};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRecord {
    pub provider: String,
    pub session_id: String,
    pub project_key: String,
    pub project_path: String,
    pub title: Option<String>,
    pub started_at: Option<i64>,
    pub ended_at: Option<i64>,
    pub transcript_path: Option<String>,
    pub metadata_json: Option<String>,
    pub parent_session_id: Option<String>,
    pub is_subagent: bool,
    pub agent_id: Option<String>,
    pub parent_tool_use_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMessageRecord {
    pub provider: String,
    pub message_id: String,
    pub session_id: String,
    pub role: String,
    pub timestamp: Option<i64>,
    pub ordinal: i64,
    pub text: String,
    pub kind: Option<String>,
    pub model: Option<String>,
    pub tool_names: Option<String>,
    pub source_path: Option<String>,
    pub source_offset: Option<i64>,
    pub metadata_json: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionMessageSearchResult {
    pub session: SessionRecord,
    pub message: SessionMessageRecord,
    pub score: f64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSearchTimeRange {
    pub start_time: Option<i64>,
    pub end_time: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionSearchScope {
    All,
    ParentsOnly,
    SubagentsOnly,
}

impl SessionSearchScope {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "all" => Some(Self::All),
            "parents_only" => Some(Self::ParentsOnly),
            "subagents_only" => Some(Self::SubagentsOnly),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::ParentsOnly => "parents_only",
            Self::SubagentsOnly => "subagents_only",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionMessageType {
    #[default]
    All,
    DirectUser,
    ToolResult,
}

impl SessionMessageType {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "all" => Some(Self::All),
            "direct_user" => Some(Self::DirectUser),
            "tool_result" => Some(Self::ToolResult),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::DirectUser => "direct_user",
            Self::ToolResult => "tool_result",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionSearchFilters<'a> {
    pub scope: SessionSearchScope,
    pub message_type: SessionMessageType,
    pub parent_session_id: Option<&'a str>,
    pub time_range: SessionSearchTimeRange,
}

impl Default for SessionSearchFilters<'_> {
    fn default() -> Self {
        Self {
            scope: SessionSearchScope::All,
            message_type: SessionMessageType::All,
            parent_session_id: None,
            time_range: SessionSearchTimeRange::default(),
        }
    }
}

pub(crate) fn current_timestamp() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs() as i64)
}
