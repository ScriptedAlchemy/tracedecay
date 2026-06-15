mod types {
    pub use tracedecay::types::*;
}

#[path = "../src/extraction/annotations.rs"]
mod annotations;

use annotations::{
    emit_annotation_usage, scan_children_for_annotation_kinds, AnnotationEmitterState,
};
use tracedecay::extraction::{ts_provider, JavaExtractor, KotlinExtractor, LanguageExtractor};
use tracedecay::types::{Edge, EdgeKind, Node, NodeKind, UnresolvedRef};
use tree_sitter::{Node as TsNode, Parser};

struct MockState {
    file_path: String,
    source: Vec<u8>,
    timestamp: u64,
    qualified_prefix: String,
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    unresolved_refs: Vec<UnresolvedRef>,
}

impl MockState {
    fn new(file_path: &str, source: &str, qualified_prefix: &str) -> Self {
        Self {
            file_path: file_path.to_string(),
            source: source.as_bytes().to_vec(),
            timestamp: 123,
            qualified_prefix: qualified_prefix.to_string(),
            nodes: Vec::new(),
            edges: Vec::new(),
            unresolved_refs: Vec::new(),
        }
    }
}

impl AnnotationEmitterState for MockState {
    fn extract_annotation_name(&self, annotation_node: TsNode<'_>) -> String {
        self.node_text(annotation_node)
            .trim()
            .trim_start_matches('@')
            .split('(')
            .next()
            .unwrap_or_default()
            .trim()
            .to_string()
    }

    fn file_path(&self) -> &str {
        &self.file_path
    }

    fn qualified_prefix(&self) -> String {
        self.qualified_prefix.clone()
    }

    fn node_text(&self, node: TsNode<'_>) -> String {
        node.utf8_text(&self.source)
            .unwrap_or("<invalid utf8>")
            .to_string()
    }

    fn timestamp(&self) -> u64 {
        self.timestamp
    }

    fn push_node(&mut self, node: Node) {
        self.nodes.push(node);
    }

    fn push_edge(&mut self, edge: Edge) {
        self.edges.push(edge);
    }

    fn push_unresolved_ref(&mut self, unresolved_ref: UnresolvedRef) {
        self.unresolved_refs.push(unresolved_ref);
    }
}

fn find_descendant_by_kind<'tree>(node: TsNode<'tree>, kind: &str) -> Option<TsNode<'tree>> {
    let mut cursor = node.walk();
    if cursor.goto_first_child() {
        loop {
            let child = cursor.node();
            if child.kind() == kind {
                return Some(child);
            }
            if let Some(found) = find_descendant_by_kind(child, kind) {
                return Some(found);
            }
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }

    None
}

fn parse_java_method(source: &str) -> TsNode<'_> {
    let mut parser = Parser::new();
    parser
        .set_language(&ts_provider::language("java"))
        .expect("java grammar should load");
    let tree = parser
        .parse(source, None)
        .expect("java source should parse");
    let leaked = Box::leak(Box::new(tree));
    find_descendant_by_kind(leaked.root_node(), "method_declaration")
        .expect("expected a method_declaration")
}

fn parse_kotlin_function(source: &str) -> TsNode<'_> {
    let mut parser = Parser::new();
    parser
        .set_language(&ts_provider::language("kotlin"))
        .expect("kotlin grammar should load");
    let tree = parser
        .parse(source, None)
        .expect("kotlin source should parse");
    let leaked = Box::leak(Box::new(tree));
    find_descendant_by_kind(leaked.root_node(), "function_declaration")
        .expect("expected a function_declaration")
}

#[test]
fn scans_java_modifier_children_for_marker_and_regular_annotations() {
    let method = parse_java_method(
        r#"
class Example {
    @Deprecated
    @SuppressWarnings("unchecked")
    void oldMethod() {}
}
"#,
    );

    let mut kinds = Vec::new();
    scan_children_for_annotation_kinds(method, &["marker_annotation", "annotation"], |node| {
        kinds.push(node.kind().to_string());
    });

    assert_eq!(kinds, vec!["marker_annotation", "annotation"]);
}

#[test]
fn scans_kotlin_modifier_children_for_annotation_only() {
    let function = parse_kotlin_function("@Deprecated(\"use other\")\nfun oldFunc() {}");

    let mut kinds = Vec::new();
    scan_children_for_annotation_kinds(function, &["annotation"], |node| {
        kinds.push(node.kind().to_string());
    });

    assert_eq!(kinds, vec!["annotation"]);
}

#[test]
fn emits_annotation_usage_node_and_edges() {
    let method = parse_java_method(
        r#"
class Example {
    @SuppressWarnings("unchecked")
    void oldMethod() {}
}
"#,
    );
    let annotation =
        find_descendant_by_kind(method, "annotation").expect("expected an annotation node");
    let mut state = MockState::new(
        "Example.java",
        r#"
class Example {
    @SuppressWarnings("unchecked")
    void oldMethod() {}
}
"#,
        "Example.java::Example::oldMethod",
    );

    emit_annotation_usage(&mut state, annotation, "method-id", 7);

    assert_eq!(state.nodes.len(), 1);
    assert_eq!(state.nodes[0].kind, NodeKind::AnnotationUsage);
    assert_eq!(state.nodes[0].name, "SuppressWarnings");
    assert_eq!(state.nodes[0].attrs_start_line, 7);
    assert_eq!(
        state.nodes[0].signature.as_deref(),
        Some("@SuppressWarnings(\"unchecked\")")
    );
    assert_eq!(state.edges.len(), 1);
    assert_eq!(state.edges[0].kind, EdgeKind::Annotates);
    assert_eq!(state.edges[0].target, "method-id");
    assert_eq!(state.unresolved_refs.len(), 1);
    assert_eq!(state.unresolved_refs[0].reference_kind, EdgeKind::Annotates);
    assert_eq!(state.unresolved_refs[0].reference_name, "SuppressWarnings");
}

#[test]
fn java_extractor_keeps_marker_and_regular_annotations() {
    let source = r#"
public class Foo {
    @Deprecated
    @SuppressWarnings("unchecked")
    public void oldMethod() {}
}
"#;
    let extractor = JavaExtractor;
    let result = extractor.extract("Foo.java", source);
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

    let annots: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::AnnotationUsage)
        .collect();
    assert_eq!(
        annots.len(),
        2,
        "expected one marker and one regular annotation"
    );
    assert!(annots.iter().any(|n| n.name == "Deprecated"));
    assert!(annots.iter().any(|n| n.name == "SuppressWarnings"));

    let annotates_edges: Vec<_> = result
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Annotates)
        .collect();
    assert_eq!(
        annotates_edges.len(),
        2,
        "expected direct annotates edges for both annotations"
    );
}

#[test]
fn kotlin_extractor_keeps_annotation_edges() {
    let source = "@Deprecated(\"use other\")\nfun oldFunc() {}";
    let extractor = KotlinExtractor;
    let result = extractor.extract("test.kt", source);
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

    let annots: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::AnnotationUsage)
        .collect();
    assert_eq!(annots.len(), 1);
    assert_eq!(annots[0].name, "Deprecated");
    assert!(result.edges.iter().any(|e| e.kind == EdgeKind::Annotates));
}
