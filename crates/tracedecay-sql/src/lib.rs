//! The owned-store SQL driver: one rusqlite-shaped API over the engine chosen
//! at build time by `//crates/tracedecay-sql:engine` (`sqlite` by default,
//! `native` under `--config=native`, which sets `--cfg td_engine="native"`).

mod control;
pub mod native;
#[cfg(not(td_engine = "native"))]
mod sqlite;

#[cfg(td_engine = "native")]
use native::driver as engine;
#[cfg(not(td_engine = "native"))]
use sqlite as engine;

pub use control::{
    Capability, CheckpointMode, CheckpointOutcome, ConnectionControl, Engine, EnginePolicy,
    ErrorExt, ErrorKind, FileIdentity, PROGRESS_INTERVAL_OPS, PageStats, StatementControl,
    StatementCounters, TransactionControl, file_identity, path_identity, stored_schema_matches,
};
pub use engine::{
    CachedStatement, Connection, DropBehavior, Error, InterruptHandle, MappedRows, OpenFlags,
    OptionalExtension, Params, Result, Row, Rows, Savepoint, Statement, ToSql, Transaction,
    TransactionBehavior, max_database_path_bytes, params_from_iter, types,
};
#[cfg(not(td_engine = "native"))]
pub use sqlite::params;

pub const ENGINE: Engine = engine::ENGINE;

/// PROBE (measurement only): turso schema and connection counters.
pub fn engine_probe_counts() -> Vec<(&'static str, u64)> {
    #[cfg(td_engine = "native")]
    return turso_core::open_probe::counts();
    #[cfg(not(td_engine = "native"))]
    Vec::new()
}
