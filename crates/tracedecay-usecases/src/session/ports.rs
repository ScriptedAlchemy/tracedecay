//! Authorized temporal execution contract.
//!
//! The registered global database owns both the only production executor and
//! this contract. Re-exporting that authority keeps retrieval callers and the
//! executor on one type identity instead of maintaining a second, structurally
//! identical port inside the use-case crate.

use super::serving::SessionProjectionServingStatus;

pub trait SessionProjectionServingStatusPort: Send + Sync {
    fn serving_status(&self) -> SessionProjectionServingStatus;
}

pub use tracedecay_global_db::session_temporal::execution::{
    AuthorizedTemporalExecutionRequest, SessionTemporalExecutionError,
    SessionTemporalExecutionPort, SessionTemporalExecutionReport, TemporalExecutionFuture,
};
