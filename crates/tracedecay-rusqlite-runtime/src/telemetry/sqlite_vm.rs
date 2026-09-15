//! SQLite VM counters collected at statement boundaries.
//!
//! Names stay static. Counters never include SQL text or user values.

use std::cell::Cell;

use rusqlite::{Statement, StatementStatus};

use super::SqliteVmSnapshot;

thread_local! {
    static OBSERVED: Cell<SqliteVmSnapshot> = const { Cell::new(SqliteVmSnapshot {
        fullscan_steps: 0,
        sort_steps: 0,
        vm_steps: 0,
    }) };
}

pub(crate) fn observe_statement(statement: &Statement<'_>) {
    let delta = SqliteVmSnapshot {
        fullscan_steps: take_status(statement, StatementStatus::FullscanStep),
        sort_steps: take_status(statement, StatementStatus::Sort),
        vm_steps: take_status(statement, StatementStatus::VmStep),
    };
    OBSERVED.with(|cell| cell.set(cell.get().saturating_add(delta)));
}

pub(crate) fn take_observed_vm() -> SqliteVmSnapshot {
    OBSERVED.with(|cell| cell.replace(SqliteVmSnapshot::default()))
}

fn take_status(statement: &Statement<'_>, status: StatementStatus) -> u64 {
    i32_to_u64(statement.reset_status(status))
}

fn i32_to_u64(value: i32) -> u64 {
    u64::try_from(value.max(0)).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    use super::*;

    #[test]
    fn fullscan_sort_and_vm_steps_are_recorded_without_sql_text() {
        let _ = take_observed_vm();
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch("CREATE TABLE items(value INTEGER); INSERT INTO items VALUES (1), (2);")
            .unwrap();
        let mut statement = connection
            .prepare("SELECT value FROM items ORDER BY value DESC")
            .unwrap();
        let count = statement
            .query_map([], |row| row.get::<_, i64>(0))
            .unwrap()
            .count();
        assert_eq!(count, 2);
        observe_statement(&statement);
        let snapshot = take_observed_vm();
        assert!(
            snapshot.fullscan_steps > 0,
            "unindexed table scan must increment fullscan"
        );
        assert!(
            snapshot.sort_steps > 0,
            "ORDER BY on an unindexed column must increment sort"
        );
        assert!(
            snapshot.vm_steps > 0,
            "a completed SELECT must increment VM steps"
        );
    }
}
