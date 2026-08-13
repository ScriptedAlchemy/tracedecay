//! Annotation emission behavior, asserted end-to-end through the real
//! `JavaExtractor` / `KotlinExtractor` production path.
//!
//! These tests previously drove the crate-private `emit_annotation_usage` and
//! `scan_children_for_annotation_kinds` helpers through a hand-rolled
//! `AnnotationEmitterState` mock, reached by `#[path]`-including a *copy* of
//! `crates/tracedecay-code-extraction/src/annotations.rs` into this test
//! binary (with a `crate::types` shim in `main.rs` to make the copy compile).
//! The two extractors are the only production callers of those helpers, so
//! driving the extractors covers the same emission logic — node kind, name,
//! signature, `attrs_start_line`, `Annotates` edge, and the unresolved ref —
//! without the mock, and without a source copy that can silently drift from
//! the crate it was copied out of.

use tracedecay::types::{Edge, EdgeKind, ExtractionResult, Node, NodeKind};
use tracedecay_code_extraction::{JavaExtractor, KotlinExtractor, LanguageExtractor};

/// 0-based tree-sitter row of the first source line containing `needle`.
fn row_of(source: &str, needle: &str) -> u32 {
    source
        .lines()
        .position(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("source should contain {needle:?}")) as u32
}

fn annotation_usages(result: &ExtractionResult) -> Vec<&Node> {
    result
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::AnnotationUsage)
        .collect()
}

fn annotation_usage_named<'a>(result: &'a ExtractionResult, name: &str) -> &'a Node {
    annotation_usages(result)
        .into_iter()
        .find(|node| node.name == name)
        .unwrap_or_else(|| panic!("expected an AnnotationUsage node named {name:?}"))
}

fn annotates_edges(result: &ExtractionResult) -> Vec<&Edge> {
    result
        .edges
        .iter()
        .filter(|edge| edge.kind == EdgeKind::Annotates)
        .collect()
}

/// Assert the `Annotates` edge emitted for `annotation` points from the
/// annotation-usage node to the node actually declaring `expected_target_name`.
fn assert_annotates_target(result: &ExtractionResult, annotation: &Node, expected_target_name: &str) {
    let edge = annotates_edges(result)
        .into_iter()
        .find(|edge| edge.source == annotation.id)
        .unwrap_or_else(|| {
            panic!(
                "expected an Annotates edge from the {:?} annotation usage",
                annotation.name
            )
        });
    let target = result
        .nodes
        .iter()
        .find(|node| node.id == edge.target)
        .unwrap_or_else(|| {
            panic!(
                "Annotates edge from {:?} targets id {:?}, which is not an extracted node",
                annotation.name, edge.target
            )
        });
    assert_eq!(
        target.name, expected_target_name,
        "Annotates edge from {:?} should target the annotated declaration",
        annotation.name
    );
}

fn assert_annotates_unresolved_ref(result: &ExtractionResult, annotation: &Node) {
    let unresolved = result
        .unresolved_refs
        .iter()
        .find(|reference| {
            reference.reference_kind == EdgeKind::Annotates
                && reference.from_node_id == annotation.id
        })
        .unwrap_or_else(|| {
            panic!(
                "expected an Annotates unresolved ref from the {:?} annotation usage",
                annotation.name
            )
        });
    assert_eq!(unresolved.reference_name, annotation.name);
    assert_eq!(unresolved.line, annotation.start_line);
    assert_eq!(unresolved.column, annotation.start_column);
}

#[test]
fn java_extractor_emits_marker_and_regular_annotation_usages() {
    let source = r#"
public class Foo {
    @Deprecated
    @SuppressWarnings("unchecked")
    public void oldMethod() {}
}
"#;
    let result = JavaExtractor.extract("Foo.java", source);
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

    // Java accepts both `marker_annotation` (`@Deprecated`) and `annotation`
    // (`@SuppressWarnings(..)`) modifier children; both must be emitted.
    let usages = annotation_usages(&result);
    assert_eq!(
        usages.len(),
        2,
        "expected one marker and one regular annotation, got {:?}",
        usages.iter().map(|node| &node.name).collect::<Vec<_>>()
    );

    let marker = annotation_usage_named(&result, "Deprecated");
    assert_eq!(marker.signature.as_deref(), Some("@Deprecated"));
    assert_eq!(marker.start_line, row_of(source, "@Deprecated"));
    assert_eq!(marker.attrs_start_line, marker.start_line);
    assert_annotates_target(&result, marker, "oldMethod");
    assert_annotates_unresolved_ref(&result, marker);

    let regular = annotation_usage_named(&result, "SuppressWarnings");
    assert_eq!(
        regular.signature.as_deref(),
        Some("@SuppressWarnings(\"unchecked\")"),
        "annotation signature should be the full source text of the annotation"
    );
    assert_eq!(regular.start_line, row_of(source, "@SuppressWarnings"));
    assert_eq!(regular.attrs_start_line, regular.start_line);
    assert_annotates_target(&result, regular, "oldMethod");
    assert_annotates_unresolved_ref(&result, regular);

    assert_eq!(
        annotates_edges(&result).len(),
        2,
        "expected direct annotates edges for both annotations"
    );
}

#[test]
fn kotlin_extractor_emits_annotation_usage() {
    let source = "@Deprecated(\"use other\")\nfun oldFunc() {}";
    let result = KotlinExtractor.extract("test.kt", source);
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

    // Kotlin accepts only the `annotation` modifier-child kind.
    let usages = annotation_usages(&result);
    assert_eq!(usages.len(), 1);

    let annotation = annotation_usage_named(&result, "Deprecated");
    assert_eq!(
        annotation.signature.as_deref(),
        Some("@Deprecated(\"use other\")")
    );
    assert_eq!(annotation.start_line, row_of(source, "@Deprecated"));
    assert_eq!(annotation.attrs_start_line, annotation.start_line);
    assert_annotates_target(&result, annotation, "oldFunc");
    assert_annotates_unresolved_ref(&result, annotation);
}
