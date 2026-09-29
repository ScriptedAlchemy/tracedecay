use tracedecay_code_extraction::LanguageExtractor;
use tracedecay_code_extraction::MarkdownExtractor;
use tracedecay_domain::*;

fn extract(source: &str) -> ExtractionResult {
    let result = MarkdownExtractor
        .extract_artifact("README.md", source)
        .result;
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    result
}

/// `(source node name, target file id)` for every `Uses` edge, in emission order.
fn uses_edges(result: &ExtractionResult) -> Vec<(&str, &str)> {
    result
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Uses)
        .map(|e| {
            let source = result
                .nodes
                .iter()
                .find(|n| n.id == e.source)
                .expect("uses edge source is an extracted node");
            (source.name.as_str(), e.target.as_str())
        })
        .collect()
}

fn file_id(path: &str) -> String {
    generate_node_id(path, &NodeKind::File, path, 0)
}

#[test]
fn test_markdown_header_hierarchy() {
    let source = "# Top\n\n## Section1\n\n### Deep\n\n## Section2";
    let result = MarkdownExtractor
        .extract_artifact("README.md", source)
        .result;
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
    let result = extract("Check [Google](https://google.com) and [main](src/main.rs).");
    let main = file_id("src/main.rs");
    assert_eq!(
        uses_edges(&result),
        [("README.md", main.as_str())],
        "only the repository code link becomes a Uses edge"
    );
}

#[test]
fn test_markdown_skips_non_code_links() {
    // .png is not a code extension, so only the .rs link is a Uses edge.
    let result = extract("See [image](docs/image.png) and [lib](src/lib.rs).");
    let lib = file_id("src/lib.rs");
    assert_eq!(uses_edges(&result), [("README.md", lib.as_str())]);
}

#[test]
fn test_markdown_handles_empty_file() {
    let source = "";
    let result = MarkdownExtractor
        .extract_artifact("README.md", source)
        .result;
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
    let result = extract("Just some plain text without any headers.");
    let nodes: Vec<_> = result
        .nodes
        .iter()
        .map(|n| (n.kind.clone(), n.name.as_str()))
        .collect();
    assert_eq!(nodes, [(NodeKind::File, "README.md")]);
}

#[test]
fn test_markdown_multiple_links_same_line() {
    let result = extract("See [main](src/main.rs) and [lib](src/lib.rs).");
    let (main, lib) = (file_id("src/main.rs"), file_id("src/lib.rs"));
    assert_eq!(
        uses_edges(&result),
        [("README.md", main.as_str()), ("README.md", lib.as_str())]
    );
}

#[test]
fn test_markdown_handles_header_with_punctuation() {
    let source = "# Hello, World! (2024)";
    let result = MarkdownExtractor
        .extract_artifact("README.md", source)
        .result;
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
    // `## See [main](src/main.rs)`. The link inside the heading should
    // be captured as a Uses edge parented to that heading.
    let result = extract("## See [main](src/main.rs)\n");
    let main = file_id("src/main.rs");
    assert_eq!(
        uses_edges(&result),
        [("See [main](src/main.rs)", main.as_str())],
        "the edge is parented to the heading, not the file"
    );
}

#[test]
fn test_markdown_link_in_heading_does_not_double_count_body_links() {
    // A heading with a link, plus a body paragraph with another link,
    // produces exactly one Uses edge per link, both under the heading.
    let result = extract("# [foo](src/foo.rs)\n\nSee also [bar](src/bar.rs).\n");
    let (foo, bar) = (file_id("src/foo.rs"), file_id("src/bar.rs"));
    assert_eq!(
        uses_edges(&result),
        [
            ("[foo](src/foo.rs)", foo.as_str()),
            ("[foo](src/foo.rs)", bar.as_str())
        ]
    );
}
