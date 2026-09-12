use tracedecay_code_extraction::LanguageExtractor;
use tracedecay_code_extraction::MarkdownExtractor;
use tracedecay_domain::*;

#[test]
fn test_markdown_header_hierarchy() {
    let source = "# Top\n\n## Section1\n\n### Deep\n\n## Section2";
    let result = MarkdownExtractor.extract("README.md", source);
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    // Section1 and Section2 should be children of Top
    // Deep should be child of Section1
    let top = result.nodes.iter().find(|n| n.name == "Top").unwrap();
    let section1 = result.nodes.iter().find(|n| n.name == "Section1").unwrap();
    let section2 = result.nodes.iter().find(|n| n.name == "Section2").unwrap();
    let deep = result.nodes.iter().find(|n| n.name == "Deep").unwrap();

    let contains_edges: Vec<_> = result
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Contains)
        .collect();

    // Check Top contains Section1 and Section2
    let top_contains: Vec<_> = contains_edges
        .iter()
        .filter(|e| e.source == top.id)
        .collect();
    assert!(top_contains.iter().any(|e| e.target == section1.id));
    assert!(top_contains.iter().any(|e| e.target == section2.id));

    // Check Section1 contains Deep
    let section1_contains: Vec<_> = contains_edges
        .iter()
        .filter(|e| e.source == section1.id)
        .collect();
    assert!(section1_contains.iter().any(|e| e.target == deep.id));
}

#[test]
fn test_markdown_skips_external_links() {
    let source = "Check [Google](https://google.com) for more.";
    let result = MarkdownExtractor.extract("README.md", source);
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    let uses_edges: Vec<_> = result
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Uses)
        .collect();
    assert!(
        uses_edges.is_empty(),
        "should not create Uses edge for external links"
    );
}

#[test]
fn test_markdown_skips_non_code_links() {
    let source = "See [image](docs/image.png) for diagram.";
    let result = MarkdownExtractor.extract("README.md", source);
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    // .png is not a code extension, so no Uses edge
    let uses_edges: Vec<_> = result
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Uses)
        .collect();
    assert!(
        uses_edges.is_empty(),
        "should not create Uses edge for non-code links"
    );
}

#[test]
fn test_markdown_handles_empty_file() {
    let source = "";
    let result = MarkdownExtractor.extract("README.md", source);
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    // Should still have a File node
    let files: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::File)
        .collect();
    assert_eq!(files.len(), 1);
}

#[test]
fn test_markdown_handles_no_headers() {
    let source = "Just some plain text without any headers.";
    let result = MarkdownExtractor.extract("README.md", source);
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    let modules: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Module)
        .collect();
    assert!(modules.is_empty(), "should have no Module nodes");
}

#[test]
fn test_markdown_multiple_links_same_line() {
    let source = "See [main](src/main.rs) and [lib](src/lib.rs).";
    let result = MarkdownExtractor.extract("README.md", source);
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    let uses_edges: Vec<_> = result
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Uses)
        .collect();
    assert_eq!(uses_edges.len(), 2);
}

#[test]
fn test_markdown_handles_header_with_punctuation() {
    let source = "# Hello, World! (2024)";
    let result = MarkdownExtractor.extract("README.md", source);
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    let modules: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Module)
        .collect();
    assert_eq!(modules.len(), 1);
    assert_eq!(modules[0].name, "Hello, World! (2024)");
}

#[test]
fn test_markdown_link_inside_heading_emits_uses_edge() {
    // `## See [main](src/main.rs)` — the link inside the heading should
    // be captured as a Uses edge parented to that heading.
    let source = "## See [main](src/main.rs)\n";
    let result = MarkdownExtractor.extract("README.md", source);
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

    let uses_edges: Vec<_> = result
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Uses)
        .collect();
    assert_eq!(
        uses_edges.len(),
        1,
        "expected 1 Uses edge for link in heading"
    );

    // The edge should be parented to the heading, not the file.
    let heading = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Module)
        .expect("heading module node");
    assert_eq!(uses_edges[0].source, heading.id);
}

#[test]
fn test_markdown_link_in_heading_does_not_double_count_body_links() {
    // A heading with a link, plus a body paragraph with another link,
    // produces exactly two Uses edges — one per link.
    let source = "# [foo](src/foo.rs)\n\nSee also [bar](src/bar.rs).\n";
    let result = MarkdownExtractor.extract("README.md", source);
    let uses_edges: Vec<_> = result
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Uses)
        .collect();
    assert_eq!(uses_edges.len(), 2);
}
