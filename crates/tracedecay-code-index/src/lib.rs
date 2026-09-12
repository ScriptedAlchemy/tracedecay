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
pub mod embedding_document;
pub mod extract;
pub mod generations;
pub mod git_join;
pub mod git_projection;
pub mod graph_projection;
pub mod grep_search;
mod hotpath_observe;
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
pub mod retained_parse;
pub mod source_walk;
pub mod test_attribution;
pub mod unmounted_files;

pub use self::intake::CodeIndexIntake;

/// Directory components (ASCII case-insensitive) that mark every file below
/// them as test code.
const TEST_DIRECTORY_COMPONENTS: [&str; 5] = ["test", "tests", "__tests__", "spec", "e2e"];

/// Filename markers (ASCII case-insensitive) that mark a file as test code
/// wherever it lives.
const TEST_FILE_NAME_MARKERS: [&str; 4] = [".test.", ".spec.", "_test.", "_spec."];

/// Naming heuristic over a `/`-separated logical path: `true` when a
/// directory component is exactly one of [`TEST_DIRECTORY_COMPONENTS`] or the
/// final filename contains one of [`TEST_FILE_NAME_MARKERS`].
///
/// Components that merely contain a marker word (`contest/`, `latest.rs`) do
/// not match, and a marker inside a directory name (`fixtures.test.d/`) is
/// judged by the directory rule, so it does not match either. This is a path
/// heuristic, distinct from parser-proven test annotations; native-path
/// normalization belongs to the admission boundary that produced the path.
pub fn is_test_file(path: &str) -> bool {
    let mut components = path.rsplit('/');
    let file_name = components.next().unwrap_or_default();
    TEST_FILE_NAME_MARKERS
        .iter()
        .any(|marker| contains_ascii_case_insensitive(file_name, marker))
        || components.any(|directory| {
            TEST_DIRECTORY_COMPONENTS
                .iter()
                .any(|name| directory.eq_ignore_ascii_case(name))
        })
}

fn contains_ascii_case_insensitive(haystack: &str, needle: &str) -> bool {
    haystack
        .as_bytes()
        .windows(needle.len())
        .any(|window| window.eq_ignore_ascii_case(needle.as_bytes()))
}

#[cfg(test)]
mod is_test_file_tests {
    use super::is_test_file;

    #[test]
    fn recognized_directory_components_and_filename_markers_match() {
        for path in [
            "tests/my_test.rs",
            "tests/integration.rs",
            "test/foo.rs",
            "spec/models/user_spec.rb",
            "e2e/login.test.ts",
            "src/utils.test.ts",
            "src/utils.spec.js",
            "src/utils_test.rs",
            "src/utils_spec.py",
            "__tests__/component.test.tsx",
            "Tests/MyTest.rs",
            "TESTS/foo.rs",
            "src/Utils.Test.ts",
            "crates/x/tests/",
            "packages/app/src/__tests__/deep/nested/helper.js",
        ] {
            assert!(is_test_file(path), "{path}");
        }
    }

    #[test]
    fn marker_words_inside_other_components_do_not_match() {
        for path in [
            "src/lib.rs",
            "src/main.rs",
            "src/utils.rs",
            "src/contest/service.rs",
            "src/latest/mod.rs",
            "src/latest.rs",
            "src/protest.rs",
            "src/spectral/analysis.rs",
            "src/e2e_helpers/setup.rs",
            "src/attests/mod.rs",
            "src/attestation/verify.rs",
            "src/inspector/report.rs",
            "src/test.rs",
            "src/spec.rb",
            "src/fixtures.test.d/data.rs",
            "tests",
            "",
        ] {
            assert!(!is_test_file(path), "{path}");
        }
    }
}
