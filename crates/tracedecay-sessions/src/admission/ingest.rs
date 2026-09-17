//! Process-lifetime admission policy for session transcript ingest.

/// Truthy `TRACEDECAY_SESSION_INGEST_DISABLED` turns off every session
/// transcript ingest lane for the process lifetime — the session-temporal
/// refresh workers and the session-sync import service alike. A dev/profiling
/// switch: session history simply stays un-ingested, reported as a typed
/// unavailable outcome rather than an empty success.
#[must_use]
pub fn session_ingest_disabled() -> bool {
    std::env::var("TRACEDECAY_SESSION_INGEST_DISABLED").is_ok_and(|v| !v.is_empty() && v != "0")
}

/// The typed unavailable reason a configured-off ingest lane reports.
///
/// Named because callers must distinguish a deliberate no-op from a genuine
/// admission failure: treating it as a failure retires the project's session
/// context and fails the whole project mount.
pub const SESSION_INGEST_DISABLED_REASON_V1: &str = "session_ingest_disabled_by_env";
