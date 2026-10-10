//! The tool list one `tracedecay serve` session shows its host.
//!
//! A host without native tool deferral pays for every `tools/list`
//! definition on every turn, and the catalog holds well over a hundred tools.
//! Hosts register the tools `tools/list` names, so a withheld name is
//! uncallable from the model until the host re-lists. Following Parsec's
//! prune (daseinlabs/parsec `stub_tool` / reactive unfreeze), the serve proxy
//! keeps every catalog name on the list: [`CORE_TOOL_NAMES`] plus tools this
//! session already called keep their full schemas; the rest are stubs (name,
//! first ~200 characters of the description, an accept-anything schema, and
//! a note to call by name). Calling a stub hydrates its full schema and
//! announces `notifications/tools/list_changed`. `tools/call` is never
//! filtered. Each served list logs a SHA-256 of the sorted tool names so the
//! session records which roster it saw.

use std::collections::BTreeSet;

use serde_json::{Value, json};
use tracedecay_domain::canonical_text::sha256_hex;
use tracedecay_mcp::JsonRpcRequest;
use tracedecay_runtime_core::logging::log_daemon_event;

/// The tools a session lists with full schemas before any call.
///
/// Every `anthropic/alwaysLoad` tool plus every tool with at least 100
/// recorded calls across local Claude Code and Codex transcripts (October
/// 2026): source reads, grep, symbol search and context, node lookup, diff
/// context, callers, test mapping, file listing, and session and fact recall.
pub(super) const CORE_TOOL_NAMES: &[&str] = &[
    "tracedecay_active_project",
    "tracedecay_callers",
    "tracedecay_context",
    "tracedecay_diff_context",
    "tracedecay_fact_store_search",
    "tracedecay_files",
    "tracedecay_grep",
    "tracedecay_lcm_grep",
    "tracedecay_message_search",
    "tracedecay_node",
    "tracedecay_search",
    "tracedecay_source_body",
    "tracedecay_source_lines",
    "tracedecay_source_outline",
    "tracedecay_status",
    "tracedecay_storage_status",
    "tracedecay_test_map",
];

const TOOL_LIST_CHANGED: &str = "notifications/tools/list_changed";
/// Chars of the original description carried into a stub (char-safe cap).
const STUB_DESC_CHARS: usize = 200;
/// Model-facing note appended to every stub: one call by name hydrates the
/// full schema from the next `tools/list` on.
pub(super) const STUB_NOTE: &str = "[This tool is available but its full schema was elided to save \
     context. To use it, call it by name with your best-guess arguments; its complete schema will \
     be provided from the next turn onward.]";
const INSTRUCTIONS_NOTE: &str = "\n\nThis session lists a core tool set with full schemas. Other \
     catalog tools are stubs (name, a short description, and an accept-anything schema). Call a \
     stub by name to use it; its complete schema is sent on the next tools/list.";

/// One host session's advertised tool set.
#[derive(Debug, Default)]
pub(super) struct ToolSurface {
    loaded: BTreeSet<String>,
    catalog: BTreeSet<String>,
    last_roster_sha256: Option<String>,
}

impl ToolSurface {
    pub(super) fn new() -> Self {
        Self::default()
    }

    fn advertises_full(&self, name: &str) -> bool {
        CORE_TOOL_NAMES.contains(&name) || self.loaded.contains(name)
    }

    /// Stub pruned tools on `tools/list`, and note how to hydrate them in the
    /// `initialize` instructions.
    pub(super) fn rewrite(&mut self, request: Option<&JsonRpcRequest>, responses: &mut [String]) {
        let Some(request) = request else {
            return;
        };
        if request.id.is_none() || !matches!(request.method.as_str(), "tools/list" | "initialize") {
            return;
        }
        for line in responses.iter_mut() {
            let Ok(mut message) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if message.get("id") != request.id.as_ref() {
                continue;
            }
            if let Some(tools) = message
                .pointer_mut("/result/tools")
                .and_then(Value::as_array_mut)
            {
                self.catalog = tools
                    .iter()
                    .filter_map(|tool| tool["name"].as_str().map(str::to_owned))
                    .collect();
                let mut full = 0_usize;
                let mut stubbed = 0_usize;
                for tool in tools.iter_mut() {
                    let name = tool["name"].as_str().unwrap_or_default();
                    if self.advertises_full(name) {
                        full += 1;
                    } else {
                        *tool = stub_tool(tool);
                        stubbed += 1;
                    }
                }
                let roster = roster_sha256(tools);
                if self.last_roster_sha256.as_deref() != Some(&roster) {
                    log_daemon_event(
                        "mcp_tool_list_roster",
                        &[
                            ("sha256", roster.clone()),
                            ("tools", tools.len().to_string()),
                            ("full", full.to_string()),
                            ("stubbed", stubbed.to_string()),
                        ],
                    );
                    self.last_roster_sha256 = Some(roster);
                }
            } else if let Some(Value::String(instructions)) =
                message.pointer_mut("/result/instructions")
            {
                instructions.push_str(INSTRUCTIONS_NOTE);
            } else {
                continue;
            }
            *line = format!("{message}\n");
        }
    }

    /// Hydrates a stub the host just called so the next `tools/list` carries
    /// its full schema, and announces the change.
    pub(super) fn unfreeze_call(&mut self, request: Option<&JsonRpcRequest>) -> Option<String> {
        let request = request.filter(|request| request.method == "tools/call")?;
        let name = request
            .params
            .as_ref()
            .and_then(|params| params.get("name"))
            .and_then(Value::as_str)?;
        if self.advertises_full(name) {
            return None;
        }
        if !self.catalog.is_empty() && !self.catalog.contains(name) {
            return None;
        }
        self.loaded
            .insert(name.to_owned())
            .then(|| list_changed_line())
    }
}

/// SHA-256 of the sorted tool names, the roster identity one session saw.
pub(super) fn roster_sha256(tools: &[Value]) -> String {
    let mut names = tools
        .iter()
        .filter_map(|tool| tool.get("name").and_then(Value::as_str))
        .collect::<Vec<_>>();
    names.sort_unstable();
    format!("sha256:{}", sha256_hex(names.join("\n").as_bytes()))
}

fn list_changed_line() -> String {
    format!(
        "{}\n",
        json!({ "jsonrpc": "2.0", "method": TOOL_LIST_CHANGED })
    )
}

/// Name + truncated description + accept-anything schema, so the model can
/// still reach the tool. Stub bytes are a pure function of the original tool.
fn stub_tool(tool: &Value) -> Value {
    let name = tool.get("name").and_then(Value::as_str).unwrap_or_default();
    let desc = tool
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let head: String = desc.chars().take(STUB_DESC_CHARS).collect();
    let description = if head.is_empty() {
        STUB_NOTE.to_string()
    } else {
        format!("{head}\n\n{STUB_NOTE}")
    };
    json!({
        "name": name,
        "description": description,
        "inputSchema": { "type": "object", "additionalProperties": true },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(line: &Value) -> JsonRpcRequest {
        JsonRpcRequest::decode(&line.to_string()).expect("request")
    }

    fn listing() -> Vec<String> {
        vec![format!(
            "{}\n",
            json!({"jsonrpc": "2.0", "id": 2, "result": {"tools": [
                {"name": "tracedecay_grep", "description": "Search text.",
                 "inputSchema": {"type": "object", "properties": {"pattern": {"type": "string"}}}},
                {"name": "tracedecay_impact", "description": "Blast radius of a change. More.",
                 "inputSchema": {"type": "object", "properties": {"node_id": {"type": "string"}}}},
                {"name": "tracedecay_git_diff", "description": "Show a diff.",
                 "inputSchema": {"type": "object"}},
            ]}})
        )]
    }

    fn listed(responses: &[String]) -> Vec<String> {
        tools_of(responses)
            .iter()
            .map(|tool| tool["name"].as_str().expect("name").to_owned())
            .collect()
    }

    fn tools_of(responses: &[String]) -> Vec<Value> {
        let message: Value = serde_json::from_str(&responses[0]).expect("json");
        message["result"]["tools"]
            .as_array()
            .expect("tools")
            .to_vec()
    }

    fn is_stub(tool: &Value) -> bool {
        tool["inputSchema"] == json!({"type": "object", "additionalProperties": true})
            && tool["description"]
                .as_str()
                .is_some_and(|description| description.contains(STUB_NOTE))
    }

    #[test]
    fn core_tools_are_cataloged_and_cover_always_load() {
        let definitions = tracedecay_mcp::get_maximal_tool_definitions().expect("tool definitions");
        let catalog = definitions
            .iter()
            .map(|definition| definition.name.as_str())
            .collect::<BTreeSet<_>>();
        let core = CORE_TOOL_NAMES.iter().copied().collect::<BTreeSet<_>>();
        assert_eq!(core.len(), CORE_TOOL_NAMES.len(), "duplicate core tool");
        assert!(
            core.is_subset(&catalog),
            "core tools missing from the catalog: {:?}",
            core.difference(&catalog).collect::<Vec<_>>()
        );
        for definition in &definitions {
            let always_load = definition
                .meta
                .as_ref()
                .and_then(|meta| meta.get("anthropic/alwaysLoad"))
                .and_then(Value::as_bool)
                == Some(true);
            assert!(
                !always_load || core.contains(definition.name.as_str()),
                "{} is alwaysLoad but not in the core list",
                definition.name
            );
        }
    }

    #[test]
    fn rewrite_stubs_pruned_tools_and_keeps_every_name() {
        let list = request(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}));
        let mut surface = ToolSurface::new();
        let mut responses = listing();
        surface.rewrite(Some(&list), &mut responses);
        assert_eq!(
            listed(&responses),
            [
                "tracedecay_grep",
                "tracedecay_impact",
                "tracedecay_git_diff",
            ]
        );
        let tools = tools_of(&responses);
        assert!(!is_stub(&tools[0]), "{tools:?}");
        assert!(is_stub(&tools[1]), "{tools:?}");
        assert!(is_stub(&tools[2]), "{tools:?}");
        assert!(
            tools[1]["description"]
                .as_str()
                .is_some_and(|description| description.starts_with("Blast radius of a change")),
            "{tools:?}"
        );
    }

    #[test]
    fn stub_tool_truncates_description_char_safe() {
        let long: String = "é".repeat(STUB_DESC_CHARS + 50);
        let stub = stub_tool(&json!({
            "name": "tracedecay_impact",
            "description": long,
            "inputSchema": {"type": "object", "properties": {"node_id": {"type": "string"}}},
        }));
        let description = stub["description"].as_str().expect("description");
        assert!(description.starts_with(&"é".repeat(STUB_DESC_CHARS)));
        assert!(!description.starts_with(&"é".repeat(STUB_DESC_CHARS + 1)));
        assert!(description.ends_with(STUB_NOTE));
        assert_eq!(
            stub["inputSchema"],
            json!({"type": "object", "additionalProperties": true})
        );
    }

    #[test]
    fn roster_sha256_is_sorted_names() {
        let hash = roster_sha256(&[
            json!({"name": "b"}),
            json!({"name": "a"}),
            json!({"name": "c"}),
        ]);
        let expected = format!("sha256:{}", sha256_hex(b"a\nb\nc"));
        assert_eq!(hash, expected);
        assert_eq!(
            hash,
            roster_sha256(&[
                json!({"name": "c"}),
                json!({"name": "a"}),
                json!({"name": "b"})
            ])
        );
    }

    #[test]
    fn calling_a_stub_hydrates_its_full_schema_and_announces_once() {
        let list = request(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}));
        let call = request(&json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {"name": "tracedecay_impact", "arguments": {"node_id": "n1"}}
        }));
        let mut surface = ToolSurface::new();
        let mut responses = listing();
        surface.rewrite(Some(&list), &mut responses);
        assert!(is_stub(&tools_of(&responses)[1]));

        let changed = surface.unfreeze_call(Some(&call)).expect("list_changed");
        assert!(changed.contains(TOOL_LIST_CHANGED), "{changed}");
        assert!(surface.unfreeze_call(Some(&call)).is_none());

        let mut responses = listing();
        surface.rewrite(Some(&list), &mut responses);
        let tools = tools_of(&responses);
        assert!(!is_stub(&tools[1]), "{tools:?}");
        assert_eq!(
            tools[1]["inputSchema"]["properties"]["node_id"]["type"],
            "string"
        );
    }
}
