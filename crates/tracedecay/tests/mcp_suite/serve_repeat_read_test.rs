//! Repeated identical reads in one `tracedecay serve` session: a pointer for
//! an unchanged answer, the full new content after an edit, and full answers
//! when the host has not marked the connection as one agent's.

use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tempfile::TempDir;

use crate::common;
use crate::serve_harness::{ServeSession, init_project_with_file, profile_root, serve_command};

const POINTER: &str = "tracedecay_repeat_read:";
const REPEAT_READ_POINTERS_ENV: &str = "TRACEDECAY_MCP_REPEAT_READ_POINTERS";
const QUERY_HINT: &str = "tracedecay_repeated_query:";
const QUERY_HINT_SETTING: &str =
    tracedecay_domain::configuration::USER_REPEATED_QUERY_HINTS_SETTING_KEY;

/// One function whose body alone is well over the pointer threshold, beside
/// enough small functions that the file outline is too.
fn source(marker: &str) -> String {
    let body = (0..60)
        .map(|index| format!("    let value_{index} = {index};\n"))
        .collect::<String>();
    let helpers = (0..6)
        .map(|index| format!("pub fn repeat_read_helper_{index}() {{}}\n"))
        .collect::<String>();
    format!("pub fn {marker}() -> usize {{\n{body}    0\n}}\n{helpers}")
}

fn call(id: i64, name: &str, arguments: &Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": { "name": name, "arguments": arguments }
    })
}

fn text(response: &Value) -> String {
    let content = response["result"]["content"]
        .as_array()
        .unwrap_or_else(|| panic!("tools/call carried no content: {response}"));
    content
        .iter()
        .filter_map(|block| block["text"].as_str())
        .collect()
}

fn is_pointer(response: &Value) -> bool {
    response["result"]["content"][0]["text"]
        .as_str()
        .is_some_and(|text| text.starts_with(POINTER))
}

fn json_body(response: &Value) -> Value {
    response["result"]["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|block| block["text"].as_str())
        .find_map(|text| serde_json::from_str::<Value>(text).ok())
        .unwrap_or_else(|| panic!("no JSON body in {response}"))
}

fn start_client(
    home: &std::path::Path,
    project: &std::path::Path,
    env: &[(&str, &str)],
) -> ServeSession {
    let mut command = serve_command(home, project, Some(project.as_os_str()));
    command.envs(env.iter().copied());
    ServeSession::start(command)
}

fn start(client: &mut ServeSession) {
    let initialize = client.request(json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "repeat-read-test", "version": "1" }
        }
    }));
    assert!(initialize.get("error").is_none(), "{initialize}");
    client.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
}

fn wait_ready(client: &mut ServeSession, id: i64) {
    let status = client.request(call(
        id,
        "tracedecay_status",
        &json!({
            "format": "json",
            "wait_for": { "state": "ready", "timeout_ms": 110_000 },
        }),
    ));
    let status = json_body(&status);
    assert_eq!(
        status["wait"],
        json!({ "outcome": "reached" }),
        "code index never became ready: {status}"
    );
}

fn exact_symbol(client: &mut ServeSession, id: i64, name: &str) -> Value {
    json_body(&client.request(call(
        id,
        "tracedecay_find_exact_symbol",
        &json!({ "name": name, "format": "json" }),
    )))
}

/// Polls an exact-symbol read, a different call from any the test repeats,
/// until the daemon serves `name` from a raw worktree write.
fn wait_until_indexed(client: &mut ServeSession, name: &str) {
    let deadline = Instant::now() + Duration::from_secs(90);
    for id in 1_000.. {
        let found = exact_symbol(client, id, name);
        if found["matches"]
            .as_array()
            .is_some_and(|matches| !matches.is_empty())
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the daemon never indexed {name}: {found}"
        );
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn provenance(response: &Value) -> String {
    text(response)
        .lines()
        .find(|line| line.contains("Provenance:"))
        .unwrap_or_else(|| panic!("source_body must render provenance: {response}"))
        .to_owned()
}

fn configuration_payload(response: &Value, outcome: &str) -> Value {
    let envelope = json_body(response);
    assert_eq!(envelope["outcome"]["outcome"], outcome, "{response}");
    envelope
        .pointer("/outcome/value/payload")
        .unwrap_or_else(|| panic!("configuration omitted its payload: {response}"))
        .clone()
}

fn set_query_hints(client: &mut ServeSession, home: &std::path::Path, enabled: bool) {
    let current = client.request(call(
        100,
        "tracedecay_configuration_get",
        &json!({ "key": QUERY_HINT_SETTING, "format": "json" }),
    ));
    let current = configuration_payload(&current, "evidence");
    assert_eq!(
        current["effective_value"],
        json!({ "kind": "boolean", "value": !enabled }),
        "{current}"
    );
    let profile = tracedecay_daemon_identity::profile_identity::load_existing(&profile_root(home))
        .expect("the isolated daemon established its profile identity");
    let changed = client.request(call(
        101,
        "tracedecay_configuration_set",
        &json!({
            "layer": { "kind": "user_profile", "profile_id": profile.profile_id() },
            "key": QUERY_HINT_SETTING,
            "format": "json",
            "value": { "kind": "boolean", "value": enabled },
            "expected_revision": current["revision_id"],
            "idempotency_key": format!("configuration.idempotency.query-hints.{enabled}"),
        }),
    ));
    configuration_payload(&changed, "effect");
}

#[tokio::test]
async fn serve_query_hints_follow_profile_configuration_on_new_connections() {
    let home = TempDir::new().unwrap();
    let project = init_project_with_file(home.path(), &source("query_hint_marker")).await;
    let _daemon = common::spawn_tracedecay_daemon(home.path());
    let mut original = start_client(home.path(), project.path(), &[]);
    start(&mut original);
    wait_ready(&mut original, 2);
    let outline = json!({ "file": "src/lib.rs" });
    for id in [10, 11] {
        let response = original.request(call(id, "tracedecay_source_outline", &outline));
        assert!(text(&response).contains("query_hint_marker"), "{response}");
        assert!(!text(&response).contains(QUERY_HINT), "{response}");
    }

    set_query_hints(&mut original, home.path(), true);
    let unchanged = original.request(call(12, "tracedecay_source_outline", &outline));
    assert!(!text(&unchanged).contains(QUERY_HINT), "{unchanged}");

    let mut enabled = start_client(home.path(), project.path(), &[]);
    start(&mut enabled);
    wait_ready(&mut enabled, 2);
    let first = enabled.request(call(10, "tracedecay_source_outline", &outline));
    assert!(!text(&first).contains(QUERY_HINT), "{first}");
    // MCP startup does not wait for the optional profile read. Observe the
    // setting taking effect under its existing configuration-read deadline.
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut id = 11;
    let repeated = loop {
        let response = enabled.request(call(id, "tracedecay_source_outline", &outline));
        if text(&response).contains(QUERY_HINT) {
            break response;
        }
        assert!(
            Instant::now() < deadline,
            "hint setting never reached serve: {response}"
        );
        id += 1;
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(text(&repeated).contains("query_hint_marker"), "{repeated}");
    assert_eq!(text(&repeated).matches(QUERY_HINT).count(), 1, "{repeated}");
    assert!(!is_pointer(&repeated), "{repeated}");

    set_query_hints(&mut enabled, home.path(), false);
    let mut disabled = start_client(home.path(), project.path(), &[]);
    start(&mut disabled);
    wait_ready(&mut disabled, 2);
    for id in [10, 11] {
        let response = disabled.request(call(id, "tracedecay_source_outline", &outline));
        assert!(text(&response).contains("query_hint_marker"), "{response}");
        assert!(!text(&response).contains(QUERY_HINT), "{response}");
    }
}

/// A session with query hints on, opened after the profile setting turned
/// them on and once it has observed the setting.
fn hinted_client(
    home: &std::path::Path,
    project: &std::path::Path,
    env: &[(&str, &str)],
    marker: &str,
) -> ServeSession {
    let mut setup = start_client(home, project, env);
    start(&mut setup);
    wait_ready(&mut setup, 2);
    set_query_hints(&mut setup, home, true);

    let mut client = start_client(home, project, env);
    start(&mut client);
    wait_ready(&mut client, 2);
    // Exact-symbol calls are not repeatable reads, so they observe the hint
    // setting reaching serve without seeding the pointer ledger.
    let deadline = Instant::now() + Duration::from_secs(15);
    let probe = json!({ "name": marker, "format": "json" });
    for id in 3.. {
        let response = client.request(call(id, "tracedecay_find_exact_symbol", &probe));
        if text(&response).contains(QUERY_HINT) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "hint setting never reached serve: {response}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    client
}

#[tokio::test]
async fn serve_pointer_and_query_hint_name_an_identical_repeat_once() {
    let home = TempDir::new().unwrap();
    let project = init_project_with_file(home.path(), &source("pointer_hint_marker")).await;
    let _daemon = common::spawn_tracedecay_daemon(home.path());
    let pointers = [(REPEAT_READ_POINTERS_ENV, "1")];
    let mut client = hinted_client(
        home.path(),
        project.path(),
        &pointers,
        "pointer_hint_marker",
    );

    let outline = json!({ "file": "src/lib.rs" });
    let first = client.request(call(100, "tracedecay_source_outline", &outline));
    assert!(
        !is_pointer(&first) && text(&first).contains("pointer_hint_marker"),
        "{first}"
    );
    let identical = client.request(call(101, "tracedecay_source_outline", &outline));
    assert!(is_pointer(&identical), "{identical}");
    assert_eq!(
        text(&identical)
            .matches("tracedecay_source_outline")
            .count(),
        1,
        "only the pointer names the identical earlier call: {identical}"
    );
    assert!(!text(&identical).contains(QUERY_HINT), "{identical}");

    let after_pointer = client.request(call(102, "tracedecay_source_outline", &outline));
    assert!(!is_pointer(&after_pointer), "{after_pointer}");
    assert_eq!(
        text(&after_pointer).matches(QUERY_HINT).count(),
        1,
        "the full answer after a pointer is hinted once: {after_pointer}"
    );
    assert!(
        text(&after_pointer).contains("tracedecay_source_outline call 2 calls earlier"),
        "the hint names the earlier full answer, not the pointer: {after_pointer}"
    );
}

#[tokio::test]
async fn serve_points_repeat_reads_at_the_earlier_result_until_content_changes() {
    let home = TempDir::new().unwrap();
    let project = init_project_with_file(home.path(), &source("repeat_read_marker")).await;
    let _daemon = common::spawn_tracedecay_daemon(home.path());
    let mut client = start_client(
        home.path(),
        project.path(),
        &[(REPEAT_READ_POINTERS_ENV, "1")],
    );
    start(&mut client);
    wait_ready(&mut client, 2);

    let lookup = exact_symbol(&mut client, 3, "repeat_read_marker");
    let node_id = lookup["matches"][0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("exact lookup must return a node id: {lookup}"))
        .to_owned();
    let body = json!({ "node_id": node_id });

    let first = client.request(call(10, "tracedecay_source_body", &body));
    assert!(
        !is_pointer(&first) && text(&first).contains("let value_59 = 59;"),
        "the first read is the full body: {first}"
    );
    let second = client.request(call(11, "tracedecay_source_body", &body));
    assert!(
        is_pointer(&second),
        "an identical unchanged read must answer with a pointer: {second}"
    );
    assert!(
        text(&second).contains("tracedecay_source_body"),
        "the pointer names the tool: {second}"
    );
    let third = client.request(call(12, "tracedecay_source_body", &body));
    assert!(
        !is_pointer(&third) && text(&third).contains("let value_59 = 59;"),
        "the call after a pointer must serve the full body again: {third}"
    );
    assert_ne!(
        provenance(&first),
        provenance(&third),
        "identical answers still differ in per-call provenance, so the pointer must not \
         depend on rendered text"
    );

    let outline = json!({ "file": "src/lib.rs" });
    let before_edit = client.request(call(20, "tracedecay_source_outline", &outline));
    assert!(
        !is_pointer(&before_edit) && text(&before_edit).contains("repeat_read_marker"),
        "{before_edit}"
    );
    std::fs::write(
        project.path().join("src/lib.rs"),
        source("repeat_read_edited"),
    )
    .unwrap();
    wait_until_indexed(&mut client, "repeat_read_edited");
    let after_edit = client.request(call(22, "tracedecay_source_outline", &outline));
    assert!(
        !is_pointer(&after_edit),
        "a read after an edit must never point at the stale answer: {after_edit}"
    );
    assert!(
        text(&after_edit).contains("repeat_read_edited")
            && !text(&after_edit).contains("repeat_read_marker"),
        "the read after an edit must carry the new content: {after_edit}"
    );
    let repeat = client.request(call(23, "tracedecay_source_outline", &outline));
    assert!(
        is_pointer(&repeat),
        "the unchanged repeat after the edit must answer with a pointer: {repeat}"
    );
}

#[tokio::test]
async fn serve_without_the_pointer_env_answers_every_repeat_in_full() {
    let home = TempDir::new().unwrap();
    let project = init_project_with_file(home.path(), &source("repeat_read_default")).await;
    let _daemon = common::spawn_tracedecay_daemon(home.path());
    let mut client = start_client(home.path(), project.path(), &[]);
    start(&mut client);
    wait_ready(&mut client, 2);
    let outline = json!({ "file": "src/lib.rs" });
    let first = client.request(call(3, "tracedecay_source_outline", &outline));
    let repeat = client.request(call(4, "tracedecay_source_outline", &outline));
    assert!(
        text(&first).contains("repeat_read_default"),
        "the outline must answer: {first}"
    );
    assert!(
        !is_pointer(&repeat),
        "a host that did not mark its connection as one agent's must always get full answers, \
         since it may share the connection across agents: {repeat}"
    );
}

#[tokio::test]
async fn serve_points_and_hints_identical_json_reads() {
    let home = TempDir::new().unwrap();
    let project = init_project_with_file(home.path(), &source("json_repeat_marker")).await;
    let _daemon = common::spawn_tracedecay_daemon(home.path());
    let pointers = [(REPEAT_READ_POINTERS_ENV, "1")];
    let mut client = hinted_client(home.path(), project.path(), &pointers, "json_repeat_marker");

    // The explorer's search over its serve session.
    let search = json!({
        "query": "json_repeat_marker",
        "limit": 20,
        "prefer_symbol": true,
        "format": "json",
    });
    let searched = client.request(call(90, "tracedecay_search", &search));
    assert!(!text(&searched).contains(QUERY_HINT), "{searched}");
    let searched_again = client.request(call(91, "tracedecay_search", &search));
    assert!(
        text(&searched_again).contains("tracedecay_search call 1 call earlier"),
        "an identical JSON search with the same answer is hinted: first {searched}\n\
         again {searched_again}"
    );

    let outline = json!({ "file": "src/lib.rs", "format": "json" });
    let first = client.request(call(100, "tracedecay_source_outline", &outline));
    assert!(
        !is_pointer(&first) && json_body(&first).to_string().contains("json_repeat_marker"),
        "{first}"
    );
    let identical = client.request(call(101, "tracedecay_source_outline", &outline));
    assert!(
        is_pointer(&identical),
        "an identical JSON read must answer with a pointer although its request id and \
         timestamps differ: first {first}\nidentical {identical}"
    );
    let after_pointer = client.request(call(102, "tracedecay_source_outline", &outline));
    assert!(!is_pointer(&after_pointer), "{after_pointer}");
    assert!(
        text(&after_pointer).contains("tracedecay_source_outline call 2 calls earlier"),
        "the full JSON answer after a pointer is hinted about the earlier one: {after_pointer}"
    );
}

#[tokio::test]
async fn serve_does_not_hint_a_call_correcting_a_refused_request() {
    let home = TempDir::new().unwrap();
    let project = init_project_with_file(home.path(), &source("refused_call_marker")).await;
    let _daemon = common::spawn_tracedecay_daemon(home.path());
    let mut client = hinted_client(home.path(), project.path(), &[], "refused_call_marker");

    let misnamed = client.request(call(
        100,
        "tracedecay_source_outline",
        &json!({ "path": "src/lib.rs", "format": "json" }),
    ));
    assert!(
        text(&misnamed).contains("invalid_request"),
        "a misnamed parameter is refused as an invalid request: {misnamed}"
    );
    let corrected = client.request(call(
        101,
        "tracedecay_source_outline",
        &json!({ "file": "src/lib.rs", "format": "json" }),
    ));
    assert!(
        text(&corrected).contains("refused_call_marker"),
        "{corrected}"
    );
    assert!(
        !text(&corrected).contains(QUERY_HINT),
        "a call correcting a refused request does not repeat it: refused {misnamed}\n\
         corrected {corrected}"
    );
}
