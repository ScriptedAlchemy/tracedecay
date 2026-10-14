//! The result shape one `tracedecay serve` session answers with follows the
//! MCP revision the host negotiated (#3818).
//!
//! From `2026-07-28` every result carries `resultType` (SEP-2322), and each
//! cacheable result also carries `ttlMs` and `cacheScope` (SEP-2549). Hosts
//! that negotiated an older revision keep the wire shape that predates those
//! fields.

use serde_json::{Value, json};
use tempfile::TempDir;

use crate::common::{self, tracedecay_command_with_home};
use crate::serve_harness::{ServeSession, init_project_with_file};

const REVISION_2026_07_28: &str = "2026-07-28";
const REVISION_FIELDS: [&str; 3] = ["resultType", "ttlMs", "cacheScope"];

/// Every request a host on `2026-07-28` can send this server, with whether
/// the revision makes its result cacheable.
fn requests() -> [(&'static str, Value, bool); 6] {
    [
        ("tools/list", json!({}), true),
        ("resources/list", json!({}), true),
        ("resources/templates/list", json!({}), true),
        ("prompts/list", json!({}), true),
        (
            "resources/read",
            json!({ "uri": "tracedecay://schema" }),
            true,
        ),
        (
            "tools/call",
            json!({ "name": "tracedecay_status", "arguments": { "format": "json" } }),
            false,
        ),
    ]
}

fn serve(home: &TempDir, project: &TempDir) -> ServeSession {
    let mut command = tracedecay_command_with_home(home.path());
    command
        .arg("serve")
        .arg("--path")
        .arg(project.path())
        .current_dir(project.path());
    ServeSession::start(command)
}

/// Sends each of [`requests`] with `meta` as its `_meta` and returns every
/// result, failing on any JSON-RPC error.
fn results(session: &mut ServeSession, meta: &Value) -> Vec<(&'static str, bool, Value)> {
    requests()
        .into_iter()
        .zip(10..)
        .map(|((method, mut params, cacheable), id)| {
            if !meta.is_null() {
                params["_meta"] = meta.clone();
            }
            let answer = session.request(json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": method,
                "params": params,
            }));
            let result = answer
                .get("result")
                .cloned()
                .unwrap_or_else(|| panic!("{method} must succeed: {answer}"));
            (method, cacheable, result)
        })
        .collect()
}

#[tokio::test]
async fn results_carry_the_fields_the_negotiated_revision_requires() {
    let home = TempDir::new().unwrap();
    let project = init_project_with_file(home.path(), "pub fn revision_marker() {}\n").await;
    let _daemon = common::spawn_tracedecay_daemon(home.path());

    let mut current = serve(&home, &project);
    let discover = current.request(json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "server/discover",
        "params": {}
    }));
    assert!(
        discover["result"]["supportedVersions"]
            .as_array()
            .is_some_and(|versions| versions.contains(&json!(REVISION_2026_07_28))),
        "serve must advertise the revision this test negotiates: {discover}"
    );
    current.await_project_ready();
    let meta = json!({ "io.modelcontextprotocol/protocolVersion": REVISION_2026_07_28 });
    for (method, cacheable, result) in results(&mut current, &meta) {
        assert_eq!(
            result["resultType"], "complete",
            "{method} on {REVISION_2026_07_28} must carry resultType: {result}"
        );
        if cacheable {
            assert_eq!(
                (&result["ttlMs"], &result["cacheScope"]),
                (&json!(0), &json!("private")),
                "{method} on {REVISION_2026_07_28} must carry its cache hints: {result}"
            );
        } else {
            assert!(
                result.get("ttlMs").is_none() && result.get("cacheScope").is_none(),
                "{method} is not cacheable: {result}"
            );
        }
    }
    drop(current);

    for legacy in ["2025-11-25", "2025-06-18", "2024-11-05"] {
        let mut session = serve(&home, &project);
        let initialized = session.request(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": legacy,
                "capabilities": {},
                "clientInfo": { "name": "revision-test", "version": "1" }
            }
        }));
        assert_eq!(
            initialized["result"]["protocolVersion"], legacy,
            "{initialized}"
        );
        session.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
        for (method, _, result) in results(&mut session, &Value::Null) {
            for field in REVISION_FIELDS {
                assert!(
                    result.get(field).is_none(),
                    "{method} on {legacy} must keep the wire shape without {field}: {result}"
                );
            }
        }
    }
}
