//! Canonical CLI/MCP wire contracts for the side-effecting operations the
//! project's graph-tool owner serves: a managed affected-test run and the
//! project dashboard server.
//!
//! Presentation-only transport keys such as `format` are removed before these
//! request bodies are decoded.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::OperationReceipt;

/// The cargo profile an affected-test run builds with.
#[derive(Clone, Copy, Debug, Default, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TestProfileV1 {
    #[default]
    Debug,
    Release,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RunAffectedTestsSurfaceRequestV1 {
    /// Explicit manifest of project-relative file paths used to compute
    /// affected tests.
    pub changed_paths: Vec<String>,
    /// Cargo profile (default: debug).
    pub profile: Option<TestProfileV1>,
    /// Maximum wall time in seconds before the cargo subprocess is killed,
    /// 1 through 300 (default: 300).
    #[schemars(range(min = 1, max = 300))]
    pub timeout_secs: Option<u64>,
    /// Cap on tests dispatched in a single invocation, 1 through 500
    /// (default: 100).
    #[schemars(range(min = 1, max = 500))]
    pub max_tests: Option<u64>,
}

/// Why a run failed or was refused.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AffectedTestErrorV1 {
    pub kind: String,
    pub operation: String,
    pub message: String,
}

/// One observed libtest outcome.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AffectedTestOutcomeV1 {
    pub test: String,
    pub passed: bool,
    /// Graph symbols this test was selected to cover.
    pub covers_source_ids: Vec<String>,
}

/// The managed test-run operation the run was recorded under.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedTestTerminalV1 {
    pub operation_id: String,
    /// The tool that reads the recorded results back.
    pub result_tool: String,
    pub receipt: OperationReceipt,
}

/// A dispatched run, completed or failed, with what it observed.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AffectedTestRunV1 {
    pub exit_code: Option<i32>,
    pub passed: u64,
    pub failed: u64,
    pub total_observed: u64,
    pub dispatched_tests: Vec<String>,
    /// More affected tests existed than `max_tests` admitted.
    pub truncated: bool,
    pub results: Vec<AffectedTestOutcomeV1>,
    pub stderr_tail: String,
    pub stdout_tail: String,
    pub terminal: ManagedTestTerminalV1,
    /// Present when the run did not complete.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<AffectedTestErrorV1>,
}

/// No test was dispatched: nothing covers the change, or the request was
/// refused before a run began.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AffectedTestsNotRunV1 {
    /// Always 0.
    pub passed: u64,
    /// Always 0.
    pub failed: u64,
    /// Always empty.
    pub results: Vec<AffectedTestOutcomeV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<AffectedTestErrorV1>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum RunAffectedTestsResultV1 {
    Ran(Box<AffectedTestRunV1>),
    NotRun(AffectedTestsNotRunV1),
}

/// What `tracedecay_dashboard` does.
#[derive(Clone, Copy, Debug, Default, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DashboardActionV1 {
    #[default]
    Start,
    Stop,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DashboardSurfaceRequestV1 {
    /// Action to perform (default: "start"). "stop" shuts down a previously
    /// started dashboard if any.
    pub action: Option<DashboardActionV1>,
    /// Loopback host address to bind: 127.0.0.1, localhost, or ::1
    /// (default: "127.0.0.1"). Wildcard, LAN, public IPs, and other
    /// hostnames are rejected.
    pub host: Option<String>,
    /// Port to listen on; 0 picks an ephemeral port (default: 7341).
    pub port: Option<u16>,
}

/// A dashboard that was already bound when `start` was asked again.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DashboardBoundV1 {
    pub url: String,
    pub host: String,
    pub port: u16,
    pub requested_host: String,
    pub requested_port: u16,
    /// Whether the bound port is the one asked for (always true for 0).
    pub requested_port_honored: bool,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum DashboardResultV1 {
    Started {
        url: String,
        host: String,
        port: u16,
    },
    AlreadyRunning(DashboardBoundV1),
    /// The bound server is shutting down.
    Stopping(DashboardBoundV1),
    Stopped {
        previous_url: String,
    },
    NotRunning,
}
