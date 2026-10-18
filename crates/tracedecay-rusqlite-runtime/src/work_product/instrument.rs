//! Row accounting for tests that prove an operation reads a bounded amount.
//!
//! Every Work query returns through `registered_work_query`, which reports the
//! rows it returned here. A test wraps one operation in [`measure`] and asserts
//! on what it read, so "appends and current reads no longer process the
//! lifetime journal" is a measurement of a real workload and not a timing.
//! Counting is per thread and compiled only for tests, so production pays
//! nothing and parallel tests cannot see each other's reads.

#[cfg(any(test, feature = "test-runtime"))]
mod counting {
    use std::cell::Cell;

    use crate::exact_sql::{ExactSqlRows, ExactSqlValue};

    /// What one measured operation read through Work queries.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct RowsRead {
        pub queries: u64,
        pub rows: u64,
        /// Bytes of text and blob values returned.
        pub bytes: u64,
    }

    thread_local! {
        static READ: Cell<RowsRead> = const { Cell::new(RowsRead { queries: 0, rows: 0, bytes: 0 }) };
    }

    pub(crate) fn record(rows: &ExactSqlRows) {
        let bytes: usize = rows
            .rows
            .iter()
            .flat_map(|row| row.values.iter())
            .map(|value| match value {
                ExactSqlValue::Text(text) => text.len(),
                ExactSqlValue::Blob(blob) => blob.len(),
                _ => 0,
            })
            .sum();
        READ.with(|read| {
            let current = read.get();
            read.set(RowsRead {
                queries: current.queries + 1,
                rows: current.rows + rows.rows.len() as u64,
                bytes: current.bytes + bytes as u64,
            });
        });
    }

    /// Runs `operation` and returns its result with what it read on this
    /// thread. Work storage runs its queries on the calling thread, so this sees
    /// the whole operation.
    pub fn measure<T>(operation: impl FnOnce() -> T) -> (T, RowsRead) {
        let before = READ.with(Cell::get);
        let result = operation();
        let after = READ.with(Cell::get);
        (
            result,
            RowsRead {
                queries: after.queries - before.queries,
                rows: after.rows - before.rows,
                bytes: after.bytes - before.bytes,
            },
        )
    }
}

#[cfg(any(test, feature = "test-runtime"))]
pub(crate) use counting::record;
#[cfg(any(test, feature = "test-runtime"))]
pub use counting::{RowsRead, measure};

#[cfg(not(any(test, feature = "test-runtime")))]
pub(crate) fn record(_rows: &crate::exact_sql::ExactSqlRows) {}
