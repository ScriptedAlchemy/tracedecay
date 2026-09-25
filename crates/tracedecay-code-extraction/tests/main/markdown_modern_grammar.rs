//! Migration smoke tests for `tree-sitter-grammars/tree-sitter-markdown`.
//!
//! Pre-migration these inputs caused the markdown extractor to hang or
//! segfault because the old `ikatyang/tree-sitter-markdown` grammar parsed
//! YAML frontmatter as ambiguous markdown (GLR fork explosion). The new
//! grammar produces an opaque `(minus_metadata)` node so the body's
//! markdown rules never see the YAML.
use std::time::{Duration, Instant};
use tracedecay_domain::NodeKind;

fn timed_extract(source: String, timeout: Duration) -> Option<(f64, Vec<(NodeKind, String)>)> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let t0 = Instant::now();
        let res = tracedecay_code_extraction::MarkdownExtractor::extract_markdown("t.md", &source);
        let nodes = res.nodes.into_iter().map(|n| (n.kind, n.name)).collect();
        let _ = tx.send((t0.elapsed().as_secs_f64(), nodes));
    });
    rx.recv_timeout(timeout).ok()
}

/// 4.4 KB / 113-line YAML-frontmatter-heavy file that hung the old grammar
/// indefinitely. With the new grammar it must parse in well under a second.
/// The fixture's frontmatter is never closed, so the whole file is metadata
/// and its YAML list items must not surface as document structure.
#[test]
fn yaml_frontmatter_hang_reproducer() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/markdown_yaml_frontmatter_hang.md"
    );
    let src = std::fs::read_to_string(path).expect("fixture missing");
    let Some((t, nodes)) = timed_extract(src, Duration::from_secs(5)) else {
        panic!("hang reproducer still hung > 5s");
    };
    assert!(t < 1.0, "should parse fast, took {t:.3}s");
    assert_eq!(nodes, vec![(NodeKind::File, "t.md".to_owned())]);
}

#[test]
fn frontmatter_is_opaque() {
    // YAML frontmatter content that would otherwise look like markdown
    // (a `# heading`-like line, a `- list item`) must NOT produce Module
    // nodes. It's metadata, not document structure.
    let src = "---\ntitle: My Doc\n# this is yaml comment style\n- bogus\n---\n\n# Real Heading\n";
    let res = tracedecay_code_extraction::MarkdownExtractor::extract_markdown("doc.md", src);
    let module_names: Vec<&str> = res
        .nodes
        .iter()
        .filter(|n| matches!(n.kind, tracedecay_domain::NodeKind::Module))
        .map(|n| n.name.as_str())
        .collect();
    assert_eq!(
        module_names,
        vec!["Real Heading"],
        "frontmatter content should not produce Module nodes"
    );
}
