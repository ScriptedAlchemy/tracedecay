use serde_json::{Value, json};

#[hotpath::measure]
pub(super) fn branch_schema(description: &str) -> Value {
    json!({
        "type": "string",
        "description": description
    })
}

#[hotpath::measure]
pub(super) fn worktree_schema(description: &str) -> Value {
    json!({
        "type": "string",
        "description": description
    })
}

#[hotpath::measure]
pub(super) fn commit_schema(description: &str) -> Value {
    json!({
        "type": "string",
        "description": description
    })
}
