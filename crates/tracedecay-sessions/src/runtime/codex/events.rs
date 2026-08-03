//! Structured Codex telemetry ingestion.
//!
//! Codex rollouts carry far more than the `user_message`/`agent_message`
//! conversation turns: patch applications, shell (`exec_command`) tool calls,
//! plan updates, per-turn boundaries, MCP tool calls (including `TraceDecay`'s
//! own), web searches, and sub-agent routing. Before this module those lines
//! were dropped (`message_from_line` returned `None` for every non-message
//! `event_msg`, and generic `response_item` tool events were only cataloged as
//! opaque `tool_event` rows). Here they become compact, provider-neutral rows
//! with the shared kind vocabulary used by the Claude/Cursor ingestion work
//! (`file_edit`, `tool_call`, `plan`, `turn_boundary`, `web_search`,
//! `subagent_activity`).
//!
//! Guardrails:
//! * Text columns stay short (a command line, a stdout summary, a query). Heavy
//!   payloads (tool arguments, diffs) never land in `text`; they are either
//!   size-capped in `metadata_json` or reduced to a shape summary (a patch keeps
//!   `path`+`change_type`+`hunk_count`, never the diff body).
//! * `exec_command` exit code and wall time exist only as free text inside the
//!   `function_call_output` ("Process exited with code N", "Wall time: X
//!   seconds"). They are parsed out; when the marker is absent the value is
//!   `null` — never guessed.
//! * Encrypted inter-agent messages record only the routing edge
//!   (`author` → `recipient`, `encrypted: true`); the ciphertext is never
//!   stored or decoded.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use serde_json::{Map, Value};

use super::CodexMeta;
use crate::SessionMessageRecord;
use crate::runtime::shared::preview_truncated;

const PROVIDER: &str = "codex";
/// Command lines, plan step lists, and stdout summaries are clipped to this
/// many bytes so the searchable text stays compact.
const TEXT_PREVIEW_BYTES: usize = 1200;
/// Tool argument blobs (MCP invocations especially) are size-capped in
/// `metadata_json` at roughly this many bytes.
const ARG_METADATA_BYTES: usize = 2000;

/// One exec (`exec_command`) tool call awaiting its `function_call_output` so
/// the pair can be joined into a single `tool_call` row.
struct PendingExec {
    offset: i64,
    timestamp: Option<i64>,
    model: Option<String>,
    call_id: String,
    cmd: String,
    workdir: Option<String>,
    turn_id: Option<String>,
}

/// Session-level context/usage summary distilled from `turn_context` and
/// `token_count` lines (item 8 + item 9): the policy/effort posture and the
/// latest rate-limit snapshot. Stored on the [`super::SessionDraft`] metadata,
/// not as per-line rows.
#[derive(Default)]
pub(super) struct CodexSessionSummary {
    approval_policy: Option<String>,
    sandbox_policy: Option<String>,
    effort: Option<String>,
    models: BTreeSet<String>,
    model_context_window: Option<i64>,
    rate_limits: Option<Value>,
}

impl CodexSessionSummary {
    /// Insert the collected summary fields into a session `metadata_json` map.
    pub(super) fn apply(&self, map: &mut Map<String, Value>) {
        if let Some(policy) = &self.approval_policy {
            map.insert(
                "codex_approval_policy".to_string(),
                Value::String(policy.clone()),
            );
        }
        if let Some(sandbox) = &self.sandbox_policy {
            map.insert(
                "codex_sandbox_policy".to_string(),
                Value::String(sandbox.clone()),
            );
        }
        if let Some(effort) = &self.effort {
            map.insert("codex_effort".to_string(), Value::String(effort.clone()));
        }
        if !self.models.is_empty() {
            map.insert(
                "codex_models".to_string(),
                Value::Array(self.models.iter().cloned().map(Value::String).collect()),
            );
        }
        if let Some(window) = self.model_context_window {
            map.insert(
                "codex_model_context_window".to_string(),
                Value::from(window),
            );
        }
        if let Some(rate_limits) = &self.rate_limits {
            map.insert("codex_rate_limits".to_string(), rate_limits.clone());
        }
    }
}

/// Streaming state for structured Codex event ingestion across one parse pass.
#[derive(Default)]
pub(super) struct CodexStructuredState {
    pending_exec: HashMap<String, PendingExec>,
    pub(super) summary: CodexSessionSummary,
}

impl CodexStructuredState {
    pub(super) fn new() -> Self {
        Self::default()
    }

    /// Harvest session-level summary fields from any line. Non-consuming: the
    /// caller still routes `turn_context`/`token_count` lines to their own
    /// handlers. Safe to call on every line.
    pub(super) fn observe_summary(&mut self, record: &Value) {
        match record.get("type").and_then(Value::as_str) {
            Some("turn_context") => {
                let Some(payload) = record.get("payload") else {
                    return;
                };
                if let Some(policy) = string_field(payload, "approval_policy") {
                    self.summary.approval_policy = Some(policy);
                }
                if let Some(sandbox) = payload
                    .pointer("/sandbox_policy/type")
                    .and_then(Value::as_str)
                {
                    self.summary.sandbox_policy = Some(sandbox.to_string());
                }
                if let Some(effort) = string_field(payload, "effort").or_else(|| {
                    payload
                        .pointer("/collaboration_mode/settings/reasoning_effort")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                }) {
                    self.summary.effort = Some(effort);
                }
                if let Some(model) = string_field(payload, "model") {
                    self.summary.models.insert(model);
                }
            }
            Some("event_msg") => {
                let Some(info) = record
                    .get("payload")
                    .filter(|p| p.get("type").and_then(Value::as_str) == Some("token_count"))
                    .and_then(|p| p.get("info"))
                else {
                    return;
                };
                if let Some(window) = info.get("model_context_window").and_then(Value::as_i64) {
                    self.summary.model_context_window = Some(window);
                }
                if let Some(snapshot) = rate_limits_snapshot(info.get("rate_limits")) {
                    self.summary.rate_limits = Some(snapshot);
                }
            }
            _ => {}
        }
    }

    /// Route a rollout line to a structured handler. Returns:
    /// * `None` — not a structured line; the caller falls through to the
    ///   generic handlers.
    /// * `Some(rows)` — the line was recognized. `rows` may be empty (an
    ///   `exec_command` call is buffered until its output arrives; a recognized
    ///   but unusable line is consumed so it is not re-processed).
    pub(super) fn event_from_line(
        &mut self,
        record: &Value,
        meta: &CodexMeta,
        model: Option<&str>,
        path: &Path,
        offset: i64,
    ) -> Option<Vec<SessionMessageRecord>> {
        match record.get("type").and_then(Value::as_str)? {
            "response_item" => self.response_item_event(record, meta, model, path, offset),
            "event_msg" => {
                let payload = record.get("payload")?;
                let row = match payload.get("type").and_then(Value::as_str)? {
                    "patch_apply_end" => {
                        patch_apply_row(record, payload, meta, model, path, offset)
                    }
                    "task_started" | "task_complete" | "turn_aborted" => {
                        turn_boundary_row(record, payload, meta, model, path, offset)
                    }
                    "mcp_tool_call_end" => {
                        mcp_tool_call_row(record, payload, meta, model, path, offset)
                    }
                    "web_search_end" => web_search_row(record, payload, meta, model, path, offset),
                    "sub_agent_activity" => {
                        sub_agent_activity_row(record, payload, meta, model, path, offset)
                    }
                    _ => return None,
                };
                Some(row.into_iter().collect())
            }
            "inter_agent_communication" => {
                let payload = record.get("payload")?;
                Some(
                    inter_agent_row(record, payload, meta, model, path, offset)
                        .into_iter()
                        .collect(),
                )
            }
            _ => None,
        }
    }

    fn response_item_event(
        &mut self,
        record: &Value,
        meta: &CodexMeta,
        model: Option<&str>,
        path: &Path,
        offset: i64,
    ) -> Option<Vec<SessionMessageRecord>> {
        let payload = record.get("payload")?;
        match payload.get("type").and_then(Value::as_str)? {
            "function_call" => match payload.get("name").and_then(Value::as_str) {
                Some("exec_command") => {
                    self.buffer_exec_call(payload, model, offset, timestamp_of(record));
                    // Consumed: emission is deferred until the paired output.
                    Some(Vec::new())
                }
                Some("update_plan") => Some(
                    update_plan_row(payload, meta, model, path, offset, timestamp_of(record))
                        .into_iter()
                        .collect(),
                ),
                // Other function calls (apply_patch, custom tools, …) keep the
                // generic `tool_event` handling.
                _ => None,
            },
            "function_call_output" => {
                let call_id = payload.get("call_id").and_then(Value::as_str)?;
                let pending = self.pending_exec.remove(call_id)?;
                let output = payload.get("output").and_then(Value::as_str);
                Some(vec![exec_command_row(&pending, meta, path, output)])
            }
            // The Codex CLI's structured-tools update emits shell commands as a
            // `custom_tool_call` named `exec` whose `input` is a JS harness
            // (`const r = await tools.exec_command({…}); text(r.output);`),
            // paired with a `custom_tool_call_output`. Join the pair into the
            // same `tool_call` row shape the classic `exec_command` join emits so
            // the command text stays searchable. `apply_patch` and any other JS
            // stay on the generic byte-counted `tool_event` path.
            "custom_tool_call" => match payload.get("name").and_then(Value::as_str) {
                Some("exec" | "exec_command") => {
                    if self.buffer_custom_exec_call(payload, model, offset, timestamp_of(record)) {
                        // Consumed: emission is deferred until the paired output.
                        Some(Vec::new())
                    } else {
                        // Not an `exec_command` harness (other JS) — generic path.
                        None
                    }
                }
                _ => None,
            },
            "custom_tool_call_output" => {
                let call_id = payload.get("call_id").and_then(Value::as_str)?;
                // Only outputs paired with a buffered custom exec call are ours;
                // everything else falls through to the generic path.
                let pending = self.pending_exec.remove(call_id)?;
                let output = custom_tool_output_text(payload.get("output"));
                Some(vec![exec_command_row(
                    &pending,
                    meta,
                    path,
                    output.as_deref(),
                )])
            }
            _ => None,
        }
    }

    fn buffer_exec_call(
        &mut self,
        payload: &Value,
        model: Option<&str>,
        offset: i64,
        timestamp: Option<i64>,
    ) {
        let Some(call_id) = payload.get("call_id").and_then(Value::as_str) else {
            return;
        };
        let args = parse_arguments(payload.get("arguments"));
        let Some(cmd) = args
            .as_ref()
            .and_then(|a| command_string(a.get("cmd")))
            .filter(|cmd| !cmd.is_empty())
        else {
            return;
        };
        let workdir = args
            .as_ref()
            .and_then(|a| a.get("workdir").and_then(Value::as_str))
            .map(str::to_string);
        let turn_id = payload
            .pointer("/internal_chat_message_metadata_passthrough/turn_id")
            .and_then(Value::as_str)
            .map(str::to_string);
        self.pending_exec.insert(
            call_id.to_string(),
            PendingExec {
                offset,
                timestamp,
                model: model.map(str::to_string),
                call_id: call_id.to_string(),
                cmd,
                workdir,
                turn_id,
            },
        );
    }

    /// Buffer a `custom_tool_call` shell invocation (the new Codex CLI shape).
    /// The command lives inside the JS harness `input` as the argument to
    /// `tools.exec_command( … )`. Returns `true` when the line is recognized as
    /// an exec call (buffered for its output); `false` leaves it on the generic
    /// `tool_event` path (`apply_patch`, or JS that is not an `exec_command`
    /// call).
    fn buffer_custom_exec_call(
        &mut self,
        payload: &Value,
        model: Option<&str>,
        offset: i64,
        timestamp: Option<i64>,
    ) -> bool {
        let Some(call_id) = payload.get("call_id").and_then(Value::as_str) else {
            return false;
        };
        let Some(input) = payload.get("input").and_then(Value::as_str) else {
            return false;
        };
        // `None` — no `tools.exec_command(` call in the harness (other JS); the
        // caller leaves the line on the generic path. `Some(inv)` — an exec call
        // was found; `cmd`/`workdir` may still be `None` when the argument could
        // not be extracted (fields fall back to null, never guessed).
        let Some(inv) = extract_exec_command_args(input) else {
            return false;
        };
        let cmd = inv.cmd.unwrap_or_default();
        let workdir = inv.workdir;
        let turn_id = payload
            .pointer("/internal_chat_message_metadata_passthrough/turn_id")
            .and_then(Value::as_str)
            .map(str::to_string);
        self.pending_exec.insert(
            call_id.to_string(),
            PendingExec {
                offset,
                timestamp,
                model: model.map(str::to_string),
                call_id: call_id.to_string(),
                cmd,
                workdir,
                turn_id,
            },
        );
        true
    }

    /// Emit `tool_call` rows for any `exec_command` calls whose output never
    /// arrived in this pass (still running, or truncated at a chunk boundary),
    /// so the call is never silently dropped. Rows are returned in call order;
    /// the caller annotates and appends them.
    pub(super) fn flush_pending(
        &mut self,
        meta: &CodexMeta,
        path: &Path,
    ) -> Vec<SessionMessageRecord> {
        let mut pending: Vec<PendingExec> = self.pending_exec.drain().map(|(_, v)| v).collect();
        pending.sort_by_key(|p| p.offset);
        pending
            .iter()
            .map(|exec| exec_command_row(exec, meta, path, None))
            .collect()
    }
}

fn string_field(payload: &Value, key: &str) -> Option<String> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn timestamp_of(record: &Value) -> Option<i64> {
    super::timestamp_from_record(record)
}

/// Reduce a `rate_limits` object to the compact snapshot we keep (item 9):
/// primary/secondary `used_percent` + `resets_at`, and `plan_type`.
fn rate_limits_snapshot(rate_limits: Option<&Value>) -> Option<Value> {
    let rate_limits = rate_limits?.as_object()?;
    let mut snapshot = Map::new();
    for key in ["primary", "secondary"] {
        if let Some(window) = rate_limits.get(key).and_then(Value::as_object) {
            let mut entry = Map::new();
            if let Some(used) = window.get("used_percent").cloned() {
                entry.insert("used_percent".to_string(), used);
            }
            if let Some(resets) = window.get("resets_at").cloned() {
                entry.insert("resets_at".to_string(), resets);
            }
            if !entry.is_empty() {
                snapshot.insert(key.to_string(), Value::Object(entry));
            }
        }
    }
    if let Some(plan) = rate_limits
        .get("plan_type")
        .filter(|p| !p.is_null())
        .cloned()
    {
        snapshot.insert("plan_type".to_string(), plan);
    }
    (!snapshot.is_empty()).then_some(Value::Object(snapshot))
}

/// Parse the JSON `arguments` blob carried on a `function_call` (Codex encodes
/// it as a JSON *string*, occasionally as an inline object).
fn parse_arguments(arguments: Option<&Value>) -> Option<Value> {
    match arguments {
        Some(Value::String(raw)) => serde_json::from_str::<Value>(raw).ok(),
        Some(value @ Value::Object(_)) => Some(value.clone()),
        _ => None,
    }
}

/// The shell command(s) and working directory extracted from a custom `exec`
/// tool's JS harness. Fields are `None` when they could not be recovered from
/// the harness text (never guessed).
struct ExecInvocation {
    cmd: Option<String>,
    workdir: Option<String>,
}

const EXEC_MARKER: &str = "tools.exec_command(";

/// Extract the shell command(s) from a custom `exec` tool's JS harness `input`.
///
/// The command is the `cmd` field of the object passed to
/// `tools.exec_command( … )`. That object is a *JavaScript* object literal, not
/// JSON: keys are frequently unquoted (`{cmd:"…"}`) and string values use
/// single, double, or backtick (template-literal) quotes. A single harness can
/// also batch several `exec_command` calls. So rather than a JSON parse (which
/// only covers the quoted-key minority), scan the literal tolerantly for the
/// top-level `cmd`/`workdir` string values.
///
/// Returns `None` when the harness contains no `exec_command` call (other JS) —
/// the caller then leaves the line on the generic `tool_event` path. When a call
/// is present but no `cmd` string can be recovered, the returned `cmd` is `None`
/// (the field falls back to null; nothing is guessed).
fn extract_exec_command_args(input: &str) -> Option<ExecInvocation> {
    if !input.contains(EXEC_MARKER) {
        return None;
    }
    let bytes = input.as_bytes();
    let mut cmds: Vec<String> = Vec::new();
    let mut workdir: Option<String> = None;
    let mut found_exec = false;
    let mut search = 0;
    while let Some(marker) = find_exec_marker(input, search) {
        found_exec = true;
        let after_marker = marker + EXEC_MARKER.len();
        // Skip whitespace to the opening brace of the argument object.
        let mut i = after_marker;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i < bytes.len() && bytes[i] == b'{' {
            let (cmd, wd, next) = scan_js_exec_object(input, i);
            if let Some(cmd) = cmd {
                cmds.push(cmd);
            }
            if workdir.is_none() {
                workdir = wd;
            }
            search = next.max(after_marker);
        } else {
            search = after_marker;
        }
    }
    if !found_exec {
        return None;
    }
    let cmd = (!cmds.is_empty()).then(|| cmds.join("\n"));
    Some(ExecInvocation { cmd, workdir })
}

/// Find an executed `tools.exec_command(` marker outside JS strings and
/// comments. Plain substring search misclassifies examples or comments that
/// merely mention the call shape as real shell execution.
fn find_exec_marker(input: &str, mut i: usize) -> Option<usize> {
    let bytes = input.as_bytes();
    while i < input.len() {
        match bytes[i] {
            b'"' | b'\'' | b'`' => {
                i = read_js_string(input, i).map_or(i + 1, |(_, next)| next);
            }
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                i += 2;
                while i < input.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < input.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i = (i + 2).min(input.len());
            }
            _ if input[i..].starts_with(EXEC_MARKER)
                && (i == 0 || !is_js_identifier_byte(bytes[i - 1])) =>
            {
                return Some(i);
            }
            _ => {
                i += input[i..].chars().next()?.len_utf8();
            }
        }
    }
    None
}

fn is_js_identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$')
}

/// Walk the JS object literal beginning at `brace_idx` (`{`), returning the
/// object. Nested objects/arrays and string contents (single/double/backtick)
/// are skipped so only genuine top-level keys are matched.
fn scan_js_exec_object(s: &str, brace_idx: usize) -> (Option<String>, Option<String>, usize) {
    let bytes = s.as_bytes();
    let mut cmd = None;
    let mut workdir = None;
    let mut i = brace_idx + 1; // past '{'
    loop {
        i = skip_ws_and_commas(s, i);
        if i >= s.len() || bytes[i] == b'}' {
            return (cmd, workdir, (i + 1).min(s.len()));
        }
        let (key, after_key) = read_js_object_key(s, i);
        i = skip_ws(s, after_key);
        // A malformed pair (no `:`) — bail rather than risk spinning.
        if i >= s.len() || bytes[i] != b':' {
            return (cmd, workdir, i);
        }
        i = skip_ws(s, i + 1);
        if i < s.len() && matches!(bytes[i], b'"' | b'\'' | b'`') {
            let Some((value, next)) = read_js_string(s, i) else {
                return (cmd, workdir, i);
            };
            match key.as_str() {
                "cmd" if cmd.is_none() => cmd = Some(value),
                "workdir" if workdir.is_none() => workdir = Some(value),
                _ => {}
            }
            i = next;
        } else {
            i = skip_js_value(s, i);
        }
    }
}

/// Read an object key at `i`: a quoted string, or a bare identifier up to the
/// next whitespace or `:`.
fn read_js_object_key(s: &str, i: usize) -> (String, usize) {
    let bytes = s.as_bytes();
    if i < s.len() && matches!(bytes[i], b'"' | b'\'' | b'`') {
        if let Some((key, next)) = read_js_string(s, i) {
            return (key, next);
        }
    }
    let mut j = i;
    while j < s.len() && !bytes[j].is_ascii_whitespace() && bytes[j] != b':' {
        j += 1;
    }
    (s[i..j].to_string(), j)
}

/// Read a JS string literal whose opening quote (`"`, `'`, or `` ` ``) is at
/// `open_idx`, honoring backslash escapes. Returns the decoded content and the
/// byte index just past the closing quote; `None` if the string is unterminated.
fn read_js_string(s: &str, open_idx: usize) -> Option<(String, usize)> {
    let quote = s[open_idx..].chars().next()?;
    let content_start = open_idx + quote.len_utf8();
    let mut out = String::new();
    let mut chars = s[content_start..].char_indices();
    while let Some((rel, c)) = chars.next() {
        if c == '\\' {
            if let Some((_, esc)) = chars.next() {
                out.push(match esc {
                    'n' => '\n',
                    't' => '\t',
                    'r' => '\r',
                    other => other,
                });
            }
        } else if c == quote {
            return Some((out, content_start + rel + c.len_utf8()));
        } else {
            out.push(c);
        }
    }
    None
}

/// Skip a non-string JS value (number, identifier, or a balanced object/array)
/// starting at `i`, returning the byte index just past it.
fn skip_js_value(s: &str, mut i: usize) -> usize {
    let bytes = s.as_bytes();
    if i >= s.len() {
        return i;
    }
    if matches!(bytes[i], b'{' | b'[') {
        let mut depth = 0usize;
        while i < s.len() {
            match bytes[i] {
                b'"' | b'\'' | b'`' => {
                    i = read_js_string(s, i).map_or(i + 1, |(_, next)| next);
                    continue;
                }
                b'{' | b'[' => depth += 1,
                b'}' | b']' => {
                    depth -= 1;
                    if depth == 0 {
                        return i + 1;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        return i;
    }
    // Bare token: run to the next top-level `,` or `}`.
    while i < s.len() && !matches!(bytes[i], b',' | b'}') {
        if matches!(bytes[i], b'"' | b'\'' | b'`') {
            i = read_js_string(s, i).map_or(i + 1, |(_, next)| next);
            continue;
        }
        i += 1;
    }
    i
}

fn skip_ws(s: &str, mut i: usize) -> usize {
    let bytes = s.as_bytes();
    while i < s.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

fn skip_ws_and_commas(s: &str, mut i: usize) -> usize {
    let bytes = s.as_bytes();
    while i < s.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b',') {
        i += 1;
    }
    i
}

/// Flatten a `custom_tool_call_output` `output` into one string for exit/wall
/// parsing. Codex emits it either as a JSON string or as an array of
/// `{ "type": "input_text", "text": … }` chunks; concatenate the chunk text.
fn custom_tool_output_text(output: Option<&Value>) -> Option<String> {
    match output? {
        Value::String(text) => Some(text.clone()),
        Value::Array(parts) => {
            let joined: String = parts
                .iter()
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect();
            (!joined.is_empty()).then_some(joined)
        }
        _ => None,
    }
}

/// A shell command is usually a string but can be an argv array; join arrays
/// with spaces so the searchable text is a single command line.
fn command_string(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(cmd) => Some(cmd.clone()),
        Value::Array(parts) => {
            let joined = parts
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" ");
            (!joined.is_empty()).then_some(joined)
        }
        _ => None,
    }
}

/// Extract `exit_code` and `wall_time_s` from an `exec_command` output body.
/// Both live only as free text; an absent marker yields `None` (never guessed).
///
/// Codex wrappers put these markers in the status header that precedes the
/// `Output:` body, so parsing is restricted to that header. This keeps a
/// command whose *own* stdout prints "Process exited with code N" (or a wall
/// time line) from spoofing the exec result. Both wrappers are covered: the
/// classic `exec_command` output ("Process exited with code N", "Wall time: X
/// seconds") and the newer custom `exec` harness ("Script completed\nWall time
/// X seconds", which carries no exit code — so it stays null).
pub(super) fn parse_exec_output(output: &str) -> (Option<i64>, Option<f64>) {
    const EXIT_MARKER: &str = "Process exited with code ";
    const WALL_MARKER: &str = "Wall time";
    let header = output
        .split_once("\nOutput:\n")
        .map_or(output, |(head, _)| head);
    let exit_code = header
        .find(EXIT_MARKER)
        .and_then(|idx| parse_leading_int(&header[idx + EXIT_MARKER.len()..]));
    let wall_time_s = header.find(WALL_MARKER).and_then(|idx| {
        // Accept both "Wall time: X" (classic) and "Wall time X" (custom exec).
        let rest = header[idx + WALL_MARKER.len()..].trim_start();
        let rest = rest.strip_prefix(':').unwrap_or(rest).trim_start();
        parse_leading_float(rest)
    });
    (exit_code, wall_time_s)
}

fn parse_leading_int(text: &str) -> Option<i64> {
    let text = text.trim_start();
    let mut chars = text.chars();
    let mut digits = String::new();
    if let Some(first) = chars.clone().next() {
        if first == '-' {
            digits.push('-');
            chars.next();
        }
    }
    for ch in chars {
        if ch.is_ascii_digit() {
            digits.push(ch);
        } else {
            break;
        }
    }
    if digits.is_empty() || digits == "-" {
        None
    } else {
        digits.parse().ok()
    }
}

fn parse_leading_float(text: &str) -> Option<f64> {
    let digits: String = text
        .chars()
        .take_while(|ch| ch.is_ascii_digit() || *ch == '.')
        .collect();
    if digits.is_empty() {
        None
    } else {
        digits.parse().ok()
    }
}

#[allow(clippy::too_many_arguments)]
fn build_row(
    meta: &CodexMeta,
    model: Option<&str>,
    path: &Path,
    offset: i64,
    timestamp: Option<i64>,
    role: &str,
    kind: &str,
    text: String,
    tool_names: Option<String>,
    metadata: &Value,
) -> SessionMessageRecord {
    SessionMessageRecord {
        provider: PROVIDER.to_string(),
        message_id: format!("{}:{offset}", meta.session_id),
        session_id: meta.session_id.clone(),
        role: role.to_string(),
        timestamp,
        ordinal: offset,
        text,
        kind: Some(kind.to_string()),
        model: model.map(str::to_string),
        tool_names,
        source_path: Some(path.to_string_lossy().to_string()),
        source_offset: Some(offset),
        metadata_json: serde_json::to_string(metadata).ok(),
    }
}

fn base_metadata(source: &str, source_event: &str) -> Map<String, Value> {
    let mut metadata = Map::new();
    metadata.insert("source".to_string(), Value::String(source.to_string()));
    metadata.insert(
        "source_event".to_string(),
        Value::String(source_event.to_string()),
    );
    metadata
}

fn exec_command_row(
    exec: &PendingExec,
    meta: &CodexMeta,
    path: &Path,
    output: Option<&str>,
) -> SessionMessageRecord {
    let (exit_code, wall_time_s) = output.map_or((None, None), parse_exec_output);
    let success = exit_code.map(|code| code == 0);

    let mut metadata = base_metadata("codex_exec_command", "exec_command");
    metadata.insert(
        "tool".to_string(),
        Value::String("exec_command".to_string()),
    );
    metadata.insert("call_id".to_string(), Value::String(exec.call_id.clone()));
    metadata.insert("cmd".to_string(), Value::String(exec.cmd.clone()));
    metadata.insert(
        "workdir".to_string(),
        exec.workdir.clone().map_or(Value::Null, Value::String),
    );
    if let Some(turn_id) = &exec.turn_id {
        metadata.insert("turn_id".to_string(), Value::String(turn_id.clone()));
    }
    metadata.insert(
        "exit_code".to_string(),
        exit_code.map_or(Value::Null, Value::from),
    );
    metadata.insert(
        "wall_time_s".to_string(),
        wall_time_s
            .and_then(serde_json::Number::from_f64)
            .map_or(Value::Null, Value::Number),
    );
    metadata.insert(
        "success".to_string(),
        success.map_or(Value::Null, Value::Bool),
    );
    if success == Some(true) {
        let candidates = output
            .map(|output| commit_candidates(&exec.cmd, output))
            .unwrap_or_default();
        if !candidates.produced.is_empty() {
            metadata.insert(
                "produced_commit_candidates".to_string(),
                Value::Array(candidates.produced.into_iter().map(Value::String).collect()),
            );
        }
        if !candidates.observed.is_empty() {
            metadata.insert(
                "observed_commit_candidates".to_string(),
                Value::Array(candidates.observed.into_iter().map(Value::String).collect()),
            );
        }
    }

    build_row(
        meta,
        exec.model.as_deref(),
        path,
        exec.offset,
        exec.timestamp,
        "tool",
        "tool_call",
        preview_truncated(&exec.cmd, TEXT_PREVIEW_BYTES),
        Some("exec_command".to_string()),
        &Value::Object(metadata),
    )
}

/// Commit refs mined from one exec result, split by evidence strength.
///
/// `produced` refs come from output a commit-creating command emits only when
/// it actually writes a commit (the `[branch sha] subject` line git prints for
/// `commit`/`merge`/`cherry-pick`/`revert`). `observed` refs come from a
/// read-only HEAD print (`git rev-parse HEAD`): that a session printed the
/// current HEAD is proof it *saw* the commit, never that it created it. A
/// pipeline like `git commit -m x; git rev-parse HEAD` exits 0 on the
/// rev-parse even when the commit failed, so the printed HEAD must never be
/// promoted to producer evidence.
#[derive(Debug, Default, PartialEq, Eq)]
struct CommitCandidates {
    produced: Vec<String>,
    observed: Vec<String>,
}

fn commit_candidates(command: &str, wrapped_output: &str) -> CommitCandidates {
    let creates_commit = command
        .split([';', '\n', '&', '|'])
        .any(segment_creates_commit);
    if !creates_commit {
        return CommitCandidates::default();
    }

    let output = wrapped_output
        .rsplit_once("\nOutput:\n")
        .map_or(wrapped_output, |(_, output)| output);
    let reports_head = command
        .split([';', '\n', '&', '|'])
        .any(segment_reports_head);
    let mut candidates = CommitCandidates::default();
    for line in output.lines() {
        let trimmed = line.trim();
        // The `[branch sha] subject` line is emitted only when a commit was
        // actually written, so it is genuine producer evidence.
        let bracket_candidate = trimmed
            .strip_prefix('[')
            .and_then(|line| line.split_once(']'))
            .and_then(|(header, _)| header.split_whitespace().next_back())
            .map(|token| token.trim_matches(|ch: char| !ch.is_ascii_hexdigit()));
        if let Some(candidate) = bracket_candidate {
            push_commit_candidate(&mut candidates.produced, candidate);
        }
        // A bare full-length sha is the output of a read-only HEAD print. It
        // only tells us the session observed the current HEAD.
        if reports_head
            && matches!(trimmed.len(), 40 | 64)
            && trimmed.chars().all(|ch| ch.is_ascii_hexdigit())
        {
            push_commit_candidate(&mut candidates.observed, trimmed);
        }
    }
    candidates
}

fn push_commit_candidate(candidates: &mut Vec<String>, candidate: &str) {
    if candidate.len() >= 7
        && candidate.chars().all(|ch| ch.is_ascii_hexdigit())
        && !candidates.iter().any(|known| known == candidate)
    {
        candidates.push(candidate.to_ascii_lowercase());
    }
}

fn segment_reports_head(segment: &str) -> bool {
    let tokens: Vec<_> = segment.split_whitespace().collect();
    tokens
        .windows(3)
        .any(|tokens| tokens == ["git", "rev-parse", "HEAD"])
}

fn segment_creates_commit(segment: &str) -> bool {
    let mut tokens = segment.split_whitespace();
    let Some(git) = tokens.next() else {
        return false;
    };
    if git.trim_matches(['\'', '"']) != "git" {
        return false;
    }
    let mut subcommand = tokens.next();
    while matches!(subcommand, Some("-C" | "-c" | "--git-dir" | "--work-tree")) {
        let _ = tokens.next();
        subcommand = tokens.next();
    }
    matches!(
        subcommand,
        Some("commit" | "cherry-pick" | "revert" | "merge")
    ) || subcommand == Some("rebase") && tokens.any(|token| token == "--continue")
}

fn patch_apply_row(
    record: &Value,
    payload: &Value,
    meta: &CodexMeta,
    model: Option<&str>,
    path: &Path,
    offset: i64,
) -> Option<SessionMessageRecord> {
    let changes = payload.get("changes").and_then(Value::as_object);
    let mut files = Vec::new();
    if let Some(changes) = changes {
        let mut entries: Vec<(&String, &Value)> = changes.iter().collect();
        entries.sort_by(|a, b| a.0.cmp(b.0));
        for (file_path, change) in entries {
            let change_type = change
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("update");
            let hunk_count = change
                .get("unified_diff")
                .and_then(Value::as_str)
                .map_or(0, hunk_count);
            let mut entry = Map::new();
            entry.insert("path".to_string(), Value::String(file_path.clone()));
            entry.insert(
                "change_type".to_string(),
                Value::String(change_type.to_string()),
            );
            entry.insert("hunk_count".to_string(), Value::from(hunk_count as i64));
            files.push(Value::Object(entry));
        }
    }

    let success = payload.get("success").and_then(Value::as_bool);
    let stdout = payload
        .get("stdout")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let text = stdout.map_or_else(
        || {
            format!(
                "Codex patch applied: {} file{}",
                files.len(),
                if files.len() == 1 { "" } else { "s" }
            )
        },
        |stdout| preview_truncated(stdout, TEXT_PREVIEW_BYTES),
    );

    let mut metadata = base_metadata("codex_patch_apply", "patch_apply_end");
    insert_str(&mut metadata, "call_id", payload.get("call_id"));
    insert_str(&mut metadata, "turn_id", payload.get("turn_id"));
    if let Some(success) = success {
        metadata.insert("success".to_string(), Value::Bool(success));
    }
    metadata.insert("files".to_string(), Value::Array(files));

    Some(build_row(
        meta,
        model,
        path,
        offset,
        timestamp_of(record),
        "tool",
        "file_edit",
        text,
        Some("apply_patch".to_string()),
        &Value::Object(metadata),
    ))
}

/// Count unified-diff hunks (`@@ … @@` headers) without keeping the diff body.
fn hunk_count(unified_diff: &str) -> usize {
    unified_diff
        .lines()
        .filter(|line| line.starts_with("@@"))
        .count()
}

fn turn_boundary_row(
    record: &Value,
    payload: &Value,
    meta: &CodexMeta,
    model: Option<&str>,
    path: &Path,
    offset: i64,
) -> Option<SessionMessageRecord> {
    let event = payload.get("type").and_then(Value::as_str)?;
    let turn_id = payload
        .get("turn_id")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let (completed, text) = match event {
        "task_started" => (false, format!("Codex turn started: {turn_id}")),
        "task_complete" => (true, format!("Codex turn completed: {turn_id}")),
        "turn_aborted" => {
            let reason = payload
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            (false, format!("Codex turn aborted ({reason}): {turn_id}"))
        }
        _ => return None,
    };

    let mut metadata = base_metadata("codex_turn", event);
    insert_str(&mut metadata, "turn_id", payload.get("turn_id"));
    metadata.insert("completed".to_string(), Value::Bool(completed));
    insert_i64(&mut metadata, "duration_ms", payload.get("duration_ms"));
    insert_i64(
        &mut metadata,
        "time_to_first_token_ms",
        payload.get("time_to_first_token_ms"),
    );
    insert_i64(
        &mut metadata,
        "model_context_window",
        payload.get("model_context_window"),
    );
    insert_i64(&mut metadata, "started_at", payload.get("started_at"));
    insert_i64(&mut metadata, "completed_at", payload.get("completed_at"));
    insert_str(&mut metadata, "reason", payload.get("reason"));

    Some(build_row(
        meta,
        model,
        path,
        offset,
        timestamp_of(record),
        "system",
        "turn_boundary",
        text,
        None,
        &Value::Object(metadata),
    ))
}

fn mcp_tool_call_row(
    record: &Value,
    payload: &Value,
    meta: &CodexMeta,
    model: Option<&str>,
    path: &Path,
    offset: i64,
) -> Option<SessionMessageRecord> {
    let invocation = payload.get("invocation");
    let server = invocation
        .and_then(|inv| inv.get("server"))
        .and_then(Value::as_str);
    let tool = invocation
        .and_then(|inv| inv.get("tool"))
        .and_then(Value::as_str);
    let tool_names = match (server, tool) {
        (Some(server), Some(tool)) => format!("{server}:{tool}"),
        (None, Some(tool)) => tool.to_string(),
        (Some(server), None) => server.to_string(),
        (None, None) => return None,
    };

    let result = payload.get("result");
    let ok = result.map(|value| value.get("Ok").is_some());
    let error = result
        .and_then(|value| value.get("Err"))
        .and_then(Value::as_str)
        .map(|err| preview_truncated(err, ARG_METADATA_BYTES));

    let mut metadata = base_metadata("codex_mcp_tool_call", "mcp_tool_call_end");
    if let Some(server) = server {
        metadata.insert("server".to_string(), Value::String(server.to_string()));
    }
    if let Some(tool) = tool {
        metadata.insert("tool".to_string(), Value::String(tool.to_string()));
    }
    insert_str(&mut metadata, "call_id", payload.get("call_id"));
    insert_str(&mut metadata, "plugin_id", payload.get("plugin_id"));
    if let Some(arguments) = invocation.and_then(|inv| inv.get("arguments")) {
        let serialized = serde_json::to_string(arguments).unwrap_or_default();
        metadata.insert(
            "arguments".to_string(),
            Value::String(preview_truncated(&serialized, ARG_METADATA_BYTES)),
        );
    }
    if let Some(duration_ms) = duration_ms(payload.get("duration")) {
        metadata.insert("duration_ms".to_string(), Value::from(duration_ms));
    }
    if let Some(ok) = ok {
        metadata.insert("ok".to_string(), Value::Bool(ok));
    }
    if let Some(error) = error {
        metadata.insert("error".to_string(), Value::String(error));
    }

    Some(build_row(
        meta,
        model,
        path,
        offset,
        timestamp_of(record),
        "tool",
        "tool_call",
        tool_names.clone(),
        Some(tool_names),
        &Value::Object(metadata),
    ))
}

/// Codex encodes MCP call durations as `{ "secs": s, "nanos": n }`.
fn duration_ms(duration: Option<&Value>) -> Option<i64> {
    let duration = duration?.as_object()?;
    let secs = duration.get("secs").and_then(Value::as_i64).unwrap_or(0);
    let nanos = duration.get("nanos").and_then(Value::as_i64).unwrap_or(0);
    if duration.get("secs").is_none() && duration.get("nanos").is_none() {
        return None;
    }
    Some(secs.saturating_mul(1000) + nanos / 1_000_000)
}

fn web_search_row(
    record: &Value,
    payload: &Value,
    meta: &CodexMeta,
    model: Option<&str>,
    path: &Path,
    offset: i64,
) -> Option<SessionMessageRecord> {
    let query = payload
        .get("query")
        .and_then(Value::as_str)
        .or_else(|| payload.pointer("/action/query").and_then(Value::as_str))
        .filter(|q| !q.is_empty())?;

    let queries: Vec<Value> = payload
        .pointer("/action/queries")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(|q| Value::String(q.to_string()))
                .collect()
        })
        .unwrap_or_else(|| vec![Value::String(query.to_string())]);

    let mut metadata = base_metadata("codex_web_search", "web_search_end");
    insert_str(&mut metadata, "call_id", payload.get("call_id"));
    metadata.insert("queries".to_string(), Value::Array(queries));

    Some(build_row(
        meta,
        model,
        path,
        offset,
        timestamp_of(record),
        "tool",
        "web_search",
        preview_truncated(query, TEXT_PREVIEW_BYTES),
        Some("web_search".to_string()),
        &Value::Object(metadata),
    ))
}

fn sub_agent_activity_row(
    record: &Value,
    payload: &Value,
    meta: &CodexMeta,
    model: Option<&str>,
    path: &Path,
    offset: i64,
) -> Option<SessionMessageRecord> {
    let activity_kind = payload
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("activity");
    let agent_path = payload.get("agent_path").and_then(Value::as_str);
    let text = agent_path.map_or_else(
        || format!("Codex sub-agent {activity_kind}"),
        |agent_path| format!("Codex sub-agent {activity_kind}: {agent_path}"),
    );

    let mut metadata = base_metadata("codex_sub_agent_activity", "sub_agent_activity");
    insert_str(
        &mut metadata,
        "agent_thread_id",
        payload.get("agent_thread_id"),
    );
    insert_str(&mut metadata, "agent_path", payload.get("agent_path"));
    metadata.insert("kind".to_string(), Value::String(activity_kind.to_string()));
    insert_str(&mut metadata, "event_id", payload.get("event_id"));
    insert_i64(
        &mut metadata,
        "occurred_at_ms",
        payload.get("occurred_at_ms"),
    );

    Some(build_row(
        meta,
        model,
        path,
        offset,
        timestamp_of(record),
        "system",
        "subagent_activity",
        preview_truncated(&text, TEXT_PREVIEW_BYTES),
        None,
        &Value::Object(metadata),
    ))
}

fn inter_agent_row(
    record: &Value,
    payload: &Value,
    meta: &CodexMeta,
    model: Option<&str>,
    path: &Path,
    offset: i64,
) -> Option<SessionMessageRecord> {
    let author = payload.get("author").and_then(Value::as_str)?;
    let recipient = payload
        .get("recipient")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    // Inter-agent messages are encrypted end-to-end. Record only the routing
    // edge; the ciphertext (`encrypted_content`) is never stored or decoded.
    let encrypted = payload
        .get("encrypted_content")
        .and_then(Value::as_str)
        .is_some_and(|content| !content.is_empty());

    let mut metadata = base_metadata("codex_inter_agent", "inter_agent_communication");
    metadata.insert("author".to_string(), Value::String(author.to_string()));
    metadata.insert(
        "recipient".to_string(),
        Value::String(recipient.to_string()),
    );
    metadata.insert("encrypted".to_string(), Value::Bool(encrypted));
    if let Some(others) = payload.get("other_recipients").and_then(Value::as_array) {
        if !others.is_empty() {
            metadata.insert("other_recipients".to_string(), Value::Array(others.clone()));
        }
    }
    if let Some(trigger) = payload.get("trigger_turn").and_then(Value::as_bool) {
        metadata.insert("trigger_turn".to_string(), Value::Bool(trigger));
    }

    Some(build_row(
        meta,
        model,
        path,
        offset,
        timestamp_of(record),
        "system",
        "subagent_activity",
        format!("Codex agent message: {author} -> {recipient} (encrypted)"),
        None,
        &Value::Object(metadata),
    ))
}

fn update_plan_row(
    payload: &Value,
    meta: &CodexMeta,
    model: Option<&str>,
    path: &Path,
    offset: i64,
    timestamp: Option<i64>,
) -> Option<SessionMessageRecord> {
    let args = parse_arguments(payload.get("arguments"))?;
    let plan = args.get("plan").and_then(Value::as_array)?;
    let mut steps = Vec::new();
    let mut lines = Vec::new();
    for entry in plan {
        let step = entry.get("step").and_then(Value::as_str).unwrap_or("");
        if step.is_empty() {
            continue;
        }
        let status = entry
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("pending");
        lines.push(format!("[{status}] {step}"));
        let mut step_obj = Map::new();
        step_obj.insert("step".to_string(), Value::String(step.to_string()));
        step_obj.insert("status".to_string(), Value::String(status.to_string()));
        steps.push(Value::Object(step_obj));
    }
    if steps.is_empty() {
        return None;
    }

    let mut metadata = base_metadata("codex_update_plan", "update_plan");
    insert_str(&mut metadata, "call_id", payload.get("call_id"));
    insert_str(&mut metadata, "explanation", args.get("explanation"));
    metadata.insert("steps".to_string(), Value::Array(steps));

    Some(build_row(
        meta,
        model,
        path,
        offset,
        timestamp,
        "assistant",
        "plan",
        preview_truncated(&lines.join("\n"), TEXT_PREVIEW_BYTES),
        Some("update_plan".to_string()),
        &Value::Object(metadata),
    ))
}

fn insert_str(metadata: &mut Map<String, Value>, key: &str, value: Option<&Value>) {
    if let Some(text) = value.and_then(Value::as_str).filter(|s| !s.is_empty()) {
        metadata.insert(key.to_string(), Value::String(text.to_string()));
    }
}

fn insert_i64(metadata: &mut Map<String, Value>, key: &str, value: Option<&Value>) {
    if let Some(number) = value.and_then(Value::as_i64) {
        metadata.insert(key.to_string(), Value::from(number));
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::unreadable_literal)]
mod tests {
    use super::*;
    use serde_json::json;

    fn meta() -> CodexMeta {
        CodexMeta {
            cwd: std::path::PathBuf::from("/tmp/project"),
            session_id: "sess-1".to_string(),
            model: None,
            git: None,
            parent_session_id: None,
            is_subagent: false,
            agent_id: None,
            agent_nickname: None,
            agent_role: None,
            thread_source: None,
        }
    }

    fn metadata_of(record: &SessionMessageRecord) -> Value {
        serde_json::from_str(record.metadata_json.as_deref().unwrap()).unwrap()
    }

    #[test]
    fn parse_exec_output_reads_exit_code_and_wall_time() {
        let output = "Chunk ID: 9f149c\nWall time: 0.4325 seconds\nProcess exited with code 2\nOriginal token count: 37\nOutput:\nboom\n";
        let (exit, wall) = parse_exec_output(output);
        assert_eq!(exit, Some(2));
        assert_eq!(wall, Some(0.4325));
    }

    #[test]
    fn parse_exec_output_absent_markers_are_null_not_guessed() {
        // A successful MCP-style command wrapper: wall time but no exit marker.
        let (exit, wall) = parse_exec_output("Wall time: 0.1655 seconds\nOutput:\n[{\"ok\":true}]");
        assert_eq!(exit, None);
        assert_eq!(wall, Some(0.1655));
        // Nothing recognizable at all.
        let (exit, wall) = parse_exec_output("still running, streaming output...");
        assert_eq!(exit, None);
        assert_eq!(wall, None);
        // Exit code zero is success (distinct from an absent marker).
        let (exit, _) = parse_exec_output("Process exited with code 0\n");
        assert_eq!(exit, Some(0));
    }

    #[test]
    fn exec_command_call_and_output_join_into_one_tool_call_row() {
        let mut state = CodexStructuredState::new();
        let path = std::path::Path::new("/tmp/rollout.jsonl");
        let call = json!({
            "timestamp": "2026-06-24T20:23:38.800Z",
            "type": "response_item",
            "payload": {
                "type": "function_call",
                "name": "exec_command",
                "arguments": "{\"cmd\":\"rg -n MEMORY.md\",\"workdir\":\"/home/zack/projects/tracedecay\"}",
                "call_id": "call-1",
                "internal_chat_message_metadata_passthrough": {"turn_id": "turn-1"}
            }
        });
        // The call buffers, emitting nothing yet.
        let buffered = state
            .event_from_line(&call, &meta(), Some("gpt-5.5"), path, 100)
            .expect("exec call is a structured line");
        assert!(buffered.is_empty());

        let output = json!({
            "timestamp": "2026-06-24T20:23:38.857Z",
            "type": "response_item",
            "payload": {
                "type": "function_call_output",
                "call_id": "call-1",
                "output": "Wall time: 1.5000 seconds\nProcess exited with code 0\nOutput:\nok\n"
            }
        });
        let rows = state
            .event_from_line(&output, &meta(), Some("gpt-5.5"), path, 260)
            .expect("output completes the join");
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.role, "tool");
        assert_eq!(row.kind.as_deref(), Some("tool_call"));
        assert_eq!(row.text, "rg -n MEMORY.md");
        assert_eq!(row.tool_names.as_deref(), Some("exec_command"));
        // The joined row keys on the CALL offset (the call site), not the output.
        assert_eq!(row.ordinal, 100);
        let md = metadata_of(row);
        assert_eq!(md["tool"], "exec_command");
        assert_eq!(md["call_id"], "call-1");
        assert_eq!(md["cmd"], "rg -n MEMORY.md");
        assert_eq!(md["workdir"], "/home/zack/projects/tracedecay");
        assert_eq!(md["turn_id"], "turn-1");
        assert_eq!(md["exit_code"], 0);
        assert_eq!(md["wall_time_s"], 1.5);
        assert_eq!(md["success"], true);
    }

    #[test]
    fn successful_git_commit_output_records_only_resolvable_candidates() {
        let mut state = CodexStructuredState::new();
        let path = std::path::Path::new("/tmp/rollout.jsonl");
        let call = json!({
            "type": "response_item",
            "payload": {
                "type": "function_call",
                "name": "exec_command",
                "arguments": "{\"cmd\":\"git commit -m test && git rev-parse HEAD\"}",
                "call_id": "commit-call"
            }
        });
        state
            .event_from_line(&call, &meta(), None, path, 10)
            .unwrap();
        let output = json!({
            "type": "response_item",
            "payload": {
                "type": "function_call_output",
                "call_id": "commit-call",
                "output": "Chunk ID: deadbe\nProcess exited with code 0\nOutput:\n[main abcdef1] test\n0123456789abcdef0123456789abcdef01234567\n"
            }
        });
        let rows = state
            .event_from_line(&output, &meta(), None, path, 20)
            .unwrap();
        let md = metadata_of(&rows[0]);
        // The `[main abcdef1]` line is producer evidence; the bare HEAD sha that
        // `git rev-parse HEAD` prints is only an observation of current HEAD.
        assert_eq!(md["produced_commit_candidates"], json!(["abcdef1"]));
        assert_eq!(
            md["observed_commit_candidates"],
            json!(["0123456789abcdef0123456789abcdef01234567"])
        );
    }

    #[test]
    fn rev_parse_head_after_failed_commit_is_observed_not_produced() {
        // `;` keeps the process exit 0 on the rev-parse even though the commit
        // failed, so the printed old HEAD must never become producer evidence.
        let candidates = commit_candidates(
            "git commit -m x; git rev-parse HEAD",
            "Output:\nabcdef1234567890abcdef1234567890abcdef12",
        );
        assert!(candidates.produced.is_empty());
        assert_eq!(
            candidates.observed,
            vec!["abcdef1234567890abcdef1234567890abcdef12"]
        );
    }

    #[test]
    fn fast_forward_merge_head_print_is_observed_not_produced() {
        // A fast-forward merge creates no commit; the HEAD it prints belongs to
        // whichever branch it advanced to, not to this session.
        let candidates = commit_candidates(
            "git merge feature && git rev-parse HEAD",
            "Output:\nUpdating 1111111..2222222\nFast-forward\n2222222222222222222222222222222222222222",
        );
        assert!(candidates.produced.is_empty());
        assert_eq!(
            candidates.observed,
            vec!["2222222222222222222222222222222222222222"]
        );
    }

    #[test]
    fn failed_or_non_commit_commands_never_claim_commit_production() {
        assert!(
            commit_candidates("git status", "Output:\nabcdef1")
                .produced
                .is_empty()
        );
        assert!(
            commit_candidates("rg git commit", "Output:\n[main abcdef1] test")
                .produced
                .is_empty()
        );
        assert_eq!(
            commit_candidates(
                "git commit -m 0123456789abcdef0123456789abcdef01234567",
                "Output:\n[main abcdef1] 0123456789abcdef0123456789abcdef01234567"
            )
            .produced,
            vec!["abcdef1"]
        );
    }

    #[test]
    fn exec_command_without_output_flushes_a_null_result_row() {
        let mut state = CodexStructuredState::new();
        let path = std::path::Path::new("/tmp/rollout.jsonl");
        let call = json!({
            "type": "response_item",
            "payload": {
                "type": "function_call",
                "name": "exec_command",
                "arguments": "{\"cmd\":\"sleep 100\"}",
                "call_id": "call-9"
            }
        });
        state
            .event_from_line(&call, &meta(), None, path, 10)
            .expect("exec call recognized");
        let flushed = state.flush_pending(&meta(), path);
        assert_eq!(flushed.len(), 1);
        let md = metadata_of(&flushed[0]);
        assert_eq!(md["exit_code"], Value::Null);
        assert_eq!(md["success"], Value::Null);
        assert_eq!(flushed[0].text, "sleep 100");
    }

    #[test]
    fn custom_tool_call_exec_joins_into_one_tool_call_row() {
        // The new Codex CLI emits shell commands as a `custom_tool_call` named
        // `exec` whose `input` is a JS harness, paired with a
        // `custom_tool_call_output` whose `output` is an array of text chunks.
        let mut state = CodexStructuredState::new();
        let path = std::path::Path::new("/tmp/rollout.jsonl");
        let call = json!({
            "timestamp": "2026-07-09T17:50:49.017Z",
            "type": "response_item",
            "payload": {
                "type": "custom_tool_call",
                "id": "ctc_1",
                "status": "completed",
                "call_id": "call_abc",
                "name": "exec",
                "input": "const r = await tools.exec_command({\"cmd\":\"gh pr merge 366\",\"workdir\":\"/home/zack/projects/tracedecay\",\"yield_time_ms\":10000});\ntext(r.output);\n",
                "internal_chat_message_metadata_passthrough": {"turn_id": "turn-9"}
            }
        });
        let buffered = state
            .event_from_line(&call, &meta(), Some("gpt-5.5"), path, 100)
            .expect("custom exec call is structured");
        assert!(buffered.is_empty(), "call buffers, emits nothing yet");

        let output = json!({
            "timestamp": "2026-07-09T17:50:49.226Z",
            "type": "response_item",
            "payload": {
                "type": "custom_tool_call_output",
                "call_id": "call_abc",
                "output": [
                    {"type": "input_text", "text": "Script completed\nWall time 0.2 seconds\nOutput:\n"},
                    {"type": "input_text", "text": "Merged pull request #366\n"}
                ]
            }
        });
        let rows = state
            .event_from_line(&output, &meta(), Some("gpt-5.5"), path, 260)
            .expect("output completes the join");
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.role, "tool");
        assert_eq!(row.kind.as_deref(), Some("tool_call"));
        // The command text is searchable (the whole point of the fix).
        assert_eq!(row.text, "gh pr merge 366");
        assert_eq!(row.tool_names.as_deref(), Some("exec_command"));
        // Keyed on the call offset, not the output offset.
        assert_eq!(row.ordinal, 100);
        let md = metadata_of(row);
        assert_eq!(md["tool"], "exec_command");
        assert_eq!(md["call_id"], "call_abc");
        assert_eq!(md["cmd"], "gh pr merge 366");
        assert_eq!(md["workdir"], "/home/zack/projects/tracedecay");
        assert_eq!(md["turn_id"], "turn-9");
        // The custom harness header carries a wall time but no exit code, so the
        // exit code and success stay null (never guessed from the body).
        assert_eq!(md["wall_time_s"], 0.2);
        assert_eq!(md["exit_code"], Value::Null);
        assert_eq!(md["success"], Value::Null);
        // The output body itself is never stored in the searchable text.
        assert!(!row.text.contains("Merged pull request"));
    }

    #[test]
    fn custom_tool_call_exec_extracts_command_with_nested_quotes_and_escapes() {
        // The command contains single quotes and escaped double quotes; the
        // scanner reads the JS string literal honoring the backslash escapes.
        let mut state = CodexStructuredState::new();
        let path = std::path::Path::new("/tmp/rollout.jsonl");
        let call = json!({
            "type": "response_item",
            "payload": {
                "type": "custom_tool_call",
                "name": "exec",
                "call_id": "call_q",
                "input": "const r = await tools.exec_command({\"cmd\":\"sed -n '1,240p' a.md && rg -n \\\"pr merge 366\\\" .\",\"workdir\":\"/repo\"});\ntext(r.output);\n"
            }
        });
        state
            .event_from_line(&call, &meta(), None, path, 5)
            .expect("custom exec call is structured");
        // A plain-string output also joins (Codex sometimes emits a bare string).
        let output = json!({
            "type": "response_item",
            "payload": {
                "type": "custom_tool_call_output",
                "call_id": "call_q",
                "output": "Script running with cell ID 10\nWall time 10.1 seconds\nOutput:\n"
            }
        });
        let rows = state
            .event_from_line(&output, &meta(), None, path, 9)
            .expect("string output completes the join");
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].text,
            "sed -n '1,240p' a.md && rg -n \"pr merge 366\" ."
        );
        let md = metadata_of(&rows[0]);
        assert_eq!(
            md["cmd"],
            "sed -n '1,240p' a.md && rg -n \"pr merge 366\" ."
        );
        assert_eq!(md["workdir"], "/repo");
        assert_eq!(md["wall_time_s"], 10.1);
    }

    #[test]
    fn extract_exec_command_args_reads_js_object_literal_shapes() {
        // Real Codex harnesses are JS object literals, not JSON: keys are often
        // unquoted and values may use single, double, or backtick quotes.
        let inv = extract_exec_command_args(
            "const r = await tools.exec_command({cmd:\"gh pr merge 366 --squash --admin\",workdir:\"/home/zack/projects/tracedecay\",yield_time_ms:10000});\ntext(r.output);\n",
        )
        .expect("exec call present");
        assert_eq!(inv.cmd.as_deref(), Some("gh pr merge 366 --squash --admin"));
        assert_eq!(
            inv.workdir.as_deref(),
            Some("/home/zack/projects/tracedecay")
        );

        // Template literals (backticks with `${…}`) are kept verbatim.
        let inv = extract_exec_command_args(
            "const r = await tools.exec_command({cmd:`gh pr view ${num} --json ${fields}`,workdir:'/repo'});\n",
        )
        .expect("exec call present");
        assert_eq!(
            inv.cmd.as_deref(),
            Some("gh pr view ${num} --json ${fields}")
        );
        assert_eq!(inv.workdir.as_deref(), Some("/repo"));

        // A single harness can batch several exec_command calls; every command
        // is captured so each stays searchable.
        let inv = extract_exec_command_args(
            "await Promise.all([\n  tools.exec_command({cmd:\"git fetch origin master\",workdir:\"/repo\",max_output_tokens:4000}),\n  tools.exec_command({cmd:\"gh pr merge 371 --merge\"})]);\n",
        )
        .expect("exec calls present");
        assert_eq!(
            inv.cmd.as_deref(),
            Some("git fetch origin master\ngh pr merge 371 --merge")
        );
        assert_eq!(inv.workdir.as_deref(), Some("/repo"));

        // Non-exec JS has no marker at all.
        assert!(extract_exec_command_args("const x = ALL_TOOLS.filter(t => t.exec);\n").is_none());

        // Marker text inside a string or comment is not an executed call.
        assert!(
            extract_exec_command_args("text(\"tools.exec_command({cmd:'not run'})\");\n").is_none()
        );
        assert!(
            extract_exec_command_args("// tools.exec_command({cmd:\"not run\"})\ntext('done');\n")
                .is_none()
        );
    }

    #[test]
    fn custom_tool_call_non_exec_js_falls_through_to_generic() {
        // A custom `exec` tool whose harness does not call `exec_command` (e.g.
        // pure JS) is not an exec join — it stays on the generic path.
        let mut state = CodexStructuredState::new();
        let path = std::path::Path::new("/tmp/rollout.jsonl");
        let call = json!({
            "type": "response_item",
            "payload": {
                "type": "custom_tool_call",
                "name": "exec",
                "call_id": "call_js",
                "input": "text('just some javascript, no exec_command call');\n"
            }
        });
        assert!(
            state
                .event_from_line(&call, &meta(), None, path, 3)
                .is_none(),
            "non-exec JS is left for the generic tool_event handler"
        );
        // Its output, never buffered, also falls through.
        let output = json!({
            "type": "response_item",
            "payload": {
                "type": "custom_tool_call_output",
                "call_id": "call_js",
                "output": "Script completed\nWall time 0.1 seconds\nOutput:\nhi\n"
            }
        });
        assert!(
            state
                .event_from_line(&output, &meta(), None, path, 4)
                .is_none(),
            "an output with no buffered exec call falls through"
        );
    }

    #[test]
    fn custom_tool_call_exec_without_output_flushes_null_result_row() {
        let mut state = CodexStructuredState::new();
        let path = std::path::Path::new("/tmp/rollout.jsonl");
        let call = json!({
            "type": "response_item",
            "payload": {
                "type": "custom_tool_call",
                "name": "exec",
                "call_id": "call_hang",
                "input": "const r = await tools.exec_command({\"cmd\":\"cargo test-all\"});\ntext(r.output);\n"
            }
        });
        state
            .event_from_line(&call, &meta(), None, path, 7)
            .expect("custom exec call recognized");
        let flushed = state.flush_pending(&meta(), path);
        assert_eq!(flushed.len(), 1);
        assert_eq!(flushed[0].text, "cargo test-all");
        let md = metadata_of(&flushed[0]);
        assert_eq!(md["exit_code"], Value::Null);
        assert_eq!(md["success"], Value::Null);
    }

    #[test]
    fn parse_exec_output_ignores_exit_marker_in_the_body() {
        // The custom harness header ("Script completed\nWall time X seconds")
        // has no exit code; a "Process exited with code N" line inside the
        // command's own stdout must not be mistaken for the exec result.
        let output =
            "Script completed\nWall time 0.3 seconds\nOutput:\nProcess exited with code 137\n";
        let (exit, wall) = parse_exec_output(output);
        assert_eq!(exit, None, "body exit marker must not spoof the result");
        assert_eq!(wall, Some(0.3));
    }

    #[test]
    fn patch_apply_end_becomes_file_edit_with_hunk_counts_no_diff_body() {
        let record = json!({
            "type": "event_msg",
            "payload": {
                "type": "patch_apply_end",
                "call_id": "call-p",
                "turn_id": "turn-p",
                "success": true,
                "stdout": "Success. Updated the following files:\nM src/lib.rs\n",
                "changes": {
                    "src/lib.rs": {"type": "update", "unified_diff": "@@ -1,2 +1,2 @@\n-a\n+b\n@@ -9,1 +9,2 @@\n c\n+d\n"},
                    "src/new.rs": {"type": "add", "unified_diff": "@@ -0,0 +1,1 @@\n+created\n"}
                }
            }
        });
        let mut state = CodexStructuredState::new();
        let rows = state
            .event_from_line(
                &record,
                &meta(),
                Some("gpt-5.5"),
                std::path::Path::new("/tmp/r.jsonl"),
                5,
            )
            .expect("patch_apply_end is structured");
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.role, "tool");
        assert_eq!(row.kind.as_deref(), Some("file_edit"));
        assert!(row.text.contains("Updated the following files"));
        let md = metadata_of(row);
        assert_eq!(md["success"], true);
        assert_eq!(md["call_id"], "call-p");
        let files = md["files"].as_array().unwrap();
        // Sorted by path: src/lib.rs (2 hunks, update), src/new.rs (1 hunk, add).
        assert_eq!(files[0]["path"], "src/lib.rs");
        assert_eq!(files[0]["change_type"], "update");
        assert_eq!(files[0]["hunk_count"], 2);
        assert_eq!(files[1]["path"], "src/new.rs");
        assert_eq!(files[1]["change_type"], "add");
        assert_eq!(files[1]["hunk_count"], 1);
        // The diff body itself is never stored.
        assert!(
            !row.metadata_json
                .as_deref()
                .unwrap()
                .contains("unified_diff")
        );
        assert!(!row.metadata_json.as_deref().unwrap().contains("+created"));
    }

    #[test]
    fn task_events_become_turn_boundary_rows() {
        let mut state = CodexStructuredState::new();
        let path = std::path::Path::new("/tmp/r.jsonl");
        let started = json!({
            "type": "event_msg",
            "payload": {"type": "task_started", "turn_id": "t1", "started_at": 100, "model_context_window": 258400}
        });
        let rows = state
            .event_from_line(&started, &meta(), None, path, 1)
            .unwrap();
        let md = metadata_of(&rows[0]);
        assert_eq!(rows[0].kind.as_deref(), Some("turn_boundary"));
        assert_eq!(md["completed"], false);
        assert_eq!(md["model_context_window"], 258400);

        let complete = json!({
            "type": "event_msg",
            "payload": {"type": "task_complete", "turn_id": "t1", "duration_ms": 78585, "time_to_first_token_ms": 6713, "last_agent_message": "should NOT be indexed as text"}
        });
        let rows = state
            .event_from_line(&complete, &meta(), None, path, 2)
            .unwrap();
        let md = metadata_of(&rows[0]);
        assert_eq!(md["completed"], true);
        assert_eq!(md["duration_ms"], 78585);
        assert_eq!(md["time_to_first_token_ms"], 6713);
        // last_agent_message duplicates the agent_message; it stays out of text.
        assert!(!rows[0].text.contains("should NOT be indexed"));

        let aborted = json!({
            "type": "event_msg",
            "payload": {"type": "turn_aborted", "turn_id": "t2", "reason": "interrupted", "duration_ms": 5626}
        });
        let rows = state
            .event_from_line(&aborted, &meta(), None, path, 3)
            .unwrap();
        let md = metadata_of(&rows[0]);
        assert_eq!(md["completed"], false);
        assert_eq!(md["reason"], "interrupted");
    }

    #[test]
    fn mcp_tool_call_end_records_server_tool_and_ok() {
        let record = json!({
            "type": "event_msg",
            "payload": {
                "type": "mcp_tool_call_end",
                "call_id": "call-m",
                "plugin_id": "tracedecay@personal",
                "invocation": {"server": "tracedecay", "tool": "tracedecay_context", "arguments": {"task": "x"}},
                "duration": {"secs": 0, "nanos": 418157495},
                "result": {"Ok": {"content": []}}
            }
        });
        let mut state = CodexStructuredState::new();
        let rows = state
            .event_from_line(
                &record,
                &meta(),
                None,
                std::path::Path::new("/tmp/r.jsonl"),
                7,
            )
            .unwrap();
        let row = &rows[0];
        assert_eq!(row.kind.as_deref(), Some("tool_call"));
        assert_eq!(row.text, "tracedecay:tracedecay_context");
        assert_eq!(
            row.tool_names.as_deref(),
            Some("tracedecay:tracedecay_context")
        );
        let md = metadata_of(row);
        assert_eq!(md["server"], "tracedecay");
        assert_eq!(md["tool"], "tracedecay_context");
        assert_eq!(md["plugin_id"], "tracedecay@personal");
        assert_eq!(md["ok"], true);
        assert_eq!(md["duration_ms"], 418);
    }

    #[test]
    fn mcp_tool_call_end_error_records_ok_false_and_capped_error() {
        let record = json!({
            "type": "event_msg",
            "payload": {
                "type": "mcp_tool_call_end",
                "invocation": {"server": "tracedecay", "tool": "tracedecay_search"},
                "result": {"Err": "tool call error: timed out"}
            }
        });
        let mut state = CodexStructuredState::new();
        let rows = state
            .event_from_line(
                &record,
                &meta(),
                None,
                std::path::Path::new("/tmp/r.jsonl"),
                8,
            )
            .unwrap();
        let md = metadata_of(&rows[0]);
        assert_eq!(md["ok"], false);
        assert_eq!(md["error"], "tool call error: timed out");
    }

    #[test]
    fn web_search_end_captures_query_and_queries() {
        let record = json!({
            "type": "event_msg",
            "payload": {
                "type": "web_search_end",
                "call_id": "ws-1",
                "query": "primary query",
                "action": {"type": "search", "query": "primary query", "queries": ["primary query", "secondary query"]}
            }
        });
        let mut state = CodexStructuredState::new();
        let rows = state
            .event_from_line(
                &record,
                &meta(),
                None,
                std::path::Path::new("/tmp/r.jsonl"),
                9,
            )
            .unwrap();
        let row = &rows[0];
        assert_eq!(row.kind.as_deref(), Some("web_search"));
        assert_eq!(row.text, "primary query");
        let md = metadata_of(row);
        assert_eq!(md["queries"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn sub_agent_activity_and_inter_agent_edge_only() {
        let mut state = CodexStructuredState::new();
        let path = std::path::Path::new("/tmp/r.jsonl");
        let activity = json!({
            "type": "event_msg",
            "payload": {"type": "sub_agent_activity", "event_id": "e1", "agent_thread_id": "thread-x", "agent_path": "/root/worker", "kind": "started"}
        });
        let rows = state
            .event_from_line(&activity, &meta(), None, path, 11)
            .unwrap();
        assert_eq!(rows[0].kind.as_deref(), Some("subagent_activity"));
        let md = metadata_of(&rows[0]);
        assert_eq!(md["agent_thread_id"], "thread-x");
        assert_eq!(md["kind"], "started");

        let comm = json!({
            "type": "inter_agent_communication",
            "payload": {"author": "/root/worker", "recipient": "/root", "content": "", "encrypted_content": "gAAAABsecret", "trigger_turn": false}
        });
        let rows = state
            .event_from_line(&comm, &meta(), None, path, 12)
            .unwrap();
        assert_eq!(rows[0].kind.as_deref(), Some("subagent_activity"));
        let md = metadata_of(&rows[0]);
        assert_eq!(md["author"], "/root/worker");
        assert_eq!(md["recipient"], "/root");
        assert_eq!(md["encrypted"], true);
        // The ciphertext is never stored.
        assert!(
            !rows[0]
                .metadata_json
                .as_deref()
                .unwrap()
                .contains("gAAAABsecret")
        );
        assert!(!rows[0].text.contains("gAAAABsecret"));
    }

    #[test]
    fn update_plan_renders_steps() {
        let record = json!({
            "type": "response_item",
            "payload": {
                "type": "function_call",
                "name": "update_plan",
                "call_id": "call-plan",
                "arguments": "{\"explanation\":\"why\",\"plan\":[{\"step\":\"design\",\"status\":\"in_progress\"},{\"step\":\"ship\",\"status\":\"pending\"}]}"
            }
        });
        let mut state = CodexStructuredState::new();
        let rows = state
            .event_from_line(
                &record,
                &meta(),
                Some("gpt-5.5"),
                std::path::Path::new("/tmp/r.jsonl"),
                13,
            )
            .unwrap();
        let row = &rows[0];
        assert_eq!(row.role, "assistant");
        assert_eq!(row.kind.as_deref(), Some("plan"));
        assert!(row.text.contains("[in_progress] design"));
        assert!(row.text.contains("[pending] ship"));
        let md = metadata_of(row);
        assert_eq!(md["explanation"], "why");
        assert_eq!(md["steps"].as_array().unwrap().len(), 2);
        assert_eq!(md["steps"][0]["status"], "in_progress");
    }

    #[test]
    fn non_structured_function_calls_fall_through() {
        let mut state = CodexStructuredState::new();
        let apply_patch = json!({
            "type": "response_item",
            "payload": {"type": "custom_tool_call", "name": "apply_patch", "call_id": "c"}
        });
        assert!(
            state
                .event_from_line(
                    &apply_patch,
                    &meta(),
                    None,
                    std::path::Path::new("/tmp/r.jsonl"),
                    14
                )
                .is_none()
        );
        // A message response_item is not a structured tool line either.
        let message = json!({
            "type": "response_item",
            "payload": {"type": "message", "role": "assistant", "content": []}
        });
        assert!(
            state
                .event_from_line(
                    &message,
                    &meta(),
                    None,
                    std::path::Path::new("/tmp/r.jsonl"),
                    15
                )
                .is_none()
        );
    }

    #[test]
    fn summary_harvests_policy_effort_models_and_rate_limits() {
        let mut state = CodexStructuredState::new();
        state.observe_summary(&json!({
            "type": "turn_context",
            "payload": {"approval_policy": "never", "sandbox_policy": {"type": "danger-full-access"}, "effort": "high", "model": "gpt-5.5"}
        }));
        state.observe_summary(&json!({
            "type": "turn_context",
            "payload": {"model": "gpt-5.3-codex"}
        }));
        state.observe_summary(&json!({
            "type": "event_msg",
            "payload": {"type": "token_count", "info": {
                "model_context_window": 258400,
                "rate_limits": {"primary": {"used_percent": 11.0, "resets_at": 1780375431}, "secondary": {"used_percent": 30.0, "resets_at": 1780848095}, "plan_type": "pro"}
            }}
        }));
        let mut map = Map::new();
        state.summary.apply(&mut map);
        let value = Value::Object(map);
        assert_eq!(value["codex_approval_policy"], "never");
        assert_eq!(value["codex_sandbox_policy"], "danger-full-access");
        assert_eq!(value["codex_effort"], "high");
        assert_eq!(value["codex_model_context_window"], 258_400);
        assert_eq!(value["codex_models"], json!(["gpt-5.3-codex", "gpt-5.5"]));
        assert_eq!(value["codex_rate_limits"]["primary"]["used_percent"], 11.0);
        assert_eq!(
            value["codex_rate_limits"]["secondary"]["resets_at"],
            1_780_848_095_i64
        );
        assert_eq!(value["codex_rate_limits"]["plan_type"], "pro");
    }
}
