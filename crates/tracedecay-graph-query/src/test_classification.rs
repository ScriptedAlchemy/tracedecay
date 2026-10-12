//! The one generation-bound answer to "is this symbol test code?" that test
//! risk, dead code, and health share.
//!
//! Evidence is canonical: the logical path ([`is_test_file`]), parser-attested
//! test annotations and `#[cfg(test)]` gates joined through `Annotates` edges,
//! and the `Contains` edges that bind each gate to the items it encloses. A
//! name alone never makes a symbol test code. Name discovery
//! ([`name_discovered_test`]) marks only the unannotated items that pytest,
//! Go's `testing`, JUnit 3, PHPUnit, and minitest invoke by name: the file
//! and the owning type must satisfy that runner's discovery rules.

use std::collections::{HashMap, HashSet};

use tracedecay_code_index::graph_projection::CodeGraphSymbolSummaryV1;
use tracedecay_code_index::lineage::LineageSymbolRecordV1;
use tracedecay_code_index::{
    is_test_file, is_test_gate_marker, is_test_marker, name_discovered_test,
};
use tracedecay_domain::{CanonicalRelationEdgeV1, RelationEdgeKindV1, SymbolOccurrenceId};

#[derive(Default)]
pub(crate) struct TestClassification {
    /// Targets of a test annotation such as `#[test]` or `#[tokio::test]`.
    annotated: HashSet<String>,
    /// The `#[cfg(test)]` items plus every symbol they enclose through
    /// `Contains` edges.
    gated: HashSet<String>,
    /// `dir` for each `#[cfg(test)]` module: `dir.rs` and every file below
    /// `dir/` hold that module's out-of-line code.
    gated_module_dirs: HashSet<String>,
    /// Each symbol's owning type name, when its `Contains` parent is a class
    /// or equivalent test-case container: name discovery only counts
    /// runner-eligible methods.
    owners: HashMap<String, String>,
}

/// `Contains` parent kinds that can hold a test-case class's methods.
const OWNER_KINDS: &[&str] = &[
    "class",
    "inner_class",
    "case_class",
    "data_class",
    "sealed_class",
    "record",
    "pascal_record",
    "struct",
    "union",
    "enum",
    "trait",
    "impl",
    "interface",
    "object",
    "kotlin_object",
    "companion_object",
    "mixin",
    "extension",
    "proto_message",
];

impl TestClassification {
    /// `symbols` pairs records with their logical paths. `edges` must hold the
    /// `Annotates` and `Contains` edges into those symbols; other edge kinds
    /// are ignored.
    pub(crate) fn new<'a>(
        symbols: &[(&str, &'a LineageSymbolRecordV1)],
        edges: impl IntoIterator<Item = &'a CanonicalRelationEdgeV1>,
    ) -> Self {
        let mut test_markers = HashSet::new();
        let mut gate_markers = HashSet::new();
        for (_, record) in symbols {
            if is_test_marker(record) {
                test_markers.insert(&record.occurrence);
            } else if is_test_gate_marker(record) {
                gate_markers.insert(&record.occurrence);
            }
        }
        let by_occurrence: HashMap<&SymbolOccurrenceId, &LineageSymbolRecordV1> = symbols
            .iter()
            .map(|(_, record)| (&record.occurrence, *record))
            .collect();
        let mut classification = Self::default();
        let mut gated = HashSet::new();
        let mut stack = Vec::new();
        let mut children: HashMap<&SymbolOccurrenceId, Vec<&SymbolOccurrenceId>> = HashMap::new();
        for edge in edges {
            match edge.kind {
                RelationEdgeKindV1::Annotates if test_markers.contains(&edge.from_occurrence) => {
                    classification
                        .annotated
                        .insert(edge.to_occurrence.as_str().to_owned());
                }
                RelationEdgeKindV1::Annotates if gate_markers.contains(&edge.from_occurrence) => {
                    if gated.insert(&edge.to_occurrence) {
                        stack.push(&edge.to_occurrence);
                    }
                }
                RelationEdgeKindV1::Contains => {
                    children
                        .entry(&edge.from_occurrence)
                        .or_default()
                        .push(&edge.to_occurrence);
                    if let Some(owner) = by_occurrence
                        .get(&edge.from_occurrence)
                        .filter(|owner| OWNER_KINDS.contains(&owner.kind.as_str()))
                    {
                        classification
                            .owners
                            .entry(edge.to_occurrence.as_str().to_owned())
                            .or_insert_with(|| owner.simple_name.clone());
                    }
                }
                _ => {}
            }
        }
        // A `#[cfg(test)]` item makes everything it encloses test code. Line
        // spans cannot tell siblings sharing one line apart, so containment
        // walks the parser's `Contains` edges instead.
        while let Some(parent) = stack.pop() {
            for &child in children.get(parent).into_iter().flatten() {
                if gated.insert(child) {
                    stack.push(child);
                }
            }
        }
        for (file, record) in symbols {
            if !gated.contains(&record.occurrence) {
                continue;
            }
            if record.kind == "module"
                && let Some(dir) = rust_module_dir(file, &record.qualified_name)
            {
                classification.gated_module_dirs.insert(dir);
            }
        }
        classification.gated = gated
            .into_iter()
            .map(|occurrence| occurrence.as_str().to_owned())
            .collect();
        classification
    }

    /// Code that exists only to test: a test file, a test annotation's target,
    /// a `#[cfg(test)]` item, or anything such an item encloses.
    pub(crate) fn is_test_code(&self, file: &str, record: &LineageSymbolRecordV1) -> bool {
        is_test_file(file)
            || self.is_test_annotated(record.occurrence.as_str())
            || self.gated.contains(record.occurrence.as_str())
            || file
                .strip_suffix(".rs")
                .is_some_and(|stem| self.gated_module_dirs.contains(stem))
            || file
                .match_indices('/')
                .any(|(end, _)| self.gated_module_dirs.contains(&file[..end]))
    }

    /// A test a runner invokes itself, which is live with no caller: a test
    /// annotation's target, or a name and owner satisfying the runner's
    /// discovery rules (`test_*` functions and methods in pytest test files,
    /// `Test*` functions in Go `_test.go` files, `test*` methods on test-case
    /// classes).
    pub(crate) fn is_test_entrypoint(&self, file: &str, record: &LineageSymbolRecordV1) -> bool {
        self.is_test_annotated(record.occurrence.as_str())
            || name_discovered_test(
                file,
                &record.simple_name,
                self.owners
                    .get(record.occurrence.as_str())
                    .map(String::as_str),
            )
    }

    pub(crate) fn is_test_annotated(&self, occurrence: &str) -> bool {
        self.annotated.contains(occurrence)
    }
}

/// The record and logical path classification reads, when the summary has both.
pub(crate) fn classified_symbol(
    symbol: &CodeGraphSymbolSummaryV1,
) -> Option<(&str, &LineageSymbolRecordV1)> {
    Some((
        symbol.binding.as_ref()?.logical_path.as_deref()?,
        symbol.metadata.as_ref()?,
    ))
}

/// Files that could declare a module whose out-of-line files include one of
/// `paths` under the default `mod` path rules. A scoped census reads them so
/// a `#[cfg(test)] mod` declared outside the scope still classifies its files.
pub(crate) fn rust_module_declarer_candidates<'a>(
    paths: impl IntoIterator<Item = &'a String>,
) -> HashSet<String> {
    let mut candidates = HashSet::new();
    for stem in paths
        .into_iter()
        .filter_map(|path| path.strip_suffix(".rs"))
    {
        for end in std::iter::once(0).chain(stem.match_indices('/').map(|(end, _)| end)) {
            let dir = &stem[..end];
            for name in ["lib.rs", "main.rs", "mod.rs"] {
                candidates.insert(if dir.is_empty() {
                    name.to_owned()
                } else {
                    format!("{dir}/{name}")
                });
            }
            if !dir.is_empty() {
                candidates.insert(format!("{dir}.rs"));
            }
        }
    }
    candidates
}

/// The directory holding the out-of-line files of the Rust module
/// `qualified_name` declared in `file`: `src/x.rs` places `x::m` at
/// `src/x/m`, while `lib.rs`, `main.rs`, and `mod.rs` place children beside
/// themselves. A `#[path]` override is not modelled.
fn rust_module_dir(file: &str, qualified_name: &str) -> Option<String> {
    let module_path = qualified_name.strip_prefix(file)?.strip_prefix("::")?;
    let stem = file.strip_suffix(".rs")?;
    let (parent, name) = stem.rsplit_once('/').unwrap_or(("", stem));
    let base = if matches!(name, "lib" | "main" | "mod") {
        parent
    } else {
        stem
    };
    let relative = module_path.replace("::", "/");
    Some(if base.is_empty() {
        relative
    } else {
        format!("{base}/{relative}")
    })
}

#[cfg(test)]
mod tests {
    use std::fmt::Debug;

    use tracedecay_code_index::lineage::LineageSymbolRecordV1;
    use tracedecay_domain::{
        CanonicalRelationEdgeV1, ComplexityAnalysisV1, EdgeAuthorityV1, RelationEdgeKindV1,
        SourceSpan, SymbolOccurrenceId,
    };

    use super::{TestClassification, rust_module_declarer_candidates, rust_module_dir};

    fn digest<T>(byte: char) -> T
    where
        T: TryFrom<String>,
        T::Error: Debug,
    {
        T::try_from(format!("sha256:{}", byte.to_string().repeat(64))).expect("digest")
    }

    fn record(
        occurrence: &str,
        qualified_name: &str,
        kind: &str,
        lines: (u32, u32),
        signature: Option<&str>,
    ) -> LineageSymbolRecordV1 {
        LineageSymbolRecordV1 {
            occurrence: SymbolOccurrenceId::new(occurrence).expect("occurrence"),
            identity: digest('1'),
            qualified_name: qualified_name.to_owned(),
            simple_name: qualified_name
                .rsplit("::")
                .next()
                .unwrap_or(qualified_name)
                .trim_start_matches('@')
                .to_owned(),
            kind: kind.to_owned(),
            visibility: "private".to_owned(),
            branches: 0,
            loops: 0,
            max_nesting: 0,
            complexity_analysis: ComplexityAnalysisV1::Complete,
            line_span: lines.1 - lines.0 + 1,
            start_line: lines.0,
            signature: signature.map(str::to_owned),
            docstring: None,
            is_async: false,
            derives: Vec::new(),
            skip_test_coverage: false,
            file_identity: digest('2'),
            content_digest: digest('3'),
        }
    }

    fn edge(
        kind: RelationEdgeKindV1,
        from: &LineageSymbolRecordV1,
        to: &LineageSymbolRecordV1,
    ) -> CanonicalRelationEdgeV1 {
        CanonicalRelationEdgeV1 {
            from_occurrence: from.occurrence.clone(),
            to_occurrence: to.occurrence.clone(),
            kind,
            authority: EdgeAuthorityV1::SyntaxExact,
            evidence_span: SourceSpan {
                start_byte: 0,
                end_byte: 1,
            },
        }
    }

    fn annotates(
        from: &LineageSymbolRecordV1,
        to: &LineageSymbolRecordV1,
    ) -> CanonicalRelationEdgeV1 {
        edge(RelationEdgeKindV1::Annotates, from, to)
    }

    fn contains(
        from: &LineageSymbolRecordV1,
        to: &LineageSymbolRecordV1,
    ) -> CanonicalRelationEdgeV1 {
        edge(RelationEdgeKindV1::Contains, from, to)
    }

    #[test]
    fn only_an_exact_cfg_test_gate_encloses_test_code() {
        let file = "src/lib.rs";
        let gate = record(
            "symbol.gate",
            "src/lib.rs::@cfg",
            "annotation_usage",
            (0, 0),
            Some("#[cfg( test )]"),
        );
        let gated = record("symbol.gated", "src/lib.rs::tests", "module", (1, 5), None);
        let member = record(
            "symbol.member",
            "src/lib.rs::tests::helper",
            "function",
            (2, 4),
            None,
        );
        // A production function on the gated item's own line is a sibling the
        // parser did not enclose; a line range alone could not tell them apart.
        let same_line = record(
            "symbol.same_line",
            "src/lib.rs::same_line",
            "function",
            (1, 1),
            None,
        );
        let feature_gate = record(
            "symbol.feature_gate",
            "src/lib.rs::@cfg",
            "annotation_usage",
            (6, 6),
            Some("#[cfg(any(test, feature = \"helpers\"))]"),
        );
        let shared = record(
            "symbol.shared",
            "src/lib.rs::support",
            "module",
            (7, 9),
            None,
        );
        let shared_member = record(
            "symbol.shared_member",
            "src/lib.rs::support::build",
            "function",
            (8, 8),
            None,
        );
        let after = record(
            "symbol.after",
            "src/lib.rs::after",
            "function",
            (10, 10),
            None,
        );
        let symbols = [
            &gate,
            &gated,
            &member,
            &same_line,
            &feature_gate,
            &shared,
            &shared_member,
            &after,
        ]
        .map(|record| (file, record));
        let edges = [
            annotates(&gate, &gated),
            annotates(&feature_gate, &shared),
            contains(&gated, &member),
            contains(&shared, &shared_member),
        ];
        let classification = TestClassification::new(&symbols, &edges);

        assert!(classification.is_test_code(file, &gated));
        assert!(classification.is_test_code(file, &member));
        assert!(!classification.is_test_code(file, &same_line));
        assert!(!classification.is_test_code(file, &shared_member));
        assert!(!classification.is_test_code(file, &after));
        assert!(classification.is_test_code("src/tests/deep.rs", &after));
        assert!(classification.is_test_code("src/tests.rs", &after));
        assert!(!classification.is_test_code("src/testsuite.rs", &after));
        assert!(!classification.is_test_entrypoint(file, &member));
    }

    #[test]
    fn gated_closure_reaches_nested_members_only() {
        let file = "src/lib.rs";
        let gate = record(
            "symbol.gate",
            "src/lib.rs::@cfg",
            "annotation_usage",
            (0, 0),
            Some("#[cfg(test)]"),
        );
        let outer = record("symbol.outer", "src/lib.rs::tests", "module", (1, 8), None);
        let inner = record(
            "symbol.inner",
            "src/lib.rs::tests::deep",
            "module",
            (2, 6),
            None,
        );
        let deep = record(
            "symbol.deep",
            "src/lib.rs::tests::deep::helper",
            "function",
            (3, 5),
            None,
        );
        let outside = record(
            "symbol.outside",
            "src/lib.rs::outside::helper",
            "function",
            (3, 5),
            None,
        );
        let parent = record(
            "symbol.parent",
            "src/lib.rs::outside",
            "module",
            (2, 6),
            None,
        );
        let symbols =
            [&gate, &outer, &inner, &deep, &outside, &parent].map(|record| (file, record));
        let edges = [
            annotates(&gate, &outer),
            contains(&outer, &inner),
            contains(&inner, &deep),
            contains(&parent, &outside),
        ];
        let classification = TestClassification::new(&symbols, &edges);

        assert!(classification.is_test_code(file, &deep));
        assert!(!classification.is_test_code(file, &outside));
    }

    #[test]
    fn test_names_are_entrypoints_only_inside_test_files() {
        let pytest = record(
            "symbol.pytest",
            "tests/test_client.py::test_connect",
            "function",
            (0, 1),
            None,
        );
        let go_test = record(
            "symbol.go_test",
            "pkg/client_test.go::TestConnect",
            "function",
            (0, 1),
            None,
        );
        let production = record(
            "symbol.prod",
            "src/client.rs::test_connection",
            "function",
            (0, 1),
            None,
        );
        let classification = TestClassification::new(&[], &[]);

        assert!(classification.is_test_entrypoint("tests/test_client.py", &pytest));
        assert!(classification.is_test_entrypoint("pkg/client_test.go", &go_test));
        assert!(!classification.is_test_entrypoint("src/client.rs", &production));
        assert!(!classification.is_test_code("src/client.rs", &production));
    }

    #[test]
    fn a_test_name_prefix_is_no_rust_entrypoint() {
        let rust_helper = record(
            "symbol.rust_helper",
            "tests/integration.rs::test_helper",
            "function",
            (0, 1),
            None,
        );
        let rust_test = record(
            "symbol.rust_test",
            "tests/integration.rs::runs_integration",
            "function",
            (0, 1),
            None,
        );
        let go_helper = record(
            "symbol.go_helper",
            "pkg/client_test.go::connect",
            "function",
            (0, 1),
            None,
        );
        let classification = TestClassification::new(&[], &[]);

        assert!(!classification.is_test_entrypoint("tests/integration.rs", &rust_helper));
        assert!(!classification.is_test_entrypoint("tests/integration.rs", &rust_test));
        assert!(!classification.is_test_entrypoint("pkg/client_test.go", &go_helper));
    }

    #[test]
    fn name_discovery_needs_the_runner_file_and_owner() {
        // Go discovers only top-level `Test*` functions in `_test.go` files
        // and testify `*Suite` methods.
        let go_helper = record(
            "symbol.go_helper",
            "tests/helper.go::TestHelper",
            "function",
            (0, 1),
            None,
        );
        let go_suite = record(
            "symbol.go_suite",
            "pkg/client_test.go::ClientSuite",
            "struct",
            (0, 5),
            None,
        );
        let go_suite_method = record(
            "symbol.go_suite_method",
            "pkg/client_test.go::ClientSuite::TestConnect",
            "struct_method",
            (0, 1),
            None,
        );
        // pytest discovers module-level `test_*` functions only in
        // `test_*.py`/`*_test.py`, and `test_*` methods only on test-case
        // classes.
        let pytest_module = record(
            "symbol.py_module",
            "tests/test_mod.py::test_runs",
            "function",
            (0, 1),
            None,
        );
        let pytest_helper = record(
            "symbol.py_helper",
            "tests/helper.py::test_helper",
            "function",
            (0, 1),
            None,
        );
        let pytest_class = record(
            "symbol.py_class",
            "tests/test_mod.py::TestClient",
            "class",
            (0, 5),
            None,
        );
        let pytest_method = record(
            "symbol.py_method",
            "tests/test_mod.py::TestClient::test_connects",
            "method",
            (0, 1),
            None,
        );
        let ordinary_class = record(
            "symbol.ordinary_class",
            "tests/test_mod.py::Client",
            "class",
            (0, 5),
            None,
        );
        let ordinary_method = record(
            "symbol.ordinary_method",
            "tests/test_mod.py::Client::test_connects",
            "method",
            (0, 1),
            None,
        );
        // JUnit 3, PHPUnit, and minitest methods run only on test-case
        // classes: a prefixed method on an ordinary class is not a test.
        let junit_class = record(
            "symbol.junit_class",
            "src/test/FormatTest.java::FormatTest",
            "class",
            (0, 5),
            None,
        );
        let junit_method = record(
            "symbol.junit_method",
            "src/test/FormatTest.java::FormatTest::testFormat",
            "method",
            (0, 1),
            None,
        );
        let helper_class = record(
            "symbol.helper_class",
            "src/test/FormatHelper.java::FormatHelper",
            "class",
            (0, 5),
            None,
        );
        let helper_method = record(
            "symbol.helper_method",
            "src/test/FormatHelper.java::FormatHelper::testFormat",
            "method",
            (0, 1),
            None,
        );
        let minitest_class = record(
            "symbol.minitest_class",
            "test/save_test.rb::SaveTest",
            "class",
            (0, 5),
            None,
        );
        let minitest_method = record(
            "symbol.minitest_method",
            "test/save_test.rb::SaveTest::test_saves",
            "method",
            (0, 1),
            None,
        );
        let symbols: Vec<(&str, &LineageSymbolRecordV1)> = vec![
            ("tests/helper.go", &go_helper),
            ("pkg/client_test.go", &go_suite),
            ("pkg/client_test.go", &go_suite_method),
            ("tests/test_mod.py", &pytest_module),
            ("tests/helper.py", &pytest_helper),
            ("tests/test_mod.py", &pytest_class),
            ("tests/test_mod.py", &pytest_method),
            ("tests/test_mod.py", &ordinary_class),
            ("tests/test_mod.py", &ordinary_method),
            ("src/test/FormatTest.java", &junit_class),
            ("src/test/FormatTest.java", &junit_method),
            ("src/test/FormatHelper.java", &helper_class),
            ("src/test/FormatHelper.java", &helper_method),
            ("test/save_test.rb", &minitest_class),
            ("test/save_test.rb", &minitest_method),
        ];
        let edges = [
            contains(&go_suite, &go_suite_method),
            contains(&pytest_class, &pytest_method),
            contains(&ordinary_class, &ordinary_method),
            contains(&junit_class, &junit_method),
            contains(&helper_class, &helper_method),
            contains(&minitest_class, &minitest_method),
        ];
        let classification = TestClassification::new(&symbols, &edges);

        assert!(!classification.is_test_entrypoint("tests/helper.go", &go_helper));
        assert!(classification.is_test_entrypoint("pkg/client_test.go", &go_suite_method));
        assert!(classification.is_test_entrypoint("tests/test_mod.py", &pytest_module));
        assert!(!classification.is_test_entrypoint("tests/helper.py", &pytest_helper));
        assert!(classification.is_test_entrypoint("tests/test_mod.py", &pytest_method));
        assert!(!classification.is_test_entrypoint("tests/test_mod.py", &ordinary_method));
        assert!(classification.is_test_entrypoint("src/test/FormatTest.java", &junit_method));
        assert!(!classification.is_test_entrypoint("src/test/FormatHelper.java", &helper_method));
        assert!(classification.is_test_entrypoint("test/save_test.rb", &minitest_method));
    }

    #[test]
    fn module_dirs_follow_the_default_mod_path_rules() {
        for (file, qualified_name, dir) in [
            ("src/lib.rs", "src/lib.rs::tests", "src/tests"),
            ("src/x/mod.rs", "src/x/mod.rs::tests", "src/x/tests"),
            ("src/x.rs", "src/x.rs::tests", "src/x/tests"),
            ("src/x.rs", "src/x.rs::inline::tests", "src/x/inline/tests"),
            ("lib.rs", "lib.rs::tests", "tests"),
        ] {
            assert_eq!(
                rust_module_dir(file, qualified_name).as_deref(),
                Some(dir),
                "{qualified_name}"
            );
        }
        assert_eq!(rust_module_dir("src/app.ts", "src/app.ts::tests"), None);
    }

    #[test]
    fn declarer_candidates_cover_each_enclosing_module_file() {
        let candidates = rust_module_declarer_candidates(&["src/x/tests.rs".to_owned()]);
        for declarer in [
            "src/x.rs",
            "src/x/mod.rs",
            "src/lib.rs",
            "src/main.rs",
            "lib.rs",
        ] {
            assert!(candidates.contains(declarer), "{declarer}: {candidates:?}");
        }
        assert!(!candidates.contains("src/x/tests.rs"));
    }
}
