//! Typed application port for post-hoc hook-hint outcome correlation.
//!
//! Hook adapters and correlation policy consume these semantic records. The
//! daemon composition owns the concrete analytics/session stores and maps
//! their rows into this boundary.

use std::future::Future;
use std::pin::Pin;

use thiserror::Error;

pub type HintOutcomePortFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, HintOutcomePortError>> + Send + 'a>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HintOutcomePortOperation {
    QueryResolvedHints,
    QueryEmittedHints,
    QuerySessionActivity,
    AppendOutcomes,
}

impl HintOutcomePortOperation {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::QueryResolvedHints => "query_resolved_hints",
            Self::QueryEmittedHints => "query_emitted_hints",
            Self::QuerySessionActivity => "query_session_activity",
            Self::AppendOutcomes => "append_outcomes",
        }
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
#[error("hint-outcome port {operation} failed: {detail}")]
pub struct HintOutcomePortError {
    operation: &'static str,
    detail: String,
}

impl HintOutcomePortError {
    pub fn new(operation: HintOutcomePortOperation, detail: impl Into<String>) -> Self {
        Self {
            operation: operation.as_str(),
            detail: detail.into(),
        }
    }

    pub const fn operation(&self) -> &'static str {
        self.operation
    }

    pub fn detail(&self) -> &str {
        &self.detail
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HintEmission {
    pub provider: String,
    pub project_id: String,
    pub session_id: String,
    pub timestamp: i64,
    pub category: String,
    pub hint_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HintToolActivity {
    pub timestamp: i64,
    pub tool_names: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HintOutcomeResolution {
    Acted { tool_name: String },
    Ignored,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HintOutcomeObservation {
    pub emission: HintEmission,
    pub observed_at_secs: i64,
    pub resolution: HintOutcomeResolution,
}

/// Store-neutral reads and writes required by one bounded correlation pass.
///
/// The port deliberately exposes neither database handles nor storage rows.
/// Implementations must preserve the exact project filter and return a typed
/// error instead of fabricating an empty result.
pub trait HintOutcomeCorrelationPort: Send + Sync {
    fn resolved_hint_ids<'a>(
        &'a self,
        project_id: &'a str,
        limit: u32,
    ) -> HintOutcomePortFuture<'a, Vec<String>>;

    fn emitted_hints<'a>(
        &'a self,
        project_id: &'a str,
        limit: u32,
    ) -> HintOutcomePortFuture<'a, Vec<HintEmission>>;

    fn session_tool_activity<'a>(
        &'a self,
        provider: &'a str,
        session_id: &'a str,
        after_timestamp: i64,
        limit: u32,
    ) -> HintOutcomePortFuture<'a, Vec<HintToolActivity>>;

    fn append_outcomes<'a>(
        &'a self,
        outcomes: &'a [HintOutcomeObservation],
    ) -> HintOutcomePortFuture<'a, ()>;
}
