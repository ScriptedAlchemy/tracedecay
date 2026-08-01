//! Process-wide runtime identity.
//!
//! Hoists the "mint a random per-process id" idiom out of the MCP server so
//! other long-lived components (notably the daemon) can adopt the *same*
//! process instance id later instead of each minting its own.

use std::sync::OnceLock;

/// Stable per-process run id, minted once on first call and reused for the
/// lifetime of the process.
///
/// 32 lowercase hex chars from 16 bytes of OS entropy. Best-effort: if the OS
/// RNG is unavailable it falls back to a timestamped token so the id is always
/// populated and the call never panics.
///
/// This is the shared home for the value the MCP server records as
/// `metadata.mcp_instance_id`. The daemon should stamp this *same* id on its own
/// events so a single process lifetime can be grouped across the MCP server and
/// the daemon, rather than each component minting an independent id.
pub fn process_run_id() -> &'static str {
    static RUN_ID: OnceLock<String> = OnceLock::new();
    RUN_ID.get_or_init(|| {
        let mut buf = [0u8; 16];
        match getrandom::getrandom(&mut buf) {
            Ok(()) => hex::encode(buf),
            Err(_) => format!("mcp-{}", crate::tracedecay::current_timestamp()),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_run_id_is_stable_within_the_process() {
        let first = process_run_id();
        let second = process_run_id();
        // Same borrow of the same OnceLock-backed value on every call.
        assert_eq!(first, second);
        assert!(std::ptr::eq(first, second));
        assert!(!first.is_empty());
    }
}
