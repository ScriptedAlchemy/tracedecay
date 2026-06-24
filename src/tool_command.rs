//! `tracedecay tool <name> [args...]` — invoke any MCP tool from the CLI.
//!
//! The CLI surface is **dynamic**: tool names and parameters come from the MCP
//! tool definitions in [`crate::mcp::tools`]. Each MCP tool's JSON Schema is
//! walked once to convert CLI `--key value` pairs into a `serde_json::Value`,
//! which is then handed to the same dispatch function the MCP server uses.
//!
//! Reserved flags (handled by this module, never forwarded to the tool):
//!
//! - `-h` / `--help` — print the tool's parameters and exit.
//! - `--json` — print the raw JSON-RPC `result.value`; default is the
//!   human-readable text inside `content[0].text`.
//! - `--project <path>` — project root to open. Defaults to the nearest
//!   initialised project walking up from cwd (falling back to cwd). We use
//!   `--project` (not `-p`) because several MCP tools have a `path` argument
//!   that filters files within the project.
//! - `--args <json>` — escape hatch. Treats the JSON value as the entire
//!   argument object; mutually exclusive with `--key value` flags. Use for
//!   complex shapes like `tracedecay_multi_str_replace`'s array-of-pairs.
//!   `--args @/path.json` reads the JSON object from that file, sidestepping
//!   the kernel's 128 KiB per-argv-string cap for large payloads.
//!
//! Any value starting with `@` is read from the file at that path, which makes
//! multi-line strings (replacements, ast-grep patterns, decision text) ergonomic.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{Map, Value};

#[cfg(unix)]
use tracedecay::daemon::call_default_tool;
use tracedecay::daemon::DaemonHandshake;
use tracedecay::errors::{Result, TraceDecayError};
use tracedecay::mcp::tools::{
    get_tool_definitions, handle_profile_scoped_lcm_tool_call, ToolDefinition,
};

/// Old CLI command names that don't match the MCP tool name. Keeps muscle
/// memory working for the seven removed top-level commands. The right-hand
/// side is the canonical MCP suffix (without the `tracedecay_` prefix).
const NAME_ALIASES: &[(&str, &str)] = &[("query", "search")];
const PROFILE_SCOPED_LCM_TOOLS: &[&str] = &[
    "tracedecay_lcm_status",
    "tracedecay_lcm_doctor",
    "tracedecay_lcm_load_session",
    "tracedecay_lcm_grep",
    "tracedecay_lcm_describe",
    "tracedecay_lcm_expand",
    "tracedecay_lcm_expand_query",
    "tracedecay_lcm_preflight",
    "tracedecay_lcm_compress",
    "tracedecay_lcm_session_boundary",
];
// Maintenance note: this CLI allowlist must match the MCP registry's
// profile-scoped LCM schemas (tools with `storage_scope` including
// `hermes_profile`) and the daemon's projectless dispatch path; update it
// alongside the handler lockstep tests so profile-scoped calls do not silently
// route through project initialization.
/// Profile-store tools the generated Hermes plugin anchors at the Hermes
/// home (`--project <hermes_home>`). The store is created on first touch —
/// a fresh profile has no `.tracedecay` until the first fact lands — instead
/// of demanding a manual `tracedecay init` of the profile directory. Gated on
/// an explicit `--project` so a bare invocation from an uninitialised cwd
/// still gets the "run tracedecay init" guidance rather than a silent store.
const FIRST_TOUCH_STORE_TOOLS: &[&str] = &[
    "tracedecay_fact_store",
    "tracedecay_fact_feedback",
    "tracedecay_memory_status",
    "tracedecay_message_search",
];

/// Entry point for `tracedecay tool ...`.
pub(crate) async fn run(
    project: Option<String>,
    name: Option<String>,
    args: Vec<String>,
) -> Result<()> {
    let defs = get_tool_definitions();

    let Some(raw_name) = name else {
        print_tool_list(&defs);
        return Ok(());
    };

    let canonical = canonical_tool_name(&raw_name);
    let Some(def) = defs.iter().find(|d| d.name == canonical) else {
        return Err(TraceDecayError::Config {
            message: format!(
                "unknown tool: '{raw_name}'. Run `tracedecay tool` to list available tools."
            ),
        });
    };

    let parsed = parse_invocation(def, &args)?;
    if parsed.show_help {
        print_tool_help(def);
        return Ok(());
    }
    let ParsedInvocation {
        tool_args,
        project: parsed_project,
        raw_json,
        show_help: _,
    } = parsed;

    if is_profile_scoped_lcm_dispatch(&def.name, &tool_args) {
        return dispatch_daemon_tool(
            DaemonToolDispatch::profile_scoped(),
            &def.name,
            tool_args,
            raw_json,
        )
        .await;
    }

    let explicit_project = project.or(parsed_project);
    dispatch_daemon_tool(
        DaemonToolDispatch::project_scoped(explicit_project, &def.name),
        &def.name,
        tool_args,
        raw_json,
    )
    .await
}

/// Result of CLI argument parsing: the JSON value to hand to the MCP handler,
/// plus the reserved-flag side-effects.
#[cfg_attr(test, derive(Debug))]
struct ParsedInvocation {
    tool_args: Value,
    project: Option<String>,
    raw_json: bool,
    show_help: bool,
}

/// Normalize a user-supplied tool name to the canonical `tracedecay_<suffix>`
/// form used by the MCP registry. Accepts aliases (e.g. `query` → `search`),
/// strips a leading `tracedecay_` if present, and converts dashes to
/// underscores so `dead-code` and `dead_code` both work.
fn canonical_tool_name(raw: &str) -> String {
    let trimmed = raw.strip_prefix("tracedecay_").unwrap_or(raw);
    let normalized = trimmed.replace('-', "_");
    let mapped = NAME_ALIASES
        .iter()
        .find(|(k, _)| *k == normalized)
        .map_or(normalized.as_str(), |(_, v)| *v);
    format!("tracedecay_{mapped}")
}

fn is_profile_scoped_lcm_dispatch(tool_name: &str, tool_args: &Value) -> bool {
    PROFILE_SCOPED_LCM_TOOLS.contains(&tool_name)
        && tool_args
            .get("storage_scope")
            .and_then(Value::as_str)
            .is_some_and(|scope| scope == "hermes_profile")
}

struct DaemonToolDispatch {
    project_path: Option<PathBuf>,
    allow_init: bool,
    allow_profile_scoped_fallback: bool,
}

impl DaemonToolDispatch {
    fn profile_scoped() -> Self {
        Self {
            project_path: None,
            allow_init: false,
            allow_profile_scoped_fallback: true,
        }
    }

    fn project_scoped(explicit_project: Option<String>, tool_name: &str) -> Self {
        // Same resolution as `tracedecay sync`/`status`/`serve`: an explicit
        // --project wins; otherwise walk up from cwd to the nearest initialised
        // project so the command works from subdirectories.
        let explicitly_targeted = explicit_project.is_some();
        let project_path = tracedecay::config::resolve_path_with_discovery(explicit_project);
        let allow_init = explicitly_targeted && FIRST_TOUCH_STORE_TOOLS.contains(&tool_name);

        Self {
            project_path: Some(project_path),
            allow_init,
            allow_profile_scoped_fallback: false,
        }
    }

    fn handshake(&self) -> Result<DaemonHandshake> {
        DaemonHandshake::for_current_client(self.project_path.clone(), None, false, self.allow_init)
    }

    async fn call(&self, tool_name: &str, tool_args: Value) -> Result<Value> {
        let handshake = self.handshake()?;
        #[cfg(unix)]
        {
            call_default_tool(&handshake, tool_name, tool_args).await
        }
        #[cfg(not(unix))]
        {
            call_in_process_tool(&handshake, tool_name, tool_args).await
        }
    }

    async fn fallback(&self, tool_name: &str, tool_args: Value) -> Result<Option<Value>> {
        if !self.allow_profile_scoped_fallback {
            let handshake = self.handshake()?;
            if handshake.project_path.is_none() {
                return Ok(None);
            }
            return Ok(Some(
                call_in_process_tool(&handshake, tool_name, tool_args).await?,
            ));
        }
        let result = handle_profile_scoped_lcm_tool_call(tool_name, tool_args).await?;
        Ok(Some(result.value))
    }
}

async fn call_in_process_tool(
    handshake: &DaemonHandshake,
    tool_name: &str,
    tool_args: Value,
) -> Result<Value> {
    let project_path = handshake
        .project_path
        .as_ref()
        .ok_or_else(|| TraceDecayError::Config {
            message: "profile-scoped daemon tool dispatch requires daemon socket support"
                .to_string(),
        })?;
    let open_options = tracedecay::tracedecay::TraceDecayOpenOptions {
        profile_root: Some(handshake.client_identity.profile_root.clone()),
        global_db_path: Some(handshake.client_identity.global_db_path.clone()),
    };
    let cg = if handshake.allow_init
        && !tracedecay::tracedecay::TraceDecay::has_initialized_store_with_options(
            project_path,
            &open_options,
        )
        .await
    {
        tracedecay::tracedecay::TraceDecay::init_with_options(project_path, open_options).await?
    } else {
        tracedecay::tracedecay::TraceDecay::open_with_options(project_path, open_options).await?
    };
    let global_db =
        tracedecay::global_db::GlobalDb::open_at(&handshake.client_identity.global_db_path).await;
    let result = tracedecay::mcp::tools::handle_tool_call_with_registry(
        &cg,
        tool_name,
        tool_args,
        None,
        handshake.scope_prefix.as_deref(),
        global_db.as_ref(),
        false,
    )
    .await?;
    Ok(result.value)
}

async fn dispatch_daemon_tool(
    dispatch: DaemonToolDispatch,
    tool_name: &str,
    tool_args: Value,
    raw_json: bool,
) -> Result<()> {
    let result_value = match dispatch.call(tool_name, tool_args.clone()).await {
        Ok(value) => value,
        Err(error) if is_daemon_unavailable(&error) => {
            match dispatch.fallback(tool_name, tool_args).await? {
                Some(value) => value,
                None => return Err(error),
            }
        }
        Err(error) => return Err(error),
    };
    print_tool_output(&result_value, raw_json);
    Ok(())
}

fn is_daemon_unavailable(error: &TraceDecayError) -> bool {
    matches!(
        error,
        TraceDecayError::Config { message }
            if message.contains("TraceDecay daemon socket")
                && message.contains("is not available")
    )
}

fn print_tool_output(result_value: &Value, raw_json: bool) {
    if raw_json {
        println!(
            "{}",
            serde_json::to_string_pretty(result_value).unwrap_or_default()
        );
    } else {
        let text = result_value
            .pointer("/content/0/text")
            .and_then(Value::as_str)
            .unwrap_or("");
        println!("{text}");
    }
}

/// Parse CLI args against the tool's JSON Schema. Returns the JSON object to
/// hand to the handler, plus side-effects from reserved flags.
fn parse_invocation(def: &ToolDefinition, args: &[String]) -> Result<ParsedInvocation> {
    let schema_properties = def
        .input_schema
        .get("properties")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();

    let required: Vec<String> = def
        .input_schema
        .get("required")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let mut out = ParsedInvocation {
        tool_args: Value::Object(Map::new()),
        project: None,
        raw_json: false,
        show_help: false,
    };

    let mut explicit_args: Option<Value> = None;
    let mut collected: Map<String, Value> = Map::new();
    let mut positionals: Vec<String> = Vec::new();

    let mut iter = args.iter();
    while let Some(raw) = iter.next() {
        match raw.as_str() {
            "-h" | "--help" => {
                out.show_help = true;
                return Ok(out);
            }
            "--json" => out.raw_json = true,
            "--project" => {
                out.project = Some(take_value(&mut iter, "--project")?);
            }
            "--args" => {
                // `--args @/path/payload.json` reads the JSON object from
                // disk. Valid JSON can never start with `@`, so the prefix is
                // unambiguous here — callers with payloads near the kernel's
                // per-argv-string cap (MAX_ARG_STRLEN, 128 KiB on Linux) spill
                // to a file instead of failing with E2BIG/EFAULT.
                let json_str = resolve_at_file(&take_value(&mut iter, "--args")?)?;
                let value: Value =
                    serde_json::from_str(&json_str).map_err(|e| TraceDecayError::Config {
                        message: format!("--args: invalid JSON: {e}"),
                    })?;
                if !value.is_object() {
                    return Err(TraceDecayError::Config {
                        message: "--args must be a JSON object".to_string(),
                    });
                }
                explicit_args = Some(value);
            }
            flag if flag.starts_with("--") => {
                let key = flag.trim_start_matches('-').replace('-', "_");
                let raw_value = take_value(&mut iter, flag)?;
                let resolved = resolve_at_file(&raw_value)?;
                let prop_schema = schema_properties.get(&key);
                let coerced = coerce_value(&key, prop_schema, &resolved)?;
                merge_value(&mut collected, &key, coerced);
            }
            _ => positionals.push(raw.clone()),
        }
    }

    if let Some(value) = explicit_args {
        if !collected.is_empty() || !positionals.is_empty() {
            return Err(TraceDecayError::Config {
                message: "--args cannot be combined with other tool flags or positionals"
                    .to_string(),
            });
        }
        out.tool_args = value;
        return Ok(out);
    }

    // Bind positionals to required string properties, in the order they appear
    // in the schema's `required` array, skipping any that were already set.
    if !positionals.is_empty() {
        let mut positional_iter = positionals.into_iter();
        for req in &required {
            if collected.contains_key(req) {
                continue;
            }
            let Some(prop) = schema_properties.get(req) else {
                continue;
            };
            let Some(value) = positional_iter.next() else {
                break;
            };
            let resolved = resolve_at_file(&value)?;
            let coerced = coerce_value(req, Some(prop), &resolved)?;
            collected.insert(req.clone(), coerced);
        }
        let leftover: Vec<String> = positional_iter.collect();
        if !leftover.is_empty() {
            return Err(TraceDecayError::Config {
                message: format!(
                    "unexpected positional argument(s): {} — use --key value flags or \
                     run `tracedecay tool {} --help`",
                    leftover.join(" "),
                    def.name.trim_start_matches("tracedecay_")
                ),
            });
        }
    }

    for req in &required {
        if !collected.contains_key(req) {
            return Err(TraceDecayError::Config {
                message: format!(
                    "missing required parameter `--{}` for tool `{}`",
                    req.replace('_', "-"),
                    def.name.trim_start_matches("tracedecay_")
                ),
            });
        }
    }

    finalize_arrays(def, &mut collected);
    out.tool_args = Value::Object(collected);
    Ok(out)
}

/// Coerce a CLI string value to the JSON type declared in the property schema.
/// Falls back to a JSON string when the schema is absent or specifies an
/// unknown type.
fn coerce_value(key: &str, prop_schema: Option<&Value>, raw: &str) -> Result<Value> {
    let ty = prop_schema
        .and_then(|p| p.get("type"))
        .and_then(Value::as_str)
        .unwrap_or("string");

    match ty {
        "string" => Ok(Value::String(raw.to_string())),
        "boolean" => match raw {
            "true" | "1" | "yes" | "on" => Ok(Value::Bool(true)),
            "false" | "0" | "no" | "off" => Ok(Value::Bool(false)),
            other => Err(TraceDecayError::Config {
                message: format!(
                    "--{}: expected a boolean (true/false), got `{other}`",
                    key.replace('_', "-")
                ),
            }),
        },
        "integer" => raw
            .parse::<i64>()
            .map(Value::from)
            .map_err(|_| TraceDecayError::Config {
                message: format!("--{}: expected integer, got `{raw}`", key.replace('_', "-")),
            }),
        // `serde_json::Number::from_f64(25.0).as_u64()` returns `None`, so MCP
        // handlers that read counts via `.as_u64()` would silently fall back
        // to defaults. Prefer integer storage when the input is whole.
        "number" => {
            if let Ok(i) = raw.parse::<i64>() {
                Ok(Value::from(i))
            } else {
                raw.parse::<f64>()
                    .ok()
                    .and_then(serde_json::Number::from_f64)
                    .map(Value::Number)
                    .ok_or_else(|| TraceDecayError::Config {
                        message: format!(
                            "--{}: expected a finite number, got `{raw}`",
                            key.replace('_', "-")
                        ),
                    })
            }
        }
        "array" => Ok(Value::String(raw.to_string())),
        _ => Ok(Value::String(raw.to_string())),
    }
}

/// Insert `value` into `map` under `key`. If the key is already present and
/// the schema-declared shape is an array, append the new value to a sibling
/// array rather than overwriting — this is how repeated `--keywords foo
/// --keywords bar` accumulates.
///
/// Called after [`coerce_value`], so the value is already the right JSON type
/// (or a string we'll wrap in an array on first sight of a second occurrence).
fn merge_value(map: &mut Map<String, Value>, key: &str, value: Value) {
    if let Some(existing) = map.get_mut(key) {
        match existing {
            Value::Array(arr) => arr.push(value),
            _ => {
                let prev = std::mem::replace(existing, Value::Null);
                *existing = Value::Array(vec![prev, value]);
            }
        }
    } else {
        map.insert(key.to_string(), value);
    }
}

/// Promote any `array<string>` properties from a single string into a real
/// array: split on commas if the user passed `--keywords foo,bar`, or wrap a
/// single-occurrence string in a one-element array. Runs after parsing so we
/// can see whether the user passed the flag once or many times.
fn finalize_arrays(def: &ToolDefinition, map: &mut Map<String, Value>) {
    let Some(props) = def
        .input_schema
        .get("properties")
        .and_then(Value::as_object)
    else {
        return;
    };
    for (key, schema) in props {
        let is_array = schema.get("type").and_then(Value::as_str) == Some("array");
        if !is_array {
            continue;
        }
        if let Some(value) = map.get_mut(key) {
            match value {
                Value::String(s) => {
                    let parts: Vec<Value> = if s.contains(',') {
                        s.split(',')
                            .map(|p| Value::String(p.trim().to_string()))
                            .collect()
                    } else {
                        vec![Value::String(std::mem::take(s))]
                    };
                    *value = Value::Array(parts);
                }
                Value::Array(_) => {}
                _ => {}
            }
        }
    }
}

/// Consume the next argument as a flag value or return a `missing value` error.
fn take_value(iter: &mut std::slice::Iter<'_, String>, flag: &str) -> Result<String> {
    iter.next().cloned().ok_or_else(|| TraceDecayError::Config {
        message: format!("flag `{flag}` requires a value"),
    })
}

/// Read a value from disk when it starts with `@`. The leading `@` is
/// stripped; the rest is treated as a path (relative to cwd). Plain values
/// pass through unchanged. To pass a literal `@` as the first character, use
/// `--args` instead.
fn resolve_at_file(raw: &str) -> Result<String> {
    if let Some(path) = raw.strip_prefix('@') {
        let buf = PathBuf::from(path);
        std::fs::read_to_string(&buf).map_err(|e| TraceDecayError::Config {
            message: format!("failed to read @{path}: {e}"),
        })
    } else {
        Ok(raw.to_string())
    }
}

/// Print a grouped list of every available tool. Tools annotated as
/// `alwaysLoad` come first since they're the most commonly used; everything
/// else is alphabetized.
fn print_tool_list(defs: &[ToolDefinition]) {
    let mut groups: BTreeMap<&str, Vec<&ToolDefinition>> = BTreeMap::new();
    let mut always = Vec::new();
    for def in defs {
        let is_always = def
            .meta
            .as_ref()
            .and_then(|m| m.get("anthropic/alwaysLoad"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if is_always {
            always.push(def);
            continue;
        }
        let group = group_for(def);
        groups.entry(group).or_default().push(def);
    }

    println!("Available tools (run `tracedecay tool <name> --help` for parameters):\n");

    if !always.is_empty() {
        println!("[always-loaded]");
        for def in &always {
            println!(
                "  {:<32}  {}",
                short_name(&def.name),
                first_line(&def.description)
            );
        }
        println!();
    }

    for (group, mut list) in groups {
        list.sort_by_key(|d| d.name.clone());
        println!("[{group}]");
        for def in list {
            println!(
                "  {:<32}  {}",
                short_name(&def.name),
                first_line(&def.description)
            );
        }
        println!();
    }
}

/// Display name without the `tracedecay_` prefix.
fn short_name(full: &str) -> &str {
    full.trim_start_matches("tracedecay_")
}

/// First line of a (possibly multi-line) description, truncated for layout.
fn first_line(s: &str) -> String {
    let line = s.lines().next().unwrap_or("");
    if line.len() > 90 {
        format!("{}…", &line[..89])
    } else {
        line.to_string()
    }
}

/// Best-effort categorisation by tool-name prefix. Matches how the codebase
/// already groups handlers (`graph`, `info`, `git`, `analysis`, `health`,
/// `edit`, `memory`). Tools that don't match any prefix fall under `other`.
fn group_for(def: &ToolDefinition) -> &'static str {
    let n = def.name.as_str();
    if n.starts_with("tracedecay_branch_")
        || n == "tracedecay_commit_context"
        || n == "tracedecay_pr_context"
        || n == "tracedecay_changelog"
        || n == "tracedecay_diff_context"
        || n == "tracedecay_affected"
    {
        "git & history"
    } else if n == "tracedecay_str_replace"
        || n == "tracedecay_multi_str_replace"
        || n == "tracedecay_insert_at"
        || n == "tracedecay_ast_grep_rewrite"
        || n == "tracedecay_replace_symbol"
        || n == "tracedecay_insert_at_symbol"
    {
        "edit"
    } else if n == "tracedecay_fact_store"
        || n == "tracedecay_fact_feedback"
        || n == "tracedecay_memory_status"
        || n == "tracedecay_session_start"
        || n == "tracedecay_session_end"
    {
        "memory & session"
    } else if n == "tracedecay_health"
        || n == "tracedecay_runtime"
        || n == "tracedecay_dsm"
        || n == "tracedecay_test_risk"
        || n == "tracedecay_test_map"
        || n == "tracedecay_gini"
        || n == "tracedecay_dependency_depth"
        || n == "tracedecay_redundancy"
    {
        "health"
    } else if n == "tracedecay_callers"
        || n == "tracedecay_callees"
        || n == "tracedecay_callers_for"
        || n == "tracedecay_call_chain"
        || n == "tracedecay_impact"
        || n == "tracedecay_file_dependents"
        || n == "tracedecay_by_qualified_name"
        || n == "tracedecay_signature"
        || n == "tracedecay_impls"
        || n == "tracedecay_implementations"
        || n == "tracedecay_derives"
        || n == "tracedecay_similar"
        || n == "tracedecay_rename_preview"
        || n == "tracedecay_find_exact_symbol"
        || n == "tracedecay_type_hierarchy"
    {
        "graph"
    } else if n == "tracedecay_diagnose"
        || n == "tracedecay_diagnostics"
        || n == "tracedecay_run_affected_tests"
    {
        "workflow"
    } else if n == "tracedecay_dead_code"
        || n == "tracedecay_unused_imports"
        || n == "tracedecay_module_api"
        || n == "tracedecay_circular"
        || n == "tracedecay_hotspots"
        || n == "tracedecay_rank"
        || n == "tracedecay_largest"
        || n == "tracedecay_coupling"
        || n == "tracedecay_inheritance_depth"
        || n == "tracedecay_distribution"
        || n == "tracedecay_recursion"
        || n == "tracedecay_complexity"
        || n == "tracedecay_doc_coverage"
        || n == "tracedecay_god_class"
        || n == "tracedecay_unsafe_patterns"
        || n == "tracedecay_constructors"
        || n == "tracedecay_field_sites"
    {
        "analysis"
    } else {
        "info"
    }
}

/// Print one tool's description and parameter table.
fn print_tool_help(def: &ToolDefinition) {
    println!("tracedecay tool {}", short_name(&def.name));
    println!();
    println!("{}", def.description);
    println!();

    let Some(props) = def
        .input_schema
        .get("properties")
        .and_then(Value::as_object)
    else {
        println!("(no parameters)");
        return;
    };
    let required: std::collections::HashSet<&str> = def
        .input_schema
        .get("required")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();

    if props.is_empty() {
        println!("(no parameters)");
        return;
    }

    println!("Parameters:");
    let mut entries: Vec<(&String, &Value)> = props.iter().collect();
    entries.sort_by_key(|(k, _)| (*k).clone());
    for (key, schema) in entries {
        let ty = schema
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("string");
        let req = if required.contains(key.as_str()) {
            "required"
        } else {
            "optional"
        };
        let desc = schema
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("");
        println!(
            "  --{:<26} {:<8} {:<8}  {}",
            key.replace('_', "-"),
            ty,
            req,
            desc
        );
    }
    println!();
    println!("Reserved flags: --json, --project <path>, --args <json|@file>, -h/--help");
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use serde_json::json;

    fn defs() -> Vec<ToolDefinition> {
        get_tool_definitions()
    }

    fn def(name: &str) -> ToolDefinition {
        defs()
            .into_iter()
            .find(|d| d.name == format!("tracedecay_{name}"))
            .unwrap()
    }

    #[test]
    fn canonicalizes_alias_and_strip_prefix() {
        assert_eq!(canonical_tool_name("query"), "tracedecay_search");
        assert_eq!(
            canonical_tool_name("tracedecay_search"),
            "tracedecay_search"
        );
        assert_eq!(canonical_tool_name("dead-code"), "tracedecay_dead_code");
    }

    #[test]
    fn parses_positional_required_string() {
        let d = def("search");
        let parsed = parse_invocation(&d, &["foo".to_string()]).unwrap();
        assert_eq!(parsed.tool_args, json!({ "query": "foo" }));
    }

    #[test]
    fn coerces_integer_flag() {
        let d = def("search");
        let parsed = parse_invocation(
            &d,
            &["foo".to_string(), "--limit".to_string(), "25".to_string()],
        )
        .unwrap();
        assert_eq!(parsed.tool_args, json!({ "query": "foo", "limit": 25 }));
    }

    #[test]
    fn rejects_non_numeric_flag() {
        let d = def("search");
        let err = parse_invocation(
            &d,
            &["foo".to_string(), "--limit".to_string(), "abc".to_string()],
        )
        .unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("number") || msg.contains("integer"),
            "got: {msg}"
        );
    }

    #[test]
    fn coerces_boolean_flag() {
        let d = def("context");
        let parsed = parse_invocation(
            &d,
            &[
                "describe X".to_string(),
                "--include-code".to_string(),
                "true".to_string(),
            ],
        )
        .unwrap();
        assert_eq!(parsed.tool_args["include_code"], json!(true));
    }

    #[test]
    fn missing_required_errors() {
        let d = def("search");
        let err = parse_invocation(&d, &[]).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("missing required parameter"), "got: {msg}");
    }

    #[test]
    fn args_escape_hatch() {
        let d = def("search");
        let parsed = parse_invocation(
            &d,
            &[
                "--args".to_string(),
                r#"{"query":"foo","limit":3}"#.to_string(),
            ],
        )
        .unwrap();
        assert_eq!(parsed.tool_args["query"], json!("foo"));
        assert_eq!(parsed.tool_args["limit"], json!(3));
    }

    #[test]
    fn args_escape_hatch_reads_at_file() {
        let d = def("search");
        let dir = std::env::temp_dir().join(format!("ts-args-at-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Payload comfortably above Linux's 128 KiB MAX_ARG_STRLEN to prove
        // the @file path carries what a literal argv string cannot.
        let big = "x".repeat(200 * 1024);
        let path = dir.join("payload.json");
        std::fs::write(&path, format!(r#"{{"query":"{big}","limit":7}}"#)).unwrap();
        let parsed =
            parse_invocation(&d, &["--args".to_string(), format!("@{}", path.display())]).unwrap();
        assert_eq!(parsed.tool_args["limit"], json!(7));
        assert_eq!(
            parsed.tool_args["query"].as_str().map(str::len),
            Some(big.len())
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn args_escape_hatch_missing_at_file_errors() {
        let d = def("search");
        let err = parse_invocation(
            &d,
            &[
                "--args".to_string(),
                "@/nonexistent/tracedecay-args.json".to_string(),
            ],
        )
        .unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("failed to read @"), "got: {msg}");
    }

    #[test]
    fn reserved_flags_extracted() {
        let d = def("search");
        let parsed = parse_invocation(
            &d,
            &[
                "foo".to_string(),
                "--json".to_string(),
                "--project".to_string(),
                "/tmp/x".to_string(),
            ],
        )
        .unwrap();
        assert!(parsed.raw_json);
        assert_eq!(parsed.project.as_deref(), Some("/tmp/x"));
    }

    #[test]
    fn help_flag_short_circuits() {
        let d = def("search");
        let parsed = parse_invocation(&d, &["--help".to_string()]).unwrap();
        assert!(parsed.show_help);
    }

    #[test]
    fn unknown_tool_name_errors() {
        // canonical_tool_name only normalises — unknown names are caught by
        // the lookup in run(). Simulate the lookup here.
        let canonical = canonical_tool_name("totally-fake-tool");
        let found = defs().into_iter().any(|d| d.name == canonical);
        assert!(!found);
    }

    #[test]
    fn array_value_collected_via_repetition() {
        let d = def("context");
        let parsed = parse_invocation(
            &d,
            &[
                "x".to_string(),
                "--keywords".to_string(),
                "auth".to_string(),
                "--keywords".to_string(),
                "login".to_string(),
            ],
        )
        .unwrap();
        // After parse, the second occurrence wraps into an array. finalize is
        // only called via the run path; here we just observe the merged shape.
        let kw = &parsed.tool_args["keywords"];
        assert!(kw.is_array(), "expected array, got {kw}");
        let arr = kw.as_array().unwrap();
        assert_eq!(arr.len(), 2);
    }

    #[test]
    fn finalize_arrays_splits_csv() {
        let d = def("context");
        let mut map = Map::new();
        map.insert("keywords".to_string(), json!("auth,login,session"));
        finalize_arrays(&d, &mut map);
        let arr = map["keywords"].as_array().unwrap();
        assert_eq!(arr.len(), 3);
        assert_eq!(arr[0], json!("auth"));
        assert_eq!(arr[2], json!("session"));
    }

    #[test]
    fn profile_scoped_lcm_dispatch_detects_allowlisted_tool_and_scope() {
        assert!(is_profile_scoped_lcm_dispatch(
            "tracedecay_lcm_status",
            &json!({"storage_scope": "hermes_profile"})
        ));
    }

    #[test]
    fn profile_scoped_lcm_dispatch_rejects_non_profile_or_non_lcm_calls() {
        assert!(!is_profile_scoped_lcm_dispatch(
            "tracedecay_lcm_status",
            &json!({"storage_scope": "project_local"})
        ));
        assert!(!is_profile_scoped_lcm_dispatch(
            "tracedecay_status",
            &json!({"storage_scope": "hermes_profile"})
        ));
    }

    // Registry integrity guardrail (companion to the handler lockstep tests in
    // `mcp::tools::handlers`): the CLI routes profile-scoped LCM calls through
    // `is_profile_scoped_lcm_dispatch`, which consults the hand-maintained
    // `PROFILE_SCOPED_LCM_TOOLS` const. Any tool the MCP registry advertises as
    // profile-scoped (storage_scope enum including `hermes_profile`) must also
    // appear here, or its CLI invocations silently fall through to project
    // initialization instead of profile-scoped dispatch. This fails in both
    // directions when the const drifts from the registry.
    #[test]
    fn cli_profile_scoped_lcm_allowlist_matches_registry() {
        use std::collections::BTreeSet;

        let registry_profile_scoped: BTreeSet<String> = get_tool_definitions()
            .into_iter()
            .filter(|tool| {
                tool.input_schema["properties"]["storage_scope"]["enum"]
                    .as_array()
                    .is_some_and(|values| values.iter().any(|value| value == "hermes_profile"))
            })
            .map(|tool| tool.name)
            .collect();
        let cli_allowlist: BTreeSet<String> = PROFILE_SCOPED_LCM_TOOLS
            .iter()
            .map(|s| s.to_string())
            .collect();

        let missing_from_cli: Vec<String> = registry_profile_scoped
            .difference(&cli_allowlist)
            .cloned()
            .collect();
        assert!(
            missing_from_cli.is_empty(),
            "profile-scoped MCP tools missing from CLI PROFILE_SCOPED_LCM_TOOLS allowlist \
             (those calls would fall through to project init): {missing_from_cli:?}"
        );
        let stale_in_cli: Vec<String> = cli_allowlist
            .difference(&registry_profile_scoped)
            .cloned()
            .collect();
        assert!(
            stale_in_cli.is_empty(),
            "CLI PROFILE_SCOPED_LCM_TOOLS allowlist references tools no longer registered as \
             profile-scoped in the MCP registry: {stale_in_cli:?}"
        );
    }
}
