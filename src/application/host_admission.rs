use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracedecay_domain::{
    ObservationScopeV1, ObservationSourceCursorV1, ObservationSourceIdentityV1, ProjectId,
};
use tracedecay_store::observation::{CursorAdvanceOutcome, ObservationCursorAdvance};
use tracedecay_store::{
    ObservationPersistOutcome, ObservationProjectionStore, ObservationStore, ObservationStoreError,
    ProjectionPersistOutcome,
};

use crate::application::observation::{
    AdvanceNonDurableSourceCursorRequest, CaptureObservationOutcome, CaptureObservationRequest,
    ObservationApplication, ObservationApplicationError, ObservationCancellation,
};
use crate::global_db::GlobalDb;
use crate::privacy::RecordSanitizerV1;
use crate::store::observation::GlobalDbObservationStore;

mod durability;
mod replay;
mod runtime;
mod schedule;
mod spool;
mod wire;

pub(crate) use durability::{DirectorySyncPolicy, sync_directory};
pub(crate) use replay::{ReplayPassDecision, classify_replay_pass, replay_backoff};

pub(crate) use runtime::HostAdmissionRuntime;
pub(crate) type SharedHostAdmissionBroker = Arc<HostAdmissionBroker>;

pub(crate) struct HostAdmissionBroker {
    runtime: Arc<Mutex<HostAdmissionRuntime>>,
    replay: tokio::sync::Mutex<()>,
    /// Coalesced wake for daemon-owned profile/project replay workers.
    replay_wake: tokio::sync::Notify,
}

pub(crate) struct HostAdmissionReplay<'a> {
    broker: &'a HostAdmissionBroker,
    _guard: tokio::sync::MutexGuard<'a, ()>,
}

impl HostAdmissionBroker {
    pub(crate) fn new(runtime: HostAdmissionRuntime) -> Self {
        Self {
            runtime: Arc::new(Mutex::new(runtime)),
            replay: tokio::sync::Mutex::new(()),
            replay_wake: tokio::sync::Notify::new(),
        }
    }

    async fn with_runtime<T, F>(&self, operation: F) -> Result<T, HostAdmissionOutcome>
    where
        T: Send + 'static,
        F: FnOnce(&mut HostAdmissionRuntime) -> Result<T, HostAdmissionOutcome> + Send + 'static,
    {
        let runtime = Arc::clone(&self.runtime);
        tokio::task::spawn_blocking(move || {
            let mut runtime = runtime.lock().map_err(|_| {
                HostAdmissionOutcome::retained_unavailable("spool_runtime_unavailable")
            })?;
            operation(&mut runtime)
        })
        .await
        .unwrap_or_else(|_| {
            Err(HostAdmissionOutcome::retained_unavailable(
                "spool_runtime_unavailable",
            ))
        })
    }

    pub(crate) async fn admit(
        &self,
        source: &str,
        payload: &[u8],
    ) -> Result<runtime::DurableHostAdmission, HostAdmissionOutcome> {
        let source = source.to_owned();
        let payload = payload.to_vec();
        let admitted = self
            .with_runtime(move |runtime| runtime.admit(&source, &payload))
            .await?;
        self.request_replay();
        Ok(admitted)
    }

    /// Wake any coalesced replay worker without holding client permits.
    pub(crate) fn request_replay(&self) {
        // notify_one retains one permit when the worker has not subscribed yet,
        // closing the broker-creation/admission lost-wake window.
        self.replay_wake.notify_one();
    }

    pub(crate) async fn wait_for_replay_request(&self) {
        self.replay_wake.notified().await;
    }

    pub(crate) async fn pending_replay_count(&self) -> Result<usize, HostAdmissionOutcome> {
        self.with_runtime(|runtime| Ok(runtime.pending_count()))
            .await
    }

    pub(crate) async fn has_pending_replay(&self) -> bool {
        self.pending_replay_count()
            .await
            .is_ok_and(|count| count > 0)
    }

    pub(crate) async fn begin_replay(
        &self,
    ) -> Result<HostAdmissionReplay<'_>, HostAdmissionOutcome> {
        let guard = self.replay.lock().await;
        self.with_runtime(HostAdmissionRuntime::recover_leases)
            .await?;
        Ok(HostAdmissionReplay {
            broker: self,
            _guard: guard,
        })
    }

    #[cfg(test)]
    pub(crate) async fn pending_count(&self) -> usize {
        self.pending_replay_count().await.unwrap_or_default()
    }

    #[cfg(test)]
    pub(crate) async fn quarantine_count(&self) -> usize {
        self.with_runtime(|runtime| Ok(runtime.quarantine_count()))
            .await
            .unwrap_or_default()
    }
}

impl HostAdmissionReplay<'_> {
    pub(crate) async fn lease_next(&self) -> Result<Option<SpoolRecord>, HostAdmissionOutcome> {
        self.broker
            .with_runtime(HostAdmissionRuntime::try_lease_next)
            .await
    }

    pub(crate) async fn defer(&self, seq: u64) -> Result<(), HostAdmissionOutcome> {
        self.broker
            .with_runtime(move |runtime| runtime.defer(seq))
            .await
    }

    pub(crate) async fn commit(&self, seq: u64) -> Result<usize, HostAdmissionOutcome> {
        self.broker
            .with_runtime(move |runtime| runtime.commit(seq))
            .await
    }

    pub(crate) async fn quarantine(
        &self,
        seq: u64,
        reason: TerminalReason,
    ) -> Result<usize, HostAdmissionOutcome> {
        self.broker
            .with_runtime(move |runtime| runtime.quarantine(seq, reason))
            .await
    }
}

pub(crate) use schedule::{FairEnqueueOutcome, FairScheduleBounds, FairSourceScheduler};
#[allow(unused_imports)]
pub(crate) use spool::{
    DEFAULT_MAX_RECORD_BYTES, DEFAULT_MAX_RECORDS, DEFAULT_MAX_SOURCE_BYTES,
    DEFAULT_MAX_SPOOL_BYTES, HostAdmissionSpool, SpoolBounds, SpoolError, SpoolIntegrity,
    SpoolOpenReport, SpoolOverflowDisposition, SpoolRecord, TerminalReason,
};
pub(crate) use wire::{
    MAX_MCP_JSONRPC_FRAME_BYTES, MAX_WIRE_MESSAGE_BYTES, MCP_OVERSIZE_ID_INSPECT_BYTES,
    WIRE_RECORD_TOO_LARGE, WireReadOutcome, is_wire_oversized_io_error, read_bounded_mcp_line,
    read_bounded_to_string, wire_oversized_inspect_prefix, wire_oversized_io_error,
    wire_oversized_io_error_with_prefix,
};
#[cfg(test)]
pub(crate) use wire::{line_outcome_to_io, read_bounded_line};

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
    pub(crate) fn from_wire(value: &str) -> Option<Self> {
        serde_json::from_value(Value::String(value.to_owned())).ok()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum HostAdmissionDispositionClass {
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
pub(crate) struct HostAdmissionTelemetryDisposition {
    pub(crate) status: HostAdmissionStatus,
    pub(crate) retryable: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reason_code: Option<String>,
    pub(crate) class: HostAdmissionDispositionClass,
}

impl HostAdmissionTelemetryDisposition {
    pub(crate) fn from_daemon_wire(value: &Value) -> Option<Self> {
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

    pub(crate) fn from_telemetry_wire(value: Option<&Value>) -> (Self, bool) {
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

    pub(crate) fn timeout(reason_code: &'static str) -> Self {
        Self::from_parts(
            HostAdmissionStatus::Degraded,
            Some(true),
            Some(bounded_reason_code(reason_code)),
        )
    }

    pub(crate) fn unknown(reason_code: &'static str) -> Self {
        Self::from_parts(
            HostAdmissionStatus::Unknown,
            Some(false),
            Some(bounded_reason_code(reason_code)),
        )
    }

    pub(crate) fn daemon_unavailable() -> Self {
        Self::from_parts(
            HostAdmissionStatus::Unavailable,
            Some(true),
            Some("daemon_unavailable".to_owned()),
        )
    }

    pub(crate) fn from_hook_runtime_error(reason_code: &str, retryable: bool) -> Self {
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

    pub(crate) fn from_parts(
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

pub(crate) fn is_bounded_reason_code(value: &str) -> bool {
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
    pub(crate) const fn is_replay_progress(self) -> bool {
        matches!(
            self,
            Self::Committed | Self::ExactDuplicate | Self::AcceptedForReplay
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct HostAdmissionOutcome {
    pub status: HostAdmissionStatus,
    pub retryable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<&'static str>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HostProjectionDrainOutcome {
    pub projected: u64,
    pub projected_outputs: u64,
    pub skipped: u64,
    pub exact_duplicates: u64,
    pub session_ids: Vec<String>,
}

impl HostAdmissionOutcome {
    const fn new(
        status: HostAdmissionStatus,
        retryable: bool,
        reason_code: Option<&'static str>,
    ) -> Self {
        Self {
            status,
            retryable,
            reason_code,
        }
    }

    pub const fn supported() -> Self {
        Self::new(HostAdmissionStatus::Supported, false, None)
    }

    pub const fn accepted_for_replay() -> Self {
        Self::new(HostAdmissionStatus::AcceptedForReplay, false, None)
    }

    pub(crate) const fn retained_backpressured(reason_code: &'static str) -> Self {
        Self::new(HostAdmissionStatus::Backpressured, true, Some(reason_code))
    }

    pub(crate) const fn retained_unavailable(reason_code: &'static str) -> Self {
        Self::new(HostAdmissionStatus::Unavailable, true, Some(reason_code))
    }

    pub(crate) const fn degraded(reason_code: &'static str) -> Self {
        Self::new(HostAdmissionStatus::Degraded, false, Some(reason_code))
    }

    pub const fn replay_completed(changed: bool, exact_duplicate: bool) -> Self {
        if changed {
            Self::new(HostAdmissionStatus::Committed, false, None)
        } else if exact_duplicate {
            Self::new(HostAdmissionStatus::ExactDuplicate, false, None)
        } else {
            Self::accepted_for_replay()
        }
    }

    pub const fn spool_overflow() -> Self {
        Self::new(
            HostAdmissionStatus::Backpressured,
            true,
            Some("spool_overflow"),
        )
    }

    pub const fn spool_record_too_large() -> Self {
        Self::new(
            HostAdmissionStatus::Degraded,
            false,
            Some("spool_record_too_large"),
        )
    }

    /// Host-event wire or MCP/daemon JSON-RPC frame exceeded its respective
    /// bound ([`wire::MAX_WIRE_MESSAGE_BYTES`] or
    /// [`wire::MAX_MCP_JSONRPC_FRAME_BYTES`]) before durable retention.
    /// Non-retryable; full payload is not retained.
    pub(crate) const fn wire_record_too_large() -> Self {
        Self::new(
            HostAdmissionStatus::Degraded,
            false,
            Some(wire::WIRE_RECORD_TOO_LARGE),
        )
    }

    pub const fn spool_source_too_large() -> Self {
        Self::new(
            HostAdmissionStatus::Degraded,
            false,
            Some("spool_source_too_large"),
        )
    }

    pub const fn spool_corrupted() -> Self {
        Self::new(
            HostAdmissionStatus::Unavailable,
            false,
            Some("spool_corrupted"),
        )
    }

    pub const fn spool_unsupported_version() -> Self {
        Self::new(
            HostAdmissionStatus::Unavailable,
            true,
            Some("spool_unsupported_version"),
        )
    }

    pub(crate) const fn durable_payload_unsupported_version() -> Self {
        Self::retained_unavailable("host_event_payload_unsupported_version")
    }

    pub(crate) const fn durable_payload_malformed() -> Self {
        Self::new(
            HostAdmissionStatus::Unavailable,
            false,
            Some("host_event_payload_malformed"),
        )
    }

    pub const fn spool_ack_conflict() -> Self {
        Self::new(
            HostAdmissionStatus::Backpressured,
            true,
            Some("spool_ack_conflict"),
        )
    }

    pub(crate) const fn spool_recovery_required() -> Self {
        Self::new(
            HostAdmissionStatus::Unavailable,
            true,
            Some("spool_recovery_required"),
        )
    }

    pub(crate) const fn quarantine_full() -> Self {
        Self::new(
            HostAdmissionStatus::Backpressured,
            true,
            Some("spool_quarantine_full"),
        )
    }

    pub(crate) const fn quarantine_corrupted() -> Self {
        Self::new(
            HostAdmissionStatus::Unavailable,
            false,
            Some("spool_quarantine_corrupted"),
        )
    }

    pub(crate) const fn quarantine_recovery_required() -> Self {
        Self::new(
            HostAdmissionStatus::Unavailable,
            true,
            Some("spool_quarantine_recovery_required"),
        )
    }

    const fn project_authority_unbound() -> Self {
        Self::new(
            HostAdmissionStatus::Unavailable,
            false,
            Some("project_authority_unbound"),
        )
    }

    const fn project_authority_mismatch() -> Self {
        Self::new(
            HostAdmissionStatus::Unavailable,
            false,
            Some("project_authority_mismatch"),
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostAdmissionScope {
    Project,
    Profile,
}

#[derive(Clone, Default)]
pub struct HostAdmissionAuthorities<'a> {
    project: Option<&'a GlobalDb>,
    project_id: Option<ProjectId>,
    profile: Option<&'a GlobalDb>,
}

impl<'a> HostAdmissionAuthorities<'a> {
    pub fn for_project(project: &'a GlobalDb, project_id: ProjectId) -> Self {
        Self {
            project: Some(project),
            project_id: Some(project_id),
            profile: None,
        }
    }

    pub const fn for_profile(profile: &'a GlobalDb) -> Self {
        Self {
            project: None,
            project_id: None,
            profile: Some(profile),
        }
    }

    #[must_use]
    pub const fn with_profile(mut self, profile: &'a GlobalDb) -> Self {
        self.profile = Some(profile);
        self
    }

    fn get(&self, scope: HostAdmissionScope) -> Option<&'a GlobalDb> {
        match scope {
            HostAdmissionScope::Project => self.project,
            HostAdmissionScope::Profile => self.profile,
        }
    }

    fn validate_scope(&self, scope: &ObservationScopeV1) -> Result<(), HostAdmissionOutcome> {
        let ObservationScopeV1::Project { project_id } = scope else {
            return Ok(());
        };
        match self.project_id.as_ref() {
            Some(expected) if expected == project_id => Ok(()),
            Some(_) => Err(HostAdmissionOutcome::project_authority_mismatch()),
            None => Err(HostAdmissionOutcome::project_authority_unbound()),
        }
    }
}

pub struct HostAdmissionFacade<'a> {
    authorities: HostAdmissionAuthorities<'a>,
}

impl<'a> HostAdmissionFacade<'a> {
    pub const fn new(authorities: HostAdmissionAuthorities<'a>) -> Self {
        Self { authorities }
    }

    pub fn probe(&self, provider: &str, scope: HostAdmissionScope) -> HostAdmissionOutcome {
        if !supported_provider(provider) {
            return HostAdmissionOutcome::new(
                HostAdmissionStatus::Unknown,
                false,
                Some("unknown_provider"),
            );
        }
        if self.authorities.get(scope).is_none() {
            return HostAdmissionOutcome::new(
                HostAdmissionStatus::Unavailable,
                true,
                Some("authority_unavailable"),
            );
        }
        if scope == HostAdmissionScope::Project && self.authorities.project_id.is_none() {
            return HostAdmissionOutcome::project_authority_unbound();
        }
        HostAdmissionOutcome::supported()
    }

    pub fn accept_replay(&self, provider: &str, scope: HostAdmissionScope) -> HostAdmissionOutcome {
        let probe = self.probe(provider, scope);
        if probe.status == HostAdmissionStatus::Supported {
            HostAdmissionOutcome::accepted_for_replay()
        } else {
            probe
        }
    }

    pub async fn get_source_cursor(
        &self,
        source: &ObservationSourceIdentityV1,
        scope: &ObservationScopeV1,
    ) -> Result<Option<ObservationSourceCursorV1>, HostAdmissionOutcome> {
        let store = self.store(source.provider().as_str(), scope)?;
        store
            .get_source_cursor(source, scope)
            .await
            .map_err(|error| classify_error(&ObservationApplicationError::Store(error)))
    }

    pub async fn capture_observation(
        &self,
        request: CaptureObservationRequest,
    ) -> Result<CaptureObservationOutcome, HostAdmissionOutcome> {
        let application = self.application(request.provider(), request.scope())?;
        application
            .capture_observation(request)
            .await
            .map_err(|error| classify_error(&error))
    }

    pub async fn capture(&self, request: CaptureObservationRequest) -> HostAdmissionOutcome {
        match self.capture_observation(request).await {
            Ok(outcome) => classify_capture(outcome),
            Err(outcome) => outcome,
        }
    }

    pub async fn advance_non_durable_source_cursor(
        &self,
        advance: ObservationCursorAdvance,
        cancellation: ObservationCancellation,
    ) -> Result<CursorAdvanceOutcome, HostAdmissionOutcome> {
        let cursor = advance.next_cursor();
        let application = self.application(cursor.source().provider().as_str(), cursor.scope())?;
        application
            .advance_non_durable_source_cursor(AdvanceNonDurableSourceCursorRequest::new(
                advance,
                cancellation,
            ))
            .await
            .map_err(|error| classify_error(&error))
    }

    pub async fn drain_projection_queue(
        &self,
        provider: &str,
        scope: &ObservationScopeV1,
        cancellation: &ObservationCancellation,
        max: usize,
    ) -> Result<HostProjectionDrainOutcome, HostAdmissionOutcome> {
        let store = self.store(provider, scope)?;
        let mut outcome = HostProjectionDrainOutcome::default();
        let mut session_ids = BTreeSet::new();
        for _ in 0..max {
            if cancellation.is_cancelled() {
                return Err(classify_error(&ObservationApplicationError::Cancelled));
            }
            let Some(observation_id) = store
                .next_queued_observation()
                .await
                .map_err(|_| projection_store_unavailable())?
            else {
                break;
            };
            match store
                .project_observation(&observation_id)
                .await
                .map_err(|_| projection_store_unavailable())?
            {
                ProjectionPersistOutcome::Projected(projected) => {
                    outcome.projected = outcome.projected.saturating_add(1);
                    outcome.projected_outputs = outcome.projected_outputs.saturating_add(
                        u64::try_from(projected.output_count()).unwrap_or(u64::MAX),
                    );
                    if let Some(observation) = store
                        .get_observation(&observation_id)
                        .await
                        .map_err(|_| projection_store_unavailable())?
                    {
                        session_ids.insert(
                            observation
                                .observation()
                                .source()
                                .session_id()
                                .as_str()
                                .to_owned(),
                        );
                    }
                }
                ProjectionPersistOutcome::Skipped { .. } => {
                    outcome.skipped = outcome.skipped.saturating_add(1);
                }
                ProjectionPersistOutcome::ExactDuplicate(_) => {
                    outcome.exact_duplicates = outcome.exact_duplicates.saturating_add(1);
                }
            }
        }
        outcome.session_ids = session_ids.into_iter().collect();
        Ok(outcome)
    }

    fn application(
        &self,
        provider: &str,
        scope: &ObservationScopeV1,
    ) -> Result<ObservationApplication<GlobalDbObservationStore<'a>>, HostAdmissionOutcome> {
        let store = self.store(provider, scope)?;
        let sanitizer = RecordSanitizerV1::observation_v1().map_err(|_| {
            HostAdmissionOutcome::new(
                HostAdmissionStatus::Unavailable,
                false,
                Some("sanitizer_unavailable"),
            )
        })?;
        Ok(ObservationApplication::new(store, sanitizer))
    }

    fn store(
        &self,
        provider: &str,
        scope: &ObservationScopeV1,
    ) -> Result<GlobalDbObservationStore<'a>, HostAdmissionOutcome> {
        self.authorities.validate_scope(scope)?;
        let scope = host_scope(scope);
        let probe = self.probe(provider, scope);
        if probe.status != HostAdmissionStatus::Supported {
            return Err(probe);
        }
        let db = self.authorities.get(scope).ok_or_else(|| {
            HostAdmissionOutcome::new(
                HostAdmissionStatus::Unavailable,
                true,
                Some("authority_unavailable"),
            )
        })?;
        Ok(GlobalDbObservationStore::new(db))
    }
}

const fn projection_store_unavailable() -> HostAdmissionOutcome {
    HostAdmissionOutcome::new(
        HostAdmissionStatus::Unavailable,
        true,
        Some("projection_store_unavailable"),
    )
}

fn host_scope(scope: &ObservationScopeV1) -> HostAdmissionScope {
    match scope {
        ObservationScopeV1::Profile => HostAdmissionScope::Profile,
        ObservationScopeV1::Project { .. } => HostAdmissionScope::Project,
    }
}

fn supported_provider(provider: &str) -> bool {
    crate::sessions::SessionProvider::parse(provider)
        .is_some_and(crate::sessions::SessionProvider::supports_host_admission)
}

fn classify_capture(outcome: CaptureObservationOutcome) -> HostAdmissionOutcome {
    match outcome {
        CaptureObservationOutcome::Persisted { outcome, .. } => match outcome {
            ObservationPersistOutcome::Committed(_) => {
                HostAdmissionOutcome::new(HostAdmissionStatus::Committed, false, None)
            }
            ObservationPersistOutcome::ExactDuplicate(_) => {
                HostAdmissionOutcome::new(HostAdmissionStatus::ExactDuplicate, false, None)
            }
            ObservationPersistOutcome::CoveredDuplicate(_) => HostAdmissionOutcome::new(
                HostAdmissionStatus::Committed,
                false,
                Some("duplicate_coverage_committed"),
            ),
        },
        CaptureObservationOutcome::Rejected { .. } => HostAdmissionOutcome::new(
            HostAdmissionStatus::Degraded,
            false,
            Some("sanitizer_rejected"),
        ),
        CaptureObservationOutcome::Quarantined { .. } => HostAdmissionOutcome::new(
            HostAdmissionStatus::Degraded,
            false,
            Some("sanitizer_quarantined"),
        ),
    }
}

fn classify_error(error: &ObservationApplicationError) -> HostAdmissionOutcome {
    match error {
        ObservationApplicationError::Cancelled => HostAdmissionOutcome::new(
            HostAdmissionStatus::Backpressured,
            true,
            Some("admission_cancelled"),
        ),
        ObservationApplicationError::Store(ObservationStoreError::CursorConflict { .. }) => {
            HostAdmissionOutcome::new(
                HostAdmissionStatus::Backpressured,
                true,
                Some("cursor_conflict"),
            )
        }
        ObservationApplicationError::Store(ObservationStoreError::Storage { .. }) => {
            HostAdmissionOutcome::new(
                HostAdmissionStatus::Unavailable,
                true,
                Some("authority_write_failed"),
            )
        }
        ObservationApplicationError::Contract(_) => HostAdmissionOutcome::new(
            HostAdmissionStatus::Degraded,
            false,
            Some("invalid_observation_contract"),
        ),
        ObservationApplicationError::Privacy(_) => HostAdmissionOutcome::new(
            HostAdmissionStatus::Degraded,
            false,
            Some("privacy_boundary_failed"),
        ),
        ObservationApplicationError::Store(_)
        | ObservationApplicationError::PersistedObservationUnavailable => {
            HostAdmissionOutcome::new(
                HostAdmissionStatus::Degraded,
                false,
                Some("observation_commit_failed"),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_distinguishes_unknown_provider_and_missing_authority() {
        let facade = HostAdmissionFacade::new(HostAdmissionAuthorities::default());
        assert_eq!(
            facade.probe("other", HostAdmissionScope::Project).status,
            HostAdmissionStatus::Unknown
        );
        assert_eq!(
            facade.probe("claude", HostAdmissionScope::Project).status,
            HostAdmissionStatus::Unavailable
        );
    }

    #[test]
    fn all_production_provider_ids_are_supported() {
        for provider in crate::sessions::SessionProvider::ALL
            .into_iter()
            .filter(|provider| provider.supports_host_admission())
        {
            assert!(
                supported_provider(provider.id()),
                "unsupported provider {}",
                provider.id()
            );
        }
        assert!(!supported_provider("roo"));
        assert!(!supported_provider("vibe"));
    }

    #[test]
    fn replay_statuses_serialize_without_provider_content() {
        for (outcome, expected_status) in [
            (
                HostAdmissionOutcome::replay_completed(false, false),
                "accepted_for_replay",
            ),
            (
                HostAdmissionOutcome::replay_completed(false, true),
                "exact_duplicate",
            ),
            (
                HostAdmissionOutcome::replay_completed(true, false),
                "committed",
            ),
        ] {
            assert_eq!(
                serde_json::to_value(outcome).unwrap(),
                serde_json::json!({
                    "status": expected_status,
                    "retryable": false,
                })
            );
        }
    }

    #[test]
    fn quarantine_outcomes_serialize_as_static_payload_free_dispositions() {
        for outcome in [
            HostAdmissionOutcome::quarantine_full(),
            HostAdmissionOutcome::quarantine_corrupted(),
            HostAdmissionOutcome::quarantine_recovery_required(),
        ] {
            let rendered = serde_json::to_string(&outcome).unwrap();
            assert!(rendered.contains("spool_quarantine_"));
            assert!(!rendered.contains("provider-private-payload"));
            assert!(!matches!(
                outcome.status,
                HostAdmissionStatus::Committed | HostAdmissionStatus::ExactDuplicate
            ));
        }
    }

    #[test]
    fn application_errors_map_to_bounded_static_outcomes() {
        assert_eq!(
            classify_error(&ObservationApplicationError::Cancelled),
            HostAdmissionOutcome::new(
                HostAdmissionStatus::Backpressured,
                true,
                Some("admission_cancelled"),
            )
        );
        assert_eq!(
            classify_error(&ObservationApplicationError::Store(
                ObservationStoreError::Storage {
                    operation: "write",
                    source: Box::new(std::io::Error::other("provider content must not escape",)),
                },
            )),
            HostAdmissionOutcome::new(
                HostAdmissionStatus::Unavailable,
                true,
                Some("authority_write_failed"),
            )
        );
    }
}
