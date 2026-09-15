use std::fs;

use serde_json::json;

use crate::support::{
    extract_json, production_composition_fixture_with_sources, wait_for_current_graph,
};

#[tokio::test]
async fn script_calls_keep_only_the_uncalled_shell_function_dead() {
    let (_isolated_env, _) = crate::common::IsolatedEnv::acquire().await;
    let fixture = production_composition_fixture_with_sources(|project| {
        fs::write(
            project.join("usage.sh"),
            r#"have() { command -v "$1" >/dev/null 2>&1; }
q() { sqlite3 "$1" "$2"; }
never_called() { echo unused; }

if have sqlite3; then
    q usage.db 'select 1'
fi
"#,
        )
        .unwrap();
    })
    .await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production shell fixture server");
    wait_for_current_graph(&server).await;

    let hierarchy = fixture
        .harness
        .call_tool(
            &fixture.project_root,
            "tracedecay_rank",
            json!({
                "edge_kind": "contains",
                "direction": "outgoing",
                "node_kind": "module",
                "path": "usage.sh",
                "format": "json"
            }),
        )
        .await
        .unwrap();
    assert!(
        hierarchy.error.is_none(),
        "rank failed: {:?}",
        hierarchy.error
    );
    let hierarchy = extract_json(&hierarchy.result.unwrap());
    assert_eq!(hierarchy["ranking"][0]["name"], "usage");
    assert_eq!(hierarchy["ranking"][0]["count"], 3);

    let response = fixture
        .harness
        .call_tool(
            &fixture.project_root,
            "tracedecay_dead_code",
            json!({"include_public": true, "kinds": ["function"], "format": "json"}),
        )
        .await
        .unwrap();
    assert!(
        response.error.is_none(),
        "dead code failed: {:?}",
        response.error
    );
    let payload = extract_json(&response.result.unwrap());
    let names = payload["symbols"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|symbol| symbol["name"].as_str())
        .collect::<Vec<_>>();

    assert_eq!(names, ["never_called"]);
    fixture.harness.shutdown().await;
}
