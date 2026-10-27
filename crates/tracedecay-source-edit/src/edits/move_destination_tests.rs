use std::fs;
use std::path::Path;
use std::process::Command;

use tempfile::tempdir;
use tracedecay_contracts::source_edit::MoveHint;
use tracedecay_contracts::{
    EffectTermination, SourceEditKind, SourceEditRequest, source_edit_operation,
};
use tracedecay_domain::errors::TraceDecayError;

use super::test_support::{
    fixture_authorization, fixture_graph, fixture_request_for_edit, fixture_symbol_code_graph,
};
use crate::execute::resolve_source_edit_preview;
use crate::{SourceEditOutcome, execute_source_edit, preview_source_edit_expected_state};

fn compile_move_destination_fixture(project: &Path) {
    let output = Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
        .current_dir(project)
        .args([
            "--edition=2024",
            "--crate-type=lib",
            "--emit=metadata",
            "--crate-name=move_destination",
            "src/lib.rs",
            "-o",
            "fixture.rmeta",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "move destination must compile: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

async fn move_with_destination_header(
    before_import: &str,
    after_import: &str,
    newline: &str,
    destination_exists: bool,
) {
    move_with_destination_dependency(
        before_import,
        after_import,
        newline,
        destination_exists,
        "use crate::model::Widget;",
        "Widget",
        Some("use crate::model::Widget;"),
    )
    .await;
}

async fn move_with_destination_dependency(
    before_import: &str,
    after_import: &str,
    newline: &str,
    destination_exists: bool,
    source_import: &str,
    binding: &str,
    expected_import: Option<&str>,
) -> Vec<MoveHint> {
    let project = tempdir().unwrap();
    fs::create_dir(project.path().join("src")).unwrap();
    let moved_text = format!("pub fn moved(value: {binding}) -> {binding} {{\n    value\n}}");
    let source = format!("{source_import}\n\n{moved_text}\n");
    let destination = format!("{before_import}{after_import}");
    fs::write(project.path().join("src/source.rs"), &source).unwrap();
    fs::write(project.path().join("src/model.rs"), "pub struct Widget;\n").unwrap();
    fs::write(
        project.path().join("src/lib.rs"),
        "#![allow(dead_code, unused_imports)]\nmod model;\nmod source;\nmod relocated;\n",
    )
    .unwrap();
    if destination_exists {
        fs::write(project.path().join("src/relocated.rs"), &destination).unwrap();
        compile_move_destination_fixture(project.path());
    }
    let (graph, _) = fixture_graph(project.path()).await;
    let code_graph = fixture_symbol_code_graph(
        "src/source.rs",
        &source,
        &moved_text,
        "moved",
        "source::moved",
    );
    let mut request = fixture_request_for_edit(
        SourceEditRequest::MoveSymbol {
            symbol: "source::moved".to_owned(),
            dest_file: "src/relocated.rs".to_owned(),
            dry_run: false,
            update_references: false,
        },
        "source-edit.move-destination",
    );
    let preview = resolve_source_edit_preview(
        &graph,
        &code_graph,
        &request.context,
        request.observed_at,
        tracedecay_graph_query::request_graph_cancellation(&request.context),
        request.edit.clone(),
    )
    .await
    .unwrap();
    let SourceEditOutcome::Move(result) = &preview.outcome else {
        panic!("expected a move preview");
    };
    assert!(result.success, "{}", result.message);
    assert_eq!(
        result.applied_imports,
        expected_import.into_iter().collect::<Vec<_>>()
    );
    assert_eq!(preview.planned_files.len(), 2);
    assert_eq!(
        fs::read_to_string(project.path().join("src/source.rs")).unwrap(),
        source
    );
    if destination_exists {
        assert_eq!(
            fs::read_to_string(project.path().join("src/relocated.rs")).unwrap(),
            destination
        );
    } else {
        assert!(!project.path().join("src/relocated.rs").exists());
    }
    request.expected_state = preview.expected_state.unwrap();
    let authorization = fixture_authorization(&request);
    let applied = execute_source_edit(
        &graph,
        &code_graph,
        &source_edit_operation(SourceEditKind::MoveSymbol).unwrap(),
        request,
        &authorization,
    )
    .await
    .unwrap();
    assert!(applied.outcome.success(), "{}", applied.outcome.message());
    for planned in preview.planned_files {
        assert_eq!(
            fs::read_to_string(project.path().join(&planned.relative_path)).unwrap(),
            planned.intended,
            "apply must publish the exact preview for {}",
            planned.relative_path
        );
    }
    compile_move_destination_fixture(project.path());
    let actual = fs::read_to_string(project.path().join("src/relocated.rs")).unwrap();
    let inserted = expected_import.map_or_else(String::new, |import| format!("{import}{newline}"));
    let unchanged_destination = format!("{before_import}{inserted}{after_import}");
    assert!(actual.starts_with(&unchanged_destination), "{actual:?}");
    assert!(actual.ends_with(&format!("{moved_text}\n")));
    let mut parser = tree_sitter::Parser::new();
    let language = tracedecay_code_extraction::ts_provider::try_language("rust").unwrap();
    parser.set_language(&language).unwrap();
    let tree = parser.parse(&actual, None).unwrap();
    assert!(!tree.root_node().has_error());
    let original_tree = parser.parse(&destination, None).unwrap();
    let original_imports = original_tree
        .root_node()
        .named_children(&mut original_tree.walk())
        .filter(|node| node.kind() == "use_declaration")
        .count();
    let actual_imports: Vec<_> = tree
        .root_node()
        .named_children(&mut tree.walk())
        .filter(|node| node.kind() == "use_declaration")
        .collect();
    assert_eq!(
        actual_imports.len(),
        original_imports + usize::from(expected_import.is_some()),
        "the move must add only the expected dependency import"
    );
    if let Some(expected_import) = expected_import {
        assert!(
            actual_imports
                .iter()
                .any(|node| node.utf8_text(actual.as_bytes()).unwrap() == expected_import),
            "the dependency must be at the destination module root"
        );
    }
    result.impact.clone()
}

#[tokio::test]
async fn move_symbol_imports_preserve_destination_syntax_in_preview_and_apply() {
    for (before, after) in [
        (
            "use std::{\n    fmt::{self, Debug as DebugTrait},\n    collections::HashMap,\n};\n",
            "pub fn marker() {}\n",
        ),
        (
            "//! Destination module.\n#![allow(dead_code)]\n/*! Inner block docs. */\n#![allow(unused_imports)]\nuse std::fmt::Debug;\n",
            "pub fn marker() {}\n",
        ),
        (
            "// Header.\nuse std::fmt::Debug; // Existing import.\n\n",
            "/// Marker documentation.\n#[inline]\npub fn marker() {}\n",
        ),
        (
            "",
            "#[cfg(any())]\nuse missing_dependency::{Missing, AlsoMissing};\n/** Marker docs. */\n#[inline]\npub fn marker() {}\n",
        ),
        (
            "",
            "mod nested {\n    use std::{fmt::Debug, collections::HashMap};\n}\npub fn marker() {}\n",
        ),
        (
            "\u{feff}#!/usr/bin/env rust-script\n//! Destination module.\n",
            "pub fn marker() {}\n",
        ),
        ("use std::fmt::Debug as _;\n", "pub fn marker() {}\n"),
    ] {
        move_with_destination_header(before, after, "\n", true).await;
    }
}

#[tokio::test]
async fn move_symbol_ignores_destination_imports_in_comments_and_nested_scopes() {
    for (before, after) in [
        (
            "/* Import example:\nuse crate::model::Widget;\n*/\n",
            "pub fn marker() {}\n",
        ),
        (
            "",
            "mod nested {\n    use crate::model::Widget;\n    pub fn marker(value: Widget) -> Widget { value }\n}\npub fn marker() {}\n",
        ),
    ] {
        move_with_destination_header(before, after, "\n", true).await;
    }
}

#[tokio::test]
async fn move_symbol_preserves_dependency_aliases_across_trivia() {
    for import in [
        "use crate::model::Widget as\n    Alias;",
        "use crate::model::Widget\tas\tAlias;",
        "use crate::model::Widget as /* local name */ Alias;",
    ] {
        move_with_destination_dependency(
            "",
            "pub fn marker() {}\n",
            "\n",
            true,
            import,
            "Alias",
            Some(import),
        )
        .await;
    }
}

#[tokio::test]
async fn move_symbol_reuses_equivalent_destination_imports() {
    for destination_import in [
        "use crate::model::{Widget};\n",
        "use crate::{model::{Widget}};\n",
        "use crate /* root */ ::\n    model :: Widget;\n",
        "use crate::model::{Widget, *};\n",
    ] {
        move_with_destination_dependency(
            destination_import,
            "pub fn marker() {}\n",
            "\n",
            true,
            "use crate::model::Widget;",
            "Widget",
            None,
        )
        .await;
    }
    move_with_destination_dependency(
        "use crate::{model::{Widget as /* retained alias */ Alias}};\n",
        "pub fn marker() {}\n",
        "\n",
        true,
        "use crate::model::Widget as\n    Alias;",
        "Alias",
        None,
    )
    .await;
}

#[tokio::test]
async fn move_symbol_preserves_destination_item_bindings() {
    let import = "use crate::relocated::Widget as\n    Alias;";
    move_with_destination_dependency(
        "",
        "pub struct Widget;\n",
        "\n",
        true,
        import,
        "Alias",
        Some(import),
    )
    .await;
    move_with_destination_dependency(
        "use std::fmt::*;\n",
        "pub struct Widget;\n",
        "\n",
        true,
        "use crate::relocated::Widget;",
        "Widget",
        None,
    )
    .await;
}

#[tokio::test]
async fn move_symbol_reports_aliases_that_require_manual_imports() {
    for source_import in [
        "use super::model::Widget as\n    Alias;",
        "#[cfg(all())]\nuse crate::model::Widget as /* source configuration */ Alias;",
    ] {
        let hints = move_with_destination_dependency(
            "use crate::model::Widget as Alias;\n",
            "pub fn marker() {}\n",
            "\n",
            true,
            source_import,
            "Alias",
            None,
        )
        .await;
        assert_eq!(
            hints
                .iter()
                .filter(|hint| hint.kind == "import_needed")
                .count(),
            1,
            "an unportable alias must remain visible in the dependency report"
        );
    }
}

#[tokio::test]
async fn move_symbol_preserves_destination_bytes_when_adding_imports() {
    move_with_destination_header(
        "//! Module ☃.\r\nuse std::fmt::Debug;\r\n\r\n",
        "/// Marker.\r\n#[inline]\r\npub fn marker() {}\r\n \t\r\n\r\n",
        "\r\n",
        true,
    )
    .await;
    move_with_destination_header("\r\n \t\r\n", "", "\r\n", true).await;
    move_with_destination_header("", "", "\n", true).await;
    move_with_destination_header("", "", "\n", false).await;
}

#[tokio::test]
async fn move_symbol_refuses_unsupported_destination_before_preview_or_apply() {
    for (destination, dependency) in [
        ("use std::{\n", true),
        ("pub fn marker() {\n", false),
        ("/* unclosed destination comment", true),
        ("/// Detached documentation.\n#[inline]\n", false),
        (
            "#[cfg(any())]\nuse crate::model::Widget;\npub fn marker() {}\n",
            true,
        ),
        (
            "#[cfg(all())]\nuse crate::model::Widget;\npub fn marker() {}\n",
            true,
        ),
        (
            "#[cfg(any())]\nuse crate::{model::{Widget}};\npub fn marker() {}\n",
            true,
        ),
        (
            "#[cfg(all())]\nuse crate::model::\n    Widget;\npub fn marker() {}\n",
            true,
        ),
        ("use std::fmt::Error as Widget;\npub fn marker() {}\n", true),
        ("pub struct Widget;\npub fn marker() {}\n", true),
        (
            "mod other { pub struct Widget; }\nuse self::other::*;\npub fn marker(value: Widget) -> Widget { value }\n",
            true,
        ),
    ] {
        let project = tempdir().unwrap();
        fs::create_dir(project.path().join("src")).unwrap();
        let moved_text = if dependency {
            "pub fn moved(value: Widget) -> Widget { value }"
        } else {
            "pub fn moved() {}"
        };
        let source = format!("use crate::model::Widget;\n\n{moved_text}\n");
        fs::write(project.path().join("src/source.rs"), &source).unwrap();
        fs::write(project.path().join("src/relocated.rs"), destination).unwrap();
        let (graph, _) = fixture_graph(project.path()).await;
        let code_graph = fixture_symbol_code_graph(
            "src/source.rs",
            &source,
            moved_text,
            "moved",
            "source::moved",
        );
        let request = fixture_request_for_edit(
            SourceEditRequest::MoveSymbol {
                symbol: "source::moved".to_owned(),
                dest_file: "src/relocated.rs".to_owned(),
                dry_run: false,
                update_references: false,
            },
            "source-edit.move-invalid-destination",
        );
        let preview = preview_source_edit_expected_state(
            &graph,
            &code_graph,
            &request.context,
            request.observed_at,
            request.edit.clone(),
        )
        .await;
        assert!(matches!(
            preview,
            Err(TraceDecayError::ProjectRoute { reason_code, retryable: false, .. })
                if reason_code == "source-edit-destination-syntax-unsupported"
        ));
        let authorization = fixture_authorization(&request);
        let applied = execute_source_edit(
            &graph,
            &code_graph,
            &source_edit_operation(SourceEditKind::MoveSymbol).unwrap(),
            request,
            &authorization,
        )
        .await
        .unwrap();
        assert!(!applied.outcome.success());
        assert_eq!(
            applied.effect.unwrap().receipt.outcome,
            EffectTermination::Failed
        );
        assert_eq!(
            fs::read_to_string(project.path().join("src/source.rs")).unwrap(),
            source
        );
        assert_eq!(
            fs::read_to_string(project.path().join("src/relocated.rs")).unwrap(),
            destination
        );
        assert!(
            !crate::journal::SourceEditDurability::for_graph(&graph)
                .journal_path()
                .exists()
        );
    }
}
