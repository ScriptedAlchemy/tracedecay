//! The tool list one `tracedecay serve` session shows its host.
//!
//! A host without native tool deferral pays for every `tools/list`
//! definition on every turn, and the catalog holds well over a hundred tools.
//! Hosts register only the names `tools/list` carries, so withholding a name
//! makes that tool uncallable on any host that does not re-list. The serve
//! proxy therefore keeps every catalog name on the list and stubs the tools
//! the catalog does not mark `_meta["anthropic/alwaysLoad"]`: the name, the
//! description's first sentence, an accept-anything schema, and a note to call
//! it by name. Calling a stub forwards the call unchanged, puts the tool's
//! full schema on this session's list, and announces
//! `notifications/tools/list_changed`. Until the host re-lists, every error
//! result from that tool carries its full input schema, so a guessed call can
//! be corrected on a host that ignores `list_changed`. `tools/call` is never
//! filtered.
//!
//! `--full-tool-list` serves the daemon's catalog unchanged, for hosts that
//! defer schemas natively and keep the `alwaysLoad` tools loaded. Each served
//! list logs a SHA-256 of the sorted tool names, the roster that session saw.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};
use tracedecay_domain::canonical_text::sha256_hex;
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_mcp::{ALWAYS_LOAD_META, JsonRpcRequest};
use tracedecay_runtime_core::logging::log_daemon_event;

/// Reads an explicit serve on/off env. Unset, empty, `0`, and `false` are
/// off; `1` and `true` are on; anything else is a configuration error rather
/// than a silent default.
pub(super) fn env_switch(name: &str) -> Result<bool> {
    parse_env_switch(name, std::env::var(name))
}

pub(super) fn parse_env_switch(
    name: &str,
    value: std::result::Result<String, std::env::VarError>,
) -> Result<bool> {
    match value {
        Err(std::env::VarError::NotPresent) => Ok(false),
        Ok(value) if value.is_empty() || value == "0" || value == "false" => Ok(false),
        Ok(value) if value == "1" || value == "true" => Ok(true),
        Ok(value) => Err(TraceDecayError::Config {
            message: format!("{name} must be \"1\", \"true\", \"0\", or \"false\", got {value:?}"),
        }),
        Err(std::env::VarError::NotUnicode(_)) => Err(TraceDecayError::Config {
            message: format!("{name} must be \"1\", \"true\", \"0\", or \"false\""),
        }),
    }
}

const TOOL_LIST_CHANGED: &str = "notifications/tools/list_changed";
/// Characters of the description's first sentence a stub keeps.
const STUB_DESCRIPTION_CHARS: usize = 200;
const STUB_NOTE: &str = "[Stub: call by name; a rejected call returns the full schema.]";
const STUB_INSTRUCTIONS_NOTE: &str = "\n\nThis session lists the most-used tools with full \
     schemas and every other tool as a stub. Call a stub by name: the next tools/list \
     carries its full schema, and a rejected call returns that schema until then.";

/// One host session's advertised tool set.
#[derive(Debug)]
pub(crate) struct ToolSurface {
    stub_deferred_tools: bool,
    /// The daemon's full definition of each tool the last list stubbed.
    stubbed: BTreeMap<String, Value>,
    loaded: BTreeSet<String>,
    last_roster_sha256: Option<String>,
}

impl ToolSurface {
    pub(crate) fn new(full_tool_list: bool) -> Self {
        Self {
            stub_deferred_tools: !full_tool_list,
            stubbed: BTreeMap::new(),
            loaded: BTreeSet::new(),
            last_roster_sha256: None,
        }
    }

    /// Stubs deferred tools in the daemon's `tools/list` answer, and adds the
    /// stub note to the `initialize` instructions.
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
                if self.stub_deferred_tools {
                    self.stubbed.clear();
                    for tool in tools.iter_mut() {
                        if !always_loaded(tool)
                            && let Some((name, stubbed)) = stub(tool)
                            && !self.loaded.contains(&name)
                        {
                            self.stubbed.insert(name, std::mem::replace(tool, stubbed));
                        }
                    }
                }
                self.log_roster(tools);
            } else if let Some(Value::String(instructions)) =
                message.pointer_mut("/result/instructions")
            {
                if !self.stub_deferred_tools {
                    continue;
                }
                instructions.push_str(STUB_INSTRUCTIONS_NOTE);
            } else {
                continue;
            }
            *line = format!("{message}\n");
        }
    }

    /// For a call to a tool the host still sees as a stub: appends the full
    /// input schema to an error result or JSON-RPC refusal, and on the first
    /// such call puts the schema on the list and announces `list_changed` so
    /// the host re-lists.
    pub(super) fn hydrate_called_stub(
        &mut self,
        request: Option<&JsonRpcRequest>,
        responses: &mut Vec<String>,
    ) {
        let Some(request) = request.filter(|request| request.method == "tools/call") else {
            return;
        };
        let Some(name) = request
            .params
            .as_ref()
            .and_then(|params| params.get("name"))
            .and_then(Value::as_str)
        else {
            return;
        };
        let Some(definition) = self.stubbed.get(name) else {
            return;
        };
        if let Some(schema) = definition.get("inputSchema") {
            for line in responses.iter_mut() {
                let Ok(mut message) = serde_json::from_str::<Value>(line) else {
                    continue;
                };
                if message.get("id") != request.id.as_ref() {
                    continue;
                }
                if message.pointer("/result/isError") == Some(&json!(true)) {
                    if let Some(content) = message
                        .pointer_mut("/result/content")
                        .and_then(Value::as_array_mut)
                    {
                        content.push(json!({
                            "type": "text",
                            "text": format!("Full input schema for {name}: {schema}"),
                        }));
                        *line = format!("{message}\n");
                    }
                } else if let Some(error) = message.get_mut("error").and_then(Value::as_object_mut)
                    && let Some(text) = error.get("message").and_then(Value::as_str)
                {
                    let text = format!("{text}\n\nFull input schema for {name}: {schema}");
                    error.insert("message".to_owned(), json!(text));
                    *line = format!("{message}\n");
                }
            }
        }
        if self.loaded.insert(name.to_owned()) {
            responses.push(format!(
                "{}\n",
                json!({ "jsonrpc": "2.0", "method": TOOL_LIST_CHANGED })
            ));
        }
    }

    fn log_roster(&mut self, tools: &[Value]) {
        let roster = roster_sha256(tools);
        if self.last_roster_sha256.as_deref() == Some(&roster) {
            return;
        }
        log_daemon_event(
            "mcp_tool_list_roster",
            &[
                ("sha256", roster.clone()),
                ("tools", tools.len().to_string()),
                ("stubbed", self.stubbed.len().to_string()),
            ],
        );
        self.last_roster_sha256 = Some(roster);
    }
}

fn always_loaded(tool: &Value) -> bool {
    tool.pointer("/_meta")
        .and_then(|meta| meta.get(ALWAYS_LOAD_META))
        .and_then(Value::as_bool)
        == Some(true)
}

/// SHA-256 of the sorted tool names, the roster identity one session saw.
fn roster_sha256(tools: &[Value]) -> String {
    let mut names = tools
        .iter()
        .filter_map(|tool| tool.get("name").and_then(Value::as_str))
        .collect::<Vec<_>>();
    names.sort_unstable();
    format!("sha256:{}", sha256_hex(names.join("\n").as_bytes()))
}

/// The name, the description's bounded first sentence, and an accept-anything
/// schema, so the host registers the tool and the model can still call it. A
/// definition without a string name and description is served as the daemon
/// sent it.
fn stub(tool: &Value) -> Option<(String, Value)> {
    let name = tool.get("name")?.as_str()?;
    let description = tool.get("description")?.as_str()?;
    let sentence = description
        .split_once(". ")
        .map_or(description, |(first, _)| first)
        .trim_end_matches('.');
    let head = sentence
        .chars()
        .take(STUB_DESCRIPTION_CHARS)
        .collect::<String>();
    let mut stubbed = json!({
        "name": name,
        "description": format!("{head}. {STUB_NOTE}"),
        "inputSchema": { "type": "object" },
    });
    if let Some(annotations) = tool.get("annotations") {
        stubbed["annotations"] = annotations.clone();
    }
    Some((name.to_owned(), stubbed))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(line: &Value) -> JsonRpcRequest {
        JsonRpcRequest::decode(&line.to_string()).expect("request")
    }

    fn tools_list() -> JsonRpcRequest {
        request(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}))
    }

    fn call(name: &str) -> JsonRpcRequest {
        request(&json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {"name": name, "arguments": {}}
        }))
    }

    fn call_result(is_error: bool) -> Vec<String> {
        vec![format!(
            "{}\n",
            json!({"jsonrpc": "2.0", "id": 3, "result": {
                "content": [{"type": "text", "text": "missing field `node_id`"}],
                "isError": is_error,
            }})
        )]
    }

    fn listing() -> Vec<String> {
        vec![format!(
            "{}\n",
            json!({"jsonrpc": "2.0", "id": 2, "result": {"tools": [
                {"name": "tracedecay_grep", "description": "Search text.",
                 "inputSchema": {"type": "object", "properties": {"pattern": {"type": "string"}}},
                 "_meta": {ALWAYS_LOAD_META: true}},
                {"name": "tracedecay_impact", "description": "Blast radius of a change. More.",
                 "inputSchema": {"type": "object", "properties": {"node_id": {"type": "string"}}},
                 "annotations": {"readOnlyHint": true}},
            ]}})
        )]
    }

    fn served(surface: &mut ToolSurface) -> Vec<Value> {
        let mut responses = listing();
        surface.rewrite(Some(&tools_list()), &mut responses);
        let message: Value = serde_json::from_str(&responses[0]).expect("json");
        message["result"]["tools"]
            .as_array()
            .expect("tools")
            .clone()
    }

    fn has_full_schema(tool: &Value) -> bool {
        tool["inputSchema"]["properties"].is_object()
    }

    fn hydrate(surface: &mut ToolSurface, name: &str, is_error: bool) -> Vec<Value> {
        let mut responses = call_result(is_error);
        surface.hydrate_called_stub(Some(&call(name)), &mut responses);
        responses
            .iter()
            .map(|line| serde_json::from_str(line).expect("json"))
            .collect()
    }

    #[test]
    fn stubs_keep_every_name_and_only_always_loaded_tools_keep_schemas() {
        let tools = served(&mut ToolSurface::new(false));
        assert_eq!(tools[0]["name"], "tracedecay_grep");
        assert!(has_full_schema(&tools[0]), "{tools:?}");
        assert_eq!(tools[1]["name"], "tracedecay_impact");
        assert!(!has_full_schema(&tools[1]), "{tools:?}");
        assert_eq!(tools[1]["annotations"]["readOnlyHint"], json!(true));
        assert_eq!(
            tools[1]["description"],
            json!(format!("Blast radius of a change. {STUB_NOTE}"))
        );
    }

    #[test]
    fn calling_a_stub_loads_its_schema_once_and_announces_it() {
        let mut surface = ToolSurface::new(false);
        served(&mut surface);
        assert_eq!(
            hydrate(&mut surface, "tracedecay_grep", false).len(),
            1,
            "an always-loaded tool is never a stub"
        );
        let frames = hydrate(&mut surface, "tracedecay_impact", false);
        assert_eq!(frames.len(), 2, "{frames:?}");
        assert_eq!(frames[1]["method"], TOOL_LIST_CHANGED);
        assert_eq!(
            frames[0]["result"]["content"].as_array().map(Vec::len),
            Some(1),
            "a successful call carries no schema: {frames:?}"
        );
        assert_eq!(
            hydrate(&mut surface, "tracedecay_impact", false).len(),
            1,
            "a loaded tool must not be announced twice"
        );
        assert!(has_full_schema(&served(&mut surface)[1]));
    }

    #[test]
    fn a_rejected_call_returns_the_schema_until_the_host_relists() {
        let appended = |frames: &[Value]| {
            frames[0]["result"]["content"][1]["text"]
                .as_str()
                .map(str::to_owned)
        };
        let mut surface = ToolSurface::new(false);
        served(&mut surface);
        assert_eq!(hydrate(&mut surface, "tracedecay_impact", false).len(), 2);
        let frames = hydrate(&mut surface, "tracedecay_impact", true);
        assert_eq!(frames.len(), 1, "announced once: {frames:?}");
        let schema = appended(&frames).expect("appended schema text");
        assert!(
            schema.starts_with("Full input schema for tracedecay_impact: ")
                && schema.contains(r#""node_id""#),
            "{schema}"
        );
        served(&mut surface);
        assert_eq!(
            appended(&hydrate(&mut surface, "tracedecay_impact", true)),
            None,
            "a re-listed tool already carries its schema"
        );
    }

    #[test]
    fn a_json_rpc_refusal_carries_the_schema_until_the_host_relists() {
        let refusal = || {
            vec![format!(
                "{}\n",
                json!({"jsonrpc": "2.0", "id": 3, "error": {
                    "code": -32602,
                    "message": "tool project route failed: missing field `node_id`",
                }})
            )]
        };
        let mut surface = ToolSurface::new(false);
        served(&mut surface);
        let mut responses = refusal();
        surface.hydrate_called_stub(Some(&call("tracedecay_impact")), &mut responses);
        assert_eq!(responses.len(), 2, "{responses:?}");
        let refused: Value = serde_json::from_str(&responses[0]).expect("json");
        let message = refused["error"]["message"].as_str().expect("message");
        assert!(
            message.contains("Full input schema for tracedecay_impact: ")
                && message.contains(r#""node_id""#),
            "{message}"
        );
        served(&mut surface);
        let mut responses = refusal();
        surface.hydrate_called_stub(Some(&call("tracedecay_impact")), &mut responses);
        assert_eq!(responses.len(), 1, "announced once: {responses:?}");
        let refused: Value = serde_json::from_str(&responses[0]).expect("json");
        assert_eq!(
            refused["error"]["message"],
            json!("tool project route failed: missing field `node_id`"),
            "a re-listed tool's refusal carries no schema"
        );
    }

    #[test]
    fn calling_a_name_never_listed_loads_nothing() {
        let mut surface = ToolSurface::new(false);
        served(&mut surface);
        assert_eq!(
            hydrate(&mut surface, "tracedecay_not_cataloged", true).len(),
            1
        );
    }

    #[test]
    fn full_tool_list_serves_the_catalog_unchanged() {
        let mut surface = ToolSurface::new(true);
        let mut responses = listing();
        surface.rewrite(Some(&tools_list()), &mut responses);
        let served: Value = serde_json::from_str(&responses[0]).expect("json");
        let daemon: Value = serde_json::from_str(&listing()[0]).expect("json");
        assert_eq!(served, daemon);
        assert_eq!(hydrate(&mut surface, "tracedecay_impact", true).len(), 1);
    }

    #[test]
    fn stub_description_is_the_bounded_first_sentence() {
        let long = "é".repeat(STUB_DESCRIPTION_CHARS + 50);
        let (_, stubbed) =
            stub(&json!({"name": "tracedecay_impact", "description": long})).expect("stub");
        let description = stubbed["description"].as_str().expect("description");
        assert!(description.starts_with(&"é".repeat(STUB_DESCRIPTION_CHARS)));
        assert!(!description.starts_with(&"é".repeat(STUB_DESCRIPTION_CHARS + 1)));
    }

    #[test]
    fn roster_sha256_is_order_independent() {
        let hash = roster_sha256(&[json!({"name": "b"}), json!({"name": "a"})]);
        assert_eq!(hash, format!("sha256:{}", sha256_hex(b"a\nb")));
        assert_eq!(
            hash,
            roster_sha256(&[json!({"name": "a"}), json!({"name": "b"})])
        );
    }
}
