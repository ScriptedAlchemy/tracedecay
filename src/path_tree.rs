//! Compact rendering for lists of related file paths.

use std::collections::BTreeMap;

#[derive(Default)]
struct PathNode {
    children: BTreeMap<String, PathNode>,
    suffix: Option<String>,
}

impl PathNode {
    fn insert(&mut self, path: &str, suffix: &str) {
        let mut node = self;
        let mut inserted = false;
        for part in path
            .split(['/', '\\'])
            .filter(|part| !part.is_empty() && *part != ".")
        {
            inserted = true;
            node = node.children.entry(part.to_string()).or_default();
        }
        if inserted {
            node.suffix = Some(suffix.to_string());
        }
    }
}

pub(crate) fn format_compact_path_list<'a>(
    paths: impl IntoIterator<Item = &'a str>,
    bullet_prefix: &str,
    tree_prefix: &str,
) -> String {
    format_compact_annotated_path_list(
        paths.into_iter().map(|path| (path, "")),
        bullet_prefix,
        tree_prefix,
    )
}

pub(crate) fn format_compact_annotated_path_list<'a, S>(
    paths: impl IntoIterator<Item = (&'a str, S)>,
    bullet_prefix: &str,
    tree_prefix: &str,
) -> String
where
    S: AsRef<str>,
{
    let mut root = PathNode::default();
    let mut bullet_lines = Vec::new();
    for (path, suffix) in paths {
        let suffix = suffix.as_ref();
        bullet_lines.push(format!("{bullet_prefix}{path}{suffix}"));
        root.insert(path, suffix);
    }

    let bullet_list = bullet_lines.join("\n");
    let tree = prefix_lines(&render_path_tree(&root), tree_prefix);
    if has_directory_shape(&root) && tree.len() < bullet_list.len() {
        tree
    } else {
        bullet_list
    }
}

fn render_path_tree(root: &PathNode) -> String {
    let mut lines = Vec::new();
    render_children(&root.children, 0, &mut lines);
    lines.join("\n")
}

fn prefix_lines(text: &str, prefix: &str) -> String {
    if prefix.is_empty() {
        return text.to_string();
    }

    text.lines()
        .map(|line| format!("{prefix}{line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn has_directory_shape(node: &PathNode) -> bool {
    node.children
        .values()
        .any(|child| !child.children.is_empty() || has_directory_shape(child))
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
    use super::{format_compact_annotated_path_list, format_compact_path_list};

    #[test]
    fn compacts_shared_directory_prefixes() {
        let list = format_compact_path_list(
            [
                "tests/gateway/test_gateway_shutdown.py",
                "tests/gateway/test_goal_verdict_send.py",
                "tests/gateway/test_homeassistant.py",
            ],
            "- ",
            "",
        );

        assert_eq!(
            list,
            "tests/gateway/\n  test_gateway_shutdown.py\n  test_goal_verdict_send.py\n  test_homeassistant.py"
        );
    }

    #[test]
    fn preserves_leaf_suffixes() {
        let list = format_compact_annotated_path_list(
            [
                ("src/a.rs", " (edited 1m ago)"),
                ("src/b.rs", " (edited 2m ago)"),
            ],
            "- ",
            "",
        );

        assert_eq!(list, "src/\n  a.rs (edited 1m ago)\n  b.rs (edited 2m ago)");
    }

    #[test]
    fn keeps_bullets_when_tree_is_not_shorter() {
        let list = format_compact_path_list(["src/main.rs"], "- ", "");

        assert_eq!(list, "- src/main.rs");
    }

    #[test]
    fn keeps_bullets_for_flat_paths() {
        let list = format_compact_path_list(["a.rs", "b.rs"], "- ", "");

        assert_eq!(list, "- a.rs\n- b.rs");
    }

    #[test]
    fn indents_compact_annotated_tree() {
        let list = format_compact_annotated_path_list(
            [
                ("src/a.rs", " (edited 1m ago)"),
                ("src/b.rs", " (edited 2m ago)"),
            ],
            "  - ",
            "  ",
        );

        assert_eq!(
            list,
            "  src/\n    a.rs (edited 1m ago)\n    b.rs (edited 2m ago)"
        );
    }
}
