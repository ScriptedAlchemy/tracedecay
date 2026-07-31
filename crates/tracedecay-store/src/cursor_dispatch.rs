//! Pure classification of Cursor-native dispatch records.
//!
//! Transcript ingest and the canonical observation projection read the same
//! provider-native shapes, so the accepted model spellings, their precedence,
//! and the subagent tool vocabulary live here once. When these rules were
//! duplicated, a divergence would have surfaced as a model or subagent
//! attribution that changed depending on which lane produced the record.

use serde_json::Value;

/// Accepted spellings for a model name on a Cursor-native record, in
/// precedence order. Cursor has emitted every one of these across versions.
///
/// Public because provider ingest outside this crate reads the same records
/// and must agree on both the spellings and their precedence; a lane that
/// keeps its own copy of this list silently attributes a different model as
/// soon as Cursor changes which spelling it emits.
pub const CURSOR_MODEL_KEYS: &[&str] = &[
    "model",
    "model_id",
    "modelId",
    "model_name",
    "modelName",
    "model_slug",
    "modelSlug",
    "model_display_name",
    "modelDisplayName",
    "display_model",
    "displayModel",
    "display_model_name",
    "displayModelName",
];

/// Keys whose values compose a dispatch's descriptive text, in join order.
const DISPATCH_TEXT_KEYS: &[&str] = &["description", "prompt", "subagent_type"];

/// Tool names that denote a subagent dispatch, compared case-insensitively.
const SUBAGENT_DISPATCH_TOOLS: &[&str] = &["task", "subagent"];

/// First non-blank model name on `value` among the accepted spellings.
pub fn cursor_model_string(value: &Value) -> Option<String> {
    CURSOR_MODEL_KEYS.iter().copied().find_map(|key| {
        value
            .get(key)
            .and_then(Value::as_str)
            .filter(|model| !model.trim().is_empty())
            .map(str::to_string)
    })
}

/// Model of a dispatch item, preferring its `input` payload over the item.
pub fn cursor_dispatch_model(item: &Value) -> Option<String> {
    item.get("input")
        .and_then(cursor_model_string)
        .or_else(|| cursor_model_string(item))
}

/// Whether a tool name denotes a subagent dispatch.
pub fn is_subagent_dispatch_tool(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    SUBAGENT_DISPATCH_TOOLS.contains(&name.as_str())
}

/// Dispatch description, prompt, and subagent type joined in that order.
///
/// Each key is read from the `input` payload first and from the item second, so
/// a partially nested dispatch still contributes every field it carries.
pub fn dispatch_text(item: &Value) -> Option<String> {
    let input = item.get("input").unwrap_or(item);
    let mut parts = Vec::new();
    for &key in DISPATCH_TEXT_KEYS {
        if let Some(value) = input
            .get(key)
            .or_else(|| item.get(key))
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
        {
            parts.push(value.to_string());
        }
    }
    (!parts.is_empty()).then(|| parts.join("\n\n"))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        cursor_dispatch_model, cursor_model_string, dispatch_text, is_subagent_dispatch_tool,
    };

    #[test]
    fn model_string_takes_the_first_non_blank_spelling_in_precedence_order() {
        assert_eq!(
            cursor_model_string(&json!({ "modelSlug": "slug", "model_name": "name" })).as_deref(),
            Some("name"),
            "model_name outranks modelSlug"
        );
        assert_eq!(
            cursor_model_string(&json!({ "model": "   ", "model_id": "identified" })).as_deref(),
            Some("identified"),
            "a blank higher-precedence key must not shadow a populated one"
        );
        assert_eq!(cursor_model_string(&json!({ "unrelated": "value" })), None);
        assert_eq!(cursor_model_string(&json!({ "model": 7 })), None);
    }

    #[test]
    fn dispatch_model_prefers_the_input_payload_over_the_item() {
        assert_eq!(
            cursor_dispatch_model(&json!({
                "model": "outer",
                "input": { "model": "inner" },
            }))
            .as_deref(),
            Some("inner")
        );
        assert_eq!(
            cursor_dispatch_model(&json!({ "model": "outer", "input": {} })).as_deref(),
            Some("outer"),
            "an input payload without a model falls back to the item"
        );
        assert_eq!(cursor_dispatch_model(&json!({})), None);
    }

    #[test]
    fn subagent_dispatch_tools_match_case_insensitively() {
        for name in ["task", "Task", "TASK", "subagent", "SubAgent"] {
            assert!(is_subagent_dispatch_tool(name), "{name} is a dispatch tool");
        }
        for name in ["taskly", "shell", "", "sub agent"] {
            assert!(
                !is_subagent_dispatch_tool(name),
                "{name} is not a dispatch tool"
            );
        }
    }

    #[test]
    fn dispatch_text_joins_present_fields_and_falls_back_per_key() {
        assert_eq!(
            dispatch_text(&json!({
                "input": { "description": "what", "prompt": "how" },
                "subagent_type": "explore",
            }))
            .as_deref(),
            Some("what\n\nhow\n\nexplore"),
            "keys missing from input are read from the item"
        );
        assert_eq!(
            dispatch_text(&json!({ "prompt": "bare" })).as_deref(),
            Some("bare"),
            "an item without an input payload is read directly"
        );
        assert_eq!(dispatch_text(&json!({ "prompt": "  " })), None);
        assert_eq!(dispatch_text(&json!({})), None);
    }
}
