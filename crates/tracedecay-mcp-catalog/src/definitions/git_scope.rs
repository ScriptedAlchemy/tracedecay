use serde_json::{Value, json};

pub(super) fn branch_schema(description: &str) -> Value {
    json!({
        "type": "string",
        "description": description
    })
}

pub(super) fn worktree_schema(description: &str) -> Value {
    json!({
        "type": "string",
        "description": description
    })
}

pub(super) fn commit_schema(description: &str) -> Value {
    json!({
        "type": "string",
        "description": description
    })
}
