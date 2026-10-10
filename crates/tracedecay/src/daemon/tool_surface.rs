//! The tool list one `tracedecay serve` session shows its host.
//!
//! A host without native tool deferral pays for every `tools/list`
//! definition on every turn, and the catalog holds well over a hundred tools.
//! Hosts register only the tools `tools/list` names, so a tool missing from
//! the list is uncallable from the model until the host re-lists. The serve
//! proxy is the one process that lives for a host session (each request
//! travels on its own daemon connection), so the session's advertised set
//! lives here: the daemon keeps answering its full session catalog, the proxy
//! narrows `tools/list` to [`CORE_TOOL_NAMES`] plus the tools this session
//! loaded through [`TOOL_SEARCH_NAME`], and announces each load with
//! `notifications/tools/list_changed` so the host re-lists. `tools/call` is
//! never filtered, so a client that calls a catalog tool by name keeps
//! working. Each served list logs a SHA-256 of the sorted tool names so the
//! session records which roster it saw.

use std::collections::BTreeSet;

use serde_json::{Value, json};
use tracedecay_domain::canonical_text::sha256_hex;
use tracedecay_mcp::JsonRpcRequest;
use tracedecay_runtime_core::logging::log_daemon_event;

/// The tools a session lists with full schemas before any search.
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

/// The proxy-served tool that finds and loads tools outside the core set.
pub(super) const TOOL_SEARCH_NAME: &str = "tracedecay_tool_search";
/// Matches loaded per search, so one broad query cannot re-inflate the list.
const MAX_LOADED_PER_SEARCH: usize = 8;
const TOOL_LIST_CHANGED: &str = "notifications/tools/list_changed";
const INSTRUCTIONS_NOTE: &str = "\n\nThis session lists a core tool set. Call \
     `tracedecay_tool_search` with keywords or a tool name to load any other TraceDecay \
     tool into tools/list; every tool also answers a direct tools/call by name.";

/// One host session's advertised tool set.
#[derive(Debug, Default)]
pub(super) struct ToolSurface {
    loaded: BTreeSet<String>,
    last_roster_sha256: Option<String>,
}

impl ToolSurface {
    pub(super) fn new() -> Self {
        Self::default()
    }

    fn advertises(&self, name: &str) -> bool {
        CORE_TOOL_NAMES.contains(&name) || self.loaded.contains(name)
    }

    /// The id and query of a tool search this proxy answers itself.
    ///
    /// Notifications (`id` absent) are not searches: a JSON-RPC notification
    /// must not receive a response.
    pub(super) fn search_request(
        &self,
        request: Option<&JsonRpcRequest>,
    ) -> Option<(Value, String)> {
        let request = request.filter(|request| request.method == "tools/call")?;
        let id = request.id.clone()?;
        let params = request.params.as_ref()?;
        if params.get("name").and_then(Value::as_str) != Some(TOOL_SEARCH_NAME) {
            return None;
        }
        let query = params
            .pointer("/arguments/query")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        Some((id, query))
    }

    /// Narrows the daemon's `tools/list` answer to this session's tools and
    /// notes the tool search in the `initialize` instructions.
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
                tools.retain(|tool| {
                    tool["name"]
                        .as_str()
                        .is_some_and(|name| self.advertises(name))
                });
                tools.push(tool_search_definition());
                let roster = roster_sha256(tools);
                if self.last_roster_sha256.as_deref() != Some(&roster) {
                    log_daemon_event(
                        "mcp_tool_list_roster",
                        &[
                            ("sha256", roster.clone()),
                            ("tools", tools.len().to_string()),
                            ("loaded", self.loaded.len().to_string()),
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

    /// Answers a tool search from the daemon's `tools/list` answer for this
    /// session, loading the best matches into the session's list.
    ///
    /// Searching the session's own listing keeps the answer to tools this
    /// session can call: a projectless session never loads project-bound tools.
    pub(super) fn answer_search(
        &mut self,
        id: &Value,
        query: &str,
        listing: &[String],
    ) -> Vec<String> {
        let Some(tools) = listing
            .iter()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .find_map(|message| {
                message
                    .pointer("/result/tools")
                    .and_then(Value::as_array)
                    .cloned()
            })
        else {
            return vec![tool_result(
                id,
                &format!(
                    "The TraceDecay tool catalog is unavailable: {}",
                    listing.concat().trim()
                ),
                true,
            )];
        };
        let catalog = tools
            .iter()
            .filter_map(|tool| Some((tool["name"].as_str()?, tool["description"].as_str()?)))
            .collect::<Vec<_>>();

        let terms = query
            .split(|c: char| !c.is_alphanumeric() && c != '_')
            .filter(|term| term.len() >= 2)
            .map(str::to_lowercase)
            .collect::<Vec<_>>();
        if terms.is_empty() {
            let unlisted = catalog
                .iter()
                .map(|(name, _)| *name)
                .filter(|name| !self.advertises(name))
                .collect::<Vec<_>>();
            let text = format!(
                "{} more TraceDecay tools are available. Call {TOOL_SEARCH_NAME} with keywords \
                 or an exact name to load some:\n{}",
                unlisted.len(),
                unlisted.join(", ")
            );
            return vec![tool_result(id, &text, false)];
        }

        let mut matches = catalog
            .iter()
            .filter_map(|(name, description)| {
                let score = search_score(name, description, &terms);
                (score > 0).then_some((score, *name, *description))
            })
            .collect::<Vec<_>>();
        matches.sort_by(|left, right| right.0.cmp(&left.0).then(left.1.cmp(right.1)));
        matches.truncate(MAX_LOADED_PER_SEARCH);
        if matches.is_empty() {
            let text = format!(
                "No TraceDecay tool matches {query:?}. Call {TOOL_SEARCH_NAME} without a query \
                 to list every tool."
            );
            return vec![tool_result(id, &text, false)];
        }

        let mut newly_loaded = false;
        for (_, name, _) in &matches {
            newly_loaded |= !self.advertises(name) && self.loaded.insert((*name).to_owned());
        }
        let listed = matches
            .iter()
            .map(|(_, name, description)| format!("- {name}: {}", summary(description)))
            .collect::<Vec<_>>()
            .join("\n");
        let text = format!(
            "Matching TraceDecay tools (now in tools/list):\n{listed}\n\nIf your host does not \
             refresh its tool list, call the tool by name or run `tracedecay tool <name> --help` \
             from a shell."
        );
        let mut lines = vec![tool_result(id, &text, false)];
        if newly_loaded {
            lines.push(format!(
                "{}\n",
                json!({ "jsonrpc": "2.0", "method": TOOL_LIST_CHANGED })
            ));
        }
        lines
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

/// An exact name wins; otherwise a term in the name outweighs one in the prose.
fn search_score(name: &str, description: &str, terms: &[String]) -> usize {
    let short = name.strip_prefix("tracedecay_").unwrap_or(name);
    if terms.len() == 1 && (terms[0] == name || terms[0] == short) {
        return 1_000;
    }
    let description = description.to_lowercase();
    terms
        .iter()
        .map(|term| {
            let term = term.strip_prefix("tracedecay_").unwrap_or(term);
            3 * usize::from(short.contains(term)) + usize::from(description.contains(term))
        })
        .sum()
}

/// The first sentence of a description, bounded for the search answer.
fn summary(description: &str) -> String {
    let sentence = description.split(". ").next().unwrap_or(description);
    let mut summary = sentence.chars().take(200).collect::<String>();
    if summary.len() < sentence.len() {
        summary.push('…');
    }
    summary
}

fn tool_result(id: &Value, text: &str, is_error: bool) -> String {
    let response = json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "content": [{ "type": "text", "text": text }],
            "isError": is_error,
        },
    });
    format!("{response}\n")
}

fn tool_search_definition() -> Value {
    json!({
        "name": TOOL_SEARCH_NAME,
        "description": "Find and load TraceDecay tools that are not in the tool list yet. The \
            list starts with a core set (grep, search, context, source reads, callers, diff \
            context, test mapping, files, session and fact recall); the rest cover impact and \
            blast radius, call chains, git, PR and branch context, code health, refactors and \
            edits, test runs, workflows, work items, memory, configuration, and diagnostics. \
            Pass keywords or an exact tool name as `query`: the best matches are added to \
            tools/list and announced with notifications/tools/list_changed. Without a query, \
            lists every tool not loaded yet.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Keywords (e.g. \"impact blast radius\") or an exact tool name."
                }
            },
            "additionalProperties": false
        },
        "annotations": { "readOnlyHint": true, "title": "Find TraceDecay Tools" },
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
                {"name": "tracedecay_grep", "description": "Search text."},
                {"name": "tracedecay_impact", "description": "Blast radius of a change. More."},
                {"name": "tracedecay_git_diff", "description": "Show a diff."},
            ]}})
        )]
    }

    fn listed(responses: &[String]) -> Vec<String> {
        let message: Value = serde_json::from_str(&responses[0]).expect("json");
        message["result"]["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .map(|tool| tool["name"].as_str().expect("name").to_owned())
            .collect()
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
        assert!(!catalog.contains(TOOL_SEARCH_NAME));
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
    fn search_loads_matches_into_the_session_list_and_announces_once() {
        let list = request(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}));
        let mut surface = ToolSurface::new();
        let mut responses = listing();
        surface.rewrite(Some(&list), &mut responses);
        assert_eq!(listed(&responses), ["tracedecay_grep", TOOL_SEARCH_NAME]);

        let answer = surface.answer_search(&json!(3), "blast radius", &listing());
        assert!(answer[0].contains("tracedecay_impact"), "{answer:?}");
        assert!(!answer[0].contains("tracedecay_git_diff"), "{answer:?}");
        assert_eq!(answer.len(), 2, "{answer:?}");
        assert!(answer[1].contains(TOOL_LIST_CHANGED), "{answer:?}");
        let mut responses = listing();
        surface.rewrite(Some(&list), &mut responses);
        assert_eq!(
            listed(&responses),
            ["tracedecay_grep", "tracedecay_impact", TOOL_SEARCH_NAME]
        );

        let again = surface.answer_search(&json!(4), "tracedecay_impact", &listing());
        assert_eq!(again.len(), 1, "a repeat load must not announce a change");
    }

    #[test]
    fn search_request_requires_a_json_rpc_id() {
        let call = request(&json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {"name": TOOL_SEARCH_NAME, "arguments": {"query": "impact"}}
        }));
        let notification = request(&json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "params": {"name": TOOL_SEARCH_NAME, "arguments": {"query": "impact"}}
        }));
        let surface = ToolSurface::new();
        assert_eq!(
            surface.search_request(Some(&call)),
            Some((json!(3), "impact".to_owned()))
        );
        assert!(
            surface.search_request(Some(&notification)).is_none(),
            "a notification must not receive a search response"
        );
    }

    #[test]
    fn empty_query_lists_unloaded_tools_without_loading_them() {
        let mut surface = ToolSurface::new();
        let answer = surface.answer_search(&json!(3), "", &listing());
        assert_eq!(answer.len(), 1, "{answer:?}");
        assert!(answer[0].contains("tracedecay_git_diff"), "{answer:?}");
        assert!(!answer[0].contains("tracedecay_grep,"), "{answer:?}");
        assert!(surface.loaded.is_empty());
    }

    #[test]
    fn a_failed_catalog_read_is_a_tool_error() {
        let mut surface = ToolSurface::new();
        let failure = format!(
            "{}\n",
            json!({"jsonrpc": "2.0", "id": 2, "error": {"code": -32603, "message": "boom"}})
        );
        let answer = surface.answer_search(&json!(3), "impact", &[failure]);
        let response: Value = serde_json::from_str(&answer[0]).expect("json");
        assert_eq!(response["result"]["isError"], json!(true), "{response}");
        assert!(surface.loaded.is_empty());
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
}
