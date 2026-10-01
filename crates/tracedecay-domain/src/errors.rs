use thiserror::Error;

use crate::ApplicationProblemDetailV1;

const STORE_OPEN_CANCELLED_REASON_CODE: &str = "store_open_cancelled";

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
    /// depends on domain, not the other way round), so the value travels
    /// as its serde wire string and the hook boundary reconstitutes it typed
    /// with `HostAdmissionStatus::from_wire`. Carrying it verbatim is what
    /// keeps the boundary from re-deriving a status by matching reason-code
    /// strings.
    status: Option<String>,
}

/// Display-preserving automation failure payload.
///
/// `tracedecay-automation` cannot be named from this crate (it depends on
/// domain). That crate implements `From<AutomationError>` into
/// [`TraceDecayError`].
#[derive(Debug, Error)]
#[error("{0}")]
pub struct AutomationErrorMessage(String);

impl AutomationErrorMessage {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[error("{message}")]
pub struct SqliteDriverError {
    message: String,
}

impl SqliteDriverError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

#[derive(Error, Debug)]
pub enum TraceDecayError {
    #[error("file error: {message} (path: {path})")]
    File { message: String, path: String },

    #[error("database error: {message} (operation: {operation})")]
    Database { message: String, operation: String },

    #[error("search error: {message} (query: {query})")]
    Search { message: String, query: String },

    #[error("config error: {message}")]
    Config { message: String },

    #[error(
        "host CLI `{program}` is unavailable for {lifecycle}; install it or add it to PATH and retry"
    )]
    HostCliUnavailable { program: String, lifecycle: String },

    #[error(
        "{component} profile schema {} is incompatible with required schema \
         {required_version}; reset the profile",
        .found_version.map_or_else(|| "unversioned".to_owned(), |version| version.to_string())
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
        typed_detail: Option<Box<ApplicationProblemDetailV1>>,
    },

    /// A project open failed for a reason its admission acts on: first-touch
    /// bootstrap, the read-only fallback, and the reopen backoff all match
    /// `kind`, never `detail`.
    #[error("{detail}")]
    ProjectOpen {
        kind: ProjectOpenFailureKind,
        detail: String,
    },

    /// A request the caller has to correct; repeating it unchanged repeats
    /// the answer.
    #[error("{message}")]
    InvalidRequest {
        reason: InvalidRequestReason,
        message: String,
    },

    /// A command answered with a typed refusal rather than failing; the
    /// process boundary names the refusal and its stable code.
    #[error(transparent)]
    ToolRefused(Box<ToolRefusal>),

    #[error("sync lock: {message}")]
    SyncLock { message: String },

    /// A writer lock stayed held by other writers past its admission
    /// deadline. `resource` names the lock class, never a filesystem path.
    #[error("{resource} stayed busy past its {deadline_ms}ms admission deadline")]
    LockDeadline {
        resource: &'static str,
        deadline_ms: u64,
    },

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("SQLite error: {0}")]
    Sqlite(#[from] SqliteDriverError),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error(transparent)]
    Automation(#[from] AutomationErrorMessage),
}

pub type Result<T> = std::result::Result<T, TraceDecayError>;

/// Why a project open failed, as the open's admission decides recovery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectOpenFailureKind {
    /// Identity resolution found no enrollment marker or registry match.
    IdentityUnregistered,
    /// The project has no index database yet; first-touch init creates it.
    IndexMissing,
    /// The project store refused a write; it can still be served read-only.
    StoreReadOnly,
    /// Every code-runtime seat is taken; one frees when another project
    /// retires.
    CodeRuntimeBudgetExhausted { limit: usize },
    /// The global authority audit judged persisted rows and rejected them.
    /// The verdict is a property of the stored data, so reopening repeats it
    /// unless `migration_pending` names a migration that can still clear it.
    AuthorityVerdict { migration_pending: bool },
    /// An earlier failure of this route is backed off until its retry time.
    BackedOff { retry_after_ms: u64 },
}

/// Why a request has to be corrected before it can succeed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InvalidRequestReason {
    MissingRequiredParameter,
    NotFound,
}

impl InvalidRequestReason {
    #[must_use]
    pub const fn reason_code(self) -> &'static str {
        match self {
            Self::MissingRequiredParameter => "missing_required_parameter",
            Self::NotFound => "not_found",
        }
    }
}

/// `code` is the refusal record's own code when the result carries one.
#[derive(Debug, Error)]
#[error("{tool} refused the request{}", refusal_suffix(code.as_deref(), reason.as_deref()))]
pub struct ToolRefusal {
    pub tool: String,
    pub code: Option<String>,
    pub reason: Option<String>,
}

impl TraceDecayError {
    pub fn tool_refused(
        tool: impl Into<String>,
        code: Option<String>,
        reason: Option<String>,
    ) -> Self {
        Self::ToolRefused(Box::new(ToolRefusal {
            tool: tool.into(),
            code,
            reason,
        }))
    }
}

fn refusal_suffix(code: Option<&str>, reason: Option<&str>) -> String {
    match (code, reason) {
        (Some(code), Some(reason)) => format!(" ({code}): {reason}"),
        (Some(code), None) => format!(" ({code})"),
        (None, Some(reason)) => format!(": {reason}"),
        (None, None) => String::new(),
    }
}

/// The one command that resets every profile-scoped persisted shape. Refused
/// shapes are never migrated or backed up: the reset deletes the old data and
/// the next open creates the shape the running binary writes.
pub const PROFILE_RESET_COMMAND: &str = "tracedecay wipe --all --yes";

/// Deletes exactly the stores the daemon reports in their typed
/// reset-required state and nothing else; the next open recreates each one
/// empty. Stores it cannot reset on their own name [`PROFILE_RESET_COMMAND`].
pub const STALE_STORE_RESET_COMMAND: &str = "tracedecay wipe --stale --yes";

/// A registered store [`STALE_STORE_RESET_COMMAND`] deletes on its own, named
/// by the `store` label [`StoreResetRequiredV1`] carries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResettableStoreV1 {
    ProfileSessions,
    ProjectSessions {
        project_id: String,
    },
    /// Every host's Hook V2 admission ledger for profile-scoped events.
    ProfileHookAdmissions,
    /// Every host's Hook V2 admission ledger, and pre-ledger pending work,
    /// in one project's hook data root.
    ProjectHookAdmissions {
        project_id: String,
    },
}

impl ResettableStoreV1 {
    const PROFILE_SESSIONS_LABEL: &'static str = "profile sessions";
    const PROJECT_SESSIONS_PREFIX: &'static str = "project sessions ";
    const PROFILE_HOOK_ADMISSIONS_LABEL: &'static str = "profile hook admissions";
    const PROJECT_HOOK_ADMISSIONS_PREFIX: &'static str = "project hook admissions ";

    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::ProfileSessions => Self::PROFILE_SESSIONS_LABEL.to_owned(),
            Self::ProjectSessions { project_id } => {
                format!("{}{project_id}", Self::PROJECT_SESSIONS_PREFIX)
            }
            Self::ProfileHookAdmissions => Self::PROFILE_HOOK_ADMISSIONS_LABEL.to_owned(),
            Self::ProjectHookAdmissions { project_id } => {
                format!("{}{project_id}", Self::PROJECT_HOOK_ADMISSIONS_PREFIX)
            }
        }
    }

    /// The store a [`Self::label`] names, `None` for a store that is not
    /// resettable on its own.
    #[must_use]
    pub fn from_label(label: &str) -> Option<Self> {
        match label {
            Self::PROFILE_SESSIONS_LABEL => return Some(Self::ProfileSessions),
            Self::PROFILE_HOOK_ADMISSIONS_LABEL => return Some(Self::ProfileHookAdmissions),
            _ => {}
        }
        let project_id = |prefix: &str| {
            label
                .strip_prefix(prefix)
                .filter(|project_id| !project_id.is_empty())
                .map(str::to_owned)
        };
        project_id(Self::PROJECT_SESSIONS_PREFIX)
            .map(|project_id| Self::ProjectSessions { project_id })
            .or_else(|| {
                project_id(Self::PROJECT_HOOK_ADMISSIONS_PREFIX)
                    .map(|project_id| Self::ProjectHookAdmissions { project_id })
            })
    }
}

/// A persisted store the daemon keeps mounted in a typed reset-required state
/// instead of refusing to serve: every read against it returns the typed
/// refusal, stores that admit keep serving, and `remedy` is the exact command
/// the operator runs to reset it.
#[derive(
    Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct StoreResetRequiredV1 {
    pub store: String,
    pub authority: String,
    pub found_version: Option<i64>,
    pub required_version: Option<i64>,
    pub reason: String,
    pub remedy: String,
}

/// Flatten an error and its [`std::error::Error::source`] chain into one
/// message string.
///
/// Many error families embed their source's `Display` inside their own, e.g.
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

/// Why a host cannot be reached on this machine. An absent host is
/// informational everywhere TraceDecay reports hosts: it is never an issue, a
/// warning, a pending operator action, or a non-zero exit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostAbsence {
    NotInstalled,
}

impl HostAbsence {
    pub fn reason(self) -> &'static str {
        match self {
            Self::NotInstalled => "not installed",
        }
    }
}

impl TraceDecayError {
    /// The host absence this error reports when it came from resolving a
    /// host's own CLI, `None` for every real failure.
    pub fn host_absence(&self) -> Option<HostAbsence> {
        match self {
            Self::HostCliUnavailable { .. } => Some(HostAbsence::NotInstalled),
            _ => None,
        }
    }

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

    /// Whether this is a persisted-shape refusal a store is served in until
    /// its reset.
    #[must_use]
    pub fn is_store_reset_required(&self) -> bool {
        matches!(
            self,
            Self::ResetRequired { .. } | Self::ProfileResetRequired { .. }
        )
    }

    /// The typed reset-required state `store` is mounted in when this error is
    /// a profile-scoped persisted-shape refusal, `None` for any other failure.
    /// `remedy` is the command that resets `store`.
    pub fn store_reset_required(
        &self,
        store: impl Into<String>,
        remedy: &str,
    ) -> Option<StoreResetRequiredV1> {
        let (authority, found_version, required_version) = match self {
            Self::ResetRequired { authority, .. } => (authority.clone(), None, None),
            Self::ProfileResetRequired {
                component,
                found_version,
                required_version,
            } => (
                (*component).to_owned(),
                *found_version,
                Some(*required_version),
            ),
            _ => return None,
        };
        Some(StoreResetRequiredV1 {
            store: store.into(),
            authority,
            found_version,
            required_version,
            reason: self.to_string(),
            remedy: remedy.to_owned(),
        })
    }

    pub fn project_open(kind: ProjectOpenFailureKind, detail: impl Into<String>) -> Self {
        Self::ProjectOpen {
            kind,
            detail: detail.into(),
        }
    }

    #[must_use]
    pub fn project_open_failure_kind(&self) -> Option<ProjectOpenFailureKind> {
        match self {
            Self::ProjectOpen { kind, .. } => Some(*kind),
            _ => None,
        }
    }

    pub fn missing_required_parameter(message: impl Into<String>) -> Self {
        Self::InvalidRequest {
            reason: InvalidRequestReason::MissingRequiredParameter,
            message: message.into(),
        }
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::InvalidRequest {
            reason: InvalidRequestReason::NotFound,
            message: message.into(),
        }
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
            typed_detail: None,
        }
    }

    /// Daemon shutdown stopped a store open or schema install at a safe point
    /// and rolled back its uncommitted work.
    pub fn store_open_cancelled(operation: impl std::fmt::Display) -> Self {
        Self::project_route(
            STORE_OPEN_CANCELLED_REASON_CODE,
            true,
            format!("{operation} was cancelled by daemon shutdown"),
        )
    }

    pub fn is_store_open_cancelled(&self) -> bool {
        self.project_route_context()
            .is_some_and(|(reason_code, _, _)| reason_code == STORE_OPEN_CANCELLED_REASON_CODE)
    }

    pub fn project_route_with_detail(
        reason_code: impl Into<String>,
        retryable: bool,
        detail: ApplicationProblemDetailV1,
    ) -> Self {
        Self::ProjectRoute {
            reason_code: reason_code.into(),
            retryable,
            detail: detail.message(),
            typed_detail: Some(Box::new(detail)),
        }
    }

    pub fn project_route_context(&self) -> Option<(&str, bool, &str)> {
        let Self::ProjectRoute {
            reason_code,
            retryable,
            detail,
            ..
        } = self
        else {
            return None;
        };
        Some((reason_code, *retryable, detail))
    }

    pub fn project_route_typed_detail(&self) -> Option<&ApplicationProblemDetailV1> {
        let Self::ProjectRoute { typed_detail, .. } = self else {
            return None;
        };
        typed_detail.as_deref()
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

    /// Every fallible workspace call returns this error by value; clippy's
    /// `result_large_err` rejects an `Err` variant of 128 bytes or more, so
    /// large payloads are boxed rather than stored inline.
    #[test]
    fn error_stays_small_enough_to_return_by_value() {
        assert!(
            std::mem::size_of::<TraceDecayError>() <= 64,
            "TraceDecayError grew to {} bytes; box the new payload",
            std::mem::size_of::<TraceDecayError>()
        );
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
}
