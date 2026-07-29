use std::fmt;
use std::future::Future;
use std::sync::{Arc, atomic::{AtomicBool, AtomicUsize, Ordering}};
use std::task::Poll;
use std::time::Instant;

use thiserror::Error;

use super::TemporalPortError;

const SHA256_PREFIX: &str = "sha256:";
const SHA256_HEX_LEN: usize = 64;
pub(super) const MAX_READ_ITEMS: usize = 8_192;
pub(super) const MAX_READ_TOTAL_BYTES: usize = 64 * 1024 * 1024;
const MAX_READ_ITEM_BYTES: usize = 8 * 1024 * 1024;
const MAX_CONTINUATION_KEY_BYTES: usize = 4_096;

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ExecutionLimitTighteningError {
    #[error(
        "temporal execution limit {field} cannot increase after authorization \
         (authorized {authorized}, requested {requested})"
    )]
    WouldLoosen {
        field: &'static str,
        authorized: usize,
        requested: usize,
    },
    #[error(transparent)]
    InvalidLimits(#[from] TemporalPortError),
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BindingDigest(String);

impl BindingDigest {
    pub fn new(field: &'static str, value: impl Into<String>) -> Result<Self, TemporalPortError> {
        let value = value.into();
        let valid = value.strip_prefix(SHA256_PREFIX).is_some_and(|hex| {
            hex.len() == SHA256_HEX_LEN
                && hex
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        });
        if !valid {
            return Err(TemporalPortError::InvalidBinding { field });
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExecutionLimits {
    pub candidate_limit: usize,
    pub candidate_total_bytes: usize,
    pub candidate_item_bytes: usize,
    pub candidate_key_bytes: usize,
    pub candidate_stable_id_bytes: usize,
    pub candidate_anchor_id_bytes: usize,
    pub candidate_metadata_field_bytes: usize,
    pub record_limit: usize,
    pub record_total_bytes: usize,
    pub record_item_bytes: usize,
    pub record_key_bytes: usize,
    pub hydration_limit: usize,
    pub hydration_total_bytes: usize,
    pub hydration_payload_bytes: usize,
    pub hydration_chunk_bytes: usize,
}

impl Default for ExecutionLimits {
    fn default() -> Self {
        Self {
            candidate_limit: 256,
            candidate_total_bytes: 4 * 1024 * 1024,
            candidate_item_bytes: 256 * 1024,
            candidate_key_bytes: 256,
            candidate_stable_id_bytes: 4 * 1024,
            candidate_anchor_id_bytes: 4 * 1024,
            candidate_metadata_field_bytes: 64 * 1024,
            record_limit: 1024,
            record_total_bytes: 16 * 1024 * 1024,
            record_item_bytes: 1024 * 1024,
            record_key_bytes: 256,
            hydration_limit: 64,
            hydration_total_bytes: 8 * 1024 * 1024,
            hydration_payload_bytes: 1024 * 1024,
            hydration_chunk_bytes: 64 * 1024,
        }
    }
}

impl ExecutionLimits {
    pub fn validate(self) -> Result<Self, TemporalPortError> {
        for (resource, value, max) in [
            ("candidate item count", self.candidate_limit, MAX_READ_ITEMS),
            (
                "candidate total bytes",
                self.candidate_total_bytes,
                MAX_READ_TOTAL_BYTES,
            ),
            (
                "candidate item bytes",
                self.candidate_item_bytes,
                MAX_READ_ITEM_BYTES,
            ),
            (
                "candidate key bytes",
                self.candidate_key_bytes,
                MAX_CONTINUATION_KEY_BYTES,
            ),
            ("record item count", self.record_limit, MAX_READ_ITEMS),
            (
                "record total bytes",
                self.record_total_bytes,
                MAX_READ_TOTAL_BYTES,
            ),
            (
                "record item bytes",
                self.record_item_bytes,
                MAX_READ_ITEM_BYTES,
            ),
            (
                "record key bytes",
                self.record_key_bytes,
                MAX_CONTINUATION_KEY_BYTES,
            ),
            ("hydration item count", self.hydration_limit, MAX_READ_ITEMS),
            (
                "hydration total bytes",
                self.hydration_total_bytes,
                MAX_READ_TOTAL_BYTES,
            ),
            (
                "hydration payload bytes",
                self.hydration_payload_bytes,
                MAX_READ_ITEM_BYTES,
            ),
            (
                "hydration chunk bytes",
                self.hydration_chunk_bytes,
                MAX_READ_ITEM_BYTES,
            ),
        ] {
            if value == 0 || value > max {
                return Err(TemporalPortError::BudgetExceeded { resource });
            }
        }
        for (resource, value, max) in [
            (
                "candidate stable id bytes",
                self.candidate_stable_id_bytes,
                MAX_READ_ITEM_BYTES,
            ),
            (
                "candidate anchor id bytes",
                self.candidate_anchor_id_bytes,
                MAX_READ_ITEM_BYTES,
            ),
            (
                "candidate metadata field bytes",
                self.candidate_metadata_field_bytes,
                MAX_READ_ITEM_BYTES,
            ),
        ] {
            if value == 0 || value > max {
                return Err(TemporalPortError::BudgetExceeded { resource });
            }
        }
        Ok(self)
    }
}

#[derive(Clone)]
pub struct ExecutionControl {
    pub(super) cancellation: Arc<AtomicBool>,
    pub(super) deadline: Option<Instant>,
    pub(super) remaining_work: Option<Arc<AtomicUsize>>,
}

impl ExecutionControl {
    pub fn new(deadline: Option<Instant>) -> Self {
        Self {
            cancellation: Arc::new(AtomicBool::new(false)),
            deadline,
            remaining_work: None,
        }
    }

    #[must_use]
    pub fn with_work_limit(mut self, work_units: usize) -> Self {
        self.remaining_work = Some(Arc::new(AtomicUsize::new(work_units)));
        self
    }

    pub fn cancel(&self) {
        self.cancellation.store(true, Ordering::Release);
    }

    pub fn checkpoint(&self) -> Result<(), TemporalPortError> {
        self.check_cancellation_and_deadline()?;
        if self.remaining_work.as_ref().is_some_and(|remaining| {
            remaining
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                    value.checked_sub(1)
                })
                .is_err()
        }) {
            return Err(TemporalPortError::BudgetExceeded {
                resource: "work units",
            });
        }
        Ok(())
    }

    fn check_cancellation_and_deadline(&self) -> Result<(), TemporalPortError> {
        if self.cancellation.load(Ordering::Acquire) {
            return Err(TemporalPortError::Cancelled);
        }
        if self
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return Err(TemporalPortError::DeadlineExceeded);
        }
        Ok(())
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancellation.load(Ordering::Acquire)
    }
}

pub(crate) async fn await_controlled<T, E>(
    control: &ExecutionControl,
    future: impl Future<Output = Result<T, E>>,
) -> Result<T, E>
where
    E: From<TemporalPortError>,
{
    let mut future = Box::pin(future);
    std::future::poll_fn(|context| {
        if let Err(error) = control.checkpoint() {
            return Poll::Ready(Err(error.into()));
        }
        match future.as_mut().poll(context) {
            Poll::Ready(result) => match control.check_cancellation_and_deadline() {
                Ok(()) => Poll::Ready(result),
                Err(error) => Poll::Ready(Err(error.into())),
            },
            Poll::Pending => match control.checkpoint() {
                Ok(()) => Poll::Pending,
                Err(error) => Poll::Ready(Err(error.into())),
            },
        }
    })
    .await
}

impl Default for ExecutionControl {
    fn default() -> Self {
        Self::new(None)
    }
}

impl fmt::Debug for ExecutionControl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExecutionControl")
            .field("cancelled", &self.is_cancelled())
            .field("deadline", &self.deadline)
            .field(
                "remaining_work",
                &self
                    .remaining_work
                    .as_ref()
                    .map(|value| value.load(Ordering::Acquire)),
            )
            .finish()
    }
}

impl PartialEq for ExecutionControl {
    fn eq(&self, other: &Self) -> bool {
        self.is_cancelled() == other.is_cancelled()
            && self.deadline == other.deadline
            && self
                .remaining_work
                .as_ref()
                .map(|value| value.load(Ordering::Acquire))
                == other
                    .remaining_work
                    .as_ref()
                    .map(|value| value.load(Ordering::Acquire))
    }
}

impl Eq for ExecutionControl {}
