use std::io::Read;
use std::path::PathBuf;

use serde_json::{Map, Value};

use tracedecay::errors::{Result, TraceDecayError};
use tracedecay::mcp::tools::{ToolDefinition, short_tool_name};

/// Old CLI command names that don't match the MCP tool name. Keeps muscle
/// memory working for the seven removed top-level commands. The right-hand
/// side is the canonical MCP suffix (without the `tracedecay_` prefix).
const NAME_ALIASES: &[(&str, &str)] = &[("query", "search")];
/// Result of CLI argument parsing: the JSON value to hand to the MCP handler,
/// plus the reserved-flag side-effects.
#[cfg_attr(test, derive(Debug))]
pub(crate) struct ParsedInvocation {
    pub(crate) tool_args: Value,
    pub(crate) project: Option<String>,
    pub(crate) raw_json: bool,
    pub(crate) dry_run: bool,
    pub(crate) show_help: bool,
}

/// Normalize a user-supplied tool name to the canonical `tracedecay_<suffix>`
/// form used by the MCP registry. Accepts aliases (e.g. `query` → `search`),
/// strips a leading `tracedecay_` if present, and converts dashes to
/// underscores so `dead-code` and `dead_code` both work.
pub(crate) fn canonical_tool_name(raw: &str) -> String {
    let trimmed = raw.strip_prefix("tracedecay_").unwrap_or(raw);
    let normalized = trimmed.replace('-', "_");
    let mapped = NAME_ALIASES
        .iter()
        .find(|(k, _)| *k == normalized)
        .map_or(normalized.as_str(), |(_, v)| *v);
    format!("tracedecay_{mapped}")
}

/// Parse CLI args against the tool's JSON Schema. Returns the JSON object to
/// hand to the handler, plus side-effects from reserved flags.
pub(crate) fn parse_invocation(def: &ToolDefinition, args: &[String]) -> Result<ParsedInvocation> {
    // stdin is a single-shot stream: memoize the first read so that referencing
    // it more than once in one invocation (e.g. `--field-a @- --field-b @-`)
    // yields the piped payload each time instead of silently emptying after the
    // first `read_to_string`.
    let mut cached: Option<String> = None;
    parse_invocation_with_stdin(def, args, || {
        if let Some(cached) = &cached {
            return Ok(cached.clone());
        }
        let mut input = String::new();
        std::io::stdin()
            .read_to_string(&mut input)
            .map_err(|e| TraceDecayError::Config {
                message: format!("failed to read stdin: {e}"),
            })?;
        cached = Some(input.clone());
        Ok(input)
    })
}

pub(super) fn parse_invocation_with_stdin(
    def: &ToolDefinition,
    args: &[String],
    mut read_stdin: impl FnMut() -> Result<String>,
) -> Result<ParsedInvocation> {
    let schema_properties = def
        .input_schema
        .get("properties")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();

    let required = schema_required_keys(def);

    let mut out = ParsedInvocation {
        tool_args: Value::Object(Map::new()),
        project: None,
        raw_json: false,
        dry_run: false,
        show_help: false,
    };

    let mut explicit_args: Option<Value> = None;
    let mut collected: Map<String, Value> = Map::new();
    let mut positionals: Vec<String> = Vec::new();

    let mut iter = args.iter();
    while let Some(raw) = iter.next() {
        // GNU-style `--flag=value` is accepted everywhere clap is, so accept
        // it here too: split once on `=` and treat the remainder as the value.
        let (flag_part, inline_value): (&str, Option<&str>) = if raw.starts_with("--") {
            match raw.split_once('=') {
                Some((flag, value)) => (flag, Some(value)),
                None => (raw.as_str(), None),
            }
        } else {
            (raw.as_str(), None)
        };
        match flag_part {
            "-h" | "--help" => {
                out.show_help = true;
                return Ok(out);
            }
            "--json" => out.raw_json = true,
            "--dry-run" if !schema_properties.contains_key("dry_run") => out.dry_run = true,
            "--project" => {
                out.project = Some(take_flag_value(&mut iter, "--project", inline_value)?);
            }
            "--args" => {
                // `--args` is a whole-payload arg: inline JSON, `-` for stdin,
                // or a file path (bare or `@`-prefixed). Reading from a file or
                // stdin sidesteps the kernel's per-argv-string cap
                // (MAX_ARG_STRLEN, 128 KiB on Linux) for large payloads.
                let raw_args = take_flag_value(&mut iter, "--args", inline_value)?;
                let json_str = resolve_args_payload(&raw_args, &mut read_stdin)?;
                let value: Value =
                    serde_json::from_str(&json_str).map_err(|e| TraceDecayError::Config {
                        message: format!(
                            "--args: invalid JSON: {e} — if the payload contains quotes or \
                             newlines, pipe it: tracedecay tool <name> --args - <<'JSON' … JSON"
                        ),
                    })?;
                if !value.is_object() {
                    return Err(TraceDecayError::Config {
                        message: "--args must be a JSON object — the same object you would \
                                  pass as MCP arguments, e.g. {\"query\":\"…\"}"
                            .to_string(),
                    });
                }
                explicit_args = Some(value);
            }
            flag if flag.starts_with("--") => {
                let key = flag.trim_start_matches('-').replace('-', "_");
                let prop_schema = schema_properties.get(&key);
                let is_boolean = prop_schema
                    .and_then(|p| p.get("type"))
                    .and_then(Value::as_str)
                    == Some("boolean");
                // A boolean flag's value is optional: `--helpful` alone means
                // `true`. Without this, a bare boolean flag would blindly
                // consume whatever token follows it as its value — including
                // the *next* flag (e.g. `--helpful --note x` would read
                // `--note` as `--helpful`'s value and error on it, or worse,
                // silently coerce it), which also drops that next flag from
                // parsing entirely. Only treat the following token as this
                // flag's value when it doesn't itself look like a flag.
                if is_boolean
                    && inline_value.is_none()
                    && iter
                        .clone()
                        .next()
                        .is_none_or(|next| next.starts_with("--"))
                {
                    merge_value(&mut collected, &key, Value::Bool(true));
                    continue;
                }
                let raw_value = take_flag_value(&mut iter, flag, inline_value)
                    .map_err(|_| missing_flag_value_error(flag, prop_schema))?;
                let resolved = resolve_at_file(&raw_value, &mut read_stdin)?;
                let coerced = coerce_value(&key, prop_schema, &resolved)?;
                merge_value(&mut collected, &key, coerced);
            }
            _ => {
                // A single-dash token matching a known property name is almost
                // certainly a typo'd flag; without this guard it would bind as
                // a positional and the error would point at the wrong token.
                if let Some(known) = single_dash_flag_typo(raw, &schema_properties) {
                    return Err(TraceDecayError::Config {
                        message: format!("unknown argument `{raw}` — did you mean `--{known}`?"),
                    });
                }
                positionals.push(raw.clone());
            }
        }
    }

    if let Some(mut value) = explicit_args {
        if !collected.is_empty() || !positionals.is_empty() {
            return Err(TraceDecayError::Config {
                message: "--args cannot be combined with other tool flags or positionals — \
                          either put everything in --args, or use only --key value flags"
                    .to_string(),
            });
        }
        if let Some(payload) = value.as_object_mut() {
            normalize_legacy_tool_args(def, payload)?;
            validate_tool_args(def, payload)?;
        }
        out.tool_args = value;
        return Ok(out);
    }

    bind_positionals(
        def,
        &schema_properties,
        &required,
        &mut collected,
        positionals,
        &mut read_stdin,
    )?;

    finalize_arrays(def, &mut collected);
    normalize_legacy_tool_args(def, &mut collected)?;
    validate_tool_args(def, &collected)?;
    out.tool_args = Value::Object(collected);
    Ok(out)
}

fn normalize_legacy_tool_args(def: &ToolDefinition, args: &mut Map<String, Value>) -> Result<()> {
    if def.name != "tracedecay_fact_store" {
        return Ok(());
    }
    let Some(fact_type) = args.remove("fact_type") else {
        return Ok(());
    };
    if let Some(category) = args.get("category") {
        if category != &fact_type {
            return Err(TraceDecayError::Config {
                message: "`fact_type` is a legacy alias for `category`; pass only `category`"
                    .to_string(),
            });
        }
    } else {
        args.insert("category".to_string(), fact_type);
    }
    Ok(())
}

/// Keys that integration layers inject into tool arguments for routing, read
/// by the dispatch layer (or the generated client itself) rather than being
/// declared per-tool in the schemas:
///
/// - `project_root` — registered-project selector alias accepted by dispatch
///   ([`crate::mcp::tools`] `rejected_tool_project_selector_present`).
/// - `response_handle_project_root` — LCM response-handle storage root when
///   the live project differs from the profile store.
/// - `cwd` — read client-side by the generated Hermes plugin for project
///   resolution and may be left in the payload it forwards.
///
/// The validation gate skips these so schema-exact integrations keep working;
/// everything else unknown is a hard error.
const DISPATCH_ROUTING_KEYS: &[&str] = &["project_root", "response_handle_project_root", "cwd"];

/// One schema-driven validation pass over the *final* arguments object,
/// shared by the `--args` and per-key paths. Turns the silent divergences —
/// unknown keys forwarded and ignored, invalid enum values accepted, wrong
/// JSON types reaching handlers — into corrective errors that state the fix.
///
/// Schemas without `properties` are treated as opaque (no validation) so
/// dynamic or profile-scoped tools cannot be bricked by a stale walker.
fn validate_tool_args(def: &ToolDefinition, args: &Map<String, Value>) -> Result<()> {
    let Some(props) = def
        .input_schema
        .get("properties")
        .and_then(Value::as_object)
        .filter(|props| !props.is_empty())
    else {
        return Ok(());
    };
    let short = short_tool_name(&def.name);

    let required = schema_required_keys(def);

    for (key, value) in args {
        let Some(schema) = props.get(key) else {
            if DISPATCH_ROUTING_KEYS.contains(&key.as_str()) {
                continue;
            }
            return Err(unknown_key_error(key, short, props, &required));
        };

        if value.is_null() && !required.contains(key) {
            continue;
        }

        if let Some(allowed) = schema.get("enum").and_then(Value::as_array)
            && !allowed.iter().any(|candidate| candidate == value)
        {
            let allowed: Vec<String> = allowed
                .iter()
                .map(|v| match v {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
                .collect();
            let displayed = value
                .as_str()
                .map_or_else(|| value.to_string(), str::to_string);
            return Err(TraceDecayError::Config {
                message: format!(
                    "--{}: `{displayed}` is not one of: {}",
                    key.replace('_', "-"),
                    allowed.join(", ")
                ),
            });
        }

        if let Some(expected) = schema.get("type").and_then(Value::as_str) {
            if !value_matches_type(value, expected) {
                let flag = key.replace('_', "-");
                let hint = if matches!(expected, "array" | "object") {
                    format!(
                        " Pass JSON (e.g. --{flag} '<json>'), {}",
                        heredoc_hint(short)
                    )
                } else {
                    String::new()
                };
                return Err(TraceDecayError::Config {
                    message: format!(
                        "--{flag} expects a JSON {expected}, got {}.{hint}",
                        json_type_name(value)
                    ),
                });
            }
            check_array_elements(key, short, schema, value)?;
        }
    }

    // Single required-key check for both `--args` and per-key paths.
    for req in &required {
        if !args.contains_key(req) {
            let usage: String = required
                .iter()
                .map(|r| format!(" --{} <value>", r.replace('_', "-")))
                .collect();
            return Err(TraceDecayError::Config {
                message: format!(
                    "missing required parameter `--{}` for tool `{short}` — \
                     e.g. tracedecay tool {short}{usage}",
                    req.replace('_', "-"),
                ),
            });
        }
    }
    Ok(())
}

fn heredoc_hint(short: &str) -> String {
    format!("or the whole payload via stdin: tracedecay tool {short} --args - <<'JSON' {{…}} JSON")
}

fn check_array_elements(key: &str, short: &str, schema: &Value, value: &Value) -> Result<()> {
    if schema.get("type").and_then(Value::as_str) != Some("array") {
        return Ok(());
    }
    let Some(items_type) = schema
        .get("items")
        .and_then(|items| items.get("type"))
        .and_then(Value::as_str)
    else {
        return Ok(());
    };
    if !matches!(items_type, "array" | "object") {
        return Ok(());
    }
    let Some(elements) = value.as_array() else {
        return Ok(());
    };
    if let Some(bad) = elements
        .iter()
        .find(|element| !value_matches_type(element, items_type))
    {
        let flag = key.replace('_', "-");
        return Err(TraceDecayError::Config {
            message: format!(
                "--{flag} expects a JSON array of {items_type}s, but an \
                 element is a {}. Pass JSON: --{flag} '<json>' — {}",
                json_type_name(bad),
                heredoc_hint(short)
            ),
        });
    }
    Ok(())
}

fn unknown_key_error(
    key: &str,
    short: &str,
    props: &Map<String, Value>,
    required: &[String],
) -> TraceDecayError {
    let suggestion = nearest_key(key, props)
        .map(|k| format!(" — did you mean `--{}`?", k.replace('_', "-")))
        .unwrap_or_default();
    let mut valid: Vec<String> = props
        .keys()
        .map(|k| {
            let flag = format!("--{}", k.replace('_', "-"));
            if required.contains(k) {
                format!("{flag} (required)")
            } else {
                flag
            }
        })
        .collect();
    valid.sort();
    TraceDecayError::Config {
        message: format!(
            "unknown parameter `--{}` for `{short}`{suggestion} Valid: {}",
            key.replace('_', "-"),
            valid.join(", ")
        ),
    }
}

/// True when a JSON value is acceptable for a schema `type` string. `number`
/// accepts integers; `integer` requires a whole number. Unknown schema types
/// validate as pass-through.
fn value_matches_type(value: &Value, expected: &str) -> bool {
    match expected {
        "string" => value.is_string(),
        "boolean" => value.is_boolean(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "number" => value.is_number(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        _ => true,
    }
}

fn json_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn schema_required_keys(def: &ToolDefinition) -> Vec<String> {
    def.input_schema
        .get("required")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

fn max_typo_distance(name: &str) -> usize {
    if name.len() > 6 { 3 } else { 2 }
}

fn nearest_by_edit_distance(
    target: &str,
    candidates: impl IntoIterator<Item = String>,
) -> Option<String> {
    let max_distance = max_typo_distance(target);
    candidates
        .into_iter()
        .map(|candidate| (edit_distance(target, &candidate), candidate))
        .filter(|(distance, _)| *distance <= max_distance)
        .min_by_key(|(distance, _)| *distance)
        .map(|(_, candidate)| candidate)
}

/// Nearest property name by edit distance, for did-you-mean suggestions.
fn nearest_key(key: &str, props: &Map<String, Value>) -> Option<String> {
    nearest_by_edit_distance(key, props.keys().cloned())
}

/// Nearest tool name (short form) for unknown-tool suggestions.
pub(crate) fn nearest_tool_name(canonical: &str, defs: &[ToolDefinition]) -> Option<String> {
    let target = short_tool_name(canonical);
    nearest_by_edit_distance(
        target,
        defs.iter()
            .map(|def| short_tool_name(&def.name).to_string()),
    )
}

/// Classic two-row Levenshtein distance; property and tool names are short so
/// the quadratic cost is irrelevant.
pub(super) fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr = vec![0; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        curr[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            curr[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(curr[j] + 1);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()]
}

/// A `-flag` (single dash) token whose name matches a known property is a
/// typo'd flag, not a positional. Returns the kebab-case flag name to suggest.
fn single_dash_flag_typo(raw: &str, props: &Map<String, Value>) -> Option<String> {
    let name = raw.strip_prefix('-')?;
    if name.is_empty() || raw.starts_with("--") {
        return None;
    }
    let key = name.replace('-', "_");
    props.contains_key(&key).then(|| key.replace('_', "-"))
}

/// Corrective error for a flag with no following value, stating the exact fix
/// (booleans get the `true`/`false` example verbatim).
fn missing_flag_value_error(flag: &str, prop_schema: Option<&Value>) -> TraceDecayError {
    let is_boolean = prop_schema
        .and_then(|p| p.get("type"))
        .and_then(Value::as_str)
        == Some("boolean");
    let message = if is_boolean {
        format!("flag `{flag}` requires a value — pass `{flag} true` or `{flag} false`")
    } else {
        format!("flag `{flag}` requires a value — write `{flag} <value>` or `{flag}=<value>`")
    };
    TraceDecayError::Config { message }
}

fn bind_positionals(
    def: &ToolDefinition,
    schema_properties: &Map<String, Value>,
    required: &[String],
    collected: &mut Map<String, Value>,
    positionals: Vec<String>,
    read_stdin: &mut impl FnMut() -> Result<String>,
) -> Result<()> {
    let mut positional_iter = positionals.into_iter();
    for req in required {
        if collected.contains_key(req) {
            continue;
        }
        let Some(prop) = schema_properties.get(req) else {
            continue;
        };
        let Some(value) = positional_iter.next() else {
            break;
        };
        let resolved = resolve_at_file(&value, read_stdin)?;
        let coerced = coerce_value(req, Some(prop), &resolved)?;
        collected.insert(req.clone(), coerced);
    }
    let leftover: Vec<String> = positional_iter.collect();
    if leftover.is_empty() {
        return Ok(());
    }
    Err(TraceDecayError::Config {
        message: format!(
            "unexpected positional argument(s): {} — use --key value flags or \
             run `tracedecay tool {} --help`",
            leftover.join(" "),
            short_tool_name(&def.name)
        ),
    })
}

/// Resolve a `--args` value to its JSON text. `--args` is a *whole-payload*
/// argument (the value IS the object), so it follows the same convention as
/// `memory curate --llm-ops`: `-` reads stdin and any non-inline value is a
/// file path — a plain path "just works" without the `@` sigil that per-key
/// values need. Inline JSON (starting `{`/`[`) is returned verbatim; `@file`
/// and `@-` stay valid as back-compat aliases so existing scripts keep working.
fn resolve_args_payload(
    raw: &str,
    read_stdin: &mut impl FnMut() -> Result<String>,
) -> Result<String> {
    let trimmed = raw.trim_start();
    if raw == "-" {
        read_stdin()
    } else if trimmed.starts_with('{') || trimmed.starts_with('[') {
        Ok(raw.to_string())
    } else if raw.starts_with('@') {
        resolve_at_file(raw, read_stdin)
    } else {
        std::fs::read_to_string(raw).map_err(|e| TraceDecayError::Config {
            message: format!(
                "--args: `{raw}` is not inline JSON, `-` (stdin), or a readable file: {e}"
            ),
        })
    }
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
            other => {
                let flag = key.replace('_', "-");
                Err(TraceDecayError::Config {
                    message: format!(
                        "--{flag}: expected a boolean (true/false), got `{other}` — \
                         pass `--{flag} true` or `--{flag} false`"
                    ),
                })
            }
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
        // Array/object params accept inline JSON per-key (`--replacements
        // '[["old","new"]]'`, `--project-selector '{"project_id":"x"}'`).
        // Non-JSON strings fall through unchanged: arrays keep the
        // comma-split/repetition behavior via `finalize_arrays`, and objects
        // are caught by `validate_tool_args` with a corrective error.
        "array" | "object" => {
            if let Ok(parsed) = serde_json::from_str::<Value>(raw)
                && value_matches_type(&parsed, ty)
            {
                return Ok(parsed);
            }
            Ok(Value::String(raw.to_string()))
        }
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
pub(super) fn finalize_arrays(def: &ToolDefinition, map: &mut Map<String, Value>) {
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

fn take_flag_value(
    iter: &mut std::slice::Iter<'_, String>,
    flag: &str,
    inline_value: Option<&str>,
) -> Result<String> {
    match inline_value {
        Some(value) => Ok(value.to_string()),
        None => take_value(iter, flag),
    }
}

/// Read a value from disk when it starts with `@`. The leading `@` is
/// stripped; the rest is treated as a path (relative to cwd). Plain values
/// pass through unchanged. To pass a literal `@` as the first character, use
/// `--args` instead.
fn resolve_at_file(raw: &str, read_stdin: &mut impl FnMut() -> Result<String>) -> Result<String> {
    if let Some(path) = raw.strip_prefix('@') {
        if path == "-" {
            return read_stdin();
        }
        let buf = PathBuf::from(path);
        std::fs::read_to_string(&buf).map_err(|e| TraceDecayError::Config {
            message: format!(
                "failed to read @{path}: {e} — the path is resolved relative to the current \
                 directory; for a literal value that begins with `@`, use --args instead"
            ),
        })
    } else {
        Ok(raw.to_string())
    }
}
