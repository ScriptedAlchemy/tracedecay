use rusqlite::Connection;

use crate::work_registered_store::RegisteredWorkStore;

const SHIPPED_EXECUTOR_JOURNAL: &str = "
CREATE TABLE work_owner_cursors_v1 (
    project_id TEXT NOT NULL,
    repository_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    actor_id TEXT NOT NULL,
    policy_digest TEXT NOT NULL,
    sequence INTEGER NOT NULL
);
CREATE TABLE work_events_v1 (
    project_id TEXT NOT NULL,
    repository_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    actor_id TEXT NOT NULL,
    policy_digest TEXT NOT NULL,
    task_id TEXT NOT NULL,
    version INTEGER NOT NULL,
    command_id TEXT NOT NULL,
    input_digest TEXT NOT NULL,
    occurred_at INTEGER NOT NULL,
    event_payload TEXT NOT NULL
);
";

#[test]
fn shipped_executor_journal_is_retired_on_open() {
    let store = RegisteredWorkStore::start_seeded("retire-executor-journal", |connection| {
        connection
            .execute_batch(SHIPPED_EXECUTOR_JOURNAL)
            .expect("seed shipped executor journal");
    });

    store.inspect(|connection| {
        for table in ["work_events_v1", "work_owner_cursors_v1"] {
            let present = connection
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                    [table],
                    |row| row.get::<_, u32>(0),
                )
                .expect("inspect retired table");
            assert_eq!(present, 0, "{table} must be retired");
        }
    });
}

#[test]
fn retirement_migration_is_idempotent() {
    let connection = Connection::open_in_memory().expect("open migration database");
    tracedecay_rusqlite_runtime::work::install_work_schema(&connection)
        .expect("first schema install");
    tracedecay_rusqlite_runtime::work::install_work_schema(&connection)
        .expect("second schema install");
}
