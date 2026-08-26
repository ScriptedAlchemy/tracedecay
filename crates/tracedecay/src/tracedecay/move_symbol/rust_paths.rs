//! Rust module-path derivation and item-kind classification for
//! `move_symbol`: which item kinds a `use` can bring into scope, mapping a
//! project-relative file path to its `crate::a::b` module path, and
//! locating (or naming) the file that would declare a destination's module.

use tree_sitter::Parser;

use crate::types::{NodeKind, Visibility};

/// Item kinds that a `use` statement can bring into scope — the ones a moved
/// body could depend on across a module boundary.
pub(super) fn is_importable_item(kind: &NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Struct
            | NodeKind::Enum
            | NodeKind::Trait
            | NodeKind::Function
            | NodeKind::Const
            | NodeKind::Static
            | NodeKind::TypeAlias
            | NodeKind::Macro
            | NodeKind::Union
            | NodeKind::Typedef
            | NodeKind::Record
    )
}

pub(super) fn visibility_word(v: &Visibility) -> &'static str {
    match v {
        Visibility::Pub => "pub",
        Visibility::PubCrate => "pub(crate)",
        Visibility::PubSuper => "pub(super)",
        Visibility::Private => "private",
    }
}

/// Derives a Rust module path (`crate::a::b`) from a project-relative `.rs`
/// file path under a `src/` root. Returns `None` for non-Rust files.
pub(super) fn rust_module_path(rel: &str) -> Option<String> {
    let stem = rel.strip_suffix(".rs")?;
    // Normalize to components after an optional `src/` (or `.../src/`) segment.
    let parts: Vec<&str> = stem.split('/').collect();
    let src_idx = parts.iter().rposition(|p| *p == "src");
    let tail: &[&str] = match src_idx {
        Some(i) => &parts[i + 1..],
        None => &parts[..],
    };
    let mut segs: Vec<&str> = tail.to_vec();
    // Crate roots and module files contribute no path segment.
    if let Some("lib" | "main" | "mod") = segs.last().copied() {
        segs.pop();
    }
    let mut path = String::from("crate");
    for seg in segs {
        if seg.is_empty() {
            continue;
        }
        path.push_str("::");
        path.push_str(seg);
    }
    Some(path)
}

/// The module stem for a destination file (`src/foo/bar.rs` -> `bar`,
/// `src/foo/mod.rs` -> `foo`).
pub(super) fn module_stem(rel: &str) -> Option<String> {
    let stem = rel.strip_suffix(".rs")?;
    let parts: Vec<&str> = stem.split('/').filter(|p| !p.is_empty()).collect();
    let last = parts.last().copied()?;
    if last == "mod" {
        return parts
            .get(parts.len().wrapping_sub(2))
            .map(|s| (*s).to_string());
    }
    if last == "lib" || last == "main" {
        return None;
    }
    Some(last.to_string())
}

/// The likely crate-root file for a destination, used to suggest where a
/// `mod` statement belongs.
pub(super) fn crate_root_file(dest_rel: &str) -> String {
    let src_prefix = dest_rel.rfind("src/").map(|i| &dest_rel[..i + 4]);
    match src_prefix {
        Some(prefix) => format!("{prefix}lib.rs"),
        None => "src/lib.rs".to_string(),
    }
}

/// Files that could declare `dest_rel`'s module with a `mod` statement.
pub(super) fn parent_module_candidates(dest_rel: &str) -> Vec<String> {
    let Some(stem) = dest_rel.strip_suffix(".rs") else {
        return Vec::new();
    };
    let parts: Vec<&str> = stem.split('/').filter(|part| !part.is_empty()).collect();
    let src_idx = parts.iter().rposition(|part| *part == "src");
    let (prefix, tail) = match src_idx {
        Some(index) => (
            format!("{}/", parts[..=index].join("/")),
            &parts[index + 1..],
        ),
        None => (String::new(), parts.as_slice()),
    };
    let Some(file_stem) = tail.last() else {
        return Vec::new();
    };
    let parent_segments = if *file_stem == "mod" {
        &tail[..tail.len().saturating_sub(2)]
    } else {
        &tail[..tail.len() - 1]
    };
    if parent_segments.is_empty() {
        let root = if prefix.is_empty() {
            "src/".to_string()
        } else {
            prefix
        };
        return vec![format!("{root}lib.rs"), format!("{root}main.rs")];
    }
    let parent = format!("{prefix}{}", parent_segments.join("/"));
    vec![format!("{parent}.rs"), format!("{parent}/mod.rs")]
}

/// Parse-level check for an external `mod name;` declaration. Text matches
/// alone are unsafe here: comments, strings, and inline `mod name { ... }`
/// blocks do not connect `name.rs` to the module tree.
pub(super) fn source_declares_external_module(source: &str, expected: &str) -> bool {
    let Ok(language) = tracedecay_code_extraction::ts_provider::try_language("rust") else {
        return false;
    };
    let mut parser = Parser::new();
    if parser.set_language(&language).is_err() {
        return false;
    }
    let Some(tree) = parser.parse(source, None) else {
        return false;
    };
    let root = tree.root_node();
    let mut cursor = root.walk();
    if !cursor.goto_first_child() {
        return false;
    }
    loop {
        let node = cursor.node();
        if node.kind() == "mod_item"
            && node.child_by_field_name("body").is_none()
            && node
                .child_by_field_name("name")
                .and_then(|name| name.utf8_text(source.as_bytes()).ok())
                == Some(expected)
        {
            return true;
        }
        if !cursor.goto_next_sibling() {
            return false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        crate_root_file, module_stem, parent_module_candidates, rust_module_path,
        source_declares_external_module,
    };

    #[test]
    fn rust_module_path_maps_src_layout() {
        assert_eq!(
            rust_module_path("src/pricing.rs").as_deref(),
            Some("crate::pricing")
        );
        assert_eq!(
            rust_module_path("src/foo/bar.rs").as_deref(),
            Some("crate::foo::bar")
        );
        assert_eq!(
            rust_module_path("src/foo/mod.rs").as_deref(),
            Some("crate::foo")
        );
        assert_eq!(rust_module_path("src/lib.rs").as_deref(), Some("crate"));
        assert_eq!(rust_module_path("src/main.rs").as_deref(), Some("crate"));
        assert_eq!(
            rust_module_path("evals/fixture/src/pricing.rs").as_deref(),
            Some("crate::pricing")
        );
        assert_eq!(rust_module_path("README.md"), None);
    }

    #[test]
    fn module_stem_and_root() {
        assert_eq!(
            module_stem("src/grand_total.rs").as_deref(),
            Some("grand_total")
        );
        assert_eq!(module_stem("src/foo/mod.rs").as_deref(), Some("foo"));
        assert_eq!(module_stem("src/lib.rs"), None);
        assert_eq!(
            crate_root_file("evals/fixture/src/grand_total.rs"),
            "evals/fixture/src/lib.rs"
        );
        assert_eq!(crate_root_file("src/a.rs"), "src/lib.rs");
    }

    #[test]
    fn parent_module_candidates_follow_rust_file_layout() {
        assert_eq!(
            parent_module_candidates("src/foo.rs"),
            vec!["src/lib.rs", "src/main.rs"]
        );
        assert_eq!(
            parent_module_candidates("src/foo/bar.rs"),
            vec!["src/foo.rs", "src/foo/mod.rs"]
        );
        assert_eq!(
            parent_module_candidates("src/foo/mod.rs"),
            vec!["src/lib.rs", "src/main.rs"]
        );
        assert_eq!(
            parent_module_candidates("src/foo/bar/mod.rs"),
            vec!["src/foo.rs", "src/foo/mod.rs"]
        );
        assert_eq!(
            parent_module_candidates("evals/fixture/src/foo/bar.rs"),
            vec!["evals/fixture/src/foo.rs", "evals/fixture/src/foo/mod.rs"]
        );
    }

    #[test]
    fn external_module_detection_ignores_comments_strings_and_inline_modules() {
        assert!(source_declares_external_module(
            "#[cfg(feature = \"x\")]\npub mod child;\n",
            "child"
        ));
        assert!(!source_declares_external_module(
            "// mod child;\nconst NOTE: &str = \"mod child;\";\n",
            "child"
        ));
        assert!(!source_declares_external_module(
            "mod child { pub fn inline() {} }\n",
            "child"
        ));
    }
}
