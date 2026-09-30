//! The shipped binary's C libraries allocate from mimalloc, not glibc.
//!
//! One test in its own binary: startup routing has to run before SQLite or
//! tree-sitter allocates anything, and nothing else may touch glibc while it
//! measures. Rust allocates through mimalloc here as it does in the binary.

#![cfg(all(
    target_os = "linux",
    target_env = "gnu",
    not(feature = "alloc-jemalloc"),
    not(feature = "hotpath-alloc")
))]

#[path = "../src/process_allocator.rs"]
mod process_allocator;

#[global_allocator]
static MIMALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

const MIB: usize = 1024 * 1024;

/// Bytes glibc's arenas and its mmapped chunks hold for live allocations.
fn glibc_in_use() -> usize {
    // SAFETY: `mallinfo2` only reads allocator statistics.
    let info = unsafe { libc::mallinfo2() };
    info.uordblks + info.hblkhd
}

#[test]
fn sqlite_and_tree_sitter_allocate_from_mimalloc_not_glibc() {
    process_allocator::configure_process_allocator();

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
