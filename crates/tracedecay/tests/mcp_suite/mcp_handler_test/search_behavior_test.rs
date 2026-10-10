//! `tracedecay_search` as an agent calls it: one concrete query in, the symbol
//! the agent would open out. The production MCP server is the subject.

#![cfg(feature = "test-transport")]

use crate::support::{
    extract_real_server_text, handle_real_server_tool_call, handle_real_server_tool_call_raw,
    production_composition_fixture_with_sources, refusal_problem, warm_code_index_search,
};
use serde_json::{Value, json};
use std::fs;

const LEDGER_SOURCE: &str = "\
pub fn ledger_post_entry(amount: u32) -> u32 {\n    \
    amount\n\
}\n\
\n\
pub fn unrelated_balance() -> u32 {\n    \
    0\n\
}\n";

fn search_displays(payload: &Value) -> Vec<Value> {
    payload["results"]
        .as_array()
        .unwrap_or_else(|| panic!("search payload has no results array: {payload}"))
        .iter()
        .map(|item| item["display"].clone())
        .collect()
}

#[tokio::test]
async fn search_returns_the_named_symbol_and_refuses_arguments_outside_its_typed_request() {
    let fixture = production_composition_fixture_with_sources(|project| {
        fs::create_dir_all(project.join("src")).expect("search fixture sources");
        fs::write(project.join("src/ledger.rs"), LEDGER_SOURCE).expect("write ledger source");
    })
    .await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production search server");

    let assert_refused = |response: &Value, detail: &str| {
        assert!(response.get("error").is_none(), "{response}");
        let problem = refusal_problem(&response["result"]);
        assert_eq!(problem["kind"], "invalid_request", "{response}");
        assert_eq!(
            problem["code"], "application.surface.invalid_request",
            "{response}"
        );
        assert_eq!(
            problem["message"],
            format!("invalid arguments for tracedecay_search: {detail}"),
            "{response}"
        );
    };
    let missing = handle_real_server_tool_call_raw(&server, "tracedecay_search", json!({})).await;
    assert_refused(&missing, "missing field `query`");
    // Arguments outside the typed request are refused, not silently ignored.
    let unknown = handle_real_server_tool_call_raw(
        &server,
        "tracedecay_search",
        json!({"query": "ledger_post_entry", "semantic_mode": "hybrid"}),
    )
    .await;
    assert_refused(
        &unknown,
        "unknown field `semantic_mode`, expected one of `query`, `limit`, `cursor`, \
         `lexical_anchors`, `prefer_symbol`, `lexical_aliases`, `lexical_phrases`, \
         `lexical_proximities`, `lexical_field_filters`, `lazy_index_ignored_dependencies`",
    );
    let untyped_limit = handle_real_server_tool_call_raw(
        &server,
        "tracedecay_search",
        json!({"query": "ledger_post_entry", "limit": "5"}),
    )
    .await;
    assert_refused(&untyped_limit, "invalid type: string \"5\", expected u64");

    warm_code_index_search(&server, "ledger_post_entry").await;

    // The warm-up waits for a ready generation and a fresh seat.
    let response = handle_real_server_tool_call(
        &server,
        "tracedecay_search",
        json!({
            "query": "ledger_post_entry",
            "prefer_symbol": true,
            "format": "json",
        }),
    )
    .await;
    let hit: Value =
        serde_json::from_str(extract_real_server_text(&response)).expect("search JSON");
    assert_eq!(hit["freshness"], json!({ "state": "fresh" }), "{hit}");
    assert_eq!(
        hit["coverage"],
        json!({
            "exact": "complete",
            "lexical": "complete",
            "graph": "complete",
            "recall": "full",
        }),
        "{hit}"
    );
    assert!(
        hit["status"].is_null(),
        "a completed search is not unavailable: {hit}"
    );
    assert_eq!(
        search_displays(&hit),
        vec![json!({
            "name": "ledger_post_entry",
            "qualified_name": "src/ledger.rs::ledger_post_entry",
            "kind": "function",
            "path": "src/ledger.rs",
        })],
        "{hit}"
    );
    assert_eq!(hit["results"][0]["final_ordinal"], 0, "{hit}");
    assert_eq!(
        hit["results"][0]["candidate"]["exact_class"], "exact_message",
        "{hit}"
    );
    assert_eq!(
        hit["lexical_routes"],
        json!([
            { "route": "query", "label": "query" },
            {
                "route": "preferred_symbol",
                "tokens": ["ledger_post_entry"],
                "label": "symbol:ledger_post_entry",
            },
            {
                "route": "identifier_split",
                "strict_query": "ledger_post_entry",
                "terms": ["ledger", "post", "entry"],
                "label": "split:ledger|post|entry",
            },
        ]),
        "{hit}"
    );
    assert_eq!(
        hit["query_route"],
        json!({ "route": "name", "margin": 0.5, "decided_by": "caller" }),
        "{hit}"
    );

    let automatic = handle_real_server_tool_call(
        &server,
        "tracedecay_search",
        json!({ "query": "ledger_post_entry", "format": "json" }),
    )
    .await;
    let automatic: Value =
        serde_json::from_str(extract_real_server_text(&automatic)).expect("search JSON");
    assert_eq!(
        automatic["lexical_routes"], hit["lexical_routes"],
        "{automatic}"
    );
    assert_eq!(
        automatic["query_route"],
        json!({ "route": "name", "margin": 0.5, "decided_by": "query_shape" }),
        "{automatic}"
    );

    let rendered = handle_real_server_tool_call(
        &server,
        "tracedecay_search",
        json!({
            "query": "ledger_post_entry",
            "prefer_symbol": true,
            "format": "markdown",
        }),
    )
    .await;
    let rendered = extract_real_server_text(&rendered);
    let mut lines = rendered.lines();
    assert_eq!(lines.next(), Some("freshness: fresh"), "{rendered}");
    assert_eq!(lines.next(), Some("## Search Results"), "{rendered}");
    let bullet = lines
        .find(|line| line.starts_with("- **"))
        .unwrap_or_else(|| panic!("markdown search has no result bullet: {rendered}"));
    let (head, rest) = bullet
        .split_once(" · utility ")
        .unwrap_or_else(|| panic!("markdown bullet has no utility suffix: {bullet} in {rendered}"));
    assert_eq!(
        head,
        "- **ledger_post_entry** (function, exact_message), rank 1"
    );
    let via = rest
        .split_once(" · via ")
        .map(|(_, via)| via)
        .unwrap_or_else(|| panic!("markdown bullet has no route suffix: {bullet}"));
    // Each matching chunk discloses the same three routes. The bullet lists
    // every disclosure the host receives, in rank order, without collapsing
    // them.
    assert_eq!(
        via,
        "query, symbol:ledger_post_entry, split:ledger|post|entry, query, symbol:ledger_post_entry, split:ledger|post|entry"
    );

    let miss = handle_real_server_tool_call(
        &server,
        "tracedecay_search",
        json!({ "query": "qxqvnomatch", "format": "json" }),
    )
    .await;
    let miss: Value = serde_json::from_str(extract_real_server_text(&miss)).expect("miss JSON");
    assert_eq!(miss["results"], json!([]), "{miss}");
    assert_eq!(miss["freshness"], json!({ "state": "fresh" }), "{miss}");
    assert_eq!(miss["coverage"]["recall"], "full", "{miss}");
    assert_eq!(
        miss["query_route"],
        json!({ "route": "prose", "margin": -0.5, "decided_by": "query_shape" }),
        "{miss}"
    );

    let anchored = handle_real_server_tool_call(
        &server,
        "tracedecay_search",
        json!({
            "query": "qxqvnomatch",
            "lexical_anchors": ["ledger_post_entry"],
            "format": "json",
        }),
    )
    .await;
    let anchored: Value =
        serde_json::from_str(extract_real_server_text(&anchored)).expect("anchor JSON");
    assert_eq!(
        search_displays(&anchored),
        vec![json!({
            "name": "ledger_post_entry",
            "qualified_name": "src/ledger.rs::ledger_post_entry",
            "kind": "function",
            "path": "src/ledger.rs",
        })],
        "an anchor must rank the named symbol when the query text does not: {anchored}"
    );
    assert_eq!(
        anchored["lexical_routes"],
        json!([
            { "route": "query", "label": "query" },
            {
                "route": "anchor",
                "anchor": "ledger_post_entry",
                "label": "anchor:ledger_post_entry",
            },
        ]),
        "{anchored}"
    );

    fixture.harness.shutdown().await;
}

/// One ledger function per file, so the per-file diversity cap keeps all of
/// them in the fused set a page walks.
const LEDGER_FAMILY: [&str; 5] = [
    "ledger_open",
    "ledger_close",
    "ledger_post",
    "ledger_void",
    "ledger_audit",
];

#[tokio::test]
async fn a_search_cursor_pages_only_the_request_and_operation_that_minted_it() {
    let fixture = production_composition_fixture_with_sources(|project| {
        fs::create_dir_all(project.join("src")).expect("search fixture sources");
        for (value, name) in LEDGER_FAMILY.iter().enumerate() {
            fs::write(
                project.join(format!("src/{name}.rs")),
                format!("pub fn {name}() -> usize {{\n    {value}\n}}\n"),
            )
            .expect("write ledger source");
        }
    })
    .await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production search server");
    warm_code_index_search(&server, "ledger_open").await;
    let search = |arguments: Value| {
        let server = &server;
        async move {
            let response =
                handle_real_server_tool_call(server, "tracedecay_search", arguments).await;
            serde_json::from_str::<Value>(extract_real_server_text(&response)).expect("search JSON")
        }
    };

    let first = search(json!({"query": "ledger", "limit": 2, "format": "json"})).await;
    let cursor = first["next_cursor"]
        .as_str()
        .unwrap_or_else(|| panic!("first page continues: {first}"))
        .to_owned();

    let resized = handle_real_server_tool_call_raw(
        &server,
        "tracedecay_search",
        json!({"query": "ledger", "limit": 3, "cursor": cursor, "format": "json"}),
    )
    .await;
    let problem = refusal_problem(&resized["result"]);
    assert_eq!(problem["kind"], "invalid_request", "{resized}");
    assert_eq!(problem["code"], "cursor.parameter_changed", "{resized}");
    assert_eq!(
        problem["message"],
        "The cursor was issued for a request with a different `limit`. Repeat the request \
         with the parameters that returned the cursor, or restart without it.",
        "{resized}"
    );

    let branch_search = handle_real_server_tool_call_raw(
        &server,
        "tracedecay_branch_search",
        json!({"branch": "master", "query": "ledger", "limit": 2, "cursor": cursor}),
    )
    .await;
    assert_eq!(
        refusal_problem(&branch_search["result"])["code"],
        "cursor.invalid",
        "{branch_search}"
    );

    let second = search(json!({
        "query": "ledger", "limit": 2, "cursor": cursor, "format": "json",
    }))
    .await;
    let names = |page: &Value| -> Vec<String> {
        search_displays(page)
            .iter()
            .map(|display| display["name"].as_str().expect("display name").to_owned())
            .collect()
    };
    let (first_names, second_names) = (names(&first), names(&second));
    assert_eq!(
        (first_names.len(), second_names.len()),
        (2, 2),
        "{first} / {second}"
    );
    assert!(
        second_names
            .iter()
            .all(|name| LEDGER_FAMILY.contains(&name.as_str()) && !first_names.contains(name)),
        "page two repeated page one: {first_names:?} / {second_names:?}"
    );

    fixture.harness.shutdown().await;
}

const UTIL_SOURCE: &str = "\
pub fn normalize(s: &str) -> String {\n    \
    s.trim().to_lowercase()\n\
}\n\
\n\
pub fn clamp(v: i64, lo: i64, hi: i64) -> i64 {\n    \
    if v < lo { lo } else if v > hi { hi } else { v }\n\
}\n";

const MATH_SOURCE: &str = "\
use crate::util::clamp;\n\
\n\
pub fn normalize_score(v: i64) -> i64 {\n    \
    clamp(v, 0, 100)\n\
}\n";

const LIB_SOURCE: &str = "mod math;\nmod util;\n";

fn display(name: &str, file: &str) -> Value {
    json!({
        "name": name,
        "qualified_name": format!("{file}::{name}"),
        "kind": "function",
        "path": file,
    })
}

/// `clamp` does not contain the query text: the graph lane ranks it as the
/// callee of `normalize_score`. That candidate is hydrated like the text hits.
#[tokio::test]
async fn search_names_every_ranked_symbol_including_graph_reached_callees() {
    let fixture = production_composition_fixture_with_sources(|project| {
        fs::create_dir_all(project.join("src")).expect("search fixture sources");
        fs::write(project.join("src/lib.rs"), LIB_SOURCE).expect("write lib source");
        fs::write(project.join("src/util.rs"), UTIL_SOURCE).expect("write util source");
        fs::write(project.join("src/math.rs"), MATH_SOURCE).expect("write math source");
    })
    .await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production search server");
    warm_code_index_search(&server, "normalize").await;

    let response = handle_real_server_tool_call(
        &server,
        "tracedecay_search",
        json!({ "query": "normalize", "limit": 5, "format": "json" }),
    )
    .await;
    let page: Value = serde_json::from_str(extract_real_server_text(&response)).expect("JSON");
    let mut displays = search_displays(&page);
    displays.sort_by(|left, right| left["name"].as_str().cmp(&right["name"].as_str()));
    assert_eq!(
        displays,
        vec![
            display("clamp", "src/util.rs"),
            display("normalize", "src/util.rs"),
            display("normalize_score", "src/math.rs"),
        ],
        "{page}"
    );
    assert!(
        page["results"]
            .as_array()
            .expect("results")
            .iter()
            .all(|row| row.get("display_unavailable").is_none()),
        "{page}"
    );

    let rendered = handle_real_server_tool_call(
        &server,
        "tracedecay_search",
        json!({ "query": "normalize", "limit": 5, "format": "markdown" }),
    )
    .await;
    let rendered = extract_real_server_text(&rendered);
    let mut titles = rendered
        .lines()
        .filter_map(|line| line.strip_prefix("- **"))
        .filter_map(|line| line.split_once("** ("))
        .map(|(title, _)| title)
        .collect::<Vec<_>>();
    titles.sort_unstable();
    assert_eq!(
        titles,
        vec!["clamp", "normalize", "normalize_score"],
        "{rendered}"
    );

    fixture.harness.shutdown().await;
}

#[tokio::test]
async fn search_ranks_the_production_definition_ahead_of_the_test_reference() {
    let fixture = production_composition_fixture_with_sources(|project| {
        fs::create_dir_all(project.join("src")).expect("production sources");
        fs::create_dir_all(project.join("tests")).expect("test sources");
        fs::write(
            project.join("src/lib.rs"),
            "pub fn ensure_daemon_running() -> i32 {\n    7\n}\n",
        )
        .expect("write production definition");
        fs::write(
            project.join("tests/ensure_daemon.rs"),
            concat!(
                "#[test]\nfn ensure_daemon_running() {\n",
                "    let _ = ensure_daemon_running;\n",
                "    let _ = ensure_daemon_running;\n",
                "    let _ = ensure_daemon_running;\n",
                "    let _ = ensure_daemon_running;\n",
                "    assert_eq!(7, 7);\n",
                "}\n",
            ),
        )
        .expect("write stronger test reference");
    })
    .await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production search server");
    warm_code_index_search(&server, "ensure_daemon_running").await;

    let response = handle_real_server_tool_call(
        &server,
        "tracedecay_search",
        json!({
            "query": "ensure_daemon_running",
            "limit": 5,
            "prefer_symbol": true,
            "format": "json",
        }),
    )
    .await;
    let page: Value = serde_json::from_str(extract_real_server_text(&response)).expect("JSON");
    let paths: Vec<&str> = page["results"]
        .as_array()
        .unwrap_or_else(|| panic!("search payload has no results: {page}"))
        .iter()
        .map(|row| {
            row["display"]["path"]
                .as_str()
                .unwrap_or_else(|| panic!("search row missing path: {row}"))
        })
        .collect();
    assert_eq!(
        paths.first().copied(),
        Some("src/lib.rs"),
        "production definition must outrank the test reference: {page}"
    );
    assert!(
        paths.contains(&"tests/ensure_daemon.rs"),
        "the same-named test reference must still appear: {page}"
    );

    let capped = handle_real_server_tool_call(
        &server,
        "tracedecay_search",
        json!({
            "query": "ensure_daemon_running",
            "limit": 1,
            "prefer_symbol": true,
            "format": "json",
        }),
    )
    .await;
    let capped_page: Value = serde_json::from_str(extract_real_server_text(&capped)).expect("JSON");
    let capped_paths: Vec<&str> = capped_page["results"]
        .as_array()
        .unwrap_or_else(|| panic!("capped search payload has no results: {capped_page}"))
        .iter()
        .map(|row| {
            row["display"]["path"]
                .as_str()
                .unwrap_or_else(|| panic!("capped search row missing path: {row}"))
        })
        .collect();
    assert_eq!(
        capped_paths,
        vec!["src/lib.rs"],
        "a one-result cap must keep the production definition: {capped_page}"
    );

    fixture.harness.shutdown().await;
}
