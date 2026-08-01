//! Disposition and telemetry classification for host admission.
//!
//! Status and disposition-class enums, the canonical privacy-bounded telemetry
//! disposition surfaced at the daemon/host boundary, and the reason-code
//! predicates that classify them. Wire strings exist only while parsing or
//! serializing JSON; internally status and class remain typed.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostAdmissionStatus {
    Supported,
    Degraded,
    Unavailable,
    Unknown,
    Backpressured,
    AcceptedForReplay,
    Committed,
    ExactDuplicate,
}

impl HostAdmissionStatus {
    pub fn from_wire(value: &str) -> Option<Self> {
        serde_json::from_value(Value::String(value.to_owned())).ok()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostAdmissionDispositionClass {
    Application,
    Transport,
    Timeout,
    Cancellation,
    Unknown,
}

impl HostAdmissionDispositionClass {
    fn from_wire(value: &str) -> Option<Self> {
        serde_json::from_value(Value::String(value.to_owned())).ok()
    }
}

/// Canonical, privacy-bounded admission telemetry at the daemon/host boundary.
/// Status and class remain typed internally; wire strings exist only while
/// parsing or serializing JSON.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct HostAdmissionTelemetryDisposition {
    pub status: HostAdmissionStatus,
    pub retryable: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<String>,
    pub class: HostAdmissionDispositionClass,
}

impl HostAdmissionTelemetryDisposition {
    pub fn from_daemon_wire(value: &Value) -> Option<Self> {
        let status = value
            .get("status")
            .and_then(Value::as_str)
            .and_then(HostAdmissionStatus::from_wire)?;
        let retryable = value.get("retryable").and_then(Value::as_bool)?;
        let reason_code = value
            .get("reason_code")
            .and_then(Value::as_str)
            .map(bounded_reason_code);
        Some(Self::from_parts(status, Some(retryable), reason_code))
    }

    pub fn from_telemetry_wire(value: Option<&Value>) -> (Self, bool) {
        let raw_status = value
            .and_then(|value| value.get("status"))
            .and_then(Value::as_str);
        let status = raw_status
            .and_then(HostAdmissionStatus::from_wire)
            .unwrap_or(HostAdmissionStatus::Unknown);
        let reason_code = value
            .and_then(|value| value.get("reason_code"))
            .and_then(Value::as_str)
            .map(bounded_reason_code);
        let disposition = Self::from_parts(
            status,
            value
                .and_then(|value| value.get("retryable"))
                .and_then(Value::as_bool),
            reason_code,
        );
        let raw_class = value
            .and_then(|value| value.get("class"))
            .and_then(Value::as_str)
            .and_then(HostAdmissionDispositionClass::from_wire);
        let folded = value.is_none()
            || raw_status
                .and_then(HostAdmissionStatus::from_wire)
                .is_none()
            || raw_class != Some(disposition.class);
        (disposition, folded)
    }

    pub fn timeout(reason_code: &'static str) -> Self {
        Self::from_parts(
            HostAdmissionStatus::Degraded,
            Some(true),
            Some(bounded_reason_code(reason_code)),
        )
    }

    pub fn unknown(reason_code: &'static str) -> Self {
        Self::from_parts(
            HostAdmissionStatus::Unknown,
            Some(false),
            Some(bounded_reason_code(reason_code)),
        )
    }

    pub fn daemon_unavailable() -> Self {
        Self::from_parts(
            HostAdmissionStatus::Unavailable,
            Some(true),
            Some("daemon_unavailable".to_owned()),
        )
    }

    pub fn from_hook_runtime_error(reason_code: &str, retryable: bool) -> Self {
        let status = if is_timeout_reason_code(reason_code) {
            HostAdmissionStatus::Degraded
        } else if is_transport_reason_code(reason_code) {
            HostAdmissionStatus::Unavailable
        } else if reason_code == "unknown_provider" {
            HostAdmissionStatus::Unknown
        } else if is_cancellation_reason_code(reason_code) {
            HostAdmissionStatus::Backpressured
        } else {
            HostAdmissionStatus::Unavailable
        };
        Self::from_parts(
            status,
            Some(retryable),
            Some(bounded_reason_code(reason_code)),
        )
    }

    pub fn from_parts(
        status: HostAdmissionStatus,
        retryable: Option<bool>,
        reason_code: Option<String>,
    ) -> Self {
        let class = classify_disposition(status, reason_code.as_deref());
        Self {
            status,
            retryable,
            reason_code,
            class,
        }
    }
}

fn classify_disposition(
    status: HostAdmissionStatus,
    reason_code: Option<&str>,
) -> HostAdmissionDispositionClass {
    if reason_code.is_some_and(is_timeout_reason_code) {
        return HostAdmissionDispositionClass::Timeout;
    }
    if reason_code.is_some_and(is_cancellation_reason_code) {
        return HostAdmissionDispositionClass::Cancellation;
    }
    if status == HostAdmissionStatus::Unknown || reason_code == Some("unknown_provider") {
        return HostAdmissionDispositionClass::Unknown;
    }
    if status == HostAdmissionStatus::Unavailable {
        return HostAdmissionDispositionClass::Transport;
    }
    HostAdmissionDispositionClass::Application
}

fn is_timeout_reason_code(reason_code: &str) -> bool {
    matches!(
        reason_code,
        "timed_out" | "timeout" | "deadline_exceeded" | "hook_timeout"
    )
}

fn is_transport_reason_code(reason_code: &str) -> bool {
    matches!(
        reason_code,
        "authority_unavailable"
            | "daemon_unavailable"
            | "transport_error"
            | "ipc_error"
            | "connection_refused"
    )
}

fn is_cancellation_reason_code(reason_code: &str) -> bool {
    matches!(
        reason_code,
        "cancelled" | "canceled" | "observation_cancelled" | "hook_cancelled"
    )
}

pub fn is_bounded_reason_code(value: &str) -> bool {
    const MAX_REASON_CODE_BYTES: usize = 64;
    !value.is_empty()
        && value.len() <= MAX_REASON_CODE_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn bounded_reason_code(value: &str) -> String {
    if is_bounded_reason_code(value) {
        value.to_owned()
    } else {
        "unclassified".to_owned()
    }
}

impl HostAdmissionStatus {
    pub const fn is_replay_progress(self) -> bool {
        matches!(
            self,
            Self::Committed | Self::ExactDuplicate | Self::AcceptedForReplay
        )
    }
}
