//! The fixed part of a settled daemon's anonymous memory that no owner
//! measures.

/// Process-lifetime memory with no live measure: the tool, MCP and SDK
/// catalogs, `SQLite`'s own heap (its memory statistics stay off; see
/// `.cargo/config.toml`) with its page caches bounded per connection, thread
/// stacks, allocator metadata, and the binary's writable data.
///
/// Derived from anonymous plus swapped memory minus the retained owners and
/// pooled canonical scratch, on settled daemons serving 293, 1,164 and 5,855
/// indexed files after a cold index and after a refresh, with the graph owners
/// resident, on a 96-core host (hosts with fewer cores run fewer threads and
/// sit lower): 211.9, 265.0 and 297.9 MB cold, 278.3 and 317.2 MB after the
/// refresh. The largest, rounded up to a whole 32 MiB.
pub const PROCESS_RUNTIME_ALLOWANCE_BYTES_V1: u64 = 320 * 1024 * 1024;
