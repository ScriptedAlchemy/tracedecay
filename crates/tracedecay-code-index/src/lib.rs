//! TraceDecay language extraction and code-index kernel.
//!
//! This crate owns intake validation, language registry, extraction,
//! chunks, and capability emission port traits, plus the generation engine:
//! immutable generation planning/sealing, increment planning, symbol lineage
//! resolution, projection receipt construction, and read-side joins over the
//! existing Git, managed-diagnostic, graph-impact, and test authorities. No
//! parser acquisition, filesystem access, storage, or scheduling lives here;
//! capture owns intake snapshots, the owning stores retain evidence, and the
//! projector composition owns publication.
//!
//! Contract constructors in this tree validate canonical identities built
//! from controlled formats; the `expect` on those constructor calls documents
//! the canonical-by-construction invariant and can never fail in practice.
#![allow(clippy::expect_used)]

pub mod ast_grep_search;
pub mod capabilities;
pub mod chunks;
pub mod diagnostics;
pub mod extract;
pub mod generations;
pub mod git_join;
pub mod graph_projection;
pub mod impact_join;
pub mod incremental;
pub mod intake;
pub mod languages;
pub mod lineage;
pub mod parallelism;
pub mod production;
pub mod projection;
pub mod provider;
pub mod receipts;
pub mod test_attribution;

pub use self::intake::CodeIndexIntake;

/// Returns `true` if the file path looks like a test file.
pub fn is_test_file(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    [
        "test/",
        "tests/",
        "__tests__/",
        "spec/",
        "e2e/",
        ".test.",
        ".spec.",
        "_test.",
        "_spec.",
    ]
    .iter()
    .any(|segment| lower.contains(segment))
}
