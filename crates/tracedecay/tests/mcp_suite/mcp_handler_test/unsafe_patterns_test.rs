#![cfg(feature = "test-transport")]

use std::fs;

use serde_json::{Value, json};

use crate::support::{
    extract_text, production_composition_fixture_with_sources, wait_for_current_graph,
};

#[tokio::test]
async fn unsafe_patterns_classifies_inline_rust_test_scopes() {
    let fixture = production_composition_fixture_with_sources(|project| {
        fs::create_dir_all(project.join("src")).unwrap();
        fs::write(
            project.join("src/lib.rs"),
            r#"pub fn production_risk() { Some(1).unwrap(); }

pub mod tests {
    pub fn production_module_named_tests() { Some(2).unwrap(); }
}

mod support {
    #[cfg(test)]
    mod nested_cfg {
        fn test_only_helper() { Some(3).unwrap(); }
    }
}

#[test]
fn attributed_test() { Some(4).unwrap(); }

#[test] fn adjacent_test() { Some(5).unwrap(); } pub fn adjacent_production() { panic!(); }
"#,
        )
        .unwrap();
    })
    .await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production MCP server");
    wait_for_current_graph(&server).await;

    let call = |exclude_tests| {
        fixture.harness.call_tool(
            &fixture.project_root,
            "tracedecay_unsafe_patterns",
            json!({"kinds": ["unwrap", "panic"], "exclude_tests": exclude_tests, "format": "json"}),
        )
    };
    let included = call(false).await.unwrap();
    assert!(included.error.is_none(), "{:?}", included.error);
    let included: Value = serde_json::from_str(extract_text(
        &included.result.expect("unsafe-pattern result"),
    ))
    .unwrap();
    assert_eq!(included["match_count"], 6, "{included}");
    let matches = included["matches"].as_array().unwrap();
    assert_eq!(
        matches.iter().filter(|hit| hit["in_test"] == true).count(),
        2
    );
    assert_eq!(
        matches.iter().filter(|hit| hit["in_test"] == false).count(),
        4
    );
    assert!(
        matches.iter().all(|hit| hit["enclosing"].is_string()),
        "{included}"
    );

    let excluded = call(true).await.unwrap();
    assert!(excluded.error.is_none(), "{:?}", excluded.error);
    let excluded: Value = serde_json::from_str(extract_text(
        &excluded.result.expect("unsafe-pattern result"),
    ))
    .unwrap();
    assert_eq!(excluded["match_count"], 4, "{excluded}");
    assert!(
        excluded["matches"]
            .as_array()
            .unwrap()
            .iter()
            .all(|hit| hit["in_test"] == false),
        "{excluded}"
    );
    assert!(
        excluded["matches"].as_array().unwrap().iter().any(|hit| {
            hit["kind"] == "panic"
                && hit["snippet"]
                    .as_str()
                    .unwrap()
                    .contains("adjacent_production")
        }),
        "production risk sharing a line with a test item was hidden: {excluded}"
    );

    fixture.harness.shutdown().await;
}
