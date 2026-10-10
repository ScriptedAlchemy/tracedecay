//! The tool list one `tracedecay serve` session shows its host.
//!
//! A host without native tool deferral pays for every `tools/list`
//! definition on every turn, and the catalog holds well over a hundred tools.
//! Hosts register only the tools `tools/list` names, so a tool missing from
//! the list is uncallable from the model until the host re-lists. The serve
//! proxy is the one process that lives for a host session, so the session's
//! advertised set lives here.
//!
//! Default serve ([`ToolAdvertisement::Search`]) lists [`CORE_TOOL_NAMES`] plus
//! [`TOOL_SEARCH_NAME`]. Search ranks an exact name first, loads matches into
//! the session list, announces `notifications/tools/list_changed`, and returns
//! the full schemas in its result text so a host that ignores `list_changed`
//! can still call them. [`ToolAdvertisement::ClaudeNativeFull`] is selected
//! only by `--claude-code-tool-search` or
//! [`CLAUDE_CODE_TOOL_SEARCH_ENV`]; it serves the full catalog and sets
//! `_meta["anthropic/alwaysLoad"]` on the core tools so Claude Code's native
//! tool search can defer the rest. `tools/call` is never filtered. Each served
//! list logs a SHA-256 of the sorted tool names.

use std::collections::BTreeSet;

use serde_json::{Value, json};
use tracedecay_domain::canonical_text::sha256_hex;
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_mcp::JsonRpcRequest;
use tracedecay_runtime_core::logging::log_daemon_event;

/// How one serve session advertises tools that are not in the core set.
///
/// Default serve is [`Self::Search`]. [`Self::ClaudeNativeFull`] is selected
/// only by an explicit CLI flag or [`CLAUDE_CODE_TOOL_SEARCH_ENV`], never by
/// guessing the host from `clientInfo` or proxy environment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ToolAdvertisement {
    /// Core tools plus [`TOOL_SEARCH_NAME`]; other tools load on search.
    Search,
    /// Full catalog; core tools carry `_meta["anthropic/alwaysLoad"]`.
    ClaudeNativeFull,
}

/// Explicit env that selects [`ToolAdvertisement::ClaudeNativeFull`].
pub(super) const CLAUDE_CODE_TOOL_SEARCH_ENV: &str = "TRACEDECAY_MCP_CLAUDE_CODE_TOOL_SEARCH";
const ANTHROPIC_ALWAYS_LOAD: &str = "anthropic/alwaysLoad";

impl ToolAdvertisement {
    fn from_env() -> Result<Self> {
        match std::env::var(CLAUDE_CODE_TOOL_SEARCH_ENV) {
            Err(std::env::VarError::NotPresent) => Ok(Self::Search),
            Ok(value) if value.is_empty() || value == "0" || value == "false" => Ok(Self::Search),
            Ok(value) if value == "1" || value == "true" => Ok(Self::ClaudeNativeFull),
            Ok(value) => Err(TraceDecayError::Config {
                message: format!(
                    "{CLAUDE_CODE_TOOL_SEARCH_ENV} must be \"1\", \"true\", \"0\", or \"false\", \
                     got {value:?}"
                ),
            }),
            Err(std::env::VarError::NotUnicode(_)) => Err(TraceDecayError::Config {
                message: format!(
                    "{CLAUDE_CODE_TOOL_SEARCH_ENV} must be \"1\", \"true\", \"0\", or \"false\""
                ),
            }),
        }
    }
}

/// The tools a session lists with full schemas before any search, and the
/// tools Claude Code must keep loaded when native tool search is on.
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
const SEARCH_INSTRUCTIONS_NOTE: &str = "\n\nThis session lists a core tool set. Call \
     `tracedecay_tool_search` with keywords or a tool name to load any other TraceDecay \
     tool into tools/list; every tool also answers a direct tools/call by name.";

/// Initialize instructions for Claude Code native tool search: a category
/// map under 2,048 characters, not the default steering paragraph.
pub(super) const CLAUDE_CATEGORY_GUIDE: &str = "\
TraceDecay groups tools by task. Core tools set _meta[\"anthropic/alwaysLoad\"] \
and stay loaded. Claude Code native tool search defers the rest; call a tool \
by name or use ToolSearch.\n\
Core: active_project, status, storage_status, grep, search, context, node, \
source_body, source_lines, source_outline, callers, files, diff_context, \
test_map, fact_store_search, message_search, lcm_grep.\n\
Impact and blast radius: impact, affected, affected_tests.\n\
Call chains: callees, call_chain.\n\
Git, PR, and branch: git_diff, pr_context, branch_*.\n\
Code health: health, complexity, coupling, hotspots, circular, god_class, \
gini, largest, dsm, dependency_depth, recursion.\n\
Edits: str_replace, ast_grep_rewrite.\n\
Tests: run_affected_tests.\n\
Work and workflows: work_*, workflow_*.\n\
Memory: fact_store_*.\n\
Configuration: configuration_*.\n\
Diagnostics: diagnostics, diagnose, runtime.\n\
Same operations: tracedecay tool <name> (--help for parameters). Do not query \
.tracedecay databases.";

/// One host session's advertised tool set.
#[derive(Debug)]
pub(super) struct ToolSurface {
    advertisement: ToolAdvertisement,
    loaded: BTreeSet<String>,
    last_roster_sha256: Option<String>,
}

impl ToolSurface {
    pub(super) fn with_advertisement(advertisement: ToolAdvertisement) -> Self {
        Self {
            advertisement,
            loaded: BTreeSet::new(),
            last_roster_sha256: None,
        }
    }

    pub(super) fn from_env() -> Result<Self> {
        Ok(Self::with_advertisement(ToolAdvertisement::from_env()?))
    }

    /// `--claude-code-tool-search` wins; otherwise the explicit env is read.
    pub(super) fn from_serve(claude_code_tool_search: bool) -> Result<Self> {
        if claude_code_tool_search {
            Ok(Self::with_advertisement(
                ToolAdvertisement::ClaudeNativeFull,
            ))
        } else {
            Self::from_env()
        }
    }

    fn advertises_full(&self, name: &str) -> bool {
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
        let request = request.filter(|request| {
            self.advertisement == ToolAdvertisement::Search && request.method == "tools/call"
        })?;
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

    /// Narrows the daemon's `tools/list` answer to this session's tools, or
    /// marks core tools for Claude Code native deferral, and rewrites
    /// `initialize` instructions for the selected advertisement.
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
                match self.advertisement {
                    ToolAdvertisement::Search => {
                        tools.retain(|tool| {
                            tool["name"]
                                .as_str()
                                .is_some_and(|name| self.advertises_full(name))
                        });
                        tools.push(tool_search_definition());
                    }
                    ToolAdvertisement::ClaudeNativeFull => {
                        for tool in tools.iter_mut() {
                            mark_core_always_load(tool);
                        }
                    }
                }
                let roster = roster_sha256(tools);
                if self.last_roster_sha256.as_deref() != Some(&roster) {
                    log_daemon_event(
                        "mcp_tool_list_roster",
                        &[
                            ("sha256", roster.clone()),
                            ("tools", tools.len().to_string()),
                            ("advertisement", format!("{:?}", self.advertisement)),
                            ("loaded", self.loaded.len().to_string()),
                        ],
                    );
                    self.last_roster_sha256 = Some(roster);
                }
            } else if let Some(Value::String(instructions)) =
                message.pointer_mut("/result/instructions")
            {
                match self.advertisement {
                    ToolAdvertisement::Search => instructions.push_str(SEARCH_INSTRUCTIONS_NOTE),
                    ToolAdvertisement::ClaudeNativeFull => {
                        *instructions = CLAUDE_CATEGORY_GUIDE.to_owned();
                    }
                }
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
    /// The result text includes each match's full schema so a host that ignores
    /// `list_changed` can still bind the tool.
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
                .filter(|name| !self.advertises_full(name))
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
            newly_loaded |= !self.advertises_full(name) && self.loaded.insert((*name).to_owned());
        }
        let listed = matches
            .iter()
            .map(|(_, name, description)| format!("- {name}: {}", summary(description)))
            .collect::<Vec<_>>()
            .join("\n");
        let matched_tools = matches
            .iter()
            .filter_map(|(_, name, _)| tools.iter().find(|tool| tool["name"] == *name).cloned())
            .collect::<Vec<_>>();
        let schemas = match serde_json::to_string(&matched_tools) {
            Ok(schemas) => schemas,
            Err(error) => {
                return vec![tool_result(
                    id,
                    &format!("Matching tool schemas failed to serialize: {error}"),
                    true,
                )];
            }
        };
        let text = format!(
            "Matching TraceDecay tools (now in tools/list):\n{listed}\n\nFull schemas:\n{schemas}"
        );
        let mut lines = vec![tool_result(id, &text, false)];
        if newly_loaded {
            lines.push(list_changed_line());
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

fn list_changed_line() -> String {
    format!(
        "{}\n",
        json!({ "jsonrpc": "2.0", "method": TOOL_LIST_CHANGED })
    )
}

fn mark_core_always_load(tool: &mut Value) {
    let name = tool
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    if !CORE_TOOL_NAMES.contains(&name.as_str()) {
        return;
    }
    match tool.get_mut("_meta") {
        Some(Value::Object(map)) => {
            map.insert(ANTHROPIC_ALWAYS_LOAD.to_owned(), json!(true));
        }
        _ => {
            tool["_meta"] = json!({ ANTHROPIC_ALWAYS_LOAD: true });
        }
    }
}

/// An exact name wins; otherwise a term in the name outweighs one in the prose.
///
/// Codex BM25 tool search can miss an exact name (openai/codex#21503), so a
/// single-term query that equals the catalog name or its `tracedecay_`-stripped
/// short name is scored above any description match.
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
        "description": "Find and load TraceDecay tools not in this list. Pass keywords or an \
            exact tool name as `query`. Matches are added to tools/list, announced with \
            notifications/tools/list_changed, and returned as full schemas in this result. \
            An empty query lists every tool not loaded yet.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Keywords or an exact tool name."
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
        let message: Value = serde_json::from_str(&responses[0]).expect("json");
        message["result"]["tools"]
            .as_array()
            .expect("tools")
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

    fn search_text(answer: &[String]) -> String {
        let message: Value = serde_json::from_str(&answer[0]).expect("json");
        message["result"]["content"][0]["text"]
            .as_str()
            .expect("text")
            .to_owned()
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
                .and_then(|meta| meta.get(ANTHROPIC_ALWAYS_LOAD))
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
    fn claude_category_guide_stays_under_the_instruction_cap() {
        assert!(
            CLAUDE_CATEGORY_GUIDE.chars().count() < 2_048,
            "Claude initialize instructions must stay under 2,048 chars, got {}",
            CLAUDE_CATEGORY_GUIDE.chars().count()
        );
    }

    #[test]
    fn search_loads_matches_into_the_session_list_and_returns_full_schemas() {
        let list = request(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}));
        let mut surface = ToolSurface::with_advertisement(ToolAdvertisement::Search);
        let mut responses = listing();
        surface.rewrite(Some(&list), &mut responses);
        assert_eq!(listed(&responses), ["tracedecay_grep", TOOL_SEARCH_NAME]);

        let answer = surface.answer_search(&json!(3), "blast radius", &listing());
        let text = search_text(&answer);
        assert!(text.contains("tracedecay_impact"), "{text}");
        assert!(!text.contains("tracedecay_git_diff"), "{text}");
        assert!(
            text.contains(r#""inputSchema""#) && text.contains(r#""node_id""#),
            "search must return the full schema in result text: {text}"
        );
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
    fn exact_name_outranks_description_matches() {
        assert_eq!(
            search_score(
                "tracedecay_git_diff",
                "unrelated",
                &["tracedecay_git_diff".to_owned()]
            ),
            1_000
        );
        assert_eq!(
            search_score("tracedecay_git_diff", "unrelated", &["git_diff".to_owned()]),
            1_000
        );
        let description_hits = search_score(
            "tracedecay_impact",
            "git_diff git_diff git_diff blast radius",
            &["git_diff".to_owned()],
        );
        assert!(
            description_hits < 1_000,
            "description matches must not beat an exact name: {description_hits}"
        );

        let mut surface = ToolSurface::with_advertisement(ToolAdvertisement::Search);
        let crowded = vec![format!(
            "{}\n",
            json!({"jsonrpc": "2.0", "id": 2, "result": {"tools": [
                {"name": "tracedecay_impact",
                 "description": "git_diff git_diff git_diff blast radius of a change.",
                 "inputSchema": {"type": "object"}},
                {"name": "tracedecay_git_diff", "description": "Show a diff.",
                 "inputSchema": {"type": "object", "properties": {"ref": {"type": "string"}}}},
            ]}})
        )];
        let text = search_text(&surface.answer_search(&json!(3), "tracedecay_git_diff", &crowded));
        let impact_at = text.find("tracedecay_impact");
        let git_at = text.find("tracedecay_git_diff");
        assert!(
            git_at.is_some_and(|git| impact_at.is_none_or(|impact| git < impact)),
            "exact name must be listed first: {text}"
        );
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
        let surface = ToolSurface::with_advertisement(ToolAdvertisement::Search);
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
        let mut surface = ToolSurface::with_advertisement(ToolAdvertisement::Search);
        let answer = surface.answer_search(&json!(3), "", &listing());
        assert_eq!(answer.len(), 1, "{answer:?}");
        assert!(answer[0].contains("tracedecay_git_diff"), "{answer:?}");
        assert!(!answer[0].contains("tracedecay_grep,"), "{answer:?}");
        assert!(surface.loaded.is_empty());
    }

    #[test]
    fn a_failed_catalog_read_is_a_tool_error() {
        let mut surface = ToolSurface::with_advertisement(ToolAdvertisement::Search);
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
    fn claude_native_full_keeps_every_name_and_marks_core_always_load() {
        let list = request(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}));
        let mut surface = ToolSurface::with_advertisement(ToolAdvertisement::ClaudeNativeFull);
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
        assert_eq!(tools[0]["_meta"][ANTHROPIC_ALWAYS_LOAD], json!(true));
        assert!(
            tools[1].get("_meta").is_none()
                || tools[1]["_meta"].get(ANTHROPIC_ALWAYS_LOAD) != Some(&json!(true)),
            "non-core tools must stay deferrable: {}",
            tools[1]
        );
        assert!(!listed(&responses).contains(&TOOL_SEARCH_NAME.to_owned()));
    }

    #[test]
    fn claude_native_full_replaces_initialize_instructions() {
        let initialize = request(&json!({"jsonrpc": "2.0", "id": 1, "method": "initialize"}));
        let mut surface = ToolSurface::with_advertisement(ToolAdvertisement::ClaudeNativeFull);
        let mut responses = vec![format!(
            "{}\n",
            json!({"jsonrpc": "2.0", "id": 1, "result": {"instructions": "long default"}})
        )];
        surface.rewrite(Some(&initialize), &mut responses);
        let message: Value = serde_json::from_str(&responses[0]).expect("json");
        assert_eq!(
            message["result"]["instructions"].as_str(),
            Some(CLAUDE_CATEGORY_GUIDE)
        );
    }

    #[test]
    fn claude_native_full_does_not_answer_tool_search() {
        let call = request(&json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {"name": TOOL_SEARCH_NAME, "arguments": {"query": "impact"}}
        }));
        let surface = ToolSurface::with_advertisement(ToolAdvertisement::ClaudeNativeFull);
        assert!(surface.search_request(Some(&call)).is_none());
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
