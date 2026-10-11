mod connection;
mod error;
mod executor;
mod native;
mod params;
mod row;
mod snapshot;
#[cfg(any(test, feature = "test-helpers"))]
mod statement;
#[cfg(any(test, feature = "test-helpers"))]
mod test_support;
mod transaction;
mod value;
mod write_statement;

pub use connection::{Connection, ReadConnection, ReaderPoolSnapshot, ReaderPoolState};
pub use error::{Error, Result};
pub use executor::{Executor, QueryExecutor, WalCheckpointExecutor};
pub use params::{IntoParams, IntoValue, Params, params, params_from_iter};
pub use row::{Row, Rows};
pub use snapshot::ReadSnapshot;
#[cfg(any(test, feature = "test-helpers"))]
pub use statement::Statement;
#[cfg(any(test, feature = "test-helpers"))]
pub use test_support::{NativeTestConnection, TestConnection};
pub use transaction::{Transaction, TransactionBehavior};
pub use value::{FromValue, Value, opt_i64, opt_text};
pub use write_statement::WriteStatement;

#[cfg(test)]
mod tests;

/// SQL dialect and index capabilities of the attached durable engine.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BackendKind {
    Sqlite,
    NativeTurso,
}

impl BackendKind {
    /// The dialect of the owned-store engine linked into this binary, which
    /// the registered exact-SQL runtime drives.
    pub const LINKED: Self = match tracedecay_sql::ENGINE {
        tracedecay_sql::Engine::Sqlite => Self::Sqlite,
        tracedecay_sql::Engine::Native => Self::NativeTurso,
    };
}

#[cfg(test)]
mod native_tests;
