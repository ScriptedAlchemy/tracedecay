//! TraceDecay language extraction and code-index kernel.
//!
//! This crate owns intake validation, language registry, extraction,
//! chunks, and capability emission port traits, plus the generation engine:
//! immutable generation planning/sealing, increment planning, symbol lineage
//! resolution, projection receipt construction, and the generation-bound
//! test-attribution join. No parser acquisition, filesystem access, storage,
//! or scheduling lives here; capture owns intake snapshots, the owning stores
//! retain evidence, and the projector composition owns publication.
//!
//! Contract constructors in this tree validate canonical identities built
//! from controlled formats; the `expect` on those constructor calls documents
//! the canonical-by-construction invariant and can never fail in practice.
#![allow(clippy::expect_used)]

pub mod ast_grep_search;
pub mod capabilities;
pub mod chunks;
pub mod clones;
pub mod extract;
pub mod generations;
pub mod git_projection;
pub mod graph_projection;
pub mod grep_search;
pub mod incremental;
pub mod intake;
pub mod languages;
pub mod lineage;
pub mod noncanonical;
mod observe;
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

use self::languages::LanguageRegistry;

/// Whether an extracted symbol is a parser-attested test annotation.
///
/// Callers combine this marker with the canonical `Annotates` edge instead of
/// inferring runnable tests from names or enclosing modules.
pub fn is_test_marker(record: &lineage::LineageSymbolRecordV1) -> bool {
    record.kind == "annotation_usage" && is_test_annotation_name(&record.simple_name)
}

/// Whether an extracted symbol is a parser-attested `#[cfg(test)]` attribute:
/// the item its canonical `Annotates` edge targets exists only in test builds.
///
/// Only the exact `test` predicate qualifies; `cfg(any(test, feature = ..))`
/// also compiles into feature builds, so it does not prove test status.
pub fn is_test_gate_marker(record: &lineage::LineageSymbolRecordV1) -> bool {
    record.kind == "annotation_usage"
        && record.simple_name == "cfg"
        && record.signature.as_deref().is_some_and(|signature| {
            signature
                .chars()
                .filter(|character| !character.is_whitespace())
                .eq("#[cfg(test)]".chars())
        })
}

fn is_test_annotation_name(name: &str) -> bool {
    matches!(
        name.rsplit("::").next().unwrap_or(name),
        "test" | "wasm_bindgen_test" | "rstest" | "parameterized"
    )
}

/// Directory components (ASCII case-insensitive) that mark every file below
/// them as test code.
const TEST_DIRECTORY_COMPONENTS: [&str; 5] = ["test", "tests", "__tests__", "spec", "e2e"];

/// Filename markers (ASCII case-insensitive) that mark a file as test code
/// wherever it lives.
const TEST_FILE_NAME_MARKERS: [&str; 4] = [".test.", ".spec.", "_test.", "_spec."];

/// Naming heuristic over a `/`-separated logical path: `true` when a
/// directory component is exactly one of `TEST_DIRECTORY_COMPONENTS`, the
/// final filename contains one of `TEST_FILE_NAME_MARKERS`, or the filename
/// follows pytest's `test_*.py` discovery pattern.
///
/// Components that merely contain a marker word (`contest/`, `latest.rs`) do
/// not match, and a marker inside a directory name (`fixtures.test.d/`) is
/// judged by the directory rule, so it does not match either. This is a path
/// heuristic, distinct from parser-proven test annotations; native-path
/// normalization belongs to the admission boundary that produced the path.
pub fn is_test_file(path: &str) -> bool {
    let mut components = path.rsplit('/');
    let file_name = components.next().unwrap_or_default();
    (file_name.starts_with("test_") && file_name.ends_with(".py"))
        || TEST_FILE_NAME_MARKERS
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

/// Whether `simple_name` is a test the runner discovers by name alone in
/// `path`, given the name of the type that owns it when the parser attested
/// one (`None` for top-level items and owners outside the census).
///
/// Each runner's discovery rules decide: `go test` runs only top-level
/// `Test*` functions in `_test.go` files; pytest runs `test_*` functions in
/// `test_*.py`/`*_test.py` files and `test_*` methods on test-case classes;
/// JUnit 3, PHPUnit, and minitest run `test*`/`test_*` methods only on
/// test-case classes. The superclass that proves class eligibility usually
/// sits outside the census, so it falls back to the conventional
/// `Test*`/`*Test`/`TestCase`/`Suite` names, and an unattested owner declines
/// rather than hide a helper on an ordinary class. Languages whose runners
/// need annotations or explicit calls return `false`.
pub fn name_discovered_test(path: &str, simple_name: &str, owner_name: Option<&str>) -> bool {
    let language = std::path::Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.to_lowercase())
        .and_then(|extension| {
            language_descriptors()
                .descriptor_for_extension(&extension)
                .map(|descriptor| descriptor.language.as_str())
        });
    let basename = std::path::Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    match language {
        Some("go") => {
            basename.ends_with("_test.go")
                && owner_name.is_none_or(is_test_case_name)
                && simple_name.starts_with("Test")
        }
        Some("python") => {
            (basename.starts_with("test_") || basename.ends_with("_test.py"))
                && owner_name.is_none_or(is_test_case_name)
                && simple_name.starts_with("test_")
        }
        Some("java") | Some("php") => {
            is_test_file(path)
                && owner_name.is_some_and(is_test_case_name)
                && simple_name.starts_with("test")
        }
        Some("ruby") => {
            is_test_file(path)
                && owner_name.is_some_and(is_test_case_name)
                && simple_name.starts_with("test_")
        }
        _ => false,
    }
}

/// A class name that conventionally holds runner-discovered tests: the
/// `Test*` and `*Test`/`*Tests`/`*TestCase` names JUnit, PHPUnit, minitest,
/// pytest, and unittest derive test cases from, plus testify's `*Suite`.
fn is_test_case_name(name: &str) -> bool {
    name.starts_with("Test")
        || name.ends_with("Test")
        || name.ends_with("Tests")
        || name.ends_with("TestCase")
        || name.ends_with("Suite")
}

fn language_descriptors() -> &'static languages::StaticLanguageRegistry {
    static REGISTRY: std::sync::OnceLock<languages::StaticLanguageRegistry> =
        std::sync::OnceLock::new();
    REGISTRY.get_or_init(languages::StaticLanguageRegistry::new)
}

#[cfg(test)]
mod is_test_file_tests {
    use super::{is_test_annotation_name, is_test_file};

    #[test]
    fn qualified_test_attributes_preserve_their_test_marker() {
        for annotation in ["test", "tokio::test", "async_std::test"] {
            assert!(is_test_annotation_name(annotation), "{annotation}");
        }
        for annotation in ["cfg", "tokio::main", "test_support"] {
            assert!(!is_test_annotation_name(annotation), "{annotation}");
        }
    }

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
            "src/pkg/test_client.py",
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
            "src/testnet/client.rs",
            "src/test_support.rs",
            "src/pkg/contest_client.py",
            "tests",
            "",
        ] {
            assert!(!is_test_file(path), "{path}");
        }
    }
}
