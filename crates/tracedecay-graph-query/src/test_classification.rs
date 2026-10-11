//! The one generation-bound answer to "is this symbol test code?" that test
//! risk, dead code, and health share.
//!
//! Evidence is canonical: the logical path ([`is_test_file`]), parser-attested
//! test annotations and `#[cfg(test)]` gates joined through `Annotates` edges,
//! and extracted spans and qualified names. A name alone never makes a symbol
//! test code. Inside a test file, a `test` prefix marks the unannotated test
//! functions that pytest, Go's `testing`, and JUnit 3 discover by name.

use std::collections::{HashMap, HashSet};

use tracedecay_code_index::graph_projection::CodeGraphSymbolSummaryV1;
use tracedecay_code_index::lineage::LineageSymbolRecordV1;
use tracedecay_code_index::{is_test_file, is_test_gate_marker, is_test_marker};
use tracedecay_domain::{CanonicalRelationEdgeV1, RelationEdgeKindV1};

#[derive(Default)]
pub(crate) struct TestClassification {
    /// Targets of a test annotation such as `#[test]` or `#[tokio::test]`.
    annotated: HashSet<String>,
    /// Zero-based inclusive line ranges of `#[cfg(test)]` items, by file.
    gated_lines: HashMap<String, Vec<(u32, u32)>>,
    /// `dir` for each `#[cfg(test)]` module: `dir.rs` and every file below
    /// `dir/` hold that module's out-of-line code.
    gated_module_dirs: HashSet<String>,
}

impl TestClassification {
    /// `symbols` pairs records with their logical paths. `edges` must hold the
    /// `Annotates` edges into those symbols; other edge kinds are ignored.
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
        let mut classification = Self::default();
        let mut gated = HashSet::new();
        for edge in edges {
            if edge.kind != RelationEdgeKindV1::Annotates {
                continue;
            }
            if test_markers.contains(&edge.from_occurrence) {
                classification
                    .annotated
                    .insert(edge.to_occurrence.as_str().to_owned());
            } else if gate_markers.contains(&edge.from_occurrence) {
                gated.insert(&edge.to_occurrence);
            }
        }
        for (file, record) in symbols {
            if !gated.contains(&record.occurrence) {
                continue;
            }
            let last_line = record
                .start_line
                .saturating_add(record.line_span.saturating_sub(1));
            classification
                .gated_lines
                .entry((*file).to_owned())
                .or_default()
                .push((record.start_line, last_line));
            if record.kind == "module"
                && let Some(dir) = rust_module_dir(file, &record.qualified_name)
            {
                classification.gated_module_dirs.insert(dir);
            }
        }
        classification
    }

    /// Code that exists only to test: a test file, a test annotation's target,
    /// or anything a `#[cfg(test)]` item encloses.
    pub(crate) fn is_test_code(&self, file: &str, record: &LineageSymbolRecordV1) -> bool {
        is_test_file(file)
            || self.is_test_annotated(record.occurrence.as_str())
            || self.gated_lines.get(file).is_some_and(|ranges| {
                ranges
                    .iter()
                    .any(|(first, last)| (*first..=*last).contains(&record.start_line))
            })
            || file
                .strip_suffix(".rs")
                .is_some_and(|stem| self.gated_module_dirs.contains(stem))
            || file
                .match_indices('/')
                .any(|(end, _)| self.gated_module_dirs.contains(&file[..end]))
    }

    /// A test a runner invokes itself, which is live with no caller.
    pub(crate) fn is_test_entrypoint(&self, file: &str, record: &LineageSymbolRecordV1) -> bool {
        self.is_test_annotated(record.occurrence.as_str())
            || (is_test_file(file)
                && record
                    .simple_name
                    .get(..4)
                    .is_some_and(|prefix| prefix.eq_ignore_ascii_case("test")))
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

    fn annotates(
        from: &LineageSymbolRecordV1,
        to: &LineageSymbolRecordV1,
    ) -> CanonicalRelationEdgeV1 {
        CanonicalRelationEdgeV1 {
            from_occurrence: from.occurrence.clone(),
            to_occurrence: to.occurrence.clone(),
            kind: RelationEdgeKindV1::Annotates,
            authority: EdgeAuthorityV1::SyntaxExact,
            evidence_span: SourceSpan {
                start_byte: 0,
                end_byte: 1,
            },
        }
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
            &feature_gate,
            &shared,
            &shared_member,
            &after,
        ]
        .map(|record| (file, record));
        let edges = [annotates(&gate, &gated), annotates(&feature_gate, &shared)];
        let classification = TestClassification::new(&symbols, &edges);

        assert!(classification.is_test_code(file, &gated));
        assert!(classification.is_test_code(file, &member));
        assert!(!classification.is_test_code(file, &shared_member));
        assert!(!classification.is_test_code(file, &after));
        assert!(classification.is_test_code("src/tests/deep.rs", &after));
        assert!(classification.is_test_code("src/tests.rs", &after));
        assert!(!classification.is_test_code("src/testsuite.rs", &after));
        assert!(!classification.is_test_entrypoint(file, &member));
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
        let production = record(
            "symbol.prod",
            "src/client.rs::test_connection",
            "function",
            (0, 1),
            None,
        );
        let classification = TestClassification::new(&[], &[]);

        assert!(classification.is_test_entrypoint("tests/test_client.py", &pytest));
        assert!(!classification.is_test_entrypoint("src/client.rs", &production));
        assert!(!classification.is_test_code("src/client.rs", &production));
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
