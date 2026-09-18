#![cfg(feature = "test-transport")]

use std::fs;
use std::path::Path;

use serde_json::{Value, json};
use tracedecay::daemon::ProductionProjectCompositionHarnessV1;
use tracedecay_mcp::jsonrpc::JsonRpcResponse;

use crate::support::{
    extract_text, production_composition_fixture_with_sources, wait_for_current_graph,
};

/// `src/lib.rs` lines (1-indexed):
/// 2 TODO, 3 FIXME inside `outer`; 8 looks like a marker but is not a word;
/// 12 HACK sits outside every symbol; 19 NOTE sits inside `Widget::draw`.
const LIB_RS: &str = "\
pub fn outer() {
    // TODO: paint the label
    // FIXME: tighten the span
    let _ = 0;
}

pub fn helper() {
    // not a marker: rendered todoist and TODOs list
    let _ = 0;
}

// HACK: module scratch
pub struct Widget {
    value: u32,
}

impl Widget {
    pub fn draw(&self) -> u32 {
        // NOTE: keep the method
        self.value
    }
}
";

/// `src/nested/extra.rs` lines (1-indexed):
/// 1 the identifier `note` is a non-comment NOTE (any line, word boundary);
/// 2 XXX, 3 lower-case todo, 4 TODO (same line also says FIXME; the first
/// requested kind wins, so an unfiltered scan keeps TODO and a FIXME-only
/// scan keeps FIXME);
/// 8 WIP and 9 UNIMPLEMENTED sit outside `note`.
const EXTRA_RS: &str = "\
pub fn note() {
    // XXX: drop this
    // todo: lower case still counts
    // TODO: first and FIXME: second
    let _ = 1;
}

// WIP: unfinished module
// UNIMPLEMENTED: leave a hole
";

fn write_marker_project(project: &Path) {
    fs::create_dir_all(project.join("src/nested")).unwrap();
    fs::write(project.join("src/lib.rs"), LIB_RS).unwrap();
    fs::write(project.join("src/nested/extra.rs"), EXTRA_RS).unwrap();
}

fn marker(kind: &str, file: &str, line: u32, text: &str, enclosing: Option<&str>) -> Value {
    json!({
        "kind": kind,
        "file": file,
        "line": line,
        "text": text,
        "enclosing": enclosing,
    })
}

fn scan(by_kind: Value, markers: Vec<Value>) -> Value {
    json!({
        "match_count": markers.len(),
        "by_kind": by_kind,
        "markers": markers,
    })
}

fn lib_markers() -> Vec<Value> {
    vec![
        marker(
            "TODO",
            "src/lib.rs",
            2,
            "// TODO: paint the label",
            Some("src/lib.rs::outer"),
        ),
        marker(
            "FIXME",
            "src/lib.rs",
            3,
            "// FIXME: tighten the span",
            Some("src/lib.rs::outer"),
        ),
        marker("HACK", "src/lib.rs", 12, "// HACK: module scratch", None),
        marker(
            "NOTE",
            "src/lib.rs",
            19,
            "// NOTE: keep the method",
            Some("src/lib.rs::Widget::draw"),
        ),
    ]
}

fn extra_markers() -> Vec<Value> {
    vec![
        marker(
            "NOTE",
            "src/nested/extra.rs",
            1,
            "pub fn note() {",
            Some("src/nested/extra.rs::note"),
        ),
        marker(
            "XXX",
            "src/nested/extra.rs",
            2,
            "// XXX: drop this",
            Some("src/nested/extra.rs::note"),
        ),
        marker(
            "TODO",
            "src/nested/extra.rs",
            3,
            "// todo: lower case still counts",
            Some("src/nested/extra.rs::note"),
        ),
        marker(
            "TODO",
            "src/nested/extra.rs",
            4,
            "// TODO: first and FIXME: second",
            Some("src/nested/extra.rs::note"),
        ),
        marker(
            "WIP",
            "src/nested/extra.rs",
            8,
            "// WIP: unfinished module",
            None,
        ),
        marker(
            "UNIMPLEMENTED",
            "src/nested/extra.rs",
            9,
            "// UNIMPLEMENTED: leave a hole",
            None,
        ),
    ]
}

fn all_markers() -> Vec<Value> {
    let mut markers = lib_markers();
    markers.extend(extra_markers());
    markers
}

fn default_scan() -> Value {
    scan(
        json!({
            "FIXME": 1,
            "HACK": 1,
            "NOTE": 2,
            "TODO": 3,
            "UNIMPLEMENTED": 1,
            "WIP": 1,
            "XXX": 1,
        }),
        all_markers(),
    )
}

const DEFAULT_MARKDOWN: &str = "\
**match_count:** 10

## by_kind
**FIXME:** 1
**HACK:** 1
**NOTE:** 2
**TODO:** 3
**UNIMPLEMENTED:** 1
**WIP:** 1
**XXX:** 1

## markers
- **src/lib.rs**
  **kind:** TODO
  **line:** 2
  **enclosing:** src/lib.rs::outer
  **text:** // TODO: paint the label
- **src/lib.rs**
  **kind:** FIXME
  **line:** 3
  **enclosing:** src/lib.rs::outer
  **text:** // FIXME: tighten the span
- **src/lib.rs**
  **kind:** HACK
  **line:** 12
  **text:** // HACK: module scratch
- **src/lib.rs**
  **kind:** NOTE
  **line:** 19
  **enclosing:** src/lib.rs::Widget::draw
  **text:** // NOTE: keep the method
- **src/nested/extra.rs**
  **kind:** NOTE
  **line:** 1
  **enclosing:** src/nested/extra.rs::note
  **text:** pub fn note() {
- **src/nested/extra.rs**
  **kind:** XXX
  **line:** 2
  **enclosing:** src/nested/extra.rs::note
  **text:** // XXX: drop this
- **src/nested/extra.rs**
  **kind:** TODO
  **line:** 3
  **enclosing:** src/nested/extra.rs::note
  **text:** // todo: lower case still counts
- **src/nested/extra.rs**
  **kind:** TODO
  **line:** 4
  **enclosing:** src/nested/extra.rs::note
  **text:** // TODO: first and FIXME: second
- **src/nested/extra.rs**
  **kind:** WIP
  **line:** 8
  **text:** // WIP: unfinished module
- **src/nested/extra.rs**
  **kind:** UNIMPLEMENTED
  **line:** 9
  **text:** // UNIMPLEMENTED: leave a hole
";

async fn call_todos(
    harness: &ProductionProjectCompositionHarnessV1,
    project_root: &Path,
    arguments: Value,
) -> JsonRpcResponse {
    harness
        .call_tool(project_root, "tracedecay_todos", arguments)
        .await
        .expect("production MCP tools/call")
}

fn json_payload(response: &JsonRpcResponse) -> Value {
    assert!(
        response.error.is_none(),
        "tracedecay_todos returned an MCP error: {:?}",
        response.error
    );
    let text = extract_text(response.result.as_ref().expect("MCP result"));
    serde_json::from_str(text).unwrap_or_else(|error| panic!("todos JSON: {error}\n{text}"))
}

#[tokio::test]
async fn todos_reports_observed_marker_behavior() {
    let fixture = production_composition_fixture_with_sources(write_marker_project).await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production MCP server");
    wait_for_current_graph(&server).await;

    let default_args = json!({"format": "json"});
    let observed = json_payload(
        &call_todos(
            &fixture.harness,
            &fixture.project_root,
            default_args.clone(),
        )
        .await,
    );
    assert_eq!(observed, default_scan());

    let empty_kinds = json_payload(
        &call_todos(
            &fixture.harness,
            &fixture.project_root,
            json!({"format": "json", "kinds": []}),
        )
        .await,
    );
    assert_eq!(empty_kinds, default_scan());

    let fixme_only = json_payload(
        &call_todos(
            &fixture.harness,
            &fixture.project_root,
            json!({"format": "json", "kinds": ["fixme"]}),
        )
        .await,
    );
    assert_eq!(
        fixme_only,
        scan(
            json!({"FIXME": 2}),
            vec![
                marker(
                    "FIXME",
                    "src/lib.rs",
                    3,
                    "// FIXME: tighten the span",
                    Some("src/lib.rs::outer"),
                ),
                marker(
                    "FIXME",
                    "src/nested/extra.rs",
                    4,
                    "// TODO: first and FIXME: second",
                    Some("src/nested/extra.rs::note"),
                ),
            ],
        )
    );

    let hack_and_wip = json_payload(
        &call_todos(
            &fixture.harness,
            &fixture.project_root,
            json!({"format": "json", "kinds": ["wip", "hack"]}),
        )
        .await,
    );
    assert_eq!(
        hack_and_wip,
        scan(
            json!({"HACK": 1, "WIP": 1}),
            vec![
                marker("HACK", "src/lib.rs", 12, "// HACK: module scratch", None),
                marker(
                    "WIP",
                    "src/nested/extra.rs",
                    8,
                    "// WIP: unfinished module",
                    None,
                ),
            ],
        )
    );

    let nested = json_payload(
        &call_todos(
            &fixture.harness,
            &fixture.project_root,
            json!({"format": "json", "path": "src/nested"}),
        )
        .await,
    );
    let missing = json_payload(
        &call_todos(
            &fixture.harness,
            &fixture.project_root,
            json!({"format": "json", "path": "src/lib"}),
        )
        .await,
    );
    assert_eq!(
        nested,
        scan(
            json!({
                "NOTE": 1,
                "TODO": 2,
                "UNIMPLEMENTED": 1,
                "WIP": 1,
                "XXX": 1,
            }),
            extra_markers(),
        )
    );
    assert_eq!(missing, scan(json!({}), Vec::new()));

    let exact_file = json_payload(
        &call_todos(
            &fixture.harness,
            &fixture.project_root,
            json!({"format": "json", "path": "src/lib.rs"}),
        )
        .await,
    );
    assert_eq!(
        exact_file,
        scan(
            json!({"FIXME": 1, "HACK": 1, "NOTE": 1, "TODO": 1}),
            lib_markers(),
        )
    );

    let limited = json_payload(
        &call_todos(
            &fixture.harness,
            &fixture.project_root,
            json!({"format": "json", "limit": 1}),
        )
        .await,
    );
    assert_eq!(
        limited,
        scan(
            json!({"TODO": 1}),
            vec![marker(
                "TODO",
                "src/lib.rs",
                2,
                "// TODO: paint the label",
                Some("src/lib.rs::outer"),
            )],
        )
    );

    let markdown = call_todos(&fixture.harness, &fixture.project_root, json!({})).await;
    assert!(markdown.error.is_none(), "{:?}", markdown.error);
    assert_eq!(
        extract_text(markdown.result.as_ref().expect("markdown result")),
        DEFAULT_MARKDOWN
    );

    let denied = call_todos(
        &fixture.harness,
        &fixture.project_root,
        json!({"not_a_field": true}),
    )
    .await;
    let error = denied.error.expect("unknown field must be an MCP error");
    assert!(denied.result.is_none(), "{:?}", denied.result);
    assert_eq!(error.code, -32603);
    assert_eq!(
        error.message,
        "tool execution failed: config error: invalid arguments for tracedecay_todos: unknown field `not_a_field`, expected one of `kinds`, `path`, `limit`"
    );

    let after_denial =
        json_payload(&call_todos(&fixture.harness, &fixture.project_root, default_args).await);
    assert_eq!(after_denial, default_scan());

    fixture.harness.shutdown().await;
}
