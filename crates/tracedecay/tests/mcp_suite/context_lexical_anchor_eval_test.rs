//! Caller-supplied `lexical_anchors` are must-have evidence for
//! `tracedecay_context`: every anchor with exact matches contributes its top
//! sites to the answer, and an anchor with no matches is reported as such.
//!
//! The corpus reproduces the shape reported in #1985 with redacted content:
//! a TypeScript monorepo whose few real `hono` import sites are short files,
//! while large minified generated schema validators (`*.check.ts`) and a
//! doc-heavy `webpack/declarations/*.d.ts` match many of the task's ordinary
//! words with high term frequency. Before the fix the anchor routes were a
//! soft additive boost truncated at the lexical lane cap, so the generated
//! files outranked every import site.

#![cfg(feature = "test-transport")]

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use serde_json::{Value, json};

use crate::support::{
    extract_real_server_text, handle_real_server_tool_call,
    production_composition_fixture_with_sources, warm_code_index_search,
};

const TASK: &str = "Locate Hono usage and summarize the local dependency surface for a patch-version safety review.";

const HONO_IMPORT_SITES: &[&str] = &[
    "packages/treeshake-server/src/app.ts",
    "packages/treeshake-server/src/http/routes/health.ts",
    "packages/treeshake-server/src/http/routes/treeshake.ts",
];

const MIDDLEWARE_HANDLER_IMPORT_SITES: &[&str] = &[
    "packages/treeshake-server/src/http/middlewares/logger.ts",
    "packages/treeshake-server/src/http/middlewares/request-id.ts",
    "packages/bridge/bridge-react/src/lazy/data-fetch/data-fetch-server-middleware.ts",
];

const GENERATED_VALIDATORS: &[&str] = &[
    "packages/enhanced/src/schemas/container/ModuleFederationPlugin.check.ts",
    "packages/enhanced/src/schemas/container/ContainerPlugin.check.ts",
    "packages/enhanced/src/schemas/sharing/SharePlugin.check.ts",
];

/// Property names a module-federation schema validator repeats for every
/// nested definition. Several coincide with ordinary words in the task.
const VALIDATOR_PROPERTIES: &[&str] = &[
    "version",
    "requiredVersion",
    "strictVersion",
    "singleton",
    "eager",
    "shareScope",
    "shareKey",
    "import",
    "local",
    "usage",
    "surface",
    "patch",
    "safety",
    "review",
    "summarize",
    "locate",
    "dependency",
    "dependencyType",
    "packageName",
    "runtime",
];

/// One minified generated validator: a header comment, then a single line of
/// several hundred kilobytes that repeats the schema's property names in
/// type checks and error messages, the way schema-utils emits them.
fn minified_validator(plugin: &str, definitions: usize) -> String {
    let mut source = String::from(
        "/*\n * This file was automatically generated.\n * DO NOT MODIFY BY HAND.\n * Run `yarn special-lint-fix` to update\n */\n\"use strict\";",
    );
    source.push_str(&format!(
        "export const validate={plugin}Check;export default {plugin}Check;const schema={{definitions:{{}}}};"
    ));
    source.push_str(&format!(
        "function {plugin}Check(t,{{instancePath:e=\"\",parentData:n,parentDataProperty:s,rootData:o=t}}={{}}){{let r=null,a=0;"
    ));
    for definition in 0..definitions {
        for (offset, property) in VALIDATOR_PROPERTIES.iter().enumerate() {
            let ordinal = definition * VALIDATOR_PROPERTIES.len() + offset;
            source.push_str(&format!(
                "if(void 0!==t.{property}){{let i=t.{property};const l=a;if(\"string\"!=typeof i){{const t={{params:{{type:\"string\"}},keyword:\"type\",instancePath:e+\"/{property}\",schemaPath:\"#/definitions/{plugin}{definition}/properties/{property}\",message:\"{property} must be a string ({ordinal})\"}};null===r?r=[t]:r.push(t);a++}}if(l===a&&i.length<1){{const t={{params:{{}},keyword:\"minLength\",instancePath:e+\"/{property}\",message:\"{property} must not be empty ({ordinal})\"}};null===r?r=[t]:r.push(t);a++}}}}"
            ));
        }
    }
    source.push_str(&format!("return {plugin}Check.errors=r,0===a}}\n"));
    source
}

fn write_anchor_eval_project(dest: &Path) {
    copy_dir_all(
        &crate::common::repository_path("tests/fixtures/context_anchor_eval_project"),
        dest,
    );
    for (ordinal, path) in GENERATED_VALIDATORS.iter().enumerate() {
        let plugin = Path::new(path)
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(".check.ts"))
            .expect("validator fixture name");
        let target = dest.join(path);
        fs::create_dir_all(target.parent().expect("validator dir")).expect("validator dir");
        fs::write(target, minified_validator(plugin, 60 + ordinal * 20)).expect("validator");
    }
}

fn copy_dir_all(src: &Path, dest: &Path) {
    fs::create_dir_all(dest).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let from = entry.path();
        let to = dest.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir_all(&from, &to);
        } else {
            fs::copy(&from, &to).unwrap();
        }
    }
}

fn matched_files(payload: &Value) -> Vec<&str> {
    payload["search_matches"]
        .as_array()
        .unwrap_or_else(|| panic!("search_matches missing in {payload}"))
        .iter()
        .filter_map(|search_match| search_match["file"].as_str())
        .collect()
}

/// `(anchor, outcome, matched, admitted)` in caller order from the
/// `lexical_anchors` receipt; counts are zero for a non-`matched` outcome.
fn anchor_receipt(payload: &Value) -> Vec<(&str, &str, u64, u64)> {
    payload["lexical_anchors"]
        .as_array()
        .unwrap_or_else(|| panic!("lexical_anchors receipt missing in {payload}"))
        .iter()
        .map(|anchor| {
            (
                anchor["anchor"].as_str().expect("anchor name"),
                anchor["outcome"].as_str().expect("anchor outcome"),
                anchor["matched"].as_u64().unwrap_or(0),
                anchor["admitted"].as_u64().unwrap_or(0),
            )
        })
        .collect()
}

async fn context_json(server: &tracedecay::mcp::McpServer, arguments: Value) -> Value {
    let result = handle_real_server_tool_call(server, "tracedecay_context", arguments).await;
    assert_ne!(
        result["isError"],
        Value::Bool(true),
        "tracedecay_context failed: {result}"
    );
    let text = extract_real_server_text(&result);
    serde_json::from_str(text)
        .unwrap_or_else(|error| panic!("tracedecay_context JSON invalid: {error}; text={text}"))
}

#[tokio::test]
async fn context_lexical_anchors_admit_every_exact_import_site() {
    let production = production_composition_fixture_with_sources(write_anchor_eval_project).await;
    let server = production
        .harness
        .server(&production.project_root)
        .expect("anchor eval server");
    warm_code_index_search(&server, "createApp").await;

    let payload = context_json(
        &server,
        json!({
            "task": TASK,
            "lexical_anchors": ["hono", "Hono", "MiddlewareHandler"],
            "max_nodes": 20,
            "include_code": true,
            "format": "json",
        }),
    )
    .await;
    assert_eq!(payload["coverage"]["recall"], "full", "{payload}");

    let files = matched_files(&payload);
    let returned: BTreeSet<&str> = files.iter().copied().collect();
    let missing: Vec<&str> = HONO_IMPORT_SITES
        .iter()
        .chain(MIDDLEWARE_HANDLER_IMPORT_SITES)
        .copied()
        .filter(|site| !returned.contains(site))
        .collect();
    assert!(
        missing.is_empty(),
        "exact anchor import sites missing from search_matches: {missing:?}; returned files in rank order: {files:?}"
    );
    let first_generated = files
        .iter()
        .position(|file| GENERATED_VALIDATORS.contains(file) || file.ends_with(".d.ts"));
    let last_import_site = files
        .iter()
        .rposition(|file| {
            HONO_IMPORT_SITES.contains(file) || MIDDLEWARE_HANDLER_IMPORT_SITES.contains(file)
        })
        .expect("import sites are present");
    if let Some(first_generated) = first_generated {
        assert!(
            first_generated > last_import_site,
            "a generated validator or declaration outranked an exact anchor import site: {files:?}"
        );
    }

    let receipt = anchor_receipt(&payload);
    assert_eq!(
        receipt
            .iter()
            .map(|(anchor, outcome, _, _)| (*anchor, *outcome))
            .collect::<Vec<_>>(),
        [
            ("hono", "matched"),
            ("Hono", "matched"),
            ("MiddlewareHandler", "matched")
        ],
        "{payload}"
    );
    for (anchor, _, matched, admitted) in &receipt {
        assert!(
            *admitted > 0 && *admitted <= *matched,
            "anchor {anchor} must rank some of its matches: {payload}"
        );
    }

    let unmatched = context_json(
        &server,
        json!({
            "task": TASK,
            "lexical_anchors": ["Hono", "HonoZeroMatchSentinel"],
            "max_nodes": 20,
            "format": "json",
        }),
    )
    .await;
    let receipt = anchor_receipt(&unmatched);
    assert_eq!(receipt.len(), 2, "{unmatched}");
    assert_eq!(
        (receipt[0].0, receipt[0].1),
        ("Hono", "matched"),
        "{unmatched}"
    );
    assert!(receipt[0].3 > 0, "{unmatched}");
    assert_eq!(
        receipt[1],
        ("HonoZeroMatchSentinel", "unmatched", 0, 0),
        "an anchor with no matches must be reported as such: {unmatched}"
    );
    let returned: BTreeSet<&str> = matched_files(&unmatched).into_iter().collect();
    for site in HONO_IMPORT_SITES {
        assert!(returned.contains(site), "{site} missing in {unmatched}");
    }

    let markdown = handle_real_server_tool_call(
        &server,
        "tracedecay_context",
        json!({
            "task": TASK,
            "lexical_anchors": ["Hono", "HonoZeroMatchSentinel"],
            "max_nodes": 20,
            "format": "markdown",
        }),
    )
    .await;
    let markdown = extract_real_server_text(&markdown);
    assert!(
        markdown.contains("`HonoZeroMatchSentinel`: no matches"),
        "markdown must name the unmatched anchor:\n{markdown}"
    );

    production.harness.shutdown().await;
}

const UPDATE_TASK: &str =
    "Explain how the update command coordinates daemon shutdown and restoration";
const UPDATE_DEFINITION: &str = "src/update_cmd.rs";
/// Every site that defines the anchor: the command and a legacy shim.
const UPDATE_SITES: &[&str] = &[UPDATE_DEFINITION, "src/legacy/update_shim.rs"];
const LIFECYCLE_STAGES: usize = 40;

/// The #2024 shape: forty graph-linked lifecycle stages declaring symbols
/// named for the task's ordinary words (`command`, `daemon`, `shutdown`,
/// `update`), which the exact lane admits as exact symbol-name hits, against
/// two rare `run_update_command` definitions and their callers that declare
/// none of them.
fn write_update_anchor_project(dest: &Path) {
    let src = dest.join("src");
    fs::create_dir_all(src.join("lifecycle")).expect("lifecycle dir");
    for stage in 0..LIFECYCLE_STAGES {
        let next = (stage + 1) % LIFECYCLE_STAGES;
        fs::write(
            src.join(format!("lifecycle/stage_{stage:02}.rs")),
            format!(
                "//! Stage {stage} of the daemon lifecycle.\n\n\
                 pub struct Stage{stage} {{\n\
                 \x20   pub command: String,\n\
                 \x20   pub daemon: u32,\n\
                 }}\n\n\
                 impl Stage{stage} {{\n\
                 \x20   /// Runs the daemon shutdown for stage {stage}, then its restoration.\n\
                 \x20   pub fn shutdown(&self) -> u32 {{\n\
                 \x20       self.daemon + super::stage_{next:02}::restore(self.daemon)\n\
                 \x20   }}\n\n\
                 \x20   /// Coordinates the update command after a daemon shutdown.\n\
                 \x20   pub fn update(&mut self) -> u32 {{\n\
                 \x20       self.daemon = self.shutdown();\n\
                 \x20       self.daemon\n\
                 \x20   }}\n\
                 }}\n\n\
                 /// Restores stage {stage} after a daemon shutdown.\n\
                 pub fn restore(daemon: u32) -> u32 {{\n\
                 \x20   daemon + {stage}\n\
                 }}\n"
            ),
        )
        .expect("lifecycle stage");
    }
    fs::write(
        src.join("update_cmd.rs"),
        "/// Refresh installed components after an upgrade.\n\
         pub fn run_update_command(no_reinstall: bool) -> Result<(), String> {\n\
         \x20   let plan = if no_reinstall { \"refresh\" } else { \"reinstall\" };\n\
         \x20   apply_plan(plan)\n\
         }\n\n\
         fn apply_plan(plan: &str) -> Result<(), String> {\n\
         \x20   if plan.is_empty() { Err(\"empty plan\".to_owned()) } else { Ok(()) }\n\
         }\n",
    )
    .expect("update definition");
    fs::write(
        src.join("main.rs"),
        "mod cli;\nmod update_cmd;\n\n\
         fn main() {\n\
         \x20   if let Err(error) = update_cmd::run_update_command(false) {\n\
         \x20       eprintln!(\"{error}\");\n\
         \x20   }\n\
         }\n",
    )
    .expect("main caller");
    fs::write(
        src.join("cli.rs"),
        "pub fn refresh_only() -> Result<(), String> {\n\
         \x20   crate::update_cmd::run_update_command(true)\n\
         }\n",
    )
    .expect("cli caller");
    fs::create_dir_all(src.join("legacy")).expect("legacy dir");
    fs::write(
        src.join("legacy/update_shim.rs"),
        "/// Forwards the pre-1.0 entry point to the current refresh.\n\
         pub fn run_update_command(force: bool) -> Result<(), String> {\n\
         \x20   crate::cli::refresh_only().map(|()| drop(force))\n\
         }\n",
    )
    .expect("legacy definition");
}

/// The single anchor receipt as `(matched, admitted, dropped)`, where
/// `dropped` is `[(reason, sites)]`.
fn single_matched_receipt(payload: &Value) -> (u64, u64, Vec<(String, u64)>) {
    let receipts = payload["lexical_anchors"]
        .as_array()
        .unwrap_or_else(|| panic!("lexical_anchors receipt missing in {payload}"));
    assert_eq!(receipts.len(), 1, "{payload}");
    let receipt = &receipts[0];
    assert_eq!(receipt["anchor"], "run_update_command", "{payload}");
    assert_eq!(receipt["outcome"], "matched", "{payload}");
    let dropped = receipt
        .get("dropped")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|drop| {
            (
                drop["reason"].as_str().expect("drop reason").to_owned(),
                drop["sites"].as_u64().expect("drop sites"),
            )
        })
        .collect();
    (
        receipt["matched"].as_u64().expect("matched"),
        receipt["admitted"].as_u64().expect("admitted"),
        dropped,
    )
}

#[tokio::test]
async fn context_returns_a_rare_anchor_site_over_an_exact_word_neighborhood() {
    let production =
        production_composition_fixture_with_sources(write_update_anchor_project).await;
    let server = production
        .harness
        .server(&production.project_root)
        .expect("update anchor server");
    warm_code_index_search(&server, "run_update_command").await;

    let payload = context_json(
        &server,
        json!({
            "task": UPDATE_TASK,
            "lexical_anchors": ["run_update_command"],
            "prefer_symbol": true,
            "include_code": true,
            "format": "json",
        }),
    )
    .await;
    let files = matched_files(&payload);
    let anchored: BTreeSet<&str> = UPDATE_SITES.iter().copied().collect();
    assert!(
        files.contains(&UPDATE_DEFINITION),
        "the admitted anchor definition is missing from search_matches: {files:?}"
    );
    let (matched, admitted, dropped) = single_matched_receipt(&payload);
    assert_eq!(
        (admitted, dropped.as_slice()),
        (anchored.len() as u64, &[][..]),
        "every anchor site fits the default page, so all are returned: {payload}"
    );
    assert!(matched >= admitted, "{payload}");
    let leading: BTreeSet<&str> = files[..anchored.len()].iter().copied().collect();
    assert_eq!(
        leading, anchored,
        "the admitted anchor sites must lead the page ahead of every exact-word hit: {files:?}"
    );
    let code = payload["code"]
        .as_array()
        .unwrap_or_else(|| panic!("code missing in {payload}"));
    let definition = code
        .iter()
        .find(|block| block["file"] == UPDATE_DEFINITION)
        .unwrap_or_else(|| panic!("the anchor definition's code is missing: {payload}"));
    assert!(
        definition["code"]
            .as_str()
            .is_some_and(|body| body.contains("pub fn run_update_command")),
        "{definition}"
    );

    // One seat: the best anchor site is returned and the receipt names the
    // admitted site the page could not carry, instead of claiming it.
    let one_seat = context_json(
        &server,
        json!({
            "task": UPDATE_TASK,
            "lexical_anchors": ["run_update_command"],
            "max_nodes": 1,
            "format": "json",
        }),
    )
    .await;
    let files = matched_files(&one_seat);
    assert_eq!(files.len(), 1, "{one_seat}");
    assert!(anchored.contains(files[0]), "{one_seat}");
    let (_, admitted, dropped) = single_matched_receipt(&one_seat);
    assert_eq!(
        (admitted, dropped),
        (
            1,
            vec![("outside_page".to_owned(), anchored.len() as u64 - 1)]
        ),
        "{one_seat}"
    );

    let markdown = handle_real_server_tool_call(
        &server,
        "tracedecay_context",
        json!({
            "task": UPDATE_TASK,
            "lexical_anchors": ["run_update_command"],
            "max_nodes": 1,
            "format": "markdown",
        }),
    )
    .await;
    let markdown = extract_real_server_text(&markdown);
    assert!(
        markdown.contains("1 returned, dropped 1 outside this page"),
        "markdown must name the dropped anchor sites:\n{markdown}"
    );

    production.harness.shutdown().await;
}
