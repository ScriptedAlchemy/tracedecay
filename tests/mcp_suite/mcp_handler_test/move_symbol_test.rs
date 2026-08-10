use crate::support::*;
use crate::support::{
    handle_production_source_edit_tool_call as handle_tool_call,
    init_production_source_edit_project as init_test_project,
};
use serde_json::{Value, json};
use std::fs;
#[cfg(unix)]
use std::os::unix::fs as unix_fs;
use std::path::Path;
use tracedecay::mcp::ToolResult;

#[tokio::test]
async fn test_move_symbol_dry_run_reports_impact_and_writes_nothing() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    let project = project_root.as_path();
    move_pricing_fixture(project).await;
    let (cg, _env) = init_test_project(project).await;

    let before_pricing = fs::read_to_string(project.join("src/pricing.rs")).unwrap();
    let before_orders = fs::read_to_string(project.join("src/orders.rs")).unwrap();

    let result = handle_tool_call(
        &cg,
        "tracedecay_move_symbol",
        json!({ "symbol": "compute_grand_total", "dest_file": "src/grand_total.rs" }),
        None,
        None,
    )
    .await
    .unwrap();
    let p = move_payload(&result);

    assert_eq!(p["success"], true, "payload: {p}");
    assert_eq!(p["dry_run"], true, "default must be a dry run: {p}");
    // Docs travel with the function.
    let span = p["moved_span"].as_str().unwrap();
    assert!(span.contains("/// Grand total in cents."), "span: {span}");
    assert!(span.contains("pub fn compute_grand_total"), "span: {span}");
    // Same-file dependency `LineItem` is auto-inserted at the destination.
    let applied: Vec<&str> = p["applied_imports"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(
        applied
            .iter()
            .any(|s| s.contains("use crate::pricing::LineItem;")),
        "applied imports: {applied:?}"
    );
    // Impact carries the caller reference (cross-file) and the missing module.
    let kinds: Vec<&str> = p["impact"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["kind"].as_str().unwrap())
        .collect();
    assert!(
        kinds.contains(&"caller_reference"),
        "impact kinds: {kinds:?}\n{p}"
    );
    assert!(
        kinds.contains(&"module_missing"),
        "impact kinds: {kinds:?}\n{p}"
    );
    let caller = p["impact"]
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["kind"] == "caller_reference")
        .unwrap();
    assert_eq!(caller["file"], "src/orders.rs");
    assert!(
        caller["suggestion"]
            .as_str()
            .unwrap()
            .contains("crate::grand_total"),
        "suggestion: {caller}"
    );
    assert!(p["diff"].as_str().unwrap().contains("compute_grand_total"));

    // A dry run writes nothing: source files are byte-identical and the
    // destination was never created.
    assert_eq!(
        fs::read_to_string(project.join("src/pricing.rs")).unwrap(),
        before_pricing
    );
    assert_eq!(
        fs::read_to_string(project.join("src/orders.rs")).unwrap(),
        before_orders
    );
    assert!(!project.join("src/grand_total.rs").exists());
}

#[tokio::test]
async fn test_move_symbol_resolves_qualified_names_like_bare_names() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    let project = project_root.as_path();
    move_pricing_fixture(project).await;
    let (cg, _env) = init_test_project(project).await;

    let before_pricing = fs::read_to_string(project.join("src/pricing.rs")).unwrap();
    let before_orders = fs::read_to_string(project.join("src/orders.rs")).unwrap();
    let mut expected: Option<Value> = None;

    for symbol in [
        "compute_grand_total",
        "pricing::compute_grand_total",
        "crate::pricing::compute_grand_total",
        "src/pricing.rs::compute_grand_total",
        r"src\pricing.rs::compute_grand_total",
    ] {
        let result = handle_tool_call(
            &cg,
            "tracedecay_move_symbol",
            json!({ "symbol": symbol, "dest_file": "src/grand_total.rs" }),
            None,
            None,
        )
        .await
        .unwrap_or_else(|error| panic!("move for {symbol:?} failed: {error}"));
        let payload = move_payload(&result);
        assert_eq!(payload["success"], true, "symbol {symbol:?}: {payload}");
        assert_eq!(payload["dry_run"], true, "symbol {symbol:?}: {payload}");

        if let Some(expected) = &expected {
            assert_eq!(
                payload["source_file"], expected["source_file"],
                "symbol {symbol:?}: {payload}"
            );
            assert_eq!(
                payload["moved_span"], expected["moved_span"],
                "symbol {symbol:?}: {payload}"
            );
            assert_eq!(
                payload["impact"], expected["impact"],
                "symbol {symbol:?}: {payload}"
            );
            assert_eq!(
                payload["applied_imports"], expected["applied_imports"],
                "symbol {symbol:?}: {payload}"
            );
        } else {
            expected = Some(payload);
        }
    }

    assert_eq!(
        fs::read_to_string(project.join("src/pricing.rs")).unwrap(),
        before_pricing
    );
    assert_eq!(
        fs::read_to_string(project.join("src/orders.rs")).unwrap(),
        before_orders
    );
    assert!(!project.join("src/grand_total.rs").exists());
}

#[tokio::test]
async fn test_move_symbol_only_prefers_callable_for_bare_names() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    let project = project_root.as_path();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub mod a;\npub mod b;\n").unwrap();
    fs::write(
        project.join("src/a.rs"),
        "pub mod common {\n    pub fn same() -> u32 { 1 }\n}\n",
    )
    .unwrap();
    fs::write(
        project.join("src/b.rs"),
        "pub mod common {\n    pub struct same;\n}\n",
    )
    .unwrap();
    let (cg, _env) = init_test_project(project).await;

    let before_a = fs::read_to_string(project.join("src/a.rs")).unwrap();
    let before_b = fs::read_to_string(project.join("src/b.rs")).unwrap();

    let bare = handle_tool_call(
        &cg,
        "tracedecay_move_symbol",
        json!({ "symbol": "same", "dest_file": "src/moved.rs" }),
        None,
        None,
    )
    .await
    .unwrap();
    let bare_payload = move_payload(&bare);
    assert_eq!(bare_payload["success"], true, "payload: {bare_payload}");
    assert_eq!(bare_payload["source_file"], "src/a.rs");

    let qualified = handle_tool_call(
        &cg,
        "tracedecay_move_symbol",
        json!({ "symbol": "common::same", "dest_file": "src/moved.rs" }),
        None,
        None,
    )
    .await
    .expect("qualified collision returns a durable failed effect");
    let qualified_payload = move_payload(&qualified);
    assert_eq!(qualified_payload["success"], false);
    assert_eq!(qualified_payload["failed"], true);
    assert_eq!(qualified_payload["replayed"], false);

    let wrong_module = handle_tool_call(
        &cg,
        "tracedecay_move_symbol",
        json!({ "symbol": "wrong::same", "dest_file": "src/moved.rs" }),
        None,
        None,
    )
    .await
    .expect("wrong module returns a durable failed effect");
    let wrong_module_payload = move_payload(&wrong_module);
    assert_eq!(wrong_module_payload["success"], false);
    assert_eq!(wrong_module_payload["failed"], true);
    assert_eq!(wrong_module_payload["replayed"], false);

    assert_eq!(
        fs::read_to_string(project.join("src/a.rs")).unwrap(),
        before_a
    );
    assert_eq!(
        fs::read_to_string(project.join("src/b.rs")).unwrap(),
        before_b
    );
    assert!(!project.join("src/moved.rs").exists());
}

#[tokio::test]
async fn test_move_symbol_apply_moves_and_rerun_errors_cleanly() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    let project = project_root.as_path();
    move_pricing_fixture(project).await;
    let (cg, _env) = init_test_project(project).await;

    let result = handle_tool_call(
        &cg,
        "tracedecay_move_symbol",
        json!({ "symbol": "compute_grand_total", "dest_file": "src/grand_total.rs", "dry_run": false }),
        None,
        None,
    )
    .await
    .unwrap();
    let p = move_payload(&result);
    assert_eq!(p["success"], true, "payload: {p}");
    // `dry_run: false` is omitted by skip_serializing_if; assert it is not true.
    assert_ne!(p["dry_run"], json!(true), "payload: {p}");
    assert_eq!(p["message"], "move applied", "payload: {p}");

    // Source lost the function; destination gained it plus its import and docs.
    let pricing = fs::read_to_string(project.join("src/pricing.rs")).unwrap();
    assert!(
        !pricing.contains("pub fn compute_grand_total"),
        "pricing: {pricing}"
    );
    assert!(
        pricing.contains("pub struct LineItem"),
        "pricing: {pricing}"
    );
    let dest = fs::read_to_string(project.join("src/grand_total.rs")).unwrap();
    assert!(
        dest.contains("use crate::pricing::LineItem;"),
        "dest: {dest}"
    );
    assert!(dest.contains("pub fn compute_grand_total"), "dest: {dest}");
    assert!(
        dest.contains("/// Grand total in cents."),
        "dest docs travel: {dest}"
    );

    // Re-running the same move now that the symbol lives at the destination is a
    // clean refusal (destination == source), not a panic or silent success.
    let result2 = handle_tool_call(
        &cg,
        "tracedecay_move_symbol",
        json!({ "symbol": "compute_grand_total", "dest_file": "src/grand_total.rs", "dry_run": false }),
        None,
        None,
    )
    .await
    .unwrap();
    let p2 = move_payload(&result2);
    assert_eq!(p2["success"], false, "re-run should refuse: {p2}");
    assert_eq!(
        result2.value["isError"], true,
        "re-run should mark the transported MCP result as an error: {}",
        result2.value
    );
}

#[tokio::test]
async fn test_move_symbol_clean_move_has_empty_impact() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    let project = project_root.as_path();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub mod a;\npub mod b;\n").unwrap();
    fs::write(
        project.join("src/a.rs"),
        "//! a\n\npub fn standalone() -> u32 {\n    42\n}\n",
    )
    .unwrap();
    fs::write(
        project.join("src/b.rs"),
        "//! b\n\npub fn other() -> u32 {\n    0\n}\n",
    )
    .unwrap();
    let (cg, _env) = init_test_project(project).await;

    let result = handle_tool_call(
        &cg,
        "tracedecay_move_symbol",
        json!({ "symbol": "standalone", "dest_file": "src/b.rs" }),
        None,
        None,
    )
    .await
    .unwrap();
    let p = move_payload(&result);
    assert_eq!(p["success"], true, "payload: {p}");
    assert!(
        p["impact"].as_array().is_none_or(|a| a.is_empty()),
        "clean move should have empty impact: {p}"
    );
    assert!(
        p["applied_imports"].as_array().is_none_or(|a| a.is_empty()),
        "clean move needs no imports: {p}"
    );
}

#[tokio::test]
async fn test_move_symbol_dest_collision_refuses() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    let project = project_root.as_path();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub mod a;\npub mod b;\n").unwrap();
    fs::write(
        project.join("src/a.rs"),
        "//! a\n\npub fn dup() -> u32 {\n    1\n}\n",
    )
    .unwrap();
    fs::write(
        project.join("src/b.rs"),
        "//! b\n\npub fn dup() -> u32 {\n    2\n}\n",
    )
    .unwrap();
    let (cg, _env) = init_test_project(project).await;

    let before_b = fs::read_to_string(project.join("src/b.rs")).unwrap();
    let result = handle_tool_call(
        &cg,
        "tracedecay_move_symbol",
        json!({ "symbol": "src/a.rs::dup", "dest_file": "src/b.rs", "dry_run": false }),
        None,
        None,
    )
    .await
    .unwrap();
    let p = move_payload(&result);
    assert_eq!(p["success"], false, "collision must refuse: {p}");
    let kinds: Vec<&str> = p["impact"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["kind"].as_str().unwrap())
        .collect();
    assert!(kinds.contains(&"collision"), "impact: {p}");
    // Refusal writes nothing.
    assert_eq!(
        fs::read_to_string(project.join("src/b.rs")).unwrap(),
        before_b
    );
}

#[tokio::test]
async fn test_move_symbol_private_dependency_hints() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    let project = project_root.as_path();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub mod a;\npub mod b;\n").unwrap();
    fs::write(
        project.join("src/a.rs"),
        "//! a\n\n\
         fn secret() -> u32 {\n    7\n}\n\n\
         pub fn uses_secret() -> u32 {\n    secret()\n}\n",
    )
    .unwrap();
    fs::write(
        project.join("src/b.rs"),
        "//! b\n\npub fn other() -> u32 {\n    0\n}\n",
    )
    .unwrap();
    let (cg, _env) = init_test_project(project).await;

    let result = handle_tool_call(
        &cg,
        "tracedecay_move_symbol",
        json!({ "symbol": "uses_secret", "dest_file": "src/b.rs" }),
        None,
        None,
    )
    .await
    .unwrap();
    let p = move_payload(&result);
    assert_eq!(p["success"], true, "payload: {p}");
    let kinds: Vec<&str> = p["impact"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["kind"].as_str().unwrap())
        .collect();
    assert!(kinds.contains(&"dependency_broken"), "impact: {p}");
    assert!(kinds.contains(&"visibility_required"), "impact: {p}");
    // A private dependency is never silently auto-imported (would not compile),
    // so applied_imports is empty and omitted from the payload entirely.
    let applied = p["applied_imports"].as_array().cloned().unwrap_or_default();
    assert!(
        !applied
            .iter()
            .any(|s| s.as_str().unwrap_or_default().contains("secret")),
        "applied: {applied:?}"
    );
}

/// Contract: a caller that invokes the symbol through a fully-qualified path
/// (`crate::pricing::compute_grand_total(...)`) rather than a `use` import still
/// earns a `caller_reference` hint whose suggestion carries the exact new path.
#[tokio::test]
async fn test_move_symbol_qualified_caller_hint() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    let project = project_root.as_path();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("src/lib.rs"),
        "pub mod pricing;\npub mod orders;\n",
    )
    .unwrap();
    fs::write(
        project.join("src/pricing.rs"),
        "//! pricing\n\
         pub struct LineItem {\n    pub unit_price: u64,\n    pub quantity: u32,\n}\n\n\
         /// Grand total in cents.\n\
         pub fn compute_grand_total(items: &[LineItem]) -> u64 {\n\
         \x20   let mut total = 0u64;\n\
         \x20   for item in items {\n\
         \x20       total += item.unit_price * item.quantity as u64;\n\
         \x20   }\n\
         \x20   total\n\
         }\n",
    )
    .unwrap();
    // Caller references the symbol via a qualified path, NOT a `use` import.
    fs::write(
        project.join("src/orders.rs"),
        "//! orders\n\
         use crate::pricing::LineItem;\n\n\
         pub fn tally(items: &[LineItem]) -> u64 {\n\
         \x20   crate::pricing::compute_grand_total(items)\n\
         }\n",
    )
    .unwrap();
    let (cg, _env) = init_test_project(project).await;

    let result = handle_tool_call(
        &cg,
        "tracedecay_move_symbol",
        json!({ "symbol": "compute_grand_total", "dest_file": "src/grand_total.rs" }),
        None,
        None,
    )
    .await
    .unwrap();
    let p = move_payload(&result);
    assert_eq!(p["success"], true, "payload: {p}");

    let callers: Vec<&Value> = p["impact"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|h| h["kind"] == "caller_reference")
        .collect();
    assert_eq!(
        callers.len(),
        1,
        "exactly one qualified caller reference: {p}"
    );
    let caller = callers[0];
    assert_eq!(caller["file"], "src/orders.rs", "caller file: {caller}");
    assert!(
        caller["suggestion"]
            .as_str()
            .unwrap()
            .contains("crate::grand_total::compute_grand_total"),
        "suggestion must carry the exact new path: {caller}"
    );
}

/// Contract (#353 `attrs_start_line` edge case): when the symbol's doc block is
/// the very first lines of the file, the doc travels with the move and the
/// source is left with no doc residue.
#[tokio::test]
async fn test_move_symbol_first_in_file_docs_travel() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    let project = project_root.as_path();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub mod a;\npub mod b;\n").unwrap();
    // The doc comment is literally the first line of the file — no module doc,
    // no leading blank — so attrs_start_line must resolve to line 0.
    fs::write(
        project.join("src/a.rs"),
        "/// The very first thing in the file.\n\
         pub fn leading() -> u32 {\n    1\n}\n",
    )
    .unwrap();
    fs::write(
        project.join("src/b.rs"),
        "//! b\n\npub fn other() -> u32 {\n    0\n}\n",
    )
    .unwrap();
    let (cg, _env) = init_test_project(project).await;

    let result = handle_tool_call(
        &cg,
        "tracedecay_move_symbol",
        json!({ "symbol": "leading", "dest_file": "src/b.rs", "dry_run": false }),
        None,
        None,
    )
    .await
    .unwrap();
    let p = move_payload(&result);
    assert_eq!(p["success"], true, "payload: {p}");
    // The moved span begins with the leading doc line.
    let span = p["moved_span"].as_str().unwrap();
    assert!(
        span.starts_with("/// The very first thing in the file."),
        "moved span must start with the leading doc: {span:?}"
    );

    // Apply left no doc residue at the source; the destination gained it.
    let a = fs::read_to_string(project.join("src/a.rs")).unwrap();
    assert!(
        !a.contains("The very first thing in the file."),
        "source must not retain the moved doc: {a:?}"
    );
    assert!(
        !a.contains("pub fn leading"),
        "source must not retain the moved fn: {a:?}"
    );
    let b = fs::read_to_string(project.join("src/b.rs")).unwrap();
    assert!(
        b.contains("The very first thing in the file.") && b.contains("pub fn leading"),
        "destination must carry the doc and fn: {b:?}"
    );
}

/// Ship-blocker (silent data loss): a `./`-prefixed destination that resolves to
/// the symbol's OWN file must be refused. Before the normalization fix,
/// `./src/pricing.rs` slipped past the same-file guard (it compared unequal to
/// the graph's `src/pricing.rs`), and the apply then wrote-then-truncated the
/// same inode, deleting the symbol while returning success.
#[tokio::test]
async fn test_move_symbol_dot_prefixed_same_file_refuses() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    let project = project_root.as_path();
    move_pricing_fixture(project).await;
    let (cg, _env) = init_test_project(project).await;

    let before_pricing = fs::read_to_string(project.join("src/pricing.rs")).unwrap();
    let result = handle_tool_call(
        &cg,
        "tracedecay_move_symbol",
        json!({ "symbol": "compute_grand_total", "dest_file": "./src/pricing.rs", "dry_run": false }),
        None,
        None,
    )
    .await
    .unwrap();
    let p = move_payload(&result);
    assert_eq!(
        p["success"], false,
        "`./`-prefixed same-file move must refuse: {p}"
    );
    assert!(
        p["message"].as_str().unwrap().contains("symbol's own file"),
        "refusal must be the same-file error: {p}"
    );
    // The symbol is untouched — no silent deletion.
    assert_eq!(
        fs::read_to_string(project.join("src/pricing.rs")).unwrap(),
        before_pricing,
        "source file must be byte-identical after the refusal"
    );
}

/// A destination reached through a symlink outside the checkout must be
/// rejected before either file is written.
#[cfg(unix)]
#[tokio::test]
async fn test_move_symbol_symlink_escape_refuses() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    let project = project_root.as_path();
    move_pricing_fixture(project).await;
    let outside = tempfile::tempdir().unwrap();
    unix_fs::symlink(outside.path(), project.join("src/escape")).unwrap();
    let (cg, _env) = init_test_project(project).await;

    let before = fs::read_to_string(project.join("src/pricing.rs")).unwrap();
    let result = handle_tool_call(
        &cg,
        "tracedecay_move_symbol",
        json!({
            "symbol": "compute_grand_total",
            "dest_file": "src/escape/grand_total.rs",
            "dry_run": false
        }),
        None,
        None,
    )
    .await
    .expect("containment refusal should return a durable failed effect");
    let payload = move_payload(&result);
    assert_eq!(payload["success"], false, "payload: {payload}");
    assert_eq!(payload["failed"], true, "payload: {payload}");
    assert_eq!(payload["replayed"], false, "payload: {payload}");
    assert_eq!(result.value["isError"], true, "result: {}", result.value);
    assert_eq!(
        fs::read_to_string(project.join("src/pricing.rs")).unwrap(),
        before
    );
    assert!(!outside.path().join("grand_total.rs").exists());
}

/// Lexically different destinations that resolve to the source inode (a
/// symlink or hard link) are the same-file data-loss case too.
#[cfg(unix)]
#[tokio::test]
async fn test_move_symbol_aliases_to_source_refuse() {
    for hard_link in [false, true] {
        let dir = test_temp_dir();
        let project_root = dir.path().join("project");
        let project = project_root.as_path();
        move_pricing_fixture(project).await;
        let source = project.join("src/pricing.rs");
        let alias = project.join("src/pricing_alias.rs");
        if hard_link {
            fs::hard_link(&source, &alias).unwrap();
        } else {
            unix_fs::symlink(&source, &alias).unwrap();
        }
        let (cg, _env) = init_test_project(project).await;

        let before = fs::read_to_string(&source).unwrap();
        let result = handle_tool_call(
            &cg,
            "tracedecay_move_symbol",
            json!({
                "symbol": "src/pricing.rs::compute_grand_total",
                "dest_file": "src/pricing_alias.rs",
                "dry_run": false
            }),
            None,
            None,
        )
        .await
        .unwrap();
        let payload = move_payload(&result);
        assert_eq!(payload["success"], false, "payload: {payload}");
        assert!(
            payload["message"]
                .as_str()
                .unwrap_or_default()
                .contains("symbol's own file"),
            "payload: {payload}"
        );
        assert_eq!(fs::read_to_string(&source).unwrap(), before);
    }
}

/// A `./`-prefixed different-file destination must behave identically to the
/// unprefixed form: the path is normalized, the move applies, and the reported
/// `dest_file` is the canonical `src/grand_total.rs`.
#[tokio::test]
async fn test_move_symbol_dot_prefixed_dest_normalizes() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    let project = project_root.as_path();
    move_pricing_fixture(project).await;
    let (cg, _env) = init_test_project(project).await;

    let result = handle_tool_call(
        &cg,
        "tracedecay_move_symbol",
        json!({ "symbol": "compute_grand_total", "dest_file": "./src/grand_total.rs", "dry_run": false }),
        None,
        None,
    )
    .await
    .unwrap();
    let p = move_payload(&result);
    assert_eq!(p["success"], true, "payload: {p}");
    assert_eq!(
        p["dest_file"], "src/grand_total.rs",
        "dest_file must be normalized (no `./`): {p}"
    );
    // The move actually landed at the canonical path.
    let dest = fs::read_to_string(project.join("src/grand_total.rs")).unwrap();
    assert!(dest.contains("pub fn compute_grand_total"), "dest: {dest}");
    let pricing = fs::read_to_string(project.join("src/pricing.rs")).unwrap();
    assert!(
        !pricing.contains("pub fn compute_grand_total"),
        "source must have lost the symbol: {pricing}"
    );
}

/// Ship-blocker (span correctness): a contiguous leading `//!` inner module-doc
/// (no blank line before the item) must NOT be swallowed into the moved span.
/// Otherwise the source loses its module doc and the destination gets a stray
/// `//!` mid-file (a hard E0753).
#[tokio::test]
async fn test_move_symbol_leaves_contiguous_module_doc_behind() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    let project = project_root.as_path();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub mod a;\npub mod b;\n").unwrap();
    // Module doc is contiguous with the first item — no blank line between.
    fs::write(
        project.join("src/a.rs"),
        "//! module a doc\npub fn fact() -> u32 {\n    1\n}\n",
    )
    .unwrap();
    fs::write(
        project.join("src/b.rs"),
        "//! b\n\npub fn other() -> u32 {\n    0\n}\n",
    )
    .unwrap();
    let (cg, _env) = init_test_project(project).await;

    let result = handle_tool_call(
        &cg,
        "tracedecay_move_symbol",
        json!({ "symbol": "fact", "dest_file": "src/b.rs", "dry_run": false }),
        None,
        None,
    )
    .await
    .unwrap();
    let p = move_payload(&result);
    assert_eq!(p["success"], true, "payload: {p}");
    // The moved span never carries the inner module doc.
    let span = p["moved_span"].as_str().unwrap();
    assert!(
        !span.contains("//!"),
        "moved span must not swallow the module doc: {span:?}"
    );
    assert!(span.contains("pub fn fact"), "span: {span:?}");

    // Source keeps its module doc; destination has no stray `//!` mid-file.
    let a = fs::read_to_string(project.join("src/a.rs")).unwrap();
    assert!(
        a.contains("//! module a doc"),
        "source must keep its module doc: {a:?}"
    );
    let b = fs::read_to_string(project.join("src/b.rs")).unwrap();
    assert!(b.contains("pub fn fact"), "dest must carry the fn: {b:?}");
    // The only `//!` in the destination is its own leading module doc (line 0).
    let stray_inner_doc = b
        .lines()
        .enumerate()
        .any(|(i, l)| i > 0 && l.trim_start().starts_with("//!"));
    assert!(
        !stray_inner_doc,
        "destination must not gain a mid-file `//!`: {b:?}"
    );
}

/// Minor (destination read safety): an existing-but-unreadable destination
/// (non-UTF8) must be refused with a clear message, never treated as empty and
/// clobbered.
#[tokio::test]
async fn test_move_symbol_non_utf8_destination_refuses() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    let project = project_root.as_path();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub mod a;\n").unwrap();
    fs::write(
        project.join("src/a.rs"),
        "//! a\n\npub fn movable() -> u32 {\n    1\n}\n",
    )
    .unwrap();
    let (cg, _env) = init_test_project(project).await;

    // Write an existing destination with invalid UTF-8 bytes AFTER indexing so
    // the indexer never has to parse it.
    let dest = project.join("src/blob.rs");
    fs::write(&dest, [0xff, 0xfe, 0x00, 0xff]).unwrap();
    let before_dest = fs::read(&dest).unwrap();

    let result = handle_tool_call(
        &cg,
        "tracedecay_move_symbol",
        json!({ "symbol": "movable", "dest_file": "src/blob.rs", "dry_run": false }),
        None,
        None,
    )
    .await
    .unwrap();
    let p = move_payload(&result);
    assert_eq!(
        p["success"], false,
        "unreadable destination must refuse: {p}"
    );
    assert!(
        p["message"]
            .as_str()
            .unwrap()
            .contains("failed to read destination"),
        "refusal message must name the read failure: {p}"
    );
    // The destination is untouched — not clobbered with the moved symbol.
    assert_eq!(
        fs::read(&dest).unwrap(),
        before_dest,
        "unreadable destination must be left byte-identical"
    );
}

/// A pricing/orders fixture mirroring the eval fixture: a `compute_grand_total`
/// that depends on the same-file `LineItem`, plus a cross-file caller.
pub(crate) async fn move_pricing_fixture(project: &Path) {
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("src/lib.rs"),
        "pub mod pricing;\npub mod orders;\n",
    )
    .unwrap();
    fs::write(
        project.join("src/pricing.rs"),
        "//! pricing\n\
         pub struct LineItem {\n    pub unit_price: u64,\n    pub quantity: u32,\n}\n\n\
         /// Grand total in cents.\n\
         pub fn compute_grand_total(items: &[LineItem]) -> u64 {\n\
         \x20   let mut total = 0u64;\n\
         \x20   for item in items {\n\
         \x20       total += item.unit_price * item.quantity as u64;\n\
         \x20   }\n\
         \x20   total\n\
         }\n",
    )
    .unwrap();
    fs::write(
        project.join("src/orders.rs"),
        "//! orders\n\
         use crate::pricing::{compute_grand_total, LineItem};\n\n\
         pub fn tally(items: &[LineItem]) -> u64 {\n    compute_grand_total(items)\n}\n",
    )
    .unwrap();
}

// --------------------------------------------------------------------------- //
// tracedecay_move_symbol — fixture-crate integration through the MCP path.
// --------------------------------------------------------------------------- //

/// Parses the JSON payload text from a move_symbol ToolResult.
pub(crate) fn move_payload(result: &ToolResult) -> Value {
    let text = extract_text(&result.value);
    serde_json::from_str(text).unwrap_or_else(|e| panic!("move payload not JSON: {e}\n{text}"))
}
