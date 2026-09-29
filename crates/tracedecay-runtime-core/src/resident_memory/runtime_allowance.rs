//! The fixed part of a settled daemon's anonymous memory that no owner
//! measures.

/// Process-lifetime memory with no live measure: the tool, MCP and SDK
/// catalogs, `SQLite`'s own heap (its memory statistics stay off; see
/// `.cargo/config.toml`), thread stacks, allocator metadata, and the binary's
/// writable data. A settled daemon's anonymous memory stays within the
/// retained owners, the pooled canonical scratch, and this allowance.
pub const PROCESS_RUNTIME_ALLOWANCE_BYTES_V1: u64 = 256 * 1024 * 1024;
