//! Who issued a `tools/call`.
//!
//! An MCP host session and a one-shot `tracedecay` client (a CLI command, a host
//! hook, the CLI's truncation recovery) reach the daemon over the same
//! `tools/call` wire. Only a host call is agent tool usage, so the one-shot
//! client names itself in `_meta` and tool analytics records host calls only.

use serde_json::{Map, Value};

/// `_meta` key naming the issuer of a `tools/call`.
pub const TOOL_CALL_INVOKER_META_KEY: &str = "tracedecay/invoker";

const ONE_SHOT_CLIENT: &str = "one-shot-client";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolCallInvoker {
    Host,
    OneShotClient,
}

impl ToolCallInvoker {
    /// The issuer a request's `_meta` declares. Anything but the one-shot
    /// client marker is a host call.
    #[must_use]
    pub fn from_meta(meta: Option<&Map<String, Value>>) -> Self {
        match meta
            .and_then(|meta| meta.get(TOOL_CALL_INVOKER_META_KEY))
            .and_then(Value::as_str)
        {
            Some(ONE_SHOT_CLIENT) => Self::OneShotClient,
            _ => Self::Host,
        }
    }

    /// Marks a request's `_meta` as issued by a one-shot `tracedecay` client.
    pub fn mark_one_shot_client(meta: &mut Map<String, Value>) {
        meta.insert(
            TOOL_CALL_INVOKER_META_KEY.to_owned(),
            Value::from(ONE_SHOT_CLIENT),
        );
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn only_the_one_shot_marker_leaves_host_attribution() {
        assert_eq!(ToolCallInvoker::from_meta(None), ToolCallInvoker::Host);
        let mut meta = Map::new();
        assert_eq!(
            ToolCallInvoker::from_meta(Some(&meta)),
            ToolCallInvoker::Host
        );
        meta.insert(TOOL_CALL_INVOKER_META_KEY.to_owned(), json!("claude-code"));
        assert_eq!(
            ToolCallInvoker::from_meta(Some(&meta)),
            ToolCallInvoker::Host
        );
        ToolCallInvoker::mark_one_shot_client(&mut meta);
        assert_eq!(
            ToolCallInvoker::from_meta(Some(&meta)),
            ToolCallInvoker::OneShotClient
        );
    }
}
