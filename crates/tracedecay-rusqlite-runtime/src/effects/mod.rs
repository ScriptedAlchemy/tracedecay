//! Durable cross-shard effect dispatch over repository-owned transactions.
//!
//! This coordinator does not open databases, expose SQL, or claim a distributed
//! transaction. The origin repository atomically creates its domain mutation
//! and canonical outbox entry before dispatch begins. The target repository
//! atomically records inbox idempotency, applies the closed repository effect,
//! and records its receipt. Origin acknowledgement is a later transaction.

mod coordinator;
mod ports;
mod sqlite;

pub use coordinator::{
    EffectCoordinator, EffectCoordinatorError, EffectDispatchOutcome, EffectDispatchResult,
    EffectReplayAttempt, EffectReplayReport, EffectUnknown, EffectUnknownCause, OriginFailureStage,
};
pub use ports::{
    OriginDispatchPreparation, OriginEffectReplayTransactions, OriginEffectTransactions,
    TargetEffectTransactions,
};
pub use sqlite::{
    SqliteEffectPersistenceError, SqliteOriginEffectTransactions, SqliteTargetEffectTransactions,
};

#[cfg(test)]
mod tests;
