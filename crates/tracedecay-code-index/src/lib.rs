//! TraceDecay language extraction and code-index kernel.
//!
//! This module tree holds the initial implementation home of the Plan 25
//! contracts defined in `tracedecay_domain::code_intelligence`. PR9 starts as
//! focused root modules; `tracedecay-code-index` extraction happens only if
//! the Plan 19 measured gate approves it, and moves this tree without
//! changing the domain values or ports (see
//! `docs/plans/tracedecay-v2/pr9/00-contract-spine.md`).
//!
//! This spine holds intake validation, language registry, extraction,
//! chunks, and capability emission port traits, plus the generation engine:
//! immutable generation planning/sealing, increment planning, symbol lineage
//! resolution, projection receipt construction, and read-side joins over the
//! existing Git, managed-diagnostic, graph-impact, and test authorities. No
//! parser acquisition, filesystem access, storage, or scheduling lives here;
//! capture owns intake snapshots, the owning stores retain evidence, and the
//! projector composition owns publication (Plan 25 boundaries).
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
pub mod impact_join;
pub mod incremental;
pub mod intake;
pub mod languages;
pub mod lineage;
pub mod production;
pub mod production_joins;
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
