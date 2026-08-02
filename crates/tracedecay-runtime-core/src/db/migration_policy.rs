//! Whether this process may execute migrations, or must only detect them.
//!
//! The running system — daemon, MCP tool calls, CLI tool invocations,
//! background workers, store opens — never auto-executes migrations. Only the
//! explicit `tracedecay migrate` command runs them. Everything else opens
//! under [`MigrationPolicyV1::ExplicitOnly`], detects pending work through the
//! schema stamps, and fails typed or serves degraded with a remedy that names
//! `tracedecay migrate`.
//!
//! This covers both schema (DDL ladder) migrations and one-time derived-data
//! migrations (projection version cutovers, repair drains, format rebuilds).
//! Continuous derived maintenance — incremental indexing, incremental
//! embedding, retention — is not a migration and is not gated here.

/// Execution permission for schema and one-time derived-data migrations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MigrationPolicyV1 {
    /// Detect pending migrations and refuse to execute them. The default for
    /// every long-running or request-serving process.
    ExplicitOnly,
    /// Execute pending migrations. Held only by `tracedecay migrate`, by
    /// first-time store initialization (creating a brand-new store is not a
    /// migration), and by tests that construct fixture stores.
    Permitted,
}

impl MigrationPolicyV1 {
    /// Whether executing a pending migration is allowed under this policy.
    #[must_use]
    pub const fn may_execute(self) -> bool {
        matches!(self, Self::Permitted)
    }

    /// The remedy a typed refusal must name so the operator knows the one
    /// sanctioned way to run the pending work.
    #[must_use]
    pub const fn remedy() -> &'static str {
        "run `tracedecay migrate` to execute pending migrations"
    }
}
