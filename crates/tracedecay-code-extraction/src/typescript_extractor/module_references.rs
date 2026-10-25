//! Module reachability evidence from an already parsed source tree.

use tree_sitter::{Node as TsNode, Tree};

use crate::traversal::find_direct_child_by_kind;

use super::string_literals::string_literal_value;

/// The syntax that names a module, without claiming that it always executes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TypeScriptModuleReferenceKind {
    Static,
    Require,
    DynamicImport,
}

/// An exact literal value or an expression that requires runtime evaluation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TypeScriptModuleSpecifier {
    Literal(String),
    Expression,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TypeScriptModuleReference {
    pub kind: TypeScriptModuleReferenceKind,
    pub specifier: TypeScriptModuleSpecifier,
}

/// Known references survive incomplete syntax, while errors prevent consumers
/// from treating the absence of further references as complete evidence.
#[derive(Debug, Default, Eq, PartialEq)]
pub struct TypeScriptModuleReferences {
    pub references: Vec<TypeScriptModuleReference>,
    pub errors: Vec<String>,
}

/// ESM source clauses and TypeScript's import-equals form share the same
/// literal authority. The latter nests its source below the statement.
pub(super) fn module_source_node(node: TsNode<'_>) -> Option<TsNode<'_>> {
    node.child_by_field_name("source").or_else(|| {
        find_direct_child_by_kind(node, "import_require_clause")
            .and_then(|clause| clause.child_by_field_name("source"))
    })
}

/// Only direct `require(...)` and `import(...)` syntax names a module here.
/// An absent/nonliteral argument remains an expression, not an empty literal.
pub(super) fn module_call<'tree>(
    source: &[u8],
    node: TsNode<'tree>,
) -> Option<(TypeScriptModuleReferenceKind, Option<TsNode<'tree>>)> {
    if node.kind() != "call_expression" {
        return None;
    }
    let callee = node.child_by_field_name("function")?;
    let kind = match (callee.kind(), callee.utf8_text(source).ok()?) {
        ("identifier", "require") => TypeScriptModuleReferenceKind::Require,
        ("import", "import") => TypeScriptModuleReferenceKind::DynamicImport,
        _ => return None,
    };
    let argument = node
        .child_by_field_name("arguments")
        .filter(|arguments| arguments.kind() == "arguments")
        .and_then(|arguments| {
            let mut cursor = arguments.walk();
            arguments
                .named_children(&mut cursor)
                .find(|child| !child.is_extra())
        });
    Some((kind, argument))
}

pub(super) fn extract_module_references(source: &str, tree: &Tree) -> TypeScriptModuleReferences {
    let _span = tracing::trace_span!("code_extraction.module_references").entered();
    let mut result = TypeScriptModuleReferences::default();
    if tree.root_node().has_error() {
        result
            .errors
            .push("source contains syntax errors".to_owned());
    }
    let mut cursor = tree.walk();
    loop {
        let node = cursor.node();
        let reference = match node.kind() {
            "import_statement" | "export_statement" => module_source_node(node)
                .map(|source| (TypeScriptModuleReferenceKind::Static, Some(source))),
            "call_expression" => module_call(source.as_bytes(), node),
            _ => None,
        };
        if let Some((kind, argument)) = reference {
            let value = match argument {
                Some(argument) => string_literal_value(source.as_bytes(), argument),
                None => Ok(None),
            };
            match value {
                Ok(value) => result.references.push(TypeScriptModuleReference {
                    kind,
                    specifier: match value {
                        Some(value) => TypeScriptModuleSpecifier::Literal(value),
                        None => TypeScriptModuleSpecifier::Expression,
                    },
                }),
                Err(error) => result.errors.push(format!(
                    "module reference at {}:{}: {error}",
                    node.start_position().row + 1,
                    node.start_position().column + 1,
                )),
            }
        }
        if cursor.goto_first_child() {
            continue;
        }
        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() {
                return result;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::parsed_extraction::ParsedExtractionScope;
    use crate::{LanguageExtractor, LanguageRegistry, TypeScriptExtractor};

    use super::{
        TypeScriptModuleReferenceKind, TypeScriptModuleReferences, TypeScriptModuleSpecifier,
    };

    fn references(source: &str, extension: &str) -> TypeScriptModuleReferences {
        let tree = TypeScriptExtractor::parse_source(source, extension).expect("parse source");
        TypeScriptExtractor::extract_module_references(source, &tree)
    }

    #[test]
    fn references_reuse_import_syntax_without_promoting_dynamic_graph_edges() {
        let source = r#"
import { run } from "./es\x63aped";
export { thing as other } from './\u0063afé';
import Equals = require("./equals");
const common = require(/* comment before argument */ "./\uD83D\uDE80");
require('./side-effect');
function nested() { return require('./scoped'); }
const lazy = import(/* comment */ `./\u{1F680}-lazy`, { with: { type: 'json' } });
const computed = import(`./${name}`);
require(path);
import('./left' + suffix);
obj.require('./member');
const text = "import('./string')";
// import './comment';
"#;
        let observed = references(source, "ts");
        assert!(observed.errors.is_empty(), "{:?}", observed.errors);
        let observed = observed
            .references
            .into_iter()
            .map(|reference| {
                (
                    reference.kind,
                    match reference.specifier {
                        TypeScriptModuleSpecifier::Literal(value) => Some(value),
                        TypeScriptModuleSpecifier::Expression => None,
                    },
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            observed,
            vec![
                (
                    TypeScriptModuleReferenceKind::Static,
                    Some("./escaped".to_owned())
                ),
                (
                    TypeScriptModuleReferenceKind::Static,
                    Some("./café".to_owned())
                ),
                (
                    TypeScriptModuleReferenceKind::Static,
                    Some("./equals".to_owned())
                ),
                (
                    TypeScriptModuleReferenceKind::Require,
                    Some("./🚀".to_owned())
                ),
                (
                    TypeScriptModuleReferenceKind::Require,
                    Some("./side-effect".to_owned())
                ),
                (
                    TypeScriptModuleReferenceKind::Require,
                    Some("./scoped".to_owned())
                ),
                (
                    TypeScriptModuleReferenceKind::DynamicImport,
                    Some("./🚀-lazy".to_owned())
                ),
                (TypeScriptModuleReferenceKind::DynamicImport, None),
                (TypeScriptModuleReferenceKind::Require, None),
                (TypeScriptModuleReferenceKind::DynamicImport, None),
            ]
        );

        let artifact = TypeScriptExtractor.extract_artifact("src/index.ts", source);
        assert!(
            artifact.result.errors.is_empty(),
            "{:?}",
            artifact.result.errors
        );
        let graph_modules = artifact
            .imports
            .iter()
            .map(|row| row.module_specifier.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            graph_modules,
            ["./escaped", "./café", "./equals", "./🚀"].into()
        );
        let equals = artifact
            .imports
            .iter()
            .find(|row| row.module_specifier == "./equals")
            .expect("import-equals binding");
        assert_eq!(equals.local_name.as_deref(), Some("Equals"));
        assert_eq!(equals.imported_name.as_deref(), Some("default"));
    }

    #[test]
    fn source_and_retained_extractors_share_all_module_suffix_grammars() {
        let registry = LanguageRegistry::new();
        for extension in [
            "ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs", "TSX", "MJS",
        ] {
            let lower = extension.to_ascii_lowercase();
            let source = match lower.as_str() {
                "tsx" | "jsx" => "import Widget from './widget'; export const view = <Widget />;",
                "ts" | "mts" | "cts" => {
                    "import Widget from './widget'; export type Value = Widget;"
                }
                _ => "import Widget from './widget'; export const view = Widget;",
            };
            let path = format!("src/index.{extension}");
            let tree = TypeScriptExtractor::parse_source(source, extension).expect("parse suffix");
            let observed = TypeScriptExtractor::extract_module_references(source, &tree);
            assert!(
                observed.errors.is_empty(),
                "{extension}: {:?}",
                observed.errors
            );
            assert_eq!(observed.references.len(), 1, "{extension}");
            let artifact = TypeScriptExtractor.extract_artifact(&path, source);
            assert!(
                artifact.result.errors.is_empty(),
                "{extension}: {:?}",
                artifact.result.errors
            );
            assert_eq!(artifact.imports.len(), 1, "{extension}");

            let key = TypeScriptExtractor.retained_grammar_key(&path);
            let retained_tree =
                crate::ts_provider::parse_extractor_source(&key, "TypeScript", source)
                    .expect("parse retained grammar");
            assert!(!retained_tree.root_node().has_error(), "{extension}");
            let retained = TypeScriptExtractor.extract_parsed_artifact_prepared(
                &path,
                source,
                source,
                &retained_tree,
                ParsedExtractionScope::FullDocument,
            );
            assert_eq!(retained.artifact.imports, artifact.imports, "{extension}");
            assert!(
                registry
                    .extractor_for_file(&format!("src/index.{lower}"))
                    .is_some()
            );
        }
    }

    #[test]
    fn malformed_source_retains_known_imports_but_reports_partial_coverage() {
        let observed = references("import './known'; export const broken = ;", "ts");
        assert!(!observed.errors.is_empty());
        assert_eq!(observed.references.len(), 1);
        assert_eq!(
            observed.references[0].specifier,
            TypeScriptModuleSpecifier::Literal("./known".to_owned())
        );
        assert!(TypeScriptExtractor::parse_source("", "unsupported").is_err());
    }

    #[test]
    fn undecodable_import_sources_never_fall_back_to_local_reexports() {
        for source in [
            r#"export { x } from './\uD800';"#,
            r#"import x from './\uD800';"#,
            r#"const x = require('./\uD800');"#,
            r#"export { x } from './\08';"#,
        ] {
            let observed = references(source, "ts");
            assert!(!observed.errors.is_empty(), "{source}");
            assert!(observed.references.is_empty(), "{source}");
            let artifact = TypeScriptExtractor.extract_artifact("src/index.ts", source);
            assert!(!artifact.result.errors.is_empty(), "{source}");
            assert!(
                artifact.imports.is_empty(),
                "{source}: {:?}",
                artifact.imports
            );
        }
        let local = "const x = 1; export { x };";
        assert!(references(local, "ts").references.is_empty());
        assert_eq!(
            TypeScriptExtractor
                .extract_artifact("src/index.ts", local)
                .imports[0]
                .module_specifier,
            "./index.ts"
        );
    }
}
