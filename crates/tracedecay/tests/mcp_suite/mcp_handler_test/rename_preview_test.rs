//! `tracedecay_rename_preview` as a host sees it: one JSON-RPC `tools/call`.
//!
//! The fixture is the production call graph, not a handler double. `checkout`
//! in `src/lib.rs` calls `reserve_stock` in `src/stock.rs` through an import.
//! The same caller file also names the symbol in a string and a comment.
//! Those two sites plus the import are not graph edges, so they must show up
//! as text-only matches, and neither preview may change the sources.

#![cfg(feature = "test-transport")]

use std::fs;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use serde_json::{Value, json};
use tracedecay::mcp::McpServer;
use tracing_subscriber::Layer;
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;

use crate::support::{
    ProductionCompositionFixture, handle_real_server_tool_call_raw,
    production_composition_fixture_with_sources, refusal_problem, warm_code_index_search,
};

const STOCK_RS: &str = "\
pub fn reserve_stock(qty: u32) -> u32 {\n\
    qty\n\
}\n";

const LIB_RS: &str = "\
mod stock;\n\
\n\
use crate::stock::reserve_stock;\n\
\n\
pub fn checkout(qty: u32) -> u32 {\n\
    let _note = \"reserve_stock\";\n\
    // reserve_stock stays in this comment\n\
    reserve_stock(qty)\n\
}\n";

const PREVIEW_NOTE: &str = "\
Preview only. Nothing is edited. 'references' are graph reference sites \
(the declaration is reported separately in 'node'); 'text_only_matches' are \
literal name occurrences NOT backed by a graph edge (comments, strings, \
dynamic dispatch, unresolved refs) and must be reviewed by hand. Graph \
call-edge coverage improves as the resolver does.";

const TEXT_ONLY_NOTE: &str = "text-only matches, review manually";

struct ReadPause {
    entered: mpsc::Sender<()>,
    resume: mpsc::Receiver<()>,
}

#[derive(Default)]
struct PreviewReads {
    count: AtomicUsize,
    pause: Mutex<Option<ReadPause>>,
    closed: tokio::sync::Notify,
}

struct ObservePreviewReads(Arc<PreviewReads>);

impl<S> Layer<S> for ObservePreviewReads
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_enter(&self, id: &tracing::span::Id, ctx: Context<'_, S>) {
        let span = ctx.span(id).unwrap();
        if span.metadata().name() != "source.read"
            || !span
                .scope()
                .any(|ancestor| ancestor.metadata().name() == "mcp.graph.rename_preview.walk")
        {
            return;
        }
        self.0.count.fetch_add(1, Ordering::AcqRel);
        let pause = self.0.pause.lock().unwrap().take();
        if let Some(pause) = pause {
            pause.entered.send(()).unwrap();
            let resumed = pause.resume.recv_timeout(Duration::from_secs(30));
            assert!(!matches!(resumed, Err(mpsc::RecvTimeoutError::Timeout)));
        }
    }

    fn on_close(&self, id: tracing::span::Id, ctx: Context<'_, S>) {
        if ctx.span(&id).unwrap().metadata().name() == "mcp.graph.rename_preview.walk" {
            self.0.closed.notify_one();
        }
    }
}

impl PreviewReads {
    fn install() -> Arc<Self> {
        // An isolated child keeps RMCP and blocking workers on one registry.
        assert!(crate::common::in_child_test());
        let reads = Arc::new(Self::default());
        tracing::subscriber::set_global_default(
            tracing_subscriber::registry().with(ObservePreviewReads(Arc::clone(&reads))),
        )
        .unwrap();
        reads
    }

    fn pause_next(&self) -> (mpsc::Receiver<()>, mpsc::Sender<()>) {
        let (entered, entered_rx) = mpsc::channel();
        let (resume_tx, resume) = mpsc::channel();
        *self.pause.lock().unwrap() = Some(ReadPause { entered, resume });
        (entered_rx, resume_tx)
    }

    async fn wait_closed(&self) {
        tokio::time::timeout(Duration::from_secs(5), self.closed.notified())
            .await
            .expect("rename file worker must finish");
    }
}

async fn wait_for_preview_read(entered: mpsc::Receiver<()>) {
    tokio::task::spawn_blocking(move || entered.recv_timeout(Duration::from_secs(5)))
        .await
        .unwrap()
        .expect("actual rename source reader must start");
}

async fn opened_fixture() -> (ProductionCompositionFixture, Arc<McpServer>) {
    let fixture = production_composition_fixture_with_sources(|project| {
        fs::create_dir_all(project.join("src")).unwrap();
        fs::write(project.join("src/stock.rs"), STOCK_RS).unwrap();
        fs::write(project.join("src/lib.rs"), LIB_RS).unwrap();
    })
    .await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production MCP server");
    warm_code_index_search(&server, "reserve_stock").await;
    (fixture, server)
}

fn tool_json(response: &Value) -> Value {
    assert!(
        response.get("error").is_none(),
        "tools/call failed: {response}"
    );
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("rename preview text content: {response}"));
    serde_json::from_str(text).unwrap_or_else(|error| panic!("{error} in {text}"))
}

async fn symbol_id(server: &McpServer, name: &str) -> String {
    let response = handle_real_server_tool_call_raw(
        server,
        "tracedecay_find_exact_symbol",
        json!({ "name": name, "limit": 20, "format": "json" }),
    )
    .await;
    let payload = tool_json(&response);
    payload["matches"]
        .as_array()
        .and_then(|matches| {
            matches.iter().find_map(|item| {
                (item["name"] == name)
                    .then(|| item["id"].as_str().map(str::to_owned))
                    .flatten()
            })
        })
        .unwrap_or_else(|| panic!("exact symbol {name} missing: {payload}"))
}

fn expected_preview(reserve_id: &str, checkout_id: &str, new_name: Option<&str>) -> Value {
    json!({
        "freshness": {"state": "fresh"},
        "read_only": true,
        "note": PREVIEW_NOTE,
        "symbol": "reserve_stock",
        "new_name": new_name,
        "node": {
            "id": reserve_id,
            "name": "reserve_stock",
            "qualified_name": "src/stock.rs::reserve_stock",
            "kind": "function",
            "file": "src/stock.rs",
            "line": 1,
            "snippet": "pub fn reserve_stock(qty: u32) -> u32 {"
        },
        "reference_count": 1,
        "references": [{
            "from_node_id": checkout_id,
            "from_name": "checkout",
            "from_kind": "function",
            "edge_kind": "calls",
            "file": "src/lib.rs",
            "line": 8,
            "snippet": "reserve_stock(qty)"
        }],
        "text_only_matches": [{
            "file": "src/lib.rs",
            "text_only_count": 3,
            "note": TEXT_ONLY_NOTE
        }]
    })
}

fn assert_sources_unchanged(fixture: &ProductionCompositionFixture) {
    let stock = fs::read_to_string(fixture.project_root.join("src/stock.rs")).unwrap();
    let lib = fs::read_to_string(fixture.project_root.join("src/lib.rs")).unwrap();
    assert_eq!(stock, STOCK_RS);
    assert_eq!(lib, LIB_RS);
}

#[tokio::test]
async fn rename_preview_reports_the_declaration_the_caller_and_text_only_names() {
    let (fixture, server) = opened_fixture().await;
    let reserve_id = symbol_id(&server, "reserve_stock").await;
    let checkout_id = symbol_id(&server, "checkout").await;

    let omitted = handle_real_server_tool_call_raw(
        &server,
        "tracedecay_rename_preview",
        json!({ "node_id": reserve_id, "format": "json" }),
    )
    .await;
    assert!(omitted["result"].get("isError").is_none(), "{omitted}");
    assert_eq!(
        tool_json(&omitted),
        expected_preview(&reserve_id, &checkout_id, None)
    );

    let named = handle_real_server_tool_call_raw(
        &server,
        "tracedecay_rename_preview",
        json!({
            "node_id": reserve_id,
            "new_name": "hold_inventory",
            "format": "json"
        }),
    )
    .await;
    assert!(named["result"].get("isError").is_none(), "{named}");
    assert_eq!(
        tool_json(&named),
        expected_preview(&reserve_id, &checkout_id, Some("hold_inventory"))
    );
    assert_sources_unchanged(&fixture);
}

fn assert_invalid_request_refused(response: &Value, message: &str) {
    assert!(response.get("error").is_none(), "{response}");
    let problem = refusal_problem(&response["result"]);
    assert_eq!(problem["kind"], "invalid_request", "{response}");
    assert_eq!(
        problem["code"], "application.surface.invalid_request",
        "{response}"
    );
    assert_eq!(problem["message"], message, "{response}");
}

#[tokio::test]
async fn rename_preview_refuses_incomplete_utf16_reference_source() {
    let (fixture, server) = opened_fixture().await;
    let reserve_id = symbol_id(&server, "reserve_stock").await;
    let source_path = fixture.project_root.join("src/lib.rs");
    let mut bytes = vec![0xFF, 0xFE];
    bytes.extend(LIB_RS.encode_utf16().flat_map(u16::to_le_bytes));
    bytes.push(1);
    fs::write(&source_path, &bytes).unwrap();

    let response = handle_real_server_tool_call_raw(
        &server,
        "tracedecay_rename_preview",
        json!({ "node_id": reserve_id, "format": "json" }),
    )
    .await;
    assert!(response.get("error").is_none(), "{response}");
    let problem = refusal_problem(&response["result"]);
    assert_eq!(problem["kind"], "execution_failed", "{response}");
    assert_eq!(problem["code"], "graph_tool.failed", "{response}");
    assert!(
        problem["message"]
            .as_str()
            .unwrap()
            .contains("incomplete UTF-16 code unit"),
        "{response}"
    );
    assert_eq!(fs::read(&source_path).unwrap(), bytes);
}

#[tokio::test]
async fn rename_preview_refuses_unknown_and_unusable_node_identity() {
    let (fixture, server) = opened_fixture().await;

    let missing = handle_real_server_tool_call_raw(
        &server,
        "tracedecay_rename_preview",
        json!({ "node_id": "nonexistent_id_12345", "format": "json" }),
    )
    .await;
    assert!(missing.get("error").is_none(), "{missing}");
    assert_eq!(missing["result"]["isError"], json!(true));
    assert_eq!(
        tool_json(&missing),
        json!({
            "freshness": {"state": "fresh"},
            "status": "not_found",
            "reason_code": "node_not_found",
            "node_id": "nonexistent_id_12345",
            "message": "Node not found: nonexistent_id_12345"
        })
    );

    let omitted = handle_real_server_tool_call_raw(
        &server,
        "tracedecay_rename_preview",
        json!({ "format": "json" }),
    )
    .await;
    assert_invalid_request_refused(
        &omitted,
        "invalid arguments for tracedecay_rename_preview: missing field `node_id`",
    );

    let empty = handle_real_server_tool_call_raw(
        &server,
        "tracedecay_rename_preview",
        json!({ "node_id": "", "format": "json" }),
    )
    .await;
    assert_invalid_request_refused(&empty, "invalid parameter: node_id must not be empty");

    let apply_shaped = handle_real_server_tool_call_raw(
        &server,
        "tracedecay_rename_preview",
        json!({
            "node_id": "nonexistent_id_12345",
            "dry_run": false,
            "format": "json"
        }),
    )
    .await;
    assert_invalid_request_refused(
        &apply_shaped,
        "invalid arguments for tracedecay_rename_preview: unknown field `dry_run`, expected `node_id` or `new_name`",
    );
    assert_sources_unchanged(&fixture);
}

#[tokio::test]
async fn rename_preview_reads_dense_reference_sources_once_per_file() {
    if !crate::common::in_child_test() {
        crate::common::rerun_test_in_child(
            "mcp_handler_test::rename_preview_test::rename_preview_reads_dense_reference_sources_once_per_file",
            &[],
        );
        return;
    }
    let reads = PreviewReads::install();
    let mut sources = vec![("src/stock.rs", STOCK_RS.to_owned())];
    for (file, prefix) in [("src/lib.rs", "lib"), ("src/other.rs", "other")] {
        let mut source = "// é😀 padding\r\n".repeat(2_000);
        source.push_str("use crate::stock::reserve_stock;\r\n");
        source.push_str("// reserve_stock needs manual review\r\n");
        for i in 0..8 {
            source.push_str(&format!(
                "pub fn {prefix}_caller_{i}(qty: u32) -> u32 {{ reserve_stock(qty) }}\r\n"
            ));
        }
        sources.push((file, source));
    }
    let fixture = production_composition_fixture_with_sources(|project| {
        fs::create_dir_all(project.join("src")).unwrap();
        for (file, source) in &sources {
            fs::write(project.join(file), source).unwrap();
        }
    })
    .await;
    let server = fixture.harness.server(&fixture.project_root).unwrap();
    warm_code_index_search(&server, "reserve_stock").await;
    let reserve_id = symbol_id(&server, "reserve_stock").await;
    let response = handle_real_server_tool_call_raw(
        &server,
        "tracedecay_rename_preview",
        json!({ "node_id": reserve_id, "format": "json" }),
    )
    .await;
    let preview = tool_json(&response);
    reads.wait_closed().await;
    assert_eq!(reads.count.load(Ordering::Acquire), sources.len());
    assert_eq!(preview["reference_count"], 16, "{preview}");
    assert_eq!(
        preview["node"]["snippet"],
        "pub fn reserve_stock(qty: u32) -> u32 {"
    );
    for reference in preview["references"].as_array().unwrap() {
        let name = reference["from_name"].as_str().unwrap();
        let ordinal = name.rsplit('_').next().unwrap().parse::<u32>().unwrap();
        assert_eq!(reference["line"], 2_003 + ordinal, "{reference}");
        assert_eq!(
            reference["snippet"],
            format!("pub fn {name}(qty: u32) -> u32 {{ reserve_stock(qty) }}")
        );
    }
    let text_only = preview["text_only_matches"].as_array().unwrap();
    assert_eq!(text_only.len(), 2, "{preview}");
    assert!(
        text_only.iter().all(|file| file["text_only_count"] == 2),
        "{preview}"
    );
    for (file, source) in &sources {
        assert_eq!(
            fs::read_to_string(fixture.project_root.join(file)).unwrap(),
            *source
        );
    }
    fixture.shutdown().await;
}

#[tokio::test]
async fn rename_preview_keeps_raw_utf8_snippets_and_utf16_line_offsets() {
    let (fixture, server) = opened_fixture().await;
    let reserve_id = symbol_id(&server, "reserve_stock").await;
    let checkout_id = symbol_id(&server, "checkout").await;

    let encode_utf8_bom = |source: &str| {
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend(source.as_bytes());
        bytes
    };
    fs::write(
        fixture.project_root.join("src/stock.rs"),
        encode_utf8_bom(STOCK_RS),
    )
    .unwrap();
    fs::write(
        fixture.project_root.join("src/lib.rs"),
        encode_utf8_bom(LIB_RS),
    )
    .unwrap();
    let bom_response = handle_real_server_tool_call_raw(
        &server,
        "tracedecay_rename_preview",
        json!({"node_id":reserve_id, "format":"json"}),
    )
    .await;
    let mut bom_expected = expected_preview(&reserve_id, &checkout_id, None);
    bom_expected["node"]["snippet"] = json!("\u{feff}pub fn reserve_stock(qty: u32) -> u32 {");
    assert_eq!(tool_json(&bom_response), bom_expected);

    for (bom, encode_unit) in [
        (&[0xFF, 0xFE][..], u16::to_le_bytes as fn(u16) -> [u8; 2]),
        (&[0xFE, 0xFF][..], u16::to_be_bytes),
    ] {
        let encode = |source: &str| {
            let mut bytes = bom.to_vec();
            bytes.extend(source.encode_utf16().flat_map(encode_unit));
            bytes
        };
        fs::write(fixture.project_root.join("src/stock.rs"), encode(STOCK_RS)).unwrap();
        fs::write(fixture.project_root.join("src/lib.rs"), encode(LIB_RS)).unwrap();
        let response = handle_real_server_tool_call_raw(
            &server,
            "tracedecay_rename_preview",
            json!({"node_id":reserve_id, "format":"json"}),
        )
        .await;
        let mut expected = expected_preview(&reserve_id, &checkout_id, None);
        expected["node"]["snippet"] = Value::Null;
        expected["references"][0]["snippet"] = Value::Null;
        expected["text_only_matches"] = json!([]);
        assert_eq!(tool_json(&response), expected, "{bom:?}");
        assert_eq!(
            fs::read(fixture.project_root.join("src/lib.rs")).unwrap(),
            encode(LIB_RS)
        );
    }
    fixture.shutdown().await;
}

#[tokio::test]
async fn rename_preview_rereads_current_text_on_the_next_request() {
    let (fixture, server) = opened_fixture().await;
    let reserve_id = symbol_id(&server, "reserve_stock").await;
    let first = handle_real_server_tool_call_raw(
        &server,
        "tracedecay_rename_preview",
        json!({"node_id":reserve_id, "format":"json"}),
    )
    .await;
    assert_eq!(
        tool_json(&first)["text_only_matches"][0]["text_only_count"],
        3
    );
    let source_path = fixture.project_root.join("src/lib.rs");
    let edited = format!("{LIB_RS}// reserve_stock was edited after the previous preview\n");
    fs::write(&source_path, &edited).unwrap();
    let second = handle_real_server_tool_call_raw(
        &server,
        "tracedecay_rename_preview",
        json!({"node_id":reserve_id, "format":"json"}),
    )
    .await;
    let preview = tool_json(&second);
    assert_eq!(preview["reference_count"], 1, "{preview}");
    assert_eq!(preview["references"][0]["line"], 8);
    assert_eq!(
        preview["text_only_matches"][0]["text_only_count"], 4,
        "{preview}"
    );
    assert_eq!(fs::read_to_string(&source_path).unwrap(), edited);
    fixture.shutdown().await;
}

#[tokio::test]
async fn rename_preview_refuses_reference_offsets_past_current_source() {
    let (fixture, server) = opened_fixture().await;
    let reserve_id = symbol_id(&server, "reserve_stock").await;
    let source_path = fixture.project_root.join("src/lib.rs");
    fs::write(&source_path, "// short\n").unwrap();
    let response = handle_real_server_tool_call_raw(
        &server,
        "tracedecay_rename_preview",
        json!({"node_id":reserve_id, "format":"json"}),
    )
    .await;
    let problem = refusal_problem(&response["result"]);
    assert_eq!(problem["kind"], "unavailable", "{response}");
    assert_eq!(
        problem["code"], "verified-code-graph-symbol-corrupt",
        "{response}"
    );
    assert_eq!(problem["retry"], "never", "{response}");
    assert!(
        problem["message"]
            .as_str()
            .unwrap()
            .contains("graph evidence byte offset"),
        "{response}"
    );
    assert_eq!(fs::read_to_string(&source_path).unwrap(), "// short\n");
    fixture.shutdown().await;
}

#[tokio::test]
async fn rename_preview_keeps_a_missing_declaration_snippet_optional() {
    let (fixture, server) = opened_fixture().await;
    let reserve_id = symbol_id(&server, "reserve_stock").await;
    let checkout_id = symbol_id(&server, "checkout").await;
    let declaration = fixture.project_root.join("src/stock.rs");
    fs::remove_file(&declaration).unwrap();
    let response = handle_real_server_tool_call_raw(
        &server,
        "tracedecay_rename_preview",
        json!({"node_id":reserve_id, "format":"json"}),
    )
    .await;
    let mut expected = expected_preview(&reserve_id, &checkout_id, None);
    expected["node"]["snippet"] = Value::Null;
    assert_eq!(tool_json(&response), expected);
    assert!(!declaration.exists());
    assert_eq!(
        fs::read_to_string(fixture.project_root.join("src/lib.rs")).unwrap(),
        LIB_RS
    );
    fixture.shutdown().await;
}

#[tokio::test]
async fn rename_preview_deadline_stops_the_worker_before_the_next_file() {
    if !crate::common::in_child_test() {
        crate::common::rerun_test_in_child(
            "mcp_handler_test::rename_preview_test::rename_preview_deadline_stops_the_worker_before_the_next_file",
            &[],
        );
        return;
    }
    let reads = PreviewReads::install();
    let (fixture, server) = opened_fixture().await;
    let reserve_id = symbol_id(&server, "reserve_stock").await;
    let (entered, resume) = reads.pause_next();
    let deadline =
        tracedecay_domain::UtcMicros(tracedecay_contracts::clock::now_micros().0 + 1_000_000);
    let response = {
        let pending = fixture.harness.call_tool_with_deadline(
            &fixture.project_root,
            "tracedecay_rename_preview",
            json!({"node_id":reserve_id, "format":"json"}),
            deadline,
        );
        tokio::pin!(pending);
        tokio::select! {
            () = wait_for_preview_read(entered) => {}
            early = &mut pending => panic!("deadline request settled before source read: {early:?}"),
        }
        tokio::time::timeout(Duration::from_secs(5), pending)
            .await
            .unwrap()
            .unwrap()
    };
    let response = serde_json::to_value(response).unwrap();
    assert_eq!(
        response["error"]["data"]["reason_code"], "tool_dispatch_deadline_exceeded",
        "{response}"
    );
    resume.send(()).unwrap();
    reads.wait_closed().await;
    assert_eq!(reads.count.load(Ordering::Acquire), 1);
    assert_sources_unchanged(&fixture);
    fixture.shutdown().await;
}

#[tokio::test]
async fn rename_preview_transport_cancel_stops_the_worker_before_the_next_file() {
    if !crate::common::in_child_test() {
        crate::common::rerun_test_in_child(
            "mcp_handler_test::rename_preview_test::rename_preview_transport_cancel_stops_the_worker_before_the_next_file",
            &[],
        );
        return;
    }
    let reads = PreviewReads::install();
    let (fixture, server) = opened_fixture().await;
    let reserve_id = symbol_id(&server, "reserve_stock").await;
    let (entered, resume) = reads.pause_next();
    let (mut transport, sender, mut responses) = tracedecay_mcp::transport::ChannelTransport::new();
    let serving = tokio::spawn(async move { server.run_connection(&mut transport).await });
    sender.send(json!({
        "jsonrpc":"2.0", "id":41, "method":"tools/call",
        "params":{"name":"tracedecay_rename_preview", "arguments":{"node_id":reserve_id,"format":"json"}}
    }).to_string()).unwrap();
    wait_for_preview_read(entered).await;
    sender
        .send(
            json!({
                "jsonrpc":"2.0", "method":"notifications/cancelled",
                "params":{"requestId":41,"reason":"stop before the next file"}
            })
            .to_string(),
        )
        .unwrap();
    sender
        .send(json!({"jsonrpc":"2.0","id":42,"method":"ping"}).to_string())
        .unwrap();
    let ping = tokio::time::timeout(Duration::from_secs(5), responses.recv())
        .await
        .unwrap()
        .unwrap();
    let ping: Value = serde_json::from_str(&ping).unwrap();
    assert_eq!(ping["id"], 42, "{ping}");
    resume.send(()).unwrap();
    let response = tokio::time::timeout(Duration::from_secs(5), responses.recv())
        .await
        .unwrap()
        .unwrap();
    let response: Value = serde_json::from_str(&response).unwrap();
    assert_eq!(response["id"], 41, "{response}");
    assert_eq!(response["error"]["code"], -32800, "{response}");
    assert_eq!(
        response["error"]["data"]["reason_code"], "request_cancelled",
        "{response}"
    );
    reads.wait_closed().await;
    assert_eq!(reads.count.load(Ordering::Acquire), 1);
    assert_sources_unchanged(&fixture);
    drop(sender);
    tokio::time::timeout(Duration::from_secs(5), serving)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    fixture.shutdown().await;
}
