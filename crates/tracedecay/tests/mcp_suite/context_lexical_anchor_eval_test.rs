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
