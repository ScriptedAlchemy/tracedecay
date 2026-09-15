//! Concrete SQLite persistence for the application-owned Work authority.

use rusqlite::Connection;
use tracedecay_domain::WorkAuthority;

use crate::exact_sql::{
    ExactSqlHandle, ExactSqlRows, ExactSqlStatement, ExactSqlTransaction, ExactSqlValue,
};
use crate::repository::RetainedExactSqlCapability;

pub(crate) mod capacity;
mod duplicate_adjudication;
mod effect_holder;
mod leak_adjudication;
mod owner_observation;
mod retry;
mod schema;
mod sql;

pub use schema::{
    RETIRE_WORK_EVENT_JOURNAL_V1, WORK_PRODUCT_SCHEMA_V1, WORK_SCHEMA_V1, install_work_schema,
};

pub(crate) use retry::insert_retry_bounded_in_transaction;
pub(crate) use sql::*;

/// Work persistence over the registered exact-SQL channel.
///
/// This is the only transaction implementation Work has: every append,
/// attempt write, and projection read goes through the same registered
/// handle the daemon binds, so no caller can reach a private connection with
/// different transaction or authority behaviour.
#[derive(Clone)]
pub struct WorkSqliteStorage {
    retained: RetainedExactSqlCapability,
}

impl WorkSqliteStorage {
    #[must_use]
    pub fn from_retained_exact_sql(retained: RetainedExactSqlCapability) -> Self {
        Self { retained }
    }

    pub(crate) fn handle(&self) -> &ExactSqlHandle {
        self.retained.handle()
    }

    pub(crate) fn retained_exact_sql(&self) -> RetainedExactSqlCapability {
        self.retained.clone()
    }
}
