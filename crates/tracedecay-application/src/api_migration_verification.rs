use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Formatter evidence captured before an immutable migration is published.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ApiMigrationFormatterReportV1 {
    pub checked_files: Vec<String>,
    pub would_change_files: Vec<String>,
    pub not_applicable_files: Vec<String>,
}

/// File-scoped diagnostic changes observed across the exact migration.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ApiMigrationDiagnosticDeltaV1 {
    pub introduced_errors: usize,
    pub resolved_errors: usize,
    pub introduced_warnings: usize,
    pub resolved_warnings: usize,
}
