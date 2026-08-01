//! Bounded-record limits shared by host admission and transcript discovery.
//!
//! These are the durable spool's per-record and per-pass ceilings. Provider
//! discovery walks charge their byte budget against the same numbers so a
//! transcript sweep can never queue more than one admission pass will accept.
//!
//! Root wiring: `src/application/host_admission/spool/bounds.rs` must re-export
//! these instead of redefining them.

/// Maximum bytes retained for a single durable host-admission record (1 MiB).
pub const DEFAULT_MAX_RECORD_BYTES: usize = 1024 * 1024;

/// Maximum bytes retained for one source identity in the spool.
pub const DEFAULT_MAX_SOURCE_BYTES: usize = 256;

/// Maximum total bytes the durable spool retains (16 MiB).
pub const DEFAULT_MAX_SPOOL_BYTES: usize = 16 * 1024 * 1024;

/// Maximum records admitted in one bounded pass.
pub const DEFAULT_MAX_RECORDS: usize = 4096;

/// Maximum spool bytes retained for any single source (4 MiB).
pub const DEFAULT_MAX_SPOOL_BYTES_PER_SOURCE: usize = 4 * 1024 * 1024;

/// Maximum records retained for any single source.
pub const DEFAULT_MAX_RECORDS_PER_SOURCE: usize = 1024;
