//! Tiny fixture crate for the fact-store adoption hermetic corpus.
//!
//! The corpus does not exercise this code directly — it exists only so
//! `tracedecay init` has a real, indexable Rust project to register as the
//! active project, so that `fact_store` / `fact_feedback` scenarios resolve to
//! an isolated, project-scoped memory store. The single marker function keeps
//! the graph non-empty.

/// Marker symbol so the indexed graph has at least one function node.
pub fn factstore_fixture_marker() -> u32 {
    30
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_is_stable() {
        assert_eq!(factstore_fixture_marker(), 30);
    }
}
