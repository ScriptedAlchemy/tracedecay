use serde_json::Value;

pub(super) fn assert_fact_list(payload: &Value, included: &str, excluded: &str, context: &str) {
    let facts = payload["facts"]
        .as_array()
        .unwrap_or_else(|| panic!("{context} must return canonical facts: {payload}"));
    assert_eq!(facts.len(), 1, "{context}: {payload}");
    let contents: Vec<&str> = facts
        .iter()
        .map(|projection| {
            assert_eq!(projection["kind"], "available", "{context}: {payload}");
            projection["fact"]["content"]
                .as_str()
                .unwrap_or_else(|| panic!("{context} fact content: {payload}"))
        })
        .collect();
    assert!(
        contents.iter().any(|content| content.contains(included)),
        "{context}: {payload}"
    );
    assert!(
        contents.iter().all(|content| !content.contains(excluded)),
        "{context}: {payload}"
    );
}
