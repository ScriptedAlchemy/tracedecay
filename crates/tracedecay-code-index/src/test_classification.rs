//! Generation-bound test classification shared by TestRisk, dead code, and health.
//!
//! A symbol is test code when this generation's canonical logical path, a
//! parser-attested test or `#[cfg(test)]` annotation, or Rust's exact `tests`
//! module says so. A name that merely begins with `test`, or a directory that
//! merely contains those letters, is not evidence. `skip_test_coverage` stays
//! a coverage flag and is not decided here.

use std::collections::{HashMap, HashSet};

use tracedecay_domain::{CanonicalRelationEdgeV1, RelationEdgeKindV1, SymbolOccurrenceId};

use crate::is_test_file;
use crate::is_test_marker;
use crate::lineage::LineageSymbolRecordV1;

/// One symbol visible to the classifier in the generation under read.
pub struct GenerationTestSymbol<'a> {
    pub occurrence: &'a SymbolOccurrenceId,
    pub logical_path: Option<&'a str>,
    pub record: &'a LineageSymbolRecordV1,
}

/// Occurrences in this generation that are test code.
pub fn generation_test_occurrences(
    symbols: &[GenerationTestSymbol<'_>],
    edges: &[CanonicalRelationEdgeV1],
) -> HashSet<SymbolOccurrenceId> {
    let mut test_annotations = HashSet::new();
    let mut cfg_annotations = HashSet::new();
    for symbol in symbols {
        if is_test_marker(symbol.record) {
            test_annotations.insert(symbol.occurrence.clone());
        } else if is_cfg_test_annotation(symbol.record) {
            cfg_annotations.insert(symbol.occurrence.clone());
        }
    }

    let mut test_targets = HashSet::new();
    let mut cfg_targets = HashSet::new();
    for edge in edges {
        if edge.kind != RelationEdgeKindV1::Annotates {
            continue;
        }
        if test_annotations.contains(&edge.from_occurrence) {
            test_targets.insert(edge.to_occurrence.clone());
        }
        if cfg_annotations.contains(&edge.from_occurrence) {
            cfg_targets.insert(edge.to_occurrence.clone());
        }
    }

    let mut module_roots = Vec::new();
    let mut containers = HashSet::new();
    let mut tests = HashSet::new();
    for symbol in symbols {
        let path_test = symbol.logical_path.is_some_and(is_test_file);
        let annotated = test_targets.contains(symbol.occurrence);
        let cfg_test = cfg_targets.contains(symbol.occurrence);
        let test_module = is_rust_test_module(symbol.record);
        let in_test_module = has_test_module_parent(
            symbol.record.qualified_name.as_str(),
            symbol.record.simple_name.as_str(),
        );
        if path_test || annotated || cfg_test || test_module || in_test_module {
            tests.insert(symbol.occurrence.clone());
        }
        if test_module || cfg_test {
            containers.insert(symbol.occurrence.clone());
        }
        if cfg_test && symbol.record.kind == "module" {
            module_roots.push(symbol.record.qualified_name.as_str());
        }
    }

    if !module_roots.is_empty() {
        for symbol in symbols {
            if tests.contains(symbol.occurrence) {
                continue;
            }
            if module_roots
                .iter()
                .any(|root| is_qualified_descendant(symbol.record.qualified_name.as_str(), root))
            {
                tests.insert(symbol.occurrence.clone());
            }
        }
    }

    if containers.is_empty() {
        return tests;
    }
    let mut children: HashMap<SymbolOccurrenceId, Vec<SymbolOccurrenceId>> = HashMap::new();
    for edge in edges {
        if edge.kind == RelationEdgeKindV1::Contains {
            children
                .entry(edge.from_occurrence.clone())
                .or_default()
                .push(edge.to_occurrence.clone());
        }
    }
    let mut stack = containers.iter().cloned().collect::<Vec<_>>();
    let mut seen = containers;
    while let Some(parent) = stack.pop() {
        let Some(contained) = children.get(&parent) else {
            continue;
        };
        let contained = contained.clone();
        for child in contained {
            if !seen.insert(child.clone()) {
                continue;
            }
            tests.insert(child.clone());
            stack.push(child);
        }
    }
    tests
}

fn is_cfg_test_annotation(record: &LineageSymbolRecordV1) -> bool {
    record.kind == "annotation_usage"
        && annotation_leaf(record.simple_name.as_str()) == "cfg"
        && record
            .signature
            .as_deref()
            .is_some_and(is_exact_cfg_test_signature)
}

fn annotation_leaf(name: &str) -> &str {
    name.rsplit("::").next().unwrap_or(name)
}

/// `#[cfg(test)]` with `test` as the only argument. `#[cfg(not(test))]` and
/// `#[cfg(feature = "test")]` are not test-only evidence.
fn is_exact_cfg_test_signature(signature: &str) -> bool {
    let Some(inner) = signature
        .trim()
        .strip_prefix("#[")
        .and_then(|value| value.strip_suffix(']'))
        .map(str::trim)
    else {
        return false;
    };
    let Some(rest) = inner.strip_prefix("cfg").map(str::trim) else {
        return false;
    };
    let Some(args) = rest
        .strip_prefix('(')
        .and_then(|value| value.strip_suffix(')'))
        .map(str::trim)
    else {
        return false;
    };
    args == "test"
}

/// Rust's unit-test module convention: the module's simple name is exactly
/// `tests`. `testnet` and `testimony` do not match.
fn is_rust_test_module(record: &LineageSymbolRecordV1) -> bool {
    record.kind == "module" && record.simple_name == "tests"
}

fn has_test_module_parent(qualified_name: &str, simple_name: &str) -> bool {
    let Some(parent) = qualified_name
        .strip_suffix(simple_name)
        .and_then(|prefix| prefix.strip_suffix("::"))
    else {
        return false;
    };
    !parent.is_empty() && parent.split("::").any(|segment| segment == "tests")
}

fn is_qualified_descendant(qualified_name: &str, root: &str) -> bool {
    qualified_name
        .strip_prefix(root)
        .is_some_and(|rest| rest.starts_with("::"))
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};
    use std::fmt::Debug;

    use tracedecay_code_extraction::parsed_extraction::ParsedExtractionScope;
    use tracedecay_code_extraction::{LanguageExtractor, RustExtractor};
    use tracedecay_domain::{
        ComplexityAnalysisV1, EdgeAuthorityV1, EdgeKind, RelationEdgeKindV1, SourceSpan,
        SymbolOccurrenceId,
    };

    use super::{GenerationTestSymbol, generation_test_occurrences, is_exact_cfg_test_signature};
    use crate::lineage::LineageSymbolRecordV1;

    fn digest<T>(byte: char) -> T
    where
        T: TryFrom<String>,
        T::Error: Debug,
    {
        T::try_from(format!("sha256:{}", byte.to_string().repeat(64))).expect("digest")
    }

    fn record(
        occurrence: &str,
        qualified: &str,
        name: &str,
        kind: &str,
        signature: Option<&str>,
    ) -> LineageSymbolRecordV1 {
        LineageSymbolRecordV1 {
            occurrence: SymbolOccurrenceId::new(occurrence).expect("occurrence"),
            identity: digest('1'),
            qualified_name: qualified.to_owned(),
            simple_name: name.to_owned(),
            kind: kind.to_owned(),
            visibility: "private".to_owned(),
            branches: 0,
            loops: 0,
            max_nesting: 0,
            complexity_analysis: ComplexityAnalysisV1::Complete,
            line_span: 1,
            start_line: 1,
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
        from: &LineageSymbolRecordV1,
        to: &LineageSymbolRecordV1,
        kind: RelationEdgeKindV1,
    ) -> tracedecay_domain::CanonicalRelationEdgeV1 {
        tracedecay_domain::CanonicalRelationEdgeV1 {
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

    fn classify<'a>(
        rows: &'a [(&'a LineageSymbolRecordV1, Option<&'a str>)],
        edges: &[tracedecay_domain::CanonicalRelationEdgeV1],
    ) -> std::collections::HashSet<String> {
        let symbols = rows
            .iter()
            .map(|(record, path)| GenerationTestSymbol {
                occurrence: &record.occurrence,
                logical_path: *path,
                record,
            })
            .collect::<Vec<_>>();
        generation_test_occurrences(&symbols, edges)
            .into_iter()
            .map(|occurrence| occurrence.as_str().to_owned())
            .collect()
    }

    #[test]
    fn cfg_test_signature_is_the_exact_attribute() {
        assert!(is_exact_cfg_test_signature("#[cfg(test)]"));
        assert!(is_exact_cfg_test_signature("  #[cfg( test )]  "));
        assert!(!is_exact_cfg_test_signature("#[cfg(not(test))]"));
        assert!(!is_exact_cfg_test_signature("#[cfg(feature = \"test\")]"));
        assert!(!is_exact_cfg_test_signature(
            "#[cfg(any(test, feature = \"test-helpers\"))]"
        ));
        assert!(!is_exact_cfg_test_signature("#[test]"));
    }

    #[test]
    fn production_names_and_test_prefixed_directories_stay_production() {
        let connection = record(
            "symbol.connection",
            "src/client.rs::test_connection",
            "test_connection",
            "function",
            None,
        );
        let testimony = record(
            "symbol.testimony",
            "src/client.rs::testimony",
            "testimony",
            "function",
            None,
        );
        let connect = record(
            "symbol.connect",
            "src/testnet/client.rs::connect",
            "connect",
            "function",
            None,
        );
        let tests_fn = record(
            "symbol.testsfn",
            "src/client.rs::tests",
            "tests",
            "function",
            None,
        );
        let rows = [
            (&connection, Some("src/client.rs")),
            (&testimony, Some("src/client.rs")),
            (&connect, Some("src/testnet/client.rs")),
            (&tests_fn, Some("src/client.rs")),
        ];
        assert!(classify(&rows, &[]).is_empty());
    }

    #[test]
    fn annotations_cfg_test_modules_and_test_paths_are_tests() {
        let case = record(
            "symbol.case",
            "src/client.rs::annotated_case",
            "annotated_case",
            "function",
            None,
        );
        let marker = record(
            "symbol.marker",
            "src/client.rs::@test",
            "tokio::test",
            "annotation_usage",
            Some("#[tokio::test]"),
        );
        let helpers = record(
            "symbol.helpers",
            "src/client.rs::helpers",
            "helpers",
            "module",
            None,
        );
        let cfg = record(
            "symbol.cfg",
            "src/client.rs::@cfg",
            "cfg",
            "annotation_usage",
            Some("#[cfg(test)]"),
        );
        let fixture = record(
            "symbol.fixture",
            "src/client.rs::helpers::fixture_fn",
            "fixture_fn",
            "function",
            None,
        );
        let tests_mod = record(
            "symbol.tests",
            "src/client.rs::tests",
            "tests",
            "module",
            None,
        );
        let helper = record(
            "symbol.helper",
            "src/client.rs::tests::module_helper",
            "module_helper",
            "function",
            None,
        );
        let covers = record(
            "symbol.covers",
            "tests/real.rs::covers_case",
            "covers_case",
            "function",
            None,
        );
        let not_cfg = record(
            "symbol.notcfg",
            "src/client.rs::kept",
            "kept",
            "function",
            None,
        );
        let not_marker = record(
            "symbol.notmarker",
            "src/client.rs::@cfg",
            "cfg",
            "annotation_usage",
            Some("#[cfg(not(test))]"),
        );
        let widget = record(
            "symbol.widget",
            "src/client.rs::Widget",
            "Widget",
            "impl",
            None,
        );
        let cfg_impl = record(
            "symbol.cfgimpl",
            "src/client.rs::@cfg",
            "cfg",
            "annotation_usage",
            Some("#[cfg(test)]"),
        );
        let hidden = record(
            "symbol.hidden",
            "src/client.rs::Widget::hidden_helper",
            "hidden_helper",
            "method",
            None,
        );
        let sibling = record(
            "symbol.sibling",
            "src/client.rs::Widget::sibling",
            "sibling",
            "method",
            None,
        );
        let rows = [
            (&case, Some("src/client.rs")),
            (&marker, Some("src/client.rs")),
            (&helpers, Some("src/client.rs")),
            (&cfg, Some("src/client.rs")),
            (&fixture, Some("src/client.rs")),
            (&tests_mod, Some("src/client.rs")),
            (&helper, Some("src/client.rs")),
            (&covers, Some("tests/real.rs")),
            (&not_cfg, Some("src/client.rs")),
            (&not_marker, Some("src/client.rs")),
            (&widget, Some("src/client.rs")),
            (&cfg_impl, Some("src/client.rs")),
            (&hidden, Some("src/client.rs")),
            (&sibling, Some("src/client.rs")),
        ];
        let edges = [
            edge(&marker, &case, RelationEdgeKindV1::Annotates),
            edge(&cfg, &helpers, RelationEdgeKindV1::Annotates),
            edge(&helpers, &fixture, RelationEdgeKindV1::Contains),
            edge(&tests_mod, &helper, RelationEdgeKindV1::Contains),
            edge(&not_marker, &not_cfg, RelationEdgeKindV1::Annotates),
            edge(&cfg_impl, &widget, RelationEdgeKindV1::Annotates),
            edge(&widget, &hidden, RelationEdgeKindV1::Contains),
        ];
        let classified = classify(&rows, &edges);
        for occurrence in [
            "symbol.case",
            "symbol.helpers",
            "symbol.fixture",
            "symbol.tests",
            "symbol.helper",
            "symbol.covers",
            "symbol.widget",
            "symbol.hidden",
        ] {
            assert!(classified.contains(occurrence), "{occurrence}");
        }
        assert!(!classified.contains("symbol.notcfg"));
        assert!(!classified.contains("symbol.marker"));
        assert!(
            !classified.contains("symbol.sibling"),
            "a cfg(test) impl does not mark every method that shares its type name"
        );
    }

    #[test]
    fn project_orchestrator_keeps_a_test_prefixed_production_method() {
        // The declarations below are the ones in
        // `crates/tracedecay-project/src/project.rs`: a `#[cfg(test)]` module
        // and a production method whose name starts with `test` but whose
        // cfg is `any(test, feature = "test-helpers")`.
        const SOURCE: &str = r#"
#[cfg(test)]
mod concrete_runtime_tests;

pub struct TraceDecay;

impl TraceDecay {
    #[doc(hidden)]
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn test_runtime_for_test(&self) {}

    pub fn profile_database(&self) {}
}
"#;
        let path = "crates/tracedecay-project/src/project.rs";
        let language =
            tracedecay_code_extraction::ts_provider::try_language("rust").expect("rust grammar");
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&language)
            .expect("configure rust parser");
        let tree = parser.parse(SOURCE, None).expect("parse project.rs");
        let extracted = RustExtractor.extract_parsed_artifact(
            path,
            SOURCE,
            SOURCE,
            &tree,
            ParsedExtractionScope::FullDocument,
        );
        let nodes = extracted.artifact.result.nodes;
        let mut occurrences = HashMap::new();
        let records = nodes
            .iter()
            .enumerate()
            .map(|(ordinal, node)| {
                let occurrence = SymbolOccurrenceId::new(format!("symbol.project.{ordinal}"))
                    .expect("occurrence");
                occurrences.insert(node.id.clone(), occurrence.clone());
                record_from_node(occurrence, node)
            })
            .collect::<Vec<_>>();
        let edges = extracted
            .artifact
            .result
            .edges
            .iter()
            .filter_map(|edge| {
                let kind = match edge.kind {
                    EdgeKind::Annotates => RelationEdgeKindV1::Annotates,
                    EdgeKind::Contains => RelationEdgeKindV1::Contains,
                    _ => return None,
                };
                Some(tracedecay_domain::CanonicalRelationEdgeV1 {
                    from_occurrence: occurrences.get(&edge.source)?.clone(),
                    to_occurrence: occurrences.get(&edge.target)?.clone(),
                    kind,
                    authority: EdgeAuthorityV1::SyntaxExact,
                    evidence_span: SourceSpan {
                        start_byte: 0,
                        end_byte: 1,
                    },
                })
            })
            .collect::<Vec<_>>();
        let symbols = records
            .iter()
            .map(|row| GenerationTestSymbol {
                occurrence: &row.occurrence,
                logical_path: Some(path),
                record: row,
            })
            .collect::<Vec<_>>();
        let classified = generation_test_occurrences(&symbols, &edges);
        let names = |wanted: &HashSet<SymbolOccurrenceId>| {
            records
                .iter()
                .filter(|row| wanted.contains(&row.occurrence))
                .map(|row| row.simple_name.as_str())
                .collect::<std::collections::BTreeSet<_>>()
        };
        let test_names = names(&classified);
        assert!(
            records
                .iter()
                .any(|row| row.simple_name == "test_runtime_for_test"),
            "project.rs must still declare test_runtime_for_test"
        );
        assert!(
            !test_names.contains("test_runtime_for_test"),
            "production method stayed out of the test set, classified={test_names:?}"
        );
        assert!(
            test_names.contains("concrete_runtime_tests"),
            "cfg(test) module stayed a test, classified={test_names:?}"
        );
    }

    fn record_from_node(
        occurrence: SymbolOccurrenceId,
        node: &tracedecay_domain::Node,
    ) -> LineageSymbolRecordV1 {
        LineageSymbolRecordV1 {
            occurrence,
            identity: digest('4'),
            qualified_name: node.qualified_name.clone(),
            simple_name: node.name.clone(),
            kind: node.kind.as_str().to_owned(),
            visibility: "private".to_owned(),
            branches: node.branches,
            loops: node.loops,
            max_nesting: node.max_nesting,
            complexity_analysis: node.complexity_analysis,
            line_span: node
                .end_line
                .saturating_sub(node.start_line)
                .saturating_add(1),
            start_line: node.start_line,
            signature: node.signature.clone(),
            docstring: node.docstring.clone(),
            is_async: node.is_async,
            derives: Vec::new(),
            skip_test_coverage: false,
            file_identity: digest('5'),
            content_digest: digest('6'),
        }
    }
}
