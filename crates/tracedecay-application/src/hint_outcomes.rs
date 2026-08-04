//! Typed application port for post-hoc hook-hint outcome correlation.
//!
//! Hook adapters and correlation policy consume these semantic records. The
//! daemon composition owns the concrete analytics/session stores and maps
//! their rows into this boundary.

use std::future::Future;
use std::pin::Pin;

use thiserror::Error;

pub type HintOutcomePortFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, HintOutcomePortErrorV1>> + Send + 'a>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HintOutcomePortOperationV1 {
    QueryResolvedHints,
    QueryEmittedHints,
    QuerySessionActivity,
    AppendOutcomes,
}

impl HintOutcomePortOperationV1 {
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
pub struct HintOutcomePortErrorV1 {
    operation: &'static str,
    detail: String,
}

impl HintOutcomePortErrorV1 {
    pub fn new(operation: HintOutcomePortOperationV1, detail: impl Into<String>) -> Self {
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
pub struct HintEmissionV1 {
    pub provider: String,
    pub project_id: String,
    pub session_id: String,
    pub timestamp: i64,
    pub category: String,
    pub hint_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HintToolActivityV1 {
    pub timestamp: i64,
    pub tool_names: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HintOutcomeResolutionV1 {
    Acted { tool_name: String },
    Ignored,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HintOutcomeObservationV1 {
    pub emission: HintEmissionV1,
    pub observed_at_secs: i64,
    pub resolution: HintOutcomeResolutionV1,
}

/// Store-neutral reads and writes required by one bounded correlation pass.
///
/// The port deliberately exposes neither database handles nor storage rows.
/// Implementations must preserve the exact project filter and return a typed
/// error instead of fabricating an empty result.
pub trait HintOutcomeCorrelationPortV1: Send + Sync {
    fn resolved_hint_ids<'a>(
        &'a self,
        project_id: &'a str,
        limit: u32,
    ) -> HintOutcomePortFuture<'a, Vec<String>>;

    fn emitted_hints<'a>(
        &'a self,
        project_id: &'a str,
        limit: u32,
    ) -> HintOutcomePortFuture<'a, Vec<HintEmissionV1>>;

    fn session_tool_activity<'a>(
        &'a self,
        provider: &'a str,
        session_id: &'a str,
        after_timestamp: i64,
        limit: u32,
    ) -> HintOutcomePortFuture<'a, Vec<HintToolActivityV1>>;

    fn append_outcomes<'a>(
        &'a self,
        outcomes: &'a [HintOutcomeObservationV1],
    ) -> HintOutcomePortFuture<'a, ()>;
}
