//! In-process publication of successfully committed writer watermarks.
//!
//! This module is deliberately notification-only: it never reads the private
//! commit ledger and never derives a sequence from telemetry.

mod publisher;

pub use publisher::{
    CommitWatermarkPublicationError, CommitWatermarkSubscription, CommittedWatermarkPublisher,
};

#[cfg(test)]
mod tests;
