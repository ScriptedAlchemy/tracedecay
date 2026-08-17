use thiserror::Error;

#[derive(Error, Debug)]
#[error("{detail}")]
struct HookRuntimeErrorContext {
    reason_code: String,
    retryable: bool,
    detail: String,
    /// The admission authority's own disposition, in the canonical
    /// `HostAdmissionStatus` wire form, when one produced this failure.
    ///
    /// The status enum is defined above this crate (`tracedecay-sessions`
    /// depends on this kernel, not the other way round), so the value travels
    /// as its serde wire string and the hook boundary reconstitutes it typed
    /// with `HostAdmissionStatus::from_wire`. Carrying it verbatim is what
    /// keeps the boundary from re-deriving a status by matching reason-code
    /// strings.
    status: Option<String>,
}

/// Errors that can occur during code graph operations.
#[derive(Error, Debug)]
pub enum TraceDecayError {
    #[error("file error: {message} (path: {path})")]
    File { message: String, path: String },

    #[error("parse error: {message} (path: {path}, line: {line:?})")]
    Parse {
        message: String,
        path: String,
        line: Option<u32>,
    },

    #[error("database error: {message} (operation: {operation})")]
    Database { message: String, operation: String },

    #[error("search error: {message} (query: {query})")]
    Search { message: String, query: String },

    #[error("config error: {message}")]
    Config { message: String },

    #[error(
        "{component} profile schema {found_version:?} is incompatible with required schema \
         {required_version}; reset the profile"
    )]
    ProfileResetRequired {
        component: &'static str,
        found_version: Option<i64>,
        required_version: i64,
    },

    #[error("{authority} persisted shape requires reset: {reason}")]
    ResetRequired { authority: String, reason: String },

    #[error("project route error ({reason_code}): {detail}")]
    ProjectRoute {
        reason_code: String,
        retryable: bool,
        detail: String,
    },

    #[error("sync lock: {message}")]
    SyncLock { message: String },

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("SQLite error: {0}")]
    Sqlite(#[from] crate::db::SqliteDriverError),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error(transparent)]
    Automation(#[from] tracedecay_automation::AutomationError),
}

/// Convenience alias for results using `TraceDecayError`.
pub type Result<T> = std::result::Result<T, TraceDecayError>;

impl From<crate::db::engine::Error> for TraceDecayError {
    fn from(error: crate::db::engine::Error) -> Self {
        Self::Sqlite(error.into())
    }
}

impl From<tracedecay_lsp::analyzer::AnalyzerRuntimeError> for TraceDecayError {
    fn from(error: tracedecay_lsp::analyzer::AnalyzerRuntimeError) -> Self {
        Self::Config {
            message: error.message().to_string(),
        }
    }
}

/// Flatten an error and its [`std::error::Error::source`] chain into one
/// message string.
///
/// Many error families embed their source's `Display` inside their own — e.g.
/// `#[error("SQLite error: {0}")]` paired with `#[from]` (the displayed field
/// *is* the `#[source]`), or every `std::io::Error::other` wrapper (its
/// `Display` delegates straight to the wrapped error). Naively appending each
/// layer's `to_string()` would then double the tail into `"...: E: E"` or
/// `"...: msg: msg"`. To avoid that, a layer is only appended when the
/// accumulated message does not already end with that layer's text.
fn flatten_error_chain(source: &(dyn std::error::Error + 'static)) -> String {
    let mut message = source.to_string();
    let mut layer = source.source();
    while let Some(current) = layer {
        let text = current.to_string();
        if !message.ends_with(&text) {
            message.push_str(": ");
            message.push_str(&text);
        }
        layer = current.source();
    }
    message
}

impl TraceDecayError {
    pub fn reset_required(authority: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::ResetRequired {
            authority: authority.into(),
            reason: reason.into(),
        }
    }

    pub fn reset_required_context(&self) -> Option<(&str, &str)> {
        let Self::ResetRequired { authority, reason } = self else {
            return None;
        };
        Some((authority, reason))
    }

    pub fn project_route(
        reason_code: impl Into<String>,
        retryable: bool,
        detail: impl Into<String>,
    ) -> Self {
        Self::ProjectRoute {
            reason_code: reason_code.into(),
            retryable,
            detail: detail.into(),
        }
    }

    pub fn project_route_context(&self) -> Option<(&str, bool, &str)> {
        let Self::ProjectRoute {
            reason_code,
            retryable,
            detail,
        } = self
        else {
            return None;
        };
        Some((reason_code, *retryable, detail))
    }

    pub fn database_operation(
        operation: impl Into<String>,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::Database {
            operation: operation.into(),
            message: flatten_error_chain(&source),
        }
    }

    pub fn is_database_error(&self) -> bool {
        matches!(self, Self::Database { .. })
    }

    /// A hook-runtime failure raised without an admission authority behind it
    /// (spool I/O, refresh ownership, test fixtures).
    ///
    /// Prefer [`Self::hook_runtime_with_status`] wherever an admission outcome
    /// is in hand: a status recorded here is reported verbatim instead of
    /// being inferred at the hook boundary.
    pub fn hook_runtime(
        reason_code: impl Into<String>,
        retryable: bool,
        detail: impl Into<String>,
    ) -> Self {
        Self::hook_runtime_context_error(reason_code, retryable, detail, None)
    }

    /// A hook-runtime failure that carries the admission authority's own
    /// status, in `HostAdmissionStatus` wire form.
    pub fn hook_runtime_with_status(
        reason_code: impl Into<String>,
        retryable: bool,
        detail: impl Into<String>,
        status: impl Into<String>,
    ) -> Self {
        Self::hook_runtime_context_error(reason_code, retryable, detail, Some(status.into()))
    }

    fn hook_runtime_context_error(
        reason_code: impl Into<String>,
        retryable: bool,
        detail: impl Into<String>,
        status: Option<String>,
    ) -> Self {
        Self::Io(std::io::Error::other(HookRuntimeErrorContext {
            reason_code: reason_code.into(),
            retryable,
            detail: detail.into(),
            status,
        }))
    }

    pub fn hook_runtime_context(&self) -> Option<(&str, bool, &str)> {
        let context = self.hook_runtime_error_context()?;
        Some((&context.reason_code, context.retryable, &context.detail))
    }

    /// The admission status recorded with this failure, in wire form.
    pub fn hook_runtime_status(&self) -> Option<&str> {
        self.hook_runtime_error_context()?.status.as_deref()
    }

    fn hook_runtime_error_context(&self) -> Option<&HookRuntimeErrorContext> {
        let Self::Io(error) = self else {
            return None;
        };
        error.get_ref()?.downcast_ref::<HookRuntimeErrorContext>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_error_display_includes_message_and_path() {
        let err = TraceDecayError::File {
            message: "not found".to_string(),
            path: "/tmp/foo.rs".to_string(),
        };
        let s = err.to_string();
        assert!(s.contains("not found"), "message missing: {s}");
        assert!(s.contains("/tmp/foo.rs"), "path missing: {s}");
    }

    #[test]
    fn parse_error_display_includes_line() {
        let err = TraceDecayError::Parse {
            message: "unexpected token".to_string(),
            path: "src/main.rs".to_string(),
            line: Some(42),
        };
        let s = err.to_string();
        assert!(s.contains("unexpected token"), "{s}");
        assert!(s.contains("src/main.rs"), "{s}");
        assert!(s.contains("42"), "{s}");
    }

    #[test]
    fn parse_error_display_no_line() {
        let err = TraceDecayError::Parse {
            message: "eof".to_string(),
            path: "src/lib.rs".to_string(),
            line: None,
        };
        let s = err.to_string();
        assert!(s.contains("eof"), "{s}");
    }

    #[test]
    fn database_error_display_includes_operation() {
        let err = TraceDecayError::Database {
            message: "constraint violated".to_string(),
            operation: "INSERT".to_string(),
        };
        let s = err.to_string();
        assert!(s.contains("constraint violated"), "{s}");
        assert!(s.contains("INSERT"), "{s}");
    }

    #[test]
    fn reset_required_preserves_typed_authority_and_reason() {
        let error =
            TraceDecayError::reset_required("configuration", "persisted format is not final");

        assert_eq!(
            error.reset_required_context(),
            Some(("configuration", "persisted format is not final"))
        );
    }

    #[test]
    fn engine_error_converts_without_exposing_the_private_engine_type() {
        let err: TraceDecayError =
            crate::db::engine::Error::Runtime("writer unavailable".to_string()).into();

        assert!(matches!(err, TraceDecayError::Sqlite(_)));
        assert!(err.to_string().contains("writer unavailable"));
    }

    #[test]
    fn database_operation_preserves_public_database_classification() {
        let err = TraceDecayError::database_operation(
            "SELECT observations",
            std::io::Error::other("database unavailable"),
        );

        let TraceDecayError::Database { operation, message } = &err else {
            panic!("database operation must retain the public Database variant");
        };
        assert_eq!(operation, "SELECT observations");
        assert_eq!(message, "database unavailable");
        assert!(err.is_database_error());
        assert!(err.to_string().contains("SELECT observations"));
    }

    #[test]
    fn database_operation_does_not_double_self_displaying_chain() {
        use std::error::Error;
        use std::fmt;

        #[derive(Debug)]
        struct Inner;
        impl fmt::Display for Inner {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "disk full")
            }
        }
        impl Error for Inner {}

        // Mimics the reachable self-displaying families (e.g.
        // `#[error("SQLite error: {0}")]` + `#[from]`, or `io::Error::other`):
        // `Display` embeds the source's own `Display`, and `source()` returns
        // that same error, so the outer text already ends with the inner text.
        #[derive(Debug)]
        struct Outer(Inner);
        impl fmt::Display for Outer {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "SQLite error: {}", self.0)
            }
        }
        impl Error for Outer {
            fn source(&self) -> Option<&(dyn Error + 'static)> {
                Some(&self.0)
            }
        }

        let err = TraceDecayError::database_operation("SELECT nodes", Outer(Inner));
        let TraceDecayError::Database { operation, message } = &err else {
            panic!("expected Database variant");
        };
        assert_eq!(operation, "SELECT nodes");
        // Without the ends-with guard this would be "SQLite error: disk full: disk full".
        assert_eq!(message, "SQLite error: disk full");
        assert_eq!(
            message.matches("disk full").count(),
            1,
            "source layer must not be doubled: {message}"
        );
    }

    #[test]
    fn database_operation_appends_distinct_chain_layers() {
        use std::error::Error;
        use std::fmt;

        #[derive(Debug)]
        struct Root;
        impl fmt::Display for Root {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "connection refused")
            }
        }
        impl Error for Root {}

        // A layer whose Display does NOT embed its source must still contribute
        // the deeper cause, so genuinely distinct chains are preserved.
        #[derive(Debug)]
        struct Middle(Root);
        impl fmt::Display for Middle {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "query failed")
            }
        }
        impl Error for Middle {
            fn source(&self) -> Option<&(dyn Error + 'static)> {
                Some(&self.0)
            }
        }

        let err = TraceDecayError::database_operation("UPDATE nodes", Middle(Root));
        let TraceDecayError::Database { message, .. } = &err else {
            panic!("expected Database variant");
        };
        assert_eq!(message, "query failed: connection refused");
    }

    #[test]
    fn hook_runtime_error_preserves_typed_context() {
        let err = TraceDecayError::hook_runtime("cursor_conflict", true, "cursor advanced");

        assert_eq!(
            err.hook_runtime_context(),
            Some(("cursor_conflict", true, "cursor advanced"))
        );
        assert!(err.hook_runtime_status().is_none());
        assert!(err.to_string().contains("cursor advanced"));
    }

    #[test]
    fn hook_runtime_error_carries_the_admission_status_verbatim() {
        let err = TraceDecayError::hook_runtime_with_status(
            "project_authority_unbound",
            false,
            "daemon observation authority is unavailable",
            "unavailable",
        );

        assert_eq!(
            err.hook_runtime_context(),
            Some((
                "project_authority_unbound",
                false,
                "daemon observation authority is unavailable"
            ))
        );
        assert_eq!(err.hook_runtime_status(), Some("unavailable"));
    }

    #[test]
    fn search_error_display_includes_query() {
        let err = TraceDecayError::Search {
            message: "timeout".to_string(),
            query: "fn main".to_string(),
        };
        let s = err.to_string();
        assert!(s.contains("timeout"), "{s}");
        assert!(s.contains("fn main"), "{s}");
    }

    #[test]
    fn config_error_display() {
        let err = TraceDecayError::Config {
            message: "bad value".to_string(),
        };
        assert!(err.to_string().contains("bad value"));
    }

    #[test]
    fn automation_error_boundary_preserves_type_message_and_classification() {
        let err = TraceDecayError::from(tracedecay_automation::AutomationError::config(
            "timed out waiting for backend",
        ));

        assert!(matches!(&err, TraceDecayError::Automation(_)));
        assert_eq!(
            err.to_string(),
            "config error: timed out waiting for backend"
        );
        assert_eq!(
            tracedecay_automation::backend::classify_agent_task_error_message(&err.to_string()),
            tracedecay_automation::backend::AgentTaskFailureClass::Timeout,
        );
    }

    #[test]
    fn sync_lock_error_display() {
        let err = TraceDecayError::SyncLock {
            message: "already running".to_string(),
        };
        assert!(err.to_string().contains("already running"));
    }

    #[test]
    fn json_error_from_serde() {
        let serde_err = serde_json::from_str::<serde_json::Value>("bad json");
        let err: TraceDecayError = match serde_err {
            Err(e) => e.into(),
            Ok(_) => panic!("expected JSON parse error"),
        };
        assert!(err.to_string().contains("json error"));
    }
}
