use tracedecay_code_extraction::parsed_extraction::{
    ParsedExtractionDisposition, ParsedExtractionScope,
};
use tracedecay_code_extraction::{
    AstroExtractor, ImportModuleKindV1, ImportNamespaceV1, LanguageExtractor, SvelteExtractor,
    TypeScriptExtractor,
};
use tracedecay_domain::{NodeKind, SourceSpan};
use tree_sitter::Parser;

#[test]
fn parser_import_rows_do_not_collapse_grouped_multiline_type_bindings() {
    let source = concat!(
        "import type {\n",
        "  Foo,\n",
        "  Bar as Baz,\n",
        "} from \"../models\";\n",
    );
    let extractor = TypeScriptExtractor;
    let artifact = extractor.extract_artifact("src/features/use-models.ts", source);
    assert!(
        artifact.result.errors.is_empty(),
        "errors: {:?}",
        artifact.result.errors
    );
    let raw_use_nodes = artifact
        .result
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Use)
        .collect::<Vec<_>>();
    assert_eq!(
        raw_use_nodes.len(),
        1,
        "one raw statement node must not become one node per binding"
    );
    assert_eq!(raw_use_nodes[0].name, "../models");
    assert!(
        artifact
            .result
            .nodes
            .iter()
            .all(|node| !matches!(node.name.as_str(), "Foo" | "Bar" | "Baz")),
        "binding evidence belongs only in artifact.imports, not raw graph nodes"
    );

    let observed = artifact
        .imports
        .iter()
        .map(|row| {
            (
                row.logical_path.as_str(),
                row.module_specifier.as_str(),
                row.imported_name.as_deref(),
                row.local_name.as_deref(),
                row.namespace,
                row.module_kind,
                row.span,
                row.start_line,
                row.start_column,
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        observed,
        vec![
            (
                "src/features/use-models.ts",
                "../models",
                Some("Foo"),
                Some("Foo"),
                ImportNamespaceV1::Type,
                ImportModuleKindV1::ProjectRelative,
                SourceSpan {
                    start_byte: 16,
                    end_byte: 19,
                },
                1,
                2,
            ),
            (
                "src/features/use-models.ts",
                "../models",
                Some("Bar"),
                Some("Baz"),
                ImportNamespaceV1::Type,
                ImportModuleKindV1::ProjectRelative,
                SourceSpan {
                    start_byte: 23,
                    end_byte: 33,
                },
                2,
                2,
            ),
        ]
    );
}

#[test]
fn parser_owned_tree_import_rows_do_not_disappear_from_parsed_artifact() {
    let source = "import type { Foo } from \"pkg\";\n";
    let mut parser = Parser::new();
    parser
        .set_language(
            &tracedecay_code_extraction::ts_provider::language("typescript")
                .expect("bundled TypeScript grammar"),
        )
        .expect("configure TypeScript parser");
    let tree = parser.parse(source, None).expect("parse TypeScript source");
    let extractor = TypeScriptExtractor;

    let parsed = extractor.extract_parsed_artifact(
        "src/parsed.ts",
        source,
        &tree,
        ParsedExtractionScope::FullDocument,
    );

    assert_eq!(
        parsed.disposition,
        ParsedExtractionDisposition::FullDocument
    );
    assert!(
        parsed.artifact.result.errors.is_empty(),
        "errors: {:?}",
        parsed.artifact.result.errors
    );
    assert_eq!(
        parsed
            .artifact
            .imports
            .iter()
            .map(|row| (
                row.logical_path.as_str(),
                row.module_specifier.as_str(),
                row.imported_name.as_deref(),
                row.local_name.as_deref(),
                row.namespace,
                row.module_kind,
                row.span,
                row.start_line,
                row.start_column,
            ))
            .collect::<Vec<_>>(),
        vec![(
            "src/parsed.ts",
            "pkg",
            Some("Foo"),
            Some("Foo"),
            ImportNamespaceV1::Type,
            ImportModuleKindV1::BareModule,
            SourceSpan {
                start_byte: 14,
                end_byte: 17,
            },
            0,
            14,
        )]
    );
}

#[test]
fn parser_import_rows_do_not_promote_per_specifier_type_bindings_to_value() {
    let source = "import { type Foo, Bar as Baz } from \"@scope/pkg\";\n";
    let extractor = TypeScriptExtractor;
    let artifact = extractor.extract_artifact("src/consumer.ts", source);
    assert!(
        artifact.result.errors.is_empty(),
        "errors: {:?}",
        artifact.result.errors
    );

    let observed = artifact
        .imports
        .iter()
        .map(|row| {
            (
                row.imported_name.as_deref(),
                row.local_name.as_deref(),
                row.namespace,
                row.module_kind,
                row.span,
                row.start_line,
                row.start_column,
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        observed,
        vec![
            (
                Some("Foo"),
                Some("Foo"),
                ImportNamespaceV1::Type,
                ImportModuleKindV1::BareModule,
                SourceSpan {
                    start_byte: 9,
                    end_byte: 17,
                },
                0,
                9,
            ),
            (
                Some("Bar"),
                Some("Baz"),
                ImportNamespaceV1::Value,
                ImportModuleKindV1::BareModule,
                SourceSpan {
                    start_byte: 19,
                    end_byte: 29,
                },
                0,
                19,
            ),
        ]
    );
}

#[test]
fn parser_import_rows_do_not_conflate_value_and_side_effect_namespaces() {
    let source = concat!(
        "import { run as execute } from \"runner\";\n",
        "import \"reflect-metadata\";\n",
    );
    let extractor = TypeScriptExtractor;
    let artifact = extractor.extract_artifact("src/bootstrap.ts", source);
    assert!(
        artifact.result.errors.is_empty(),
        "errors: {:?}",
        artifact.result.errors
    );

    let observed = artifact
        .imports
        .iter()
        .map(|row| {
            (
                row.module_specifier.as_str(),
                row.imported_name.as_deref(),
                row.local_name.as_deref(),
                row.namespace,
                row.module_kind,
                row.span,
                row.start_line,
                row.start_column,
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        observed,
        vec![
            (
                "runner",
                Some("run"),
                Some("execute"),
                ImportNamespaceV1::Value,
                ImportModuleKindV1::BareModule,
                SourceSpan {
                    start_byte: 9,
                    end_byte: 23,
                },
                0,
                9,
            ),
            (
                "reflect-metadata",
                None,
                None,
                ImportNamespaceV1::SideEffect,
                ImportModuleKindV1::BareModule,
                SourceSpan {
                    start_byte: 41,
                    end_byte: 67,
                },
                1,
                0,
            ),
        ]
    );
}

#[test]
fn parser_import_rows_do_not_classify_external_modules_as_project_relative() {
    let source = concat!(
        "import type { Local } from \"./local\";\n",
        "import type { External } from \"@scope/pkg\";\n",
    );
    let extractor = TypeScriptExtractor;
    let artifact = extractor.extract_artifact("src/module-kinds.ts", source);
    assert!(
        artifact.result.errors.is_empty(),
        "errors: {:?}",
        artifact.result.errors
    );

    let observed = artifact
        .imports
        .iter()
        .map(|row| {
            (
                row.logical_path.as_str(),
                row.module_specifier.as_str(),
                row.imported_name.as_deref(),
                row.module_kind,
                row.span,
                row.start_line,
                row.start_column,
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        observed,
        vec![
            (
                "src/module-kinds.ts",
                "./local",
                Some("Local"),
                ImportModuleKindV1::ProjectRelative,
                SourceSpan {
                    start_byte: 14,
                    end_byte: 19,
                },
                0,
                14,
            ),
            (
                "src/module-kinds.ts",
                "@scope/pkg",
                Some("External"),
                ImportModuleKindV1::BareModule,
                SourceSpan {
                    start_byte: 52,
                    end_byte: 60,
                },
                1,
                14,
            ),
        ]
    );
}

#[test]
fn bare_module_rows_do_not_claim_package_or_ignored_source_resolution() {
    let source = concat!(
        "import type { SvelteAlias } from \"$lib/types\";\n",
        "import type { TsAlias } from \"@/types\";\n",
        "import type { BuiltinType } from \"node:fs\";\n",
        "import type { UrlType } from \"https://example.invalid/types.ts\";\n",
        "import type { ScopedPackage } from \"@scope/pkg\";\n",
    );
    let artifact = TypeScriptExtractor.extract_artifact("src/bare-modules.ts", source);
    assert!(artifact.result.errors.is_empty());
    assert_eq!(
        artifact
            .imports
            .iter()
            .map(|row| (row.module_specifier.as_str(), row.module_kind))
            .collect::<Vec<_>>(),
        vec![
            ("$lib/types", ImportModuleKindV1::BareModule),
            ("@/types", ImportModuleKindV1::BareModule),
            ("node:fs", ImportModuleKindV1::BareModule),
            (
                "https://example.invalid/types.ts",
                ImportModuleKindV1::BareModule,
            ),
            ("@scope/pkg", ImportModuleKindV1::BareModule),
        ]
    );
}

#[test]
fn astro_artifact_wrapper_does_not_drop_frontmatter_type_import_evidence() {
    let source = concat!(
        "---\n",
        "import type { Card as CardModel } from \"../models/card\";\n",
        "---\n",
        "<Card />\n",
    );
    let artifact = AstroExtractor.extract_artifact("src/components/Card.astro", source);
    assert!(artifact.result.errors.is_empty());
    assert_eq!(
        artifact
            .result
            .nodes
            .iter()
            .filter(|node| node.kind == NodeKind::Use)
            .count(),
        1,
        "frontmatter retains one raw import statement node"
    );
    assert!(
        artifact
            .result
            .nodes
            .iter()
            .all(|node| { !matches!(node.name.as_str(), "Card" | "CardModel") })
    );
    assert_eq!(artifact.imports.len(), 1);
    let row = &artifact.imports[0];
    assert_eq!(row.logical_path, "src/components/Card.astro");
    assert_eq!(row.module_specifier, "../models/card");
    assert_eq!(row.imported_name.as_deref(), Some("Card"));
    assert_eq!(row.local_name.as_deref(), Some("CardModel"));
    assert_eq!(row.namespace, ImportNamespaceV1::Type);
    assert_eq!(row.module_kind, ImportModuleKindV1::ProjectRelative);
    assert_eq!(
        row.span,
        SourceSpan {
            start_byte: 18,
            end_byte: 35,
        }
    );
    assert_eq!(row.start_line, 1);
    assert_eq!(row.start_column, 14);
}

#[test]
fn svelte_artifact_wrapper_does_not_drop_script_type_import_evidence() {
    let source = concat!(
        "<script lang=\"ts\">\n",
        "import type { Widget as WidgetProps } from \"$lib/types\";\n",
        "</script>\n",
        "<h1>Widget</h1>\n",
    );
    let artifact = SvelteExtractor.extract_artifact("src/routes/+page.svelte", source);
    assert!(artifact.result.errors.is_empty());
    assert_eq!(
        artifact
            .result
            .nodes
            .iter()
            .filter(|node| node.kind == NodeKind::Use)
            .count(),
        1,
        "script retains one raw import statement node"
    );
    assert!(
        artifact
            .result
            .nodes
            .iter()
            .all(|node| { !matches!(node.name.as_str(), "Widget" | "WidgetProps") })
    );
    assert_eq!(artifact.imports.len(), 1);
    let row = &artifact.imports[0];
    assert_eq!(row.logical_path, "src/routes/+page.svelte");
    assert_eq!(row.module_specifier, "$lib/types");
    assert_eq!(row.imported_name.as_deref(), Some("Widget"));
    assert_eq!(row.local_name.as_deref(), Some("WidgetProps"));
    assert_eq!(row.namespace, ImportNamespaceV1::Type);
    assert_eq!(row.module_kind, ImportModuleKindV1::BareModule);
    assert_eq!(
        row.span,
        SourceSpan {
            start_byte: 33,
            end_byte: 54,
        }
    );
    assert_eq!(row.start_line, 1);
    assert_eq!(row.start_column, 14);
}
