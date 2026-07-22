#![allow(dead_code)]

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use tree_sitter::{Node, Parser, Tree};
use walkdir::WalkDir;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct CallSite {
    pub callee: String,
    pub line: usize,
}

pub struct RustAst {
    source: String,
    tree: Tree,
}

impl RustAst {
    pub fn parse(relative: &str) -> Self {
        let path = repository_root().join(relative);
        let source = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read Rust source {}: {error}", path.display()));
        let mut parser = Parser::new();
        parser
            .set_language(
                &tracedecay::extraction::ts_provider::language("rust")
                    .expect("bundled Rust grammar"),
            )
            .expect("configure Rust parser");
        let tree = parser
            .parse(&source, None)
            .unwrap_or_else(|| panic!("parse Rust source {}", path.display()));
        assert!(
            !tree.root_node().has_error(),
            "Rust source must parse without syntax errors: {}",
            path.display()
        );
        Self { source, tree }
    }

    pub fn item_names(&self, kind: &str) -> BTreeSet<String> {
        let mut names = BTreeSet::new();
        walk(self.tree.root_node(), &mut |node| {
            if node.kind() == kind
                && let Some(name) = node.child_by_field_name("name")
            {
                names.insert(self.text(name).to_owned());
            }
        });
        names
    }

    pub fn enum_variants(&self, enum_name: &str) -> BTreeSet<String> {
        let Some(item) = self.named_item("enum_item", enum_name) else {
            return BTreeSet::new();
        };
        let mut variants = BTreeSet::new();
        walk(item, &mut |node| {
            if node.kind() == "enum_variant"
                && let Some(name) = node.child_by_field_name("name")
            {
                variants.insert(self.text(name).to_owned());
            }
        });
        variants
    }

    pub fn method_names(&self, impl_type: &str) -> BTreeSet<String> {
        let mut methods = BTreeSet::new();
        for item in self.impl_items(impl_type) {
            walk(item, &mut |node| {
                if node.kind() == "function_item"
                    && let Some(name) = node.child_by_field_name("name")
                {
                    methods.insert(self.text(name).to_owned());
                }
            });
        }
        methods
    }

    pub fn trait_methods(&self, trait_name: &str) -> BTreeSet<String> {
        let Some(item) = self.named_item("trait_item", trait_name) else {
            return BTreeSet::new();
        };
        let mut methods = BTreeSet::new();
        walk(item, &mut |node| {
            if matches!(node.kind(), "function_item" | "function_signature_item")
                && let Some(name) = node.child_by_field_name("name")
            {
                methods.insert(self.text(name).to_owned());
            }
        });
        methods
    }

    pub fn method_paths(&self, impl_type: &str, method_name: &str) -> BTreeSet<String> {
        let mut paths = BTreeSet::new();
        for method in self.methods(impl_type, method_name) {
            self.collect_paths(method, &mut paths);
        }
        paths
    }

    pub fn method_identifiers(&self, impl_type: &str, method_name: &str) -> BTreeSet<String> {
        let mut identifiers = BTreeSet::new();
        for method in self.methods(impl_type, method_name) {
            walk(method, &mut |node| {
                if node.kind() == "identifier" {
                    identifiers.insert(self.text(node).to_owned());
                }
            });
        }
        identifiers
    }

    pub fn function_paths(&self, function_name: &str) -> BTreeSet<String> {
        let mut paths = BTreeSet::new();
        for function in self.functions(function_name) {
            self.collect_paths(function, &mut paths);
        }
        paths
    }

    pub fn method_calls(&self, impl_type: &str, method_name: &str) -> Vec<CallSite> {
        let mut calls = Vec::new();
        for method in self.methods(impl_type, method_name) {
            self.collect_calls(method, &mut calls, false);
        }
        calls.sort();
        calls
    }

    pub fn function_calls(&self, function_name: &str) -> Vec<CallSite> {
        let mut calls = Vec::new();
        for function in self.functions(function_name) {
            self.collect_calls(function, &mut calls, false);
        }
        calls.sort();
        calls
    }

    pub fn production_calls(&self) -> Vec<CallSite> {
        let mut calls = Vec::new();
        self.collect_calls(self.tree.root_node(), &mut calls, true);
        calls.sort();
        calls
    }

    pub fn function_binary_expressions(&self, function_name: &str) -> BTreeSet<String> {
        let mut expressions = BTreeSet::new();
        for function in self.functions(function_name) {
            walk(function, &mut |node| {
                if node.kind() == "binary_expression" {
                    expressions.insert(normalize(self.text(node)));
                }
            });
        }
        expressions
    }

    pub fn const_string_literals(&self, const_name: &str) -> BTreeSet<String> {
        let Some(item) = self.named_item("const_item", const_name) else {
            return BTreeSet::new();
        };
        let mut values = BTreeSet::new();
        walk(item, &mut |node| {
            if node.kind() == "string_literal" {
                let raw = self.text(node);
                if let Ok(value) = serde_json::from_str::<String>(raw) {
                    values.insert(value);
                }
            }
        });
        values
    }

    fn named_item<'tree>(&'tree self, kind: &str, name: &str) -> Option<Node<'tree>> {
        let mut found = None;
        walk(self.tree.root_node(), &mut |node| {
            if found.is_none()
                && node.kind() == kind
                && node
                    .child_by_field_name("name")
                    .is_some_and(|item_name| self.text(item_name) == name)
            {
                found = Some(node);
            }
        });
        found
    }

    fn impl_items<'tree>(&'tree self, impl_type: &str) -> Vec<Node<'tree>> {
        let mut items = Vec::new();
        walk(self.tree.root_node(), &mut |node| {
            if node.kind() == "impl_item"
                && node
                    .child_by_field_name("type")
                    .is_some_and(|item_type| normalize(self.text(item_type)).starts_with(impl_type))
            {
                items.push(node);
            }
        });
        items
    }

    fn methods<'tree>(&'tree self, impl_type: &str, method_name: &str) -> Vec<Node<'tree>> {
        let mut methods = Vec::new();
        for item in self.impl_items(impl_type) {
            walk(item, &mut |node| {
                if node.kind() == "function_item"
                    && node
                        .child_by_field_name("name")
                        .is_some_and(|name| self.text(name) == method_name)
                {
                    methods.push(node);
                }
            });
        }
        methods
    }

    fn functions<'tree>(&'tree self, function_name: &str) -> Vec<Node<'tree>> {
        let mut functions = Vec::new();
        walk(self.tree.root_node(), &mut |node| {
            if node.kind() == "function_item"
                && node
                    .child_by_field_name("name")
                    .is_some_and(|name| self.text(name) == function_name)
            {
                functions.push(node);
            }
        });
        functions
    }

    fn collect_paths(&self, root: Node<'_>, paths: &mut BTreeSet<String>) {
        walk(root, &mut |node| {
            if matches!(node.kind(), "scoped_identifier" | "scoped_type_identifier") {
                paths.insert(normalize(self.text(node)));
            }
        });
    }

    fn collect_calls(&self, root: Node<'_>, calls: &mut Vec<CallSite>, skip_tests: bool) {
        walk(root, &mut |node| {
            if node.kind() != "call_expression" || (skip_tests && self.is_test_scope(node)) {
                return;
            }
            let Some(function) = node.child_by_field_name("function") else {
                return;
            };
            calls.push(CallSite {
                callee: normalize(self.text(function)),
                line: node.start_position().row + 1,
            });
        });
    }

    fn is_test_scope(&self, mut node: Node<'_>) -> bool {
        while let Some(parent) = node.parent() {
            if matches!(parent.kind(), "function_item" | "mod_item")
                && direct_children(parent).any(|child| {
                    child.kind() == "attribute_item"
                        && matches!(
                            normalize(self.text(child)).as_str(),
                            "#[test]" | "#[tokio::test]" | "#[cfg(test)]"
                        )
                })
            {
                return true;
            }
            node = parent;
        }
        false
    }

    fn text(&self, node: Node<'_>) -> &str {
        &self.source[node.byte_range()]
    }
}

pub fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

pub fn rust_files_below(relative_roots: &[String]) -> Vec<String> {
    let root = repository_root();
    let mut files = relative_roots
        .iter()
        .flat_map(|relative| {
            WalkDir::new(root.join(relative))
                .follow_links(false)
                .into_iter()
                .filter_map(Result::ok)
                .filter(|entry| entry.file_type().is_file())
                .filter(|entry| {
                    entry
                        .path()
                        .extension()
                        .is_some_and(|extension| extension == "rs")
                })
                .filter_map(|entry| {
                    let relative = entry.path().strip_prefix(&root).ok()?;
                    let display = relative.to_string_lossy().replace('\\', "/");
                    (!display.contains("/tests/")
                        && !display.ends_with("/tests.rs")
                        && !display.contains("/fixtures/")
                        && !display.contains("/test_support.rs"))
                    .then_some(display)
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    files.sort();
    files.dedup();
    files
}

pub fn has_path_suffix(paths: &BTreeSet<String>, expected: &str) -> bool {
    paths
        .iter()
        .any(|path| path == expected || path.ends_with(&format!("::{expected}")))
}

pub fn has_call_suffix(calls: &[CallSite], expected: &str) -> bool {
    calls
        .iter()
        .any(|call| call.callee == expected || call.callee.ends_with(expected))
}

pub fn first_call_line(calls: &[CallSite], expected: &str) -> usize {
    calls
        .iter()
        .find(|call| call.callee == expected || call.callee.ends_with(expected))
        .unwrap_or_else(|| panic!("missing call ending in {expected}: {calls:?}"))
        .line
}

fn direct_children(node: Node<'_>) -> impl Iterator<Item = Node<'_>> {
    let mut cursor = node.walk();
    node.children(&mut cursor).collect::<Vec<_>>().into_iter()
}

fn walk<'tree>(node: Node<'tree>, visit: &mut impl FnMut(Node<'tree>)) {
    visit(node);
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk(child, visit);
    }
}

fn normalize(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect()
}

pub fn path_exists(relative: &str) -> bool {
    repository_root().join(relative).exists()
}
