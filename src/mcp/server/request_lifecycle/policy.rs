use std::time::Duration;

const DEFAULT_WORKER_CLEANUP: Duration = Duration::from_millis(100);
const MAX_RESPONSE_WRITE_RESERVE: Duration = Duration::from_millis(100);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum McpToolDispatchStage {
    QueueAdmission,
    SchemaValidation,
    ProjectGate,
    ProjectSelection,
    Readiness,
    ApplicationRoute,
    Handler,
    ResultMaterialization,
    ResponseWrite,
}

impl McpToolDispatchStage {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::QueueAdmission => "queue_admission",
            Self::SchemaValidation => "schema_validation",
            Self::ProjectGate => "project_gate",
            Self::ProjectSelection => "project_selection",
            Self::Readiness => "readiness",
            Self::ApplicationRoute => "application_route",
            Self::Handler => "handler",
            Self::ResultMaterialization => "result_materialization",
            Self::ResponseWrite => "response_write",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct McpRequestStart {
    pub(super) runtime: tokio::time::Instant,
    pub(super) wall: tracedecay_domain::UtcMicros,
}

impl McpRequestStart {
    pub(crate) fn now() -> Self {
        Self {
            runtime: tokio::time::Instant::now(),
            wall: tracedecay_application::clock::now_micros(),
        }
    }

    pub(crate) fn elapsed(&self) -> Duration {
        self.runtime.elapsed()
    }

    pub(crate) fn runtime_deadline(&self, duration: Duration) -> Option<tokio::time::Instant> {
        self.runtime.checked_add(duration)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct McpToolLifecyclePolicy {
    pub(super) maximum_duration: Duration,
    pub(super) cancellation_cleanup: Duration,
    pub(super) externally_cancellable: bool,
}

impl McpToolLifecyclePolicy {
    pub(crate) const fn new(maximum_duration: Duration, externally_cancellable: bool) -> Self {
        Self {
            maximum_duration,
            cancellation_cleanup: DEFAULT_WORKER_CLEANUP,
            externally_cancellable,
        }
    }

    pub(crate) const fn maximum_duration(self) -> Duration {
        self.maximum_duration
    }

    pub(crate) const fn externally_cancellable(self) -> bool {
        self.externally_cancellable
    }

    pub(super) fn response_write_reserve(self) -> Duration {
        (self.maximum_duration / 10).min(MAX_RESPONSE_WRITE_RESERVE)
    }

    pub(super) fn execution_duration(self) -> Duration {
        self.maximum_duration
            .saturating_sub(self.response_write_reserve())
    }

    pub(crate) fn bounded_by(mut self, remaining: Duration) -> Self {
        self.maximum_duration = self.maximum_duration.min(remaining);
        self
    }
}
