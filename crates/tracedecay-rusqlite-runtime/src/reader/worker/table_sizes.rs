//! Payload census on the reader's existing snapshot, without a page-sized sorter.

use rusqlite::Connection;

use super::TableSizeTelemetrySample;

/// Aggregate one table at a time. Joining every `dbstat` page before `GROUP BY`
/// materializes a temporary sorter proportional to the database's page count,
/// just to return one row per table. This correlated aggregate streams the pages
/// into one `SUM` and sorts only the completed table totals.
///
/// Keep `dbstat` in page mode: `SQLite` can observe interruption between pages,
/// rather than spending an entire large btree inside one aggregate-mode step.
/// Payload bytes deliberately exclude indexes, free pages and WAL/SHM files;
/// the separate store/profile footprint owns those physical-size measurements.
pub(super) fn read(connection: &Connection) -> rusqlite::Result<Vec<TableSizeTelemetrySample>> {
    // LIKE treats the underscore as a wildcard and hides ordinary tables
    // such as `sqlitemessages`; only the literal `sqlite_` prefix is internal.
    let mut statement = connection.prepare_cached(
        "SELECT schema_entry.name, \
                (SELECT COALESCE(SUM(payload), 0) FROM dbstat \
                 WHERE name = schema_entry.name) \
         FROM sqlite_schema AS schema_entry \
         WHERE schema_entry.type = 'table' \
           AND schema_entry.name NOT GLOB 'sqlite_*' \
         ORDER BY schema_entry.name",
    )?;
    statement
        .query_map([], |row| {
            let bytes = row.get::<_, i64>(1)?;
            Ok(TableSizeTelemetrySample {
                table_name: row.get(0)?,
                bytes: u64::try_from(bytes)
                    .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(1, bytes))?,
            })
        })?
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use super::*;

    // Independent, per-page reference for the previous census. Keeping it in
    // the test also makes the work regression falsifiable on the same SQLite
    // build, without wall-clock thresholds or query-plan string assertions.
    const GROUPED_CENSUS: &str = "SELECT schema_entry.name, COALESCE(SUM(dbstat.payload), 0) \
         FROM sqlite_schema AS schema_entry \
         LEFT JOIN dbstat ON dbstat.name = schema_entry.name \
         WHERE schema_entry.type = 'table' \
           AND schema_entry.name NOT LIKE 'sqlite_%' \
         GROUP BY schema_entry.name ORDER BY schema_entry.name";

    fn fixture() -> (tempfile::TempDir, Connection) {
        let directory = tempfile::tempdir().unwrap();
        let connection = Connection::open(directory.path().join("growth.db")).unwrap();
        connection
            .execute_batch(
                "PRAGMA page_size = 4096;
                 PRAGMA auto_vacuum = NONE;
                 CREATE TABLE events(id INTEGER PRIMARY KEY AUTOINCREMENT, body BLOB);
                 CREATE INDEX event_size ON events(length(body));
                 CREATE TABLE empty_table(body BLOB);
                 CREATE TABLE keyed(name TEXT PRIMARY KEY, body BLOB) WITHOUT ROWID;
                 CREATE VIRTUAL TABLE page_probe USING dbstat;
                 INSERT INTO keyed VALUES ('one', zeroblob(32768));
                 WITH RECURSIVE n(x) AS (
                     SELECT 1 UNION ALL SELECT x + 1 FROM n WHERE x < 16
                 ) INSERT INTO events(body) SELECT zeroblob(262144) FROM n;",
            )
            .unwrap();
        (directory, connection)
    }

    fn reference(connection: &Connection) -> Vec<TableSizeTelemetrySample> {
        let mut statement = connection.prepare(GROUPED_CENSUS).unwrap();
        statement
            .query_map([], |row| {
                let bytes = row.get::<_, i64>(1)?;
                Ok(TableSizeTelemetrySample {
                    table_name: row.get(0)?,
                    bytes: u64::try_from(bytes).unwrap(),
                })
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    fn measure_vm<T>(connection: &Connection, run: impl FnOnce() -> T) -> (T, usize) {
        let steps = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&steps);
        connection
            .progress_handler(
                1,
                Some(move || {
                    observed.fetch_add(1, Ordering::Relaxed);
                    false
                }),
            )
            .unwrap();
        let result = run();
        connection
            .progress_handler(0, None::<fn() -> bool>)
            .unwrap();
        (result, steps.load(Ordering::Relaxed))
    }

    fn page_count(connection: &Connection) -> i64 {
        connection
            .pragma_query_value(None, "page_count", |row| row.get(0))
            .unwrap()
    }

    #[test]
    fn payload_census_tracks_growth_and_reclamation_without_counting_indexes() {
        let (_directory, connection) = fixture();
        let before = read(&connection).unwrap();
        assert_eq!(before, reference(&connection));
        let bytes: BTreeMap<_, _> = before
            .iter()
            .map(|sample| (sample.table_name.as_str(), sample.bytes))
            .collect();
        assert!(bytes["events"] > 16 * 262_144);
        assert!(bytes["keyed"] > 32_768);
        assert_eq!(bytes["empty_table"], 0);
        assert_eq!(
            bytes["page_probe"], 0,
            "rootless tables still have a sample"
        );
        assert!(!bytes.contains_key("event_size"));
        assert!(!bytes.contains_key("sqlite_sequence"));
        assert!(
            before
                .windows(2)
                .all(|pair| pair[0].table_name < pair[1].table_name)
        );

        connection
            .execute("INSERT INTO events(body) VALUES (zeroblob(262144))", [])
            .unwrap();
        let enlarged = read(&connection).unwrap();
        assert_eq!(enlarged, reference(&connection));
        assert!(
            enlarged
                .iter()
                .any(|sample| { sample.table_name == "events" && sample.bytes > bytes["events"] })
        );

        let grown_pages = page_count(&connection);
        connection
            .execute("DELETE FROM events WHERE id > 1", [])
            .unwrap();
        let reclaimed = read(&connection).unwrap();
        assert_eq!(reclaimed, reference(&connection));
        let retained = reclaimed
            .iter()
            .find(|sample| sample.table_name == "events")
            .unwrap();
        assert!(retained.bytes > 262_144);
        assert!(retained.bytes < bytes["events"]);
        assert_eq!(
            page_count(&connection),
            grown_pages,
            "free pages are not live payload"
        );

        // Only this disposable fixture is vacuumed. Payload accounting must
        // not confuse physical compaction with another logical deletion.
        connection.execute_batch("VACUUM").unwrap();
        assert!(page_count(&connection) < grown_pages);
        assert_eq!(read(&connection).unwrap(), reclaimed);
    }

    #[test]
    fn payload_census_includes_sqlite_named_user_tables_and_their_growth() {
        let (_directory, connection) = fixture();
        connection
            .execute_batch(
                "CREATE TABLE sqlitemessages(body BLOB);
                 INSERT INTO sqlitemessages VALUES (zeroblob(1048576));",
            )
            .unwrap();
        let before = read(&connection).unwrap();
        let initial = before
            .iter()
            .find(|sample| sample.table_name == "sqlitemessages")
            .expect("a user table without the reserved underscore must be measured");
        assert!(initial.bytes > 1_048_576);

        connection
            .execute("INSERT INTO sqlitemessages VALUES (zeroblob(1048576))", [])
            .unwrap();
        let after = read(&connection).unwrap();
        let grown = after
            .iter()
            .find(|sample| sample.table_name == "sqlitemessages")
            .unwrap();
        let expected: i64 = connection
            .query_row(
                "SELECT SUM(payload) FROM dbstat WHERE name = 'sqlitemessages'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(grown.bytes, u64::try_from(expected).unwrap());
        assert!(grown.bytes > initial.bytes);
        assert!(after.iter().all(|sample| sample.table_name != "sqlite_sequence"));
    }

    #[test]
    fn page_census_avoids_group_sort_work_under_the_same_vm_budget() {
        let (_directory, connection) = fixture();
        let (expected, grouped_steps) = measure_vm(&connection, || reference(&connection));
        let (actual, streamed_steps) = measure_vm(&connection, || read(&connection).unwrap());
        assert_eq!(actual, expected);
        assert!(actual.iter().any(|sample| sample.bytes > 4 * 1024 * 1024));
        assert!(streamed_steps > 0);
        assert!(
            streamed_steps < grouped_steps / 2,
            "page sorting consumed the census budget: streamed={streamed_steps}, grouped={grouped_steps}"
        );
    }

    #[test]
    fn interrupted_census_returns_an_error_not_partial_or_zero_sizes_and_can_retry() {
        let (_directory, connection) = fixture();
        connection.progress_handler(100, Some(|| true)).unwrap();
        let interrupted = read(&connection);
        connection
            .progress_handler(0, None::<fn() -> bool>)
            .unwrap();
        assert!(matches!(
            interrupted,
            Err(rusqlite::Error::SqliteFailure(error, _))
                if error.code == rusqlite::ErrorCode::OperationInterrupted
        ));
        let retried = read(&connection).unwrap();
        assert_eq!(retried, reference(&connection));
        assert!(retried.iter().any(|sample| sample.bytes > 4 * 1024 * 1024));
    }
}
