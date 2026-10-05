//! Code-navigation queries refresh generation-bound identities before timing.

use serde_json::{Value, json};

use crate::queries::{
    EffectCleanup, PrimeStep, Query, QueryContext, ToolGroup, five, prime_class, prime_function,
    prime_function_pair, prime_symbol, symbol_name,
};

use super::{eqc, rq};

/// `SymbolGraphScope` — symbol-surface tools narrow by path prefix only.
fn scope() -> Value {
    json!({ "path_prefix": null })
}

/// `CodeQueryScope` — lexical/code-query tools pin the mounted generation.
fn cq_scope() -> Value {
    json!({
        "generation": "code-generation:unpinned-latest.v1",
        "path_prefix": null,
    })
}

/// Verify source-derived identities and relationships in the small fixture.
pub(crate) fn verify_code_fixture_read(
    tool: &str,
    label: &str,
    args: &Value,
    payload: &Value,
) -> Option<Result<(), String>> {
    let payload = payload.pointer("/outcome/value/payload").unwrap_or(payload);
    let query_is_fixture = args.get("query").and_then(Value::as_str) == Some("buildFixtureReport");
    match tool {
        "tracedecay_callees" if label == "fixture_callees" => Some((|| {
            let items = payload["items"]
                .as_array()
                .ok_or("fixture callees omitted call targets")?;
            if items.len() != 2 {
                return Err("fixture report must expose its two direct call targets".to_owned());
            }
            for name in [
                "src/graph.ts::dependencyCount",
                "src/graph.ts::fixtureGraph",
            ] {
                let item = items
                    .iter()
                    .find(|item| item["symbol"]["qualified_name"] == name)
                    .ok_or_else(|| format!("callees omitted {name}"))?;
                if item["depth"] != 1
                    || item["edge_kind"] != "calls"
                    || item["symbol"]["node_id"] == args["node_id"]
                {
                    return Err("fixture callees changed the direct call relationship".to_owned());
                }
                verify_literal_symbol(&item["symbol"])?;
            }
            Ok(())
        })()),
        "tracedecay_code_exact_occurrence" if args["literal"] == "flag:--fixture-catalog-mode" => {
            Some((|| {
                let source =
                    include_str!("../../../../benchmark_data/runtime/fixtures/project/src/lib.rs");
                let body = "pub fn fixture_cli_flag() -> &'static str {\n    \"--fixture-catalog-mode\"\n}";
                let start = source
                    .find(body)
                    .ok_or("fixture flag has no real source body")?;
                let items = payload["items"]
                    .as_array()
                    .ok_or("exact flag query omitted occurrences")?;
                if items.len() != 1
                    || items[0]["matched_literal"] != "--fixture-catalog-mode"
                    || items[0]["matched_kind"] != "cli_flag"
                    || items[0]["occurrence"]["path"] != "src/lib.rs"
                    || items[0]["occurrence"]["span"]
                        != json!({"start_byte": start, "end_byte": start + body.len()})
                {
                    return Err(
                        "exact flag query changed its literal owning function or byte range"
                            .to_owned(),
                    );
                }
                Ok(())
            })())
        }
        "tracedecay_code_exact_occurrence" if crate::repos::small_fixture_enabled() => {
            Some((|| {
                let literal = args["literal"]
                    .as_str()
                    .ok_or("exact occurrence has no literal")?;
                let item = payload["items"]
                    .as_array()
                    .and_then(|items| {
                        items.iter().find(|item| {
                            item["matched_literal"] == literal
                                && item["matched_kind"] == "whole_symbol"
                        })
                    })
                    .ok_or("exact occurrence omitted the requested whole-symbol match")?;
                let file = item
                    .pointer("/occurrence/path")
                    .and_then(Value::as_str)
                    .ok_or("exact occurrence omitted source path")?;
                let symbol = json!({"qualified_name": format!("{file}::{literal}")});
                let (source, start, end) = literal_symbol_source(&symbol)?;
                let body = source
                    .lines()
                    .skip(start - 1)
                    .take(end - start + 1)
                    .collect::<Vec<_>>()
                    .join("\n");
                let start_byte = source
                    .find(&body)
                    .ok_or("literal source has no whole-symbol body")?;
                if item.pointer("/occurrence/span/start_byte") != Some(&json!(start_byte))
                    || item.pointer("/occurrence/span/end_byte")
                        != Some(&json!(start_byte + body.len()))
                {
                    return Err(
                        "exact occurrence byte range differs from its actual fixture body"
                            .to_owned(),
                    );
                }
                Ok(())
            })())
        }
        "tracedecay_module_api"
            if crate::repos::small_fixture_enabled() && args["path"] == "src" =>
        {
            Some((|| {
                let symbols = payload["symbols"]
                    .as_array()
                    .ok_or("module API omitted symbols")?;
                for name in [
                    "src/report.ts::buildFixtureReport",
                    "src/graph.ts::fixtureGraph",
                    "src/catalog.py::fixture_catalog",
                    "src/main.py::render_summary",
                ] {
                    let symbol = symbols
                        .iter()
                        .find(|symbol| symbol["qualified_name"] == name)
                        .ok_or_else(|| format!("module API omitted {name}"))?;
                    verify_literal_symbol(symbol)?;
                }
                Ok(())
            })())
        }
        "tracedecay_code_timeline" if crate::repos::small_fixture_enabled() => Some((|| {
            let items = payload["items"]
                .as_array()
                .ok_or("timeline omitted generation events")?;
            if items.len() != 1
                || items[0]["generation"] != args["scope"]["generation"]
                || payload["generation"] != args["scope"]["generation"]
                || items[0]["file_count"] != 1
                || items[0]["symbol_count"] != 3
            {
                return Err("timeline did not report its pinned report.ts generation and three literal declarations".to_owned());
            }
            Ok(())
        })()),
        "tracedecay_code_declaration" if crate::repos::small_fixture_enabled() => Some((|| {
            let items = payload["items"]
                .as_array()
                .ok_or("declaration omitted items")?;
            if items.len() != 1
                || items[0]["node_id"] != args["node_id"]
                || payload["generation"] != args["scope"]["generation"]
            {
                return Err(
                    "declaration did not retain its exact prepared identity and generation"
                        .to_owned(),
                );
            }
            verify_literal_symbol(&items[0])
        })(
        )),
        "tracedecay_code_references" if crate::repos::small_fixture_enabled() => Some((|| {
            let items = payload["items"]
                .as_array()
                .ok_or("references omitted items")?;
            let reference = items
                .iter()
                .find(|item| {
                    item.pointer("/symbol/qualified_name")
                        == Some(&json!("src/report.ts::buildFixtureReport"))
                })
                .ok_or("references omitted buildFixtureReport's real fixtureGraph call")?;
            if reference["edge_kind"] != "calls"
                || payload["generation"] != args["scope"]["generation"]
            {
                return Err(
                    "references changed the real call relationship or generation".to_owned(),
                );
            }
            verify_literal_symbol(&reference["symbol"])
        })()),
        "tracedecay_code_type_definition" if crate::repos::small_fixture_enabled() => {
            Some((|| {
                let items = payload["items"]
                    .as_array()
                    .ok_or("type definition omitted items")?;
                if items.len() != 1
                    || items[0]["qualified_name"] != "src/report.ts::FixtureReport"
                    || payload["generation"] != args["scope"]["generation"]
                {
                    return Err("typed reportTemplate did not resolve to FixtureReport".to_owned());
                }
                verify_literal_symbol(&items[0])
            })())
        }
        "tracedecay_code_facets" if crate::repos::small_fixture_enabled() => Some((|| {
            let (value, count) = match args["dimension"].as_str() {
                Some("kind") => ("interface", 1),
                Some("language") => ("typescript", 1),
                Some("path") => ("src/report.ts", 1),
                _ => return Err("fixture facet query has an unsupported dimension".to_owned()),
            };
            if args.pointer("/scope/path_prefix") != Some(&json!("src/report.ts"))
                || !payload["items"].as_array().is_some_and(|items| {
                    items.iter().any(|item| {
                        item["dimension"] == args["dimension"]
                            && item["value"] == value
                            && item["count"] == count
                    })
                })
            {
                return Err(format!("scoped facets omitted exact {value} count {count}"));
            }
            Ok(())
        })()),
        "tracedecay_type_hierarchy" if crate::repos::small_fixture_enabled() => Some((|| {
            let root = payload["items"]
                .as_array()
                .and_then(|items| items.iter().find(|item| item["depth"] == 0))
                .ok_or("type hierarchy omitted its real CatalogItem root")?;
            if root["symbol"]["qualified_name"] != "src/catalog.py::CatalogItem"
                || root["symbol"]["node_id"] != args["node_id"]
                || root["edge_kind"] != "root"
            {
                return Err("type hierarchy changed the exact class root".to_owned());
            }
            verify_literal_symbol(&root["symbol"])
        })()),
        "tracedecay_constructors" if crate::repos::small_fixture_enabled() => Some((|| {
            if args["struct"] != "FixtureOptions"
                || payload["struct"] != "FixtureOptions"
                || payload["expected_fields"] != json!(["enabled", "retries"])
                || payload["match_count"] != 1
            {
                return Err(
                    "constructors omitted the real FixtureOptions literal or fields".to_owned(),
                );
            }
            let site = payload["sites"]
                .as_array()
                .and_then(|items| items.first())
                .ok_or("constructors omitted its literal site")?;
            if site["file"] != "src/lib.rs"
                || site["line"] != 12
                || site["fields"] != json!(["retries", "enabled"])
                || site["missing_fields"] != json!([])
                || site["update_fields"] != json!([])
                || site["field_coverage"] != "complete"
            {
                return Err(
                    "constructors changed the literal field coverage at src/lib.rs:12".to_owned(),
                );
            }
            Ok(())
        })()),
        "tracedecay_qualified_name" if crate::repos::small_fixture_enabled() => Some((|| {
            let items = payload["symbols"]
                .as_array()
                .ok_or("qualified name omitted symbols")?;
            if items.len() != 1 || items[0]["qualified_name"] != args["qualified_name"] {
                return Err("qualified name did not resolve its exact fixture symbol".to_owned());
            }
            verify_literal_symbol(&items[0])
        })()),
        "tracedecay_implementations" if crate::repos::small_fixture_enabled() => Some((|| {
            let name = args
                .pointer("/selector/name")
                .and_then(Value::as_str)
                .ok_or("implementation has no method name")?;
            let item = payload["items"]
                .as_array()
                .and_then(|items| items.iter().find(|item| item["symbol"]["name"] == name))
                .ok_or("implementation omitted its exact fixture function")?;
            verify_literal_symbol(&item["symbol"])?;
            let (source, start, end) = literal_symbol_source(&item["symbol"])?;
            let body = source
                .lines()
                .skip(start - 1)
                .take(end - start + 1)
                .collect::<Vec<_>>()
                .join("\n");
            if item["body"] != body {
                return Err("implementation body differs from literal fixture bytes".to_owned());
            }
            Ok(())
        })()),
        "tracedecay_signature_search" if crate::repos::small_fixture_enabled() => Some((|| {
            if args["is_async"] != false {
                return Err(
                    "fixture signature query must exercise its real synchronous function"
                        .to_owned(),
                );
            }
            let item = payload["items"]
                .as_array()
                .and_then(|items| {
                    items
                        .iter()
                        .find(|item| item["qualified_name"] == "src/report.ts::buildFixtureReport")
                })
                .ok_or("signature search omitted synchronous buildFixtureReport")?;
            if item["signature"] != "function buildFixtureReport(): FixtureReport"
                || item["is_async"] != false
            {
                return Err("signature search changed the literal report signature".to_owned());
            }
            verify_literal_symbol(item)
        })(
        )),
        "tracedecay_find_exact_symbol" if crate::repos::small_fixture_enabled() => Some((|| {
            let item = payload["matches"]
                .as_array()
                .and_then(|items| items.iter().find(|item| item["name"] == args["name"]))
                .ok_or("exact symbol lookup omitted its literal requested name")?;
            verify_literal_symbol(item)
        })(
        )),
        "tracedecay_field_sites"
            if crate::repos::small_fixture_enabled() && args["field"] == "quantity" =>
        {
            Some((|| {
                if args["writes_only"] != false {
                    return Err("fixture quantity query must include its real read site".to_owned());
                }
                let expected_site = json!({
                    "file": "src/catalog.py",
                    "line": 23,
                    "enclosing": "src/catalog.py::total_quantity",
                    "snippet": "return sum(item.quantity for item in items)",
                });
                if payload["field"] != "quantity"
                    || payload.pointer("/freshness/state") != Some(&json!("fresh"))
                    || payload["read_sites"] != json!([expected_site])
                    || payload["write_sites"] != json!([])
                {
                    return Err(
                        "field_sites omitted or changed the literal catalog quantity read"
                            .to_owned(),
                    );
                }
                Ok(())
            })())
        }
        "tracedecay_call_chain" if crate::repos::small_fixture_enabled() => Some((|| {
            let from = args["from_node_id"]
                .as_str()
                .filter(|id| !id.is_empty())
                .ok_or_else(|| "fixture call chain has no prepared source occurrence".to_owned())?;
            let to = args["to_node_id"]
                .as_str()
                .filter(|id| !id.is_empty())
                .ok_or_else(|| "fixture call chain has no prepared target occurrence".to_owned())?;
            if from == to
                || payload["node_ids"] != json!([from, to])
                || payload["edge_kinds"] != json!(["calls"])
            {
                return Err(
                    "call_chain omitted the direct render_summary -> fixture_catalog call"
                        .to_owned(),
                );
            }
            Ok(())
        })()),
        "tracedecay_code_symbol_search" if query_is_fixture => Some(
            if payload
                .get("items")
                .and_then(Value::as_array)
                .is_some_and(|items| {
                    items.iter().any(|item| {
                        item.get("qualified_name").and_then(Value::as_str)
                            == Some("src/report.ts::buildFixtureReport")
                    })
                })
            {
                Ok(())
            } else {
                Err("symbol_search omitted src/report.ts::buildFixtureReport".to_owned())
            },
        ),
        "tracedecay_code_phrase_search" if query_is_fixture => Some(
            if payload
                .get("items")
                .and_then(Value::as_array)
                .is_some_and(|items| {
                    items.iter().any(|item| {
                        item.get("matched_terms")
                            .and_then(Value::as_array)
                            .is_some_and(|terms| {
                                terms.iter().any(|term| term == "buildFixtureReport")
                            })
                            && item.pointer("/occurrence/path").and_then(Value::as_str)
                                == Some("src/report.ts")
                    })
                })
            {
                Ok(())
            } else {
                Err(
                    "phrase_search omitted the src/report.ts buildFixtureReport occurrence"
                        .to_owned(),
                )
            },
        ),
        _ => None,
    }
}

fn literal_symbol_source(symbol: &Value) -> Result<(&'static str, usize, usize), String> {
    let qname = symbol["qualified_name"]
        .as_str()
        .ok_or("fixture symbol omitted qualified name")?;
    let (source, start, end) = match qname {
        "src/report.ts::buildFixtureReport" => (
            include_str!("../../../../benchmark_data/runtime/fixtures/project/src/report.ts"),
            8,
            14,
        ),
        "src/report.ts::FixtureReport" => (
            include_str!("../../../../benchmark_data/runtime/fixtures/project/src/report.ts"),
            3,
            6,
        ),
        "src/graph.ts::fixtureGraph" => (
            include_str!("../../../../benchmark_data/runtime/fixtures/project/src/graph.ts"),
            6,
            11,
        ),
        "src/graph.ts::dependencyCount" => (
            include_str!("../../../../benchmark_data/runtime/fixtures/project/src/graph.ts"),
            13,
            15,
        ),
        "src/catalog.py::fixture_catalog" => (
            include_str!("../../../../benchmark_data/runtime/fixtures/project/src/catalog.py"),
            15,
            19,
        ),
        "src/catalog.py::total_quantity" => (
            include_str!("../../../../benchmark_data/runtime/fixtures/project/src/catalog.py"),
            22,
            23,
        ),
        "src/catalog.py::CatalogItem" => (
            include_str!("../../../../benchmark_data/runtime/fixtures/project/src/catalog.py"),
            9,
            12,
        ),
        "src/main.py::render_summary" => (
            include_str!("../../../../benchmark_data/runtime/fixtures/project/src/main.py"),
            8,
            11,
        ),
        "src/lib.rs::fixture_catalog_total" => (
            include_str!("../../../../benchmark_data/runtime/fixtures/project/src/lib.rs"),
            1,
            4,
        ),
        "src/lib.rs::fixture_options" => (
            include_str!("../../../../benchmark_data/runtime/fixtures/project/src/lib.rs"),
            11,
            13,
        ),
        "src/lib.rs::fixture_cli_flag" => (
            include_str!("../../../../benchmark_data/runtime/fixtures/project/src/lib.rs"),
            15,
            17,
        ),
        "tests/catalog.rs::fixture_catalog_has_stable_total" => (
            include_str!("../../../../benchmark_data/runtime/fixtures/project/tests/catalog.rs"),
            4,
            7,
        ),
        _ => return Err(format!("no literal source oracle for {qname}")),
    };
    Ok((source, start, end))
}

fn verify_literal_symbol(symbol: &Value) -> Result<(), String> {
    let (_, start, end) = literal_symbol_source(symbol)?;
    let qname = symbol["qualified_name"]
        .as_str()
        .ok_or("fixture symbol omitted qualified name")?;
    let (file, name) = qname
        .split_once("::")
        .ok_or("fixture qualified name has no file")?;
    if symbol["file"] != file
        || symbol["name"] != name
        || symbol["line"] != start
        || symbol
            .get("end_line")
            .is_some_and(|line| line != &json!(end))
    {
        return Err(format!(
            "symbol changed literal identity or source range for {qname}"
        ));
    }
    Ok(())
}

/// Disk verification runs before rollback cleanup, outside dispatch timing.
pub(crate) fn verify_code_fixture_effect(
    root: &std::path::Path,
    tool: &str,
    args: &Value,
    response: &Value,
) -> Option<Result<(), String>> {
    if !crate::repos::small_fixture_enabled()
        || !matches!(
            tool,
            "tracedecay_rename_preview"
                | "tracedecay_rename_symbol"
                | "tracedecay_replace_symbol"
                | "tracedecay_insert_at_symbol"
                | "tracedecay_move_symbol"
                | "tracedecay_source_edit_rollback"
        )
    {
        return None;
    }
    Some((|| {
        let payload = response
            .pointer("/outcome/value/payload")
            .unwrap_or(response);
        let catalog =
            include_str!("../../../../benchmark_data/runtime/fixtures/project/src/catalog.py");
        let main = include_str!("../../../../benchmark_data/runtime/fixtures/project/src/main.py");
        let actual_catalog = std::fs::read_to_string(root.join("src/catalog.py"))
            .map_err(|error| error.to_string())?;
        let actual_main =
            std::fs::read_to_string(root.join("src/main.py")).map_err(|error| error.to_string())?;
        let moved = "def total_quantity(items: tuple[CatalogItem, ...]) -> int:\n    return sum(item.quantity for item in items)";
        if tool == "tracedecay_rename_preview" {
            let node = &payload["node"];
            if payload["read_only"] != true
                || payload["new_name"] != args["new_name"]
                || node["id"] != args["node_id"]
            {
                return Err(
                    "rename preview changed its prepared identity or target name".to_owned(),
                );
            }
            let (source, start, _) = literal_symbol_source(node)?;
            if node["snippet"]
                != source
                    .lines()
                    .nth(start - 1)
                    .ok_or("rename source has no declaration")?
                || std::fs::read_to_string(
                    root.join(node["file"].as_str().ok_or("rename preview has no file")?),
                )
                .map_err(|error| error.to_string())?
                    != source
            {
                return Err(
                    "rename preview changed the literal declaration or wrote source bytes"
                        .to_owned(),
                );
            }
            return Ok(());
        }
        if payload["success"] != true
            || payload.pointer("/effect/receipt/outcome") != Some(&json!("completed"))
        {
            return Err("symbol edit did not return a completed successful effect".to_owned());
        }
        match tool {
            "tracedecay_move_symbol" => {
                let remaining_catalog = format!(
                    "{}\n",
                    catalog.lines().take(20).collect::<Vec<_>>().join("\n")
                );
                let relocated_main = format!("{}\n\n{moved}\n", main.trim_end());
                if args["symbol"] != "src/catalog.py::total_quantity"
                    || args["dest_file"] != "src/main.py"
                    || payload["source_file"] != "src/catalog.py"
                    || payload["dest_file"] != args["dest_file"]
                    || payload["moved_span"] != moved
                    || actual_catalog != remaining_catalog
                    || actual_main != relocated_main
                    || payload["predicted_state"] != payload["effect"]["receipt"]["committed_state"]
                {
                    return Err(
                        "move did not publish the exact source removal and destination bytes"
                            .to_owned(),
                    );
                }
            }
            "tracedecay_source_edit_rollback" => {
                if payload["reconciled"] != true
                    || payload.pointer("/effect/reconciliation") != Some(&json!("reconciled"))
                    || actual_catalog != catalog
                    || actual_main != main
                {
                    return Err(
                        "journaled rollback did not restore both exact fixture preimages"
                            .to_owned(),
                    );
                }
            }
            "tracedecay_rename_symbol" => {
                let sites = payload["sites"]
                    .as_array()
                    .ok_or("rename plan omitted exact edit sites")?;
                if args["old_name"] != "render_summary"
                    || sites.len() != 2
                    || !sites.iter().all(|site| {
                        site["file"] == "src/main.py"
                            && site["expected_bytes"] == "render_summary"
                            && site["replacement_bytes"] == args["new_name"]
                            && site["disposition"] == "changed"
                    })
                    || payload["dispositions"]["changed"] != 2
                {
                    return Err(
                        "rename plan did not bind the declaration and real module call".to_owned(),
                    );
                }
            }
            "tracedecay_replace_symbol" => {
                if args["symbol"] != "src/catalog.py::total_quantity"
                    || payload["file_path"] != "src/catalog.py"
                    || payload["replaced_span"] != moved
                    || payload["new_str"] != args["new_source"]
                    || !payload["diff"]
                        .as_str()
                        .is_some_and(|diff| diff.contains("+    return 77"))
                {
                    return Err(
                        "replacement preview did not replace the literal quantity function"
                            .to_owned(),
                    );
                }
            }
            "tracedecay_insert_at_symbol" => {
                if args["symbol"] != "src/catalog.py::total_quantity"
                    || payload["file_path"] != "src/catalog.py"
                    || payload["anchor_line"] != 24
                    || payload["before"] != false
                    || payload["content"] != args["content"]
                    || !payload["diff"]
                        .as_str()
                        .is_some_and(|diff| diff.contains("+# bench plan marker"))
                {
                    return Err(
                        "insert preview did not bind the exact function boundary and comment"
                            .to_owned(),
                    );
                }
            }
            _ => return Err("unexpected fixture edit operation".to_owned()),
        }
        if !matches!(
            tool,
            "tracedecay_move_symbol" | "tracedecay_source_edit_rollback"
        ) {
            if args["dry_run"] != true
                || payload["dry_run"] != true
                || actual_catalog != catalog
                || actual_main != main
            {
                return Err("symbol-edit dry run changed real fixture bytes".to_owned());
            }
        } else if payload.pointer("/effect/receipt/idempotency_key")
            != Some(&args["idempotency_key"])
            || payload.pointer("/effect/receipt/expected_state") != Some(&args["expected_state"])
        {
            return Err(
                "applied edit receipt lost its request identity or expected state".to_owned(),
            );
        }
        Ok(())
    })())
}

fn prime_fixture_references(ctx: &QueryContext, iteration: u64) -> Vec<PrimeStep> {
    if crate::repos::small_fixture_enabled() {
        vec![prime_symbol(
            "src/graph.ts::fixtureGraph".to_owned(),
            &[
                ("outcome.value.payload.items.0.node_id", "live_node"),
                ("outcome.value.payload.generation", "live_generation"),
            ],
        )]
    } else {
        prime_function(ctx, iteration)
    }
}

fn fixture_function_qname(ctx: &QueryContext, iteration: usize) -> String {
    if crate::repos::small_fixture_enabled() {
        "src/report.ts::buildFixtureReport".to_owned()
    } else {
        QueryContext::pick(&ctx.function_qnames, iteration)
    }
}

fn fixture_symbol_name(ctx: &QueryContext, iteration: usize) -> String {
    if crate::repos::small_fixture_enabled() {
        "buildFixtureReport".to_owned()
    } else {
        symbol_name(ctx, iteration)
    }
}

fn prime_fixture_function(ctx: &QueryContext, iteration: u64) -> Vec<PrimeStep> {
    if crate::repos::small_fixture_enabled() {
        vec![prime_symbol(
            "src/report.ts::buildFixtureReport".to_owned(),
            &[
                ("outcome.value.payload.items.0.node_id", "live_node"),
                ("outcome.value.payload.generation", "live_generation"),
            ],
        )]
    } else {
        prime_function(ctx, iteration)
    }
}

fn prime_fixture_type(ctx: &QueryContext, iteration: u64) -> Vec<PrimeStep> {
    if crate::repos::small_fixture_enabled() {
        vec![prime_symbol(
            "src/report.ts::reportTemplate".to_owned(),
            &[
                ("outcome.value.payload.items.0.node_id", "live_node"),
                ("outcome.value.payload.generation", "live_generation"),
            ],
        )]
    } else {
        prime_function(ctx, iteration)
    }
}

fn prime_fixture_class(ctx: &QueryContext, iteration: u64) -> Vec<PrimeStep> {
    if crate::repos::small_fixture_enabled() {
        vec![prime_symbol(
            "src/catalog.py::CatalogItem".to_owned(),
            &[("outcome.value.payload.items.0.node_id", "live_node")],
        )]
    } else {
        prime_class(ctx, iteration)
    }
}

fn prime_call_chain(ctx: &QueryContext, iteration: u64) -> Vec<PrimeStep> {
    if crate::repos::small_fixture_enabled() {
        vec![
            prime_symbol(
                "src/main.py::render_summary".to_owned(),
                &[("outcome.value.payload.items.0.node_id", "live_node")],
            ),
            prime_symbol(
                "src/catalog.py::fixture_catalog".to_owned(),
                &[("outcome.value.payload.items.0.node_id", "live_next_node")],
            ),
        ]
    } else {
        prime_function_pair(ctx, iteration)
    }
}

fn meta(projection: &str, order: &str) -> Value {
    json!({"projection": projection, "order": order, "cursor": null})
}

pub(crate) fn groups(ctx: &QueryContext, out: &mut Vec<ToolGroup>) {
    if crate::repos::small_fixture_enabled() {
        out.push(ToolGroup {
            tool: "tracedecay_code_exact_occurrence",
            queries: vec![rq(
                "tracedecay_code_exact_occurrence",
                "fixture_exact_flag",
                json!({
                    "literal": "flag:--fixture-catalog-mode", "scope": cq_scope(),
                    "meta": meta("references_only", "source_position"),
                }),
            )],
        });
    }
    #[cfg(unix)]
    if crate::repos::small_fixture_enabled() {
        out.push(ToolGroup {
            tool: "tracedecay_source_edit_reconcile",
            queries: five(|_i| {
                super::eq(
                    "tracedecay_source_edit_reconcile",
                    "reconcile_rolled_back",
                    json!({}),
                    super::no_primes,
                )
            }),
        });
    }
    out.push(ToolGroup {
        tool: "tracedecay_code_symbol_search",
        queries: five(|i| {
            rq(
                "tracedecay_code_symbol_search",
                "symbol_search",
                json!({
                    "query": fixture_symbol_name(ctx, i),
                    "lazy_index_ignored_dependencies": false,
                    "scope": scope(),
                    "meta": meta("summary", "relevance"),
                }),
            )
        }),
    });
    for tool in [
        "tracedecay_code_declaration",
        "tracedecay_code_references",
        "tracedecay_code_type_definition",
    ] {
        out.push(ToolGroup {
            tool,
            queries: five(|i| {
                Query::prepared_read(
                    "code_nav",
                    tool,
                    json!({
                        "node_id": "{{live_node}}",
                        "scope": {"generation": "{{live_generation}}", "path_prefix": null},
                        "meta": meta("summary", "relevance"),
                    }),
                    i,
                    match tool {
                        "tracedecay_code_references" => prime_fixture_references,
                        "tracedecay_code_type_definition" => prime_fixture_type,
                        _ => prime_fixture_function,
                    },
                )
            }),
        });
    }
    out.push(ToolGroup {
        tool: "tracedecay_code_phrase_search",
        queries: five(|i| {
            rq(
                "tracedecay_code_phrase_search",
                "phrase_search",
                json!({
                    "query": fixture_symbol_name(ctx, i),
                    "phrases": [fixture_symbol_name(ctx, i)],
                    "scope": cq_scope(),
                    "meta": meta("summary", "relevance"),
                }),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_code_exact_occurrence",
        queries: five(|i| {
            rq(
                "tracedecay_code_exact_occurrence",
                "exact_occurrence",
                json!({
                    "literal": fixture_symbol_name(ctx, i),
                    "scope": cq_scope(),
                    "meta": meta("references_only", "source_position"),
                }),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_code_facets",
        queries: five(|i| {
            rq(
                "tracedecay_code_facets",
                "facets",
                json!({
                    "dimension": *["kind", "language", "path", "kind", "language"].get(i).unwrap_or(&"kind"),
                    "scope": if crate::repos::small_fixture_enabled() {
                        json!({"generation": "code-generation:unpinned-latest.v1", "path_prefix": "src/report.ts"})
                    } else { cq_scope() },
                    "meta": meta("summary", "stable_identity"),
                }),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_code_timeline",
        queries: five(|i| {
            if crate::repos::small_fixture_enabled() {
                return Query::prepared_read("timeline", "tracedecay_code_timeline", json!({
                    "scope": {"generation": "{{live_generation}}", "path_prefix": "src/report.ts"},
                    "meta": meta("summary", "temporal_descending"),
                }), i, prime_fixture_type);
            }
            rq(
                "tracedecay_code_timeline",
                "timeline",
                json!({
                    "scope": {
                        "generation": "code-generation:unpinned-latest.v1",
                        "path_prefix": crate::queries::dir(ctx, i),
                    },
                    "meta": meta("summary", "temporal_descending"),
                }),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_type_hierarchy",
        queries: five(|i| {
            Query::prepared_read(
                "type_hierarchy",
                "tracedecay_type_hierarchy",
                json!({
                    "node_id": "{{live_node}}",
                    "maximum_depth": 4,
                    "scope": scope(),
                    "meta": meta("summary", "relevance"),
                }),
                i,
                prime_fixture_class,
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_call_chain",
        queries: five(|i| {
            Query::prepared_read(
                "call_chain",
                "tracedecay_call_chain",
                json!({
                    "from_node_id": "{{live_node}}",
                    "to_node_id": "{{live_next_node}}",
                    "maximum_depth": 4,
                }),
                i,
                prime_call_chain,
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_constructors",
        queries: five(|i| {
            let name = if crate::repos::small_fixture_enabled() {
                Some("FixtureOptions".to_owned())
            } else {
                QueryContext::pick(&ctx.struct_qnames, i)
                    .rsplit("::")
                    .next()
                    .map(str::to_owned)
            };
            rq(
                "tracedecay_constructors",
                "constructors",
                json!({"struct": name, "limit": 10}),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_field_sites",
        queries: five(|i| {
            rq(
                "tracedecay_field_sites",
                "field_sites",
                if crate::repos::small_fixture_enabled() {
                    json!({"field": "quantity", "limit": 5 + i, "writes_only": false})
                } else {
                    json!({"field": fixture_symbol_name(ctx, i), "limit": 20, "writes_only": i % 2 == 0})
                },
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_implementations",
        queries: five(|i| {
            rq(
                "tracedecay_implementations",
                "implementations",
                json!({
                    "selector": {"selector": "method", "name": fixture_symbol_name(ctx, i)},
                    "scope": scope(),
                    "meta": meta("summary", "relevance"),
                }),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_qualified_name",
        queries: five(|i| {
            rq(
                "tracedecay_qualified_name",
                "qualified_name",
                json!({
                    "qualified_name": fixture_function_qname(ctx, i),
                    "page": {"page_size": 10, "cursor": null},
                }),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_signature_search",
        queries: five(|i| {
            rq(
                "tracedecay_signature_search",
                "signature_search",
                json!({
                    "returns": null,
                    "params": [],
                    "is_async": if crate::repos::small_fixture_enabled() { false } else { i % 2 == 0 },
                    "scope": scope(),
                    "meta": meta("summary", "relevance"),
                }),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_find_exact_symbol",
        queries: five(|i| {
            rq(
                "tracedecay_find_exact_symbol",
                "find_exact",
                json!({
                    "name": fixture_function_qname(ctx, i)
                        .rsplit("::")
                        .next()
                        .unwrap_or("main")
                        .to_owned(),
                    "limit": 10,
                    "lazy_index_ignored_dependencies": true,
                }),
            )
        }),
    });

    // Symbol operations plan against fresh claims. Move is applied and rolled
    // back through the retained journal; other symbol lanes measure previews.
    out.push(ToolGroup {
        tool: "tracedecay_rename_preview",
        queries: five(|i| {
            Query::prepared_read(
                "rename_preview",
                "tracedecay_rename_preview",
                json!({"node_id": "{{live_node}}", "new_name": format!("bench_renamed_{i}")}),
                i,
                prime_fixture_function,
            )
        }),
    });
    // The plan lane only runs when seed probing found a renameable node —
    // an absent target is reported via seeds.skipped, not a fabricated one.
    if ctx.seeds.rename_node.is_some() {
        out.push(ToolGroup {
            tool: "tracedecay_rename_symbol",
            queries: five(|_i| {
                eqc(
                    "tracedecay_rename_symbol",
                    "rename_plan",
                    json!({
                    "node_id": "{{rp_id}}",
                    "qualified_name": "{{rp_qname}}",
                    "kind": "{{rp_kind}}",
                    "file": "{{rp_file}}",
                    "old_name": "{{rp_name}}",
                    "new_name": "bench_plan_{{iter}}",
                    "dry_run": true,
                    "format": "json",
                    }),
                    p_rename,
                    no_cleanup(),
                )
            }),
        });
    }
    // Symbol-edit apply lanes: each runs only against the symbol seed time
    // proved unblocked + small; absence is reported via seeds.skipped.
    if ctx.seeds.replace_target.is_some() {
        out.push(ToolGroup {
            tool: "tracedecay_replace_symbol",
            queries: five(|_i| {
                eqc(
                    "tracedecay_replace_symbol",
                    "replace_plan",
                    json!({
                        "symbol": "{{sym}}",
                        "new_source": if crate::repos::small_fixture_enabled() {
                            "def total_quantity(items: tuple[CatalogItem, ...]) -> int:\n    return 77"
                        } else { "pub fn bench_target() -> i32 { 77 }" },
                        "dry_run": true,
                        "format": "json",
                    }),
                    p_replace,
                    no_cleanup(),
                )
            }),
        });
    }
    if ctx.seeds.insert_target.is_some() {
        out.push(ToolGroup {
            tool: "tracedecay_insert_at_symbol",
            queries: five(|_i| {
                eqc(
                    "tracedecay_insert_at_symbol",
                    "insert_plan",
                    json!({
                        "symbol": "{{sym}}",
                        "content": if crate::repos::small_fixture_enabled() { "# bench plan marker" } else { "// bench plan marker" },
                        "position": "after",
                        "dry_run": true,
                        "format": "json",
                    }),
                    p_insert,
                    no_cleanup(),
                )
            }),
        });
    }
    if ctx.seeds.move_target.is_some() {
        out.push(ToolGroup {
            tool: "tracedecay_move_symbol",
            queries: five(|_i| {
                eqc(
                    "tracedecay_move_symbol",
                    "move_apply",
                    json!({
                        "symbol": "{{sym}}",
                        "dest_file": "{{dest_file}}",
                        "dry_run": false,
                        "update_references": false,
                        "expected_state": "{{expected_state}}",
                        "idempotency_key": "bench-move-src-{{iter}}",
                    }),
                    p_move,
                    rollback_cleanup("bench-move"),
                )
            }),
        });
        // source_edit_rollback itself: a fresh journaled move per iteration
        // mints the receipt identity the timed rollback consumes. Only
        // move_symbol retains rollback material, so this pair is also the
        // sole real-apply coverage in the family.

        out.push(ToolGroup {
            tool: "tracedecay_source_edit_rollback",
            queries: five(|_i| {
                eqc(
                    "tracedecay_source_edit_rollback",
                    "rollback_apply",
                    json!({
                        "effect_id": "{{jr_effect_id}}",
                        "original_idempotency_key": "bench-jr-src-{{iter}}",
                        "idempotency_key": "bench-jr-rollback-{{iter}}",
                        "original_input_digest": "{{jr_input_digest}}",
                        "expected_state": "{{jr_committed_state}}",
                        "confirm": true,
                    }),
                    p_journaled_move,
                    no_cleanup(),
                )
            }),
        });
    }
}

/// Permission denial retains a real journaled effect without publishing bytes.
#[cfg(unix)]
pub(crate) async fn prepare_source_reconciliation(
    harness: &tracedecay::daemon::ProductionProjectCompositionHarnessV1,
    project_root: &std::path::Path,
    iteration: u64,
) -> Result<(Value, Value), String> {
    use std::os::unix::fs::PermissionsExt;
    let relative = format!(
        "{}/reconcile-{iteration}/candidate.txt",
        crate::queries::SCRATCH_DIR
    );
    let file = project_root.join(&relative);
    let parent = file
        .parent()
        .ok_or("reconciliation candidate has no parent")?;
    std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    std::fs::write(&file, b"before reconciliation\n").map_err(|error| error.to_string())?;
    let preview = crate::queries::call_json_tool(
        harness,
        project_root,
        "tracedecay_str_replace",
        json!({"path": relative, "old_str": "before reconciliation",
        "new_str": "after reconciliation", "dry_run": true}),
    )
    .await?;
    let expected = super::extract_token(&preview, "dig:expected_state")
        .ok_or_else(|| format!("reconciliation preview omitted expected state: {preview}"))?;
    let permissions = std::fs::metadata(parent)
        .map_err(|error| error.to_string())?
        .permissions();
    std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o500))
        .map_err(|error| error.to_string())?;
    let original_key = format!("bench.reconcile.original.{iteration}");
    let response = harness.call_tool(project_root, "tracedecay_str_replace", json!({
        "path": relative, "old_str": "before reconciliation", "new_str": "after reconciliation",
        "expected_state": expected, "idempotency_key": original_key, "format": "json",
    })).await;
    std::fs::set_permissions(parent, permissions).map_err(|error| error.to_string())?;
    let response = response.map_err(|error| error.to_string())?;
    if response.error.is_some() {
        return Err(format!(
            "reconciliation producer transport error: {:?}",
            response.error
        ));
    }
    let text = response
        .result
        .as_ref()
        .and_then(|value| value.pointer("/content/0/text"))
        .and_then(Value::as_str)
        .ok_or("reconciliation producer omitted JSON content")?;
    let body: Value = serde_json::from_str(text).map_err(|error| error.to_string())?;
    let body = body.pointer("/outcome/value/payload").unwrap_or(&body);
    if body.get("effect_unknown") != Some(&json!(true))
        || body.pointer("/effect/receipt/outcome") != Some(&json!("effect_unknown"))
        || body.pointer("/effect/reconciliation") != Some(&json!("pending"))
        || std::fs::read(&file).map_err(|error| error.to_string())? != b"before reconciliation\n"
    {
        return Err(format!(
            "publication denial did not retain the exact unknown effect: {body}"
        ));
    }
    let effect_id = body
        .pointer("/effect/effect_id")
        .and_then(Value::as_str)
        .ok_or("retained effect omitted effect ID")?;
    let input_digest = body
        .pointer("/effect/receipt/input_digest")
        .and_then(Value::as_str)
        .ok_or("retained effect omitted input digest")?;
    Ok((
        json!({"kind": "str_replace", "effect_id": effect_id,
        "idempotency_key": original_key, "attempt_idempotency_key": format!("bench.reconcile.attempt.{iteration}"),
        "input_digest": input_digest, "disposition": "confirm_rolled_back",
        "confirm": true, "format": "json"}),
        json!(relative),
    ))
}

fn edit_dest(ctx: &QueryContext) -> String {
    if crate::repos::small_fixture_enabled() {
        return "src/main.py".to_owned();
    }
    ctx.seeds
        .sample_files
        .first()
        .cloned()
        .unwrap_or_else(|| "src/__init__.py".to_owned())
}

fn p_rename(ctx: &QueryContext, _iter: u64) -> Vec<PrimeStep> {
    let qualified_name = if crate::repos::small_fixture_enabled() {
        json!("src/main.py::render_summary")
    } else {
        ctx.seeds
            .rename_node
            .as_ref()
            .and_then(|node| node.get("qualified_name"))
            .cloned()
            .unwrap_or(Value::Null)
    };
    vec![
        PrimeStep {
            inject: Vec::new(),
            tool: "tracedecay_qualified_name",
            args: json!({
                "qualified_name": qualified_name,
                "page": {"page_size": 10, "cursor": null},
                "format": "json",
            }),
            capture: &[("outcome.value.payload.symbols.0.node_id", "rename_node")],
        },
        PrimeStep {
            inject: Vec::new(),
            tool: "tracedecay_rename_preview",
            args: json!({
                "node_id": "{{rename_node}}",
                "new_name": "bench_renamed_{{iter}}",
                "format": "json",
            }),
            capture: &[
                ("digpath:node:id", "rp_id"),
                ("digpath:node:qualified_name", "rp_qname"),
                ("digpath:node:kind", "rp_kind"),
                ("digpath:node:file", "rp_file"),
                ("digpath:node:name", "rp_name"),
            ],
        },
    ]
}

fn sym_inject(ctx: &QueryContext, target: &Option<String>) -> Vec<(String, Value)> {
    vec![
        (
            "sym".to_owned(),
            json!(if crate::repos::small_fixture_enabled() {
                "src/catalog.py::total_quantity".to_owned()
            } else {
                target.clone().unwrap_or_else(|| "missing".to_owned())
            }),
        ),
        (
            "sym_source".to_owned(),
            json!("pub fn bench_target() -> i32 { 42 }"),
        ),
        ("dest_file".to_owned(), json!(edit_dest(ctx))),
    ]
}

fn p_replace(ctx: &QueryContext, _iter: u64) -> Vec<PrimeStep> {
    vec![PrimeStep {
        inject: sym_inject(ctx, &ctx.seeds.replace_target),
        tool: "tracedecay_replace_symbol",
        args: json!({
            "symbol": "{{sym}}",
            "new_source": "{{sym_source}}",
            "dry_run": true,
            "format": "json",
        }),
        capture: &[("dig:expected_state", "expected_state")],
    }]
}

fn p_insert(ctx: &QueryContext, _iter: u64) -> Vec<PrimeStep> {
    vec![PrimeStep {
        inject: sym_inject(ctx, &ctx.seeds.insert_target),
        tool: "tracedecay_insert_at_symbol",
        args: json!({
            "symbol": "{{sym}}",
            "content": "// bench insert marker",
            "position": "after",
            "dry_run": true,
            "format": "json",
        }),
        capture: &[("dig:expected_state", "expected_state")],
    }]
}

fn p_move(ctx: &QueryContext, _iter: u64) -> Vec<PrimeStep> {
    vec![PrimeStep {
        inject: sym_inject(ctx, &ctx.seeds.move_target),
        tool: "tracedecay_move_symbol",
        args: json!({
            "symbol": "{{sym}}",
            "dest_file": "{{dest_file}}",
            "dry_run": true,
            "format": "json",
        }),
        capture: &[("dig:expected_state", "expected_state")],
    }]
}

fn p_journaled_move(ctx: &QueryContext, _iter: u64) -> Vec<PrimeStep> {
    vec![
        PrimeStep {
            inject: sym_inject(ctx, &ctx.seeds.move_target),
            tool: "tracedecay_move_symbol",
            args: json!({
                "symbol": "{{sym}}",
                "dest_file": "{{dest_file}}",
                "dry_run": true,
                "format": "json",
            }),
            capture: &[("dig:expected_state", "jr_expected_state")],
        },
        PrimeStep {
            inject: Vec::new(),
            tool: "tracedecay_move_symbol",
            args: json!({
                "symbol": "{{sym}}",
                "dest_file": "{{dest_file}}",
                "dry_run": false,
                "update_references": false,
                "expected_state": "{{jr_expected_state}}",
                "idempotency_key": "bench-jr-src-{{iter}}",
                "format": "json",
            }),
            capture: &[
                ("dig:effect_id", "jr_effect_id"),
                ("dig:input_digest", "jr_input_digest"),
                ("dig:committed_state", "jr_committed_state"),
            ],
        },
    ]
}

/// Journaled restore after a timed source-edit apply: the timed response's
/// receipt mints every identity the rollback consumes.
fn rollback_cleanup(key_prefix: &'static str) -> EffectCleanup {
    let _ = key_prefix;
    EffectCleanup {
        capture: &[
            ("dig:effect_id", "rb_effect_id"),
            ("dig:input_digest", "rb_input_digest"),
            ("dig:committed_state", "rb_committed_state"),
        ],
        steps: rb_move,
    }
}

fn rb_args(prefix: &str) -> Value {
    json!({
        "effect_id": "{{rb_effect_id}}",
        "original_idempotency_key": format!("{prefix}-src-{{{{iter}}}}"),
        "idempotency_key": format!("{prefix}-rollback-{{{{iter}}}}"),
        "original_input_digest": "{{rb_input_digest}}",
        "expected_state": "{{rb_committed_state}}",
        "confirm": true,
        "format": "json",
    })
}

fn rb_move(_ctx: &QueryContext, _iter: u64) -> Vec<PrimeStep> {
    vec![PrimeStep {
        inject: Vec::new(),
        tool: "tracedecay_source_edit_rollback",
        args: rb_args("bench-move"),
        capture: &[],
    }]
}

/// `eqc` needs an `EffectCleanup` value; a zero-step one expresses "no
/// restore needed" without `Option` plumbing at the call site.
fn no_cleanup() -> EffectCleanup {
    EffectCleanup {
        capture: &[],
        steps: |_ctx, _iter| Vec::new(),
    }
}
