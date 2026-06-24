//! Compact rendering for lists of related file paths.

use std::collections::BTreeMap;

#[derive(Default)]
struct PathNode {
    children: BTreeMap<String, PathNode>,
    suffix: Option<String>,
}

impl PathNode {
    fn insert(&mut self, path: &str, suffix: &str) {
        let normalized = path.replace('\\', "/");
        let parts = normalized
            .split('/')
            .filter(|part| !part.is_empty() && *part != ".")
            .collect::<Vec<_>>();
        if parts.is_empty() {
            return;
        }

        let mut node = self;
        for part in parts {
            node = node.children.entry(part.to_string()).or_default();
        }
        node.suffix = Some(suffix.to_string());
    }
}

pub(crate) fn format_path_tree<'a>(paths: impl IntoIterator<Item = &'a str>) -> String {
    format_annotated_path_tree(paths.into_iter().map(|path| (path, String::new())))
}

pub(crate) fn format_annotated_path_tree<'a>(
    paths: impl IntoIterator<Item = (&'a str, String)>,
) -> String {
    let mut root = PathNode::default();
    for (path, suffix) in paths {
        root.insert(path, &suffix);
    }

    let mut lines = Vec::new();
    render_children(&root.children, 0, &mut lines);
    lines.join("\n")
}

fn render_children(children: &BTreeMap<String, PathNode>, indent: usize, lines: &mut Vec<String>) {
    for (segment, child) in children {
        render_entry(segment, child, indent, lines);
    }
}

fn render_entry(segment: &str, node: &PathNode, indent: usize, lines: &mut Vec<String>) {
    let padding = " ".repeat(indent);
    if let Some(suffix) = &node.suffix {
        lines.push(format!("{padding}{segment}{suffix}"));
    }

    if node.children.is_empty() {
        return;
    }

    let (label, remainder) = compact_directory_chain(segment, node);
    lines.push(format!("{padding}{label}/"));
    render_children(&remainder.children, indent + 2, lines);
}

fn compact_directory_chain<'a>(segment: &str, mut node: &'a PathNode) -> (String, &'a PathNode) {
    let mut label = segment.to_string();
    while node.suffix.is_none() && node.children.len() == 1 {
        let Some((next_segment, next_node)) = node.children.iter().next() else {
            break;
        };
        if next_node.children.is_empty() {
            break;
        }
        label.push('/');
        label.push_str(next_segment);
        node = next_node;
    }
    (label, node)
}

#[cfg(test)]
mod tests {
    use super::{format_annotated_path_tree, format_path_tree};

    #[test]
    fn compacts_shared_directory_prefixes() {
        let tree = format_path_tree([
            "tests/gateway/test_gateway_shutdown.py",
            "tests/gateway/test_goal_verdict_send.py",
            "tests/gateway/test_homeassistant.py",
        ]);

        assert_eq!(
            tree,
            "tests/gateway/\n  test_gateway_shutdown.py\n  test_goal_verdict_send.py\n  test_homeassistant.py"
        );
    }

    #[test]
    fn preserves_leaf_suffixes() {
        let tree = format_annotated_path_tree([
            ("src/a.rs", " (edited 1m ago)".to_string()),
            ("src/b.rs", " (edited 2m ago)".to_string()),
        ]);

        assert_eq!(tree, "src/\n  a.rs (edited 1m ago)\n  b.rs (edited 2m ago)");
    }
}
