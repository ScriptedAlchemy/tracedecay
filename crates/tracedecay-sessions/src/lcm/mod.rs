//! Provider-neutral LCM contracts and reducers.

use serde_json::Value;

pub mod compression_policy;
pub mod contracts;
pub mod replay_transactions;
pub mod security;

/// The LCM token-budget heuristic: whitespace-delimited words, never zero.
///
/// Every LCM budget decision is denominated in this unit — the compression
/// trigger, the replay accounting, the retrieval window, and the policy
/// reducer. It lives here, above both the contract reducers and the runtime,
/// because the four of them must agree: a heuristic that only some callers
/// adopt would let a session compress against one budget and be replayed
/// against another.
///
/// Named distinctly from the chars/4 `estimate_tokens` helpers in read-mode
/// and global-db surfaces so those cannot be imported into this budget path
/// by accident.
pub(crate) fn lcm_budget_tokens(text: &str) -> i64 {
    text.split_whitespace().count().max(1) as i64
}

/// Visible text that feeds [`lcm_budget_tokens`] for a JSON message.
///
/// The budget unit is words of user-visible content, not serialized JSON.
/// String bodies stay strings; `{ "text": ... }` objects and arrays of
/// `{ "text": ... }` parts contribute that text. Structured payloads with no
/// text parts fall through to `Value`'s compact Display so a count is still
/// produced — never a silent empty from a failed stringify.
pub(crate) fn lcm_message_visible_text(message: &Value) -> String {
    let Some(content) = message.get("content") else {
        return String::new();
    };
    match content {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        other => {
            if let Some(text) = other.get("text").and_then(Value::as_str) {
                return text.to_string();
            }
            if let Some(items) = other.as_array() {
                let texts = items
                    .iter()
                    .filter_map(|item| item.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>();
                if !texts.is_empty() {
                    return texts.join("\n\n");
                }
            }
            other.to_string()
        }
    }
}

/// [`lcm_budget_tokens`] over [`lcm_message_visible_text`].
pub(crate) fn lcm_message_budget_tokens(message: &Value) -> i64 {
    lcm_budget_tokens(&lcm_message_visible_text(message))
}

#[cfg(test)]
mod tests {
    use super::{lcm_budget_tokens, lcm_message_budget_tokens, lcm_message_visible_text};
    use serde_json::json;

    #[test]
    fn object_with_text_exposes_visible_words_not_json_keys() {
        let message = json!({
            "content": {
                "extra": "ignored key words",
                "text": "one",
            }
        });
        assert_eq!(lcm_message_visible_text(&message), "one");
        assert_eq!(
            lcm_message_budget_tokens(&message),
            lcm_budget_tokens("one")
        );
        assert_eq!(lcm_message_budget_tokens(&message), 1);
    }

    #[test]
    fn array_of_text_parts_joins_visible_words() {
        let message = json!({
            "content": [
                { "extra": "ignored key words", "text": "one" },
                { "text": "two three" },
            ]
        });
        assert_eq!(lcm_message_visible_text(&message), "one\n\ntwo three");
        assert_eq!(
            lcm_message_budget_tokens(&message),
            lcm_budget_tokens("one\n\ntwo three")
        );
        assert_eq!(lcm_message_budget_tokens(&message), 3);
    }
}
