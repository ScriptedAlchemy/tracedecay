//! The shipped binary's routed C allocations survive collection and TEMP I/O.
//!
//! One test owns this process: startup routing must precede every SQLite and
//! tree-sitter allocation. The glibc counter control remains Linux-specific;
//! live-page integrity and committed-data checks also run on other Unix hosts.

#![cfg(all(unix, not(feature = "alloc-jemalloc")))]

#[path = "../src/process_allocator.rs"]
mod process_allocator;

use std::collections::BTreeSet;

use tracedecay_domain::process_heap::collect_calling_thread_allocator_v1;

#[global_allocator]
static MIMALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

const MIB: usize = 1024 * 1024;
const ROW_COUNT: i64 = 1024;

/// Bytes glibc's arenas and its mmapped chunks hold for live allocations.
///
/// The hermetic toolchain links against glibc 2.31, which predates
/// `mallinfo2`. `mallinfo` reports the same counters as `int`, which this
/// test's few-MiB working set stays far below.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn glibc_in_use() -> usize {
    // SAFETY: `mallinfo` only reads allocator statistics.
    let info = unsafe { libc::mallinfo() };
    usize::try_from(info.uordblks).expect("in-use arena bytes fit in int")
        + usize::try_from(info.hblkhd).expect("mmapped bytes fit in int")
}

#[test]
fn routed_c_libraries_preserve_live_pages_and_release_temp_files() {
    process_allocator::configure_process_allocator();
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    assert_c_libraries_avoid_glibc();

    // Both modes matter: an integrity check of a copied TEMP file cannot
    // inspect dirty pages that exist only in the live connection's cache.
    for temp_store in [2, 1] {
        assert_temp_integrity_and_reclamation(temp_store);
    }
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn assert_c_libraries_avoid_glibc() {
    let before = glibc_in_use();
    // SAFETY: a plain allocation, written in full and freed below.
    let direct = std::hint::black_box(unsafe { libc::malloc(8 * MIB) });
    assert!(!direct.is_null());
    unsafe { direct.cast::<u8>().write_bytes(1, 8 * MIB) };
    assert!(
        glibc_in_use() >= before + 8 * MIB,
        "the counter sees a direct glibc allocation"
    );
    // SAFETY: `direct` came from `libc::malloc` above.
    unsafe { libc::free(direct) };

    let before = glibc_in_use();
    let connection = rusqlite::Connection::open_in_memory().expect("in-memory database");
    connection
        .execute_batch(
            "CREATE TABLE rows(id INTEGER PRIMARY KEY, body TEXT);
             WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 40000)
             INSERT INTO rows(body) SELECT printf('%0200d', i) FROM n;
             CREATE INDEX rows_body ON rows(body);",
        )
        .expect("populate the in-memory database");
    let database_bytes: i64 = connection
        .query_row(
            "SELECT page_count * page_size FROM pragma_page_count(), pragma_page_size()",
            [],
            |row| row.get(0),
        )
        .expect("database size");
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_rust::LANGUAGE.into())
        .expect("rust grammar");
    let source =
        "pub fn f(input: &[u32]) -> u32 { input.iter().map(|v| v * 3).sum() }\n".repeat(20_000);
    let tree = parser
        .parse(source, None)
        .expect("tree-sitter parses the source");
    let grown = glibc_in_use().saturating_sub(before);

    assert!(
        database_bytes >= 16 * 1024 * 1024,
        "SQLite holds its database in heap pages: {database_bytes} bytes"
    );
    assert_eq!(tree.root_node().named_child_count(), 20_000);
    assert!(
        grown < MIB,
        "SQLite and tree-sitter grew glibc by {grown} bytes while holding a \
         {database_bytes}-byte database and a 20,000-function tree"
    );
    drop((tree, parser, connection));
}

fn payload(id: i64) -> Vec<u8> {
    id.to_le_bytes().repeat(512)
}

fn assert_rows(connection: &rusqlite::Connection, table: &str) {
    let mut statement = connection
        .prepare(&format!("SELECT id, body FROM {table} ORDER BY id"))
        .expect("read the fixture's complete row set");
    let mut rows = statement.query([]).expect("query fixture rows");
    for id in 0..ROW_COUNT {
        let row = rows.next().expect("read row").expect("no missing row");
        assert_eq!(row.get::<_, i64>(0).expect("id"), id);
        assert_eq!(row.get::<_, Vec<u8>>(1).expect("body"), payload(id));
    }
    assert!(rows.next().expect("end of rows").is_none());
}

fn assert_integrity(connection: &rusqlite::Connection, schema: &str) {
    let mut statement = connection
        .prepare(&format!("PRAGMA {schema}.integrity_check"))
        .expect("prepare live integrity check");
    let results = statement
        .query_map([], |row| row.get::<_, String>(0))
        .expect("run live integrity check")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("read every integrity result");
    assert_eq!(
        results,
        vec!["ok".to_owned()],
        "{schema} must remain structurally sound"
    );
}

fn collect_with_cross_thread_frees() {
    let handed_off: Vec<_> = (0..64).map(payload).collect();
    let live = payload(97);
    std::thread::spawn(move || {
        drop(handed_off);
        collect_calling_thread_allocator_v1();
    })
    .join()
    .expect("cross-thread frees complete");
    collect_calling_thread_allocator_v1();
    assert_eq!(live, payload(97), "collection must preserve live blocks");
}

fn assert_temp_integrity_and_reclamation(temp_store: i64) {
    let root = tempfile::tempdir().expect("isolated storage root");
    let path = root.path().join("durable.db");
    #[cfg(target_os = "linux")]
    let before = sqlite_spills::snapshot();
    let mut connection = rusqlite::Connection::open(&path).expect("open durable fixture");
    connection
        .execute_batch(&format!(
            "PRAGMA journal_mode=WAL;
             PRAGMA temp_store={temp_store};
             PRAGMA cache_size=8;
             PRAGMA temp.cache_size=8;
             CREATE TABLE durable(id INTEGER PRIMARY KEY, body BLOB NOT NULL);"
        ))
        .expect("configure fixture");
    assert_eq!(
        connection
            .query_row("PRAGMA temp_store", [], |row| row.get::<_, i64>(0))
            .expect("read TEMP mode"),
        temp_store
    );
    {
        let transaction = connection.transaction().expect("seed durable data");
        {
            let mut insert = transaction
                .prepare("INSERT INTO durable VALUES (?1, ?2)")
                .expect("prepare fixture insert");
            for id in 0..ROW_COUNT {
                insert
                    .execute(rusqlite::params![id, payload(id)])
                    .expect("insert fixture row");
            }
        }
        transaction.commit().expect("commit durable data");
    }
    connection
        .execute_batch(
            "CREATE TEMP TABLE scratch(id INTEGER PRIMARY KEY, body BLOB NOT NULL);
             INSERT INTO scratch SELECT id, body FROM main.durable;",
        )
        .expect("populate live TEMP pages");
    let temp_pages: i64 = connection
        .query_row("PRAGMA temp.page_count", [], |row| row.get(0))
        .expect("TEMP page count");
    let page_size: i64 = connection
        .query_row("PRAGMA temp.page_size", [], |row| row.get(0))
        .expect("TEMP page size");
    assert!(temp_pages * page_size > i64::try_from(MIB).expect("MiB fits"));

    #[cfg(target_os = "linux")]
    let temp_files = sqlite_spills::new_since(&before);
    #[cfg(target_os = "linux")]
    if temp_store == 1 {
        assert!(
            temp_files.values().sum::<u64>() > 1024 * 1024,
            "FILE mode must really spill; a heap-only run is not disk evidence"
        );
    } else {
        assert!(temp_files.is_empty(), "MEMORY mode must keep TEMP in heap");
    }

    for _ in 0..8 {
        connection
            .execute_batch(
                "BEGIN IMMEDIATE;
                 UPDATE main.durable SET body=zeroblob(4096);
                 DELETE FROM temp.scratch WHERE id % 3 = 0;",
            )
            .expect("start mutations in both databases");
        collect_with_cross_thread_frees();
        connection
            .execute_batch("ROLLBACK")
            .expect("roll back both databases");
        assert_rows(&connection, "main.durable");
        assert_rows(&connection, "temp.scratch");
        assert_integrity(&connection, "main");
        assert_integrity(&connection, "temp");
    }
    assert_sorter_reclamation(&connection, temp_store);
    connection
        .execute_batch("DROP TABLE temp.scratch; PRAGMA shrink_memory;")
        .expect("release unused scratch without deleting durable data");
    collect_with_cross_thread_frees();
    assert_rows(&connection, "main.durable");
    connection
        .close()
        .expect("close the owning SQLite connection");
    #[cfg(target_os = "linux")]
    sqlite_spills::assert_reclaimed(&temp_files);

    let reopened = rusqlite::Connection::open(&path).expect("reopen the same durable store");
    assert_rows(&reopened, "main.durable");
    assert_integrity(&reopened, "main");
    reopened.close().expect("close reopened fixture");
}

fn assert_sorter_reclamation(connection: &rusqlite::Connection, temp_store: i64) {
    #[cfg(target_os = "linux")]
    let before = sqlite_spills::snapshot();
    // The expression defeats the rowid order; wide keys force a real sorter
    // spill with the eight-page cache, distinct from the TEMP database file.
    let mut statement = connection
        .prepare("SELECT id, body FROM temp.scratch ORDER BY hex(body)")
        .expect("prepare an unindexed sort");
    let mut rows = statement.query([]).expect("start sort");
    let first = rows.next().expect("sort first row").expect("nonempty sort");
    let mut seen = BTreeSet::new();
    let id = first.get::<_, i64>(0).expect("first sorted id");
    assert_eq!(first.get::<_, Vec<u8>>(1).expect("body"), payload(id));
    assert!(seen.insert(id));
    #[cfg(target_os = "linux")]
    let sorter_files = sqlite_spills::new_since(&before);
    #[cfg(target_os = "linux")]
    if temp_store == 1 {
        assert!(
            sorter_files.values().sum::<u64>() > 1024 * 1024,
            "the active statement must own a real unlinked sorter file"
        );
    }
    #[cfg(not(target_os = "linux"))]
    let _ = temp_store;
    collect_with_cross_thread_frees();
    while let Some(row) = rows.next().expect("read sorted rows") {
        let id = row.get::<_, i64>(0).expect("sorted id");
        assert_eq!(row.get::<_, Vec<u8>>(1).expect("body"), payload(id));
        assert!(seen.insert(id), "sort must not duplicate rows");
    }
    assert_eq!(seen, (0..ROW_COUNT).collect::<BTreeSet<_>>());
    drop(rows);
    drop(statement);
    #[cfg(target_os = "linux")]
    sqlite_spills::assert_reclaimed(&sorter_files);
}

#[cfg(target_os = "linux")]
mod sqlite_spills {
    use std::collections::BTreeMap;
    use std::io::ErrorKind;
    use std::os::unix::fs::MetadataExt;
    use std::path::Path;

    use tracedecay_maintenance::retention::footprint::census_sqlite_temp_spill;

    type Files = BTreeMap<(u64, u64), u64>;

    pub(super) fn snapshot() -> Files {
        let mut files = Files::new();
        for entry in std::fs::read_dir("/proc/self/fd").expect("read process descriptors") {
            let path = entry.expect("enumerate descriptor").path();
            let target = match std::fs::read_link(&path) {
                Ok(target) => target,
                Err(error) if error.kind() == ErrorKind::NotFound => continue,
                Err(error) => panic!("inspect descriptor {path:?}: {error}"),
            };
            if !target
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("etilqs_") && name.ends_with(" (deleted)"))
            {
                continue;
            }
            let metadata = std::fs::metadata(&path).expect("stat live SQLite spill");
            assert!(metadata.is_file());
            assert_eq!(metadata.nlink(), 0);
            // Stat, never open or duplicate the descriptor: observing lifetime
            // must not itself keep the file alive or disturb SQLite locks.
            files.insert((metadata.dev(), metadata.ino()), metadata.len());
        }
        let census = census_sqlite_temp_spill(Path::new("/proc/self/fd"), &BTreeMap::new())
            .expect("production census can inspect this process");
        assert_eq!(census.unavailable_entry_count, 0);
        assert_eq!(census.bytes, files.values().sum::<u64>());
        files
    }

    pub(super) fn new_since(before: &Files) -> Files {
        let mut files = snapshot();
        files.retain(|identity, _| !before.contains_key(identity));
        files
    }

    pub(super) fn assert_reclaimed(owned: &Files) {
        let remaining = snapshot();
        for identity in owned.keys() {
            assert!(
                !remaining.contains_key(identity),
                "spill {identity:?} is still open"
            );
        }
    }
}
