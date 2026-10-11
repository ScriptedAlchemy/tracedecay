//! Answers a repeated identical read with a pointer to the earlier result.
//!
//! The serve proxy lives exactly as long as one host MCP connection, so this
//! ledger is per session without any daemon state. It digests the content
//! blocks the daemon actually returned, skipping trailer blocks and the
//! renderer's per-call lines (request ids, timings) but including whether a
//! stale code-graph seat was served. A pointer is sent only when this session
//! already received those same bytes for the same tool and canonical
//! arguments, so an edited file, a re-index, a fresh/stale seat transition, or
//! output rendered differently from the same typed answer always arrives in
//! full. The call after a pointer is always served in full, which is how an
//! agent that lost the earlier result gets it back, and a pointer keeps the
//! stale-seat trailer so the warning is never hidden.
//!
//! The ledger is on only when the host sets [`REPEAT_READ_POINTERS_ENV`],
//! because that is valid only where one connection serves one agent (Codex
//! starts a serve per thread). A host that multiplexes several agents over one
//! connection, as Claude Code does with its subagents, would get pointers to
//! content the calling agent never saw.

use std::collections::HashMap;

use serde_json::{Value, json};
use tracedecay_domain::canonical_json_value;
use tracedecay_domain::canonical_text::canonical_framed_sha256;
use tracedecay_domain::errors::Result;
use tracedecay_mcp::JsonRpcRequest;
use tracedecay_mcp::application_output::markdown::without_per_call_lines;
use tracedecay_mcp::tools::response_trailers::{
    CODE_GRAPH_FRESHNESS_TRAILER_PREFIX, is_beside_result_block, is_code_graph_freshness_block,
};

use super::tool_surface::env_switch;

/// Explicit env a host sets when each serve connection belongs to one agent.
pub(super) const REPEAT_READ_POINTERS_ENV: &str = "TRACEDECAY_MCP_REPEAT_READ_POINTERS";
/// Reads that return file content or listings an agent re-requests.
const REPEATABLE_READS: &[&str] = &[
    "tracedecay_context",
    "tracedecay_files",
    "tracedecay_source_body",
    "tracedecay_source_lines",
    "tracedecay_source_outline",
];
const DIGEST_DOMAIN: &[u8] = b"tracedecay.serve.repeat_read.v1";
/// Smallest answer worth a pointer: the pointer is about 400 bytes, and a
/// lost earlier result costs one more full repeat.
const MIN_POINTER_BYTES: usize = 1024;
const POINTER_PREFIX: &str = "tracedecay_repeat_read:";

/// What this session last received for one tool call.
#[derive(Clone, Debug, Eq, PartialEq)]
enum Served {
    /// The full answer with this digest.
    Full(String),
    /// A pointer; the next identical call is served in full.
    Pointer,
}

/// The next state for one call key; a pointer is sent iff it is
/// [`Served::Pointer`], and `None` forgets the key.
fn transition(previous: Option<&Served>, digest: &str, bytes: usize) -> Option<Served> {
    if bytes < MIN_POINTER_BYTES {
        return None;
    }
    match previous {
        Some(Served::Full(seen)) if seen == digest => Some(Served::Pointer),
        _ => Some(Served::Full(digest.to_owned())),
    }
}

/// One serve session's repeat-read ledger.
#[derive(Debug, Default)]
pub(super) struct RepeatReads {
    // ponytail: unbounded per-session map of ~100 B entries keyed by distinct
    // repeatable reads; add an LRU bound if serve RSS shows growth.
    served: HashMap<(String, String), Served>,
}

impl RepeatReads {
    /// The ledger for this session, or `None` unless the host marked the
    /// connection as one agent's.
    pub(super) fn from_env() -> Result<Option<Self>> {
        Ok(env_switch(REPEAT_READ_POINTERS_ENV)?.then(Self::default))
    }

    /// Replaces the answer to a repeated identical read with a pointer, and
    /// records what this session received. Returns whether it sent a pointer.
    pub(super) fn rewrite(
        &mut self,
        request: Option<&JsonRpcRequest>,
        responses: &mut [String],
    ) -> bool {
        let Some((id, key)) = request.and_then(call_key) else {
            return false;
        };
        for line in responses.iter_mut() {
            let Ok(mut message) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if message.get("id") != Some(id) {
                continue;
            }
            let Some(result) = message.get_mut("result") else {
                return false;
            };
            let Some((digest, bytes)) = answer_digest(result) else {
                return false;
            };
            let next = transition(self.served.get(&key), &digest, bytes);
            let pointer = next == Some(Served::Pointer);
            match next {
                Some(served) => {
                    self.served.insert(key.clone(), served);
                }
                None => {
                    self.served.remove(&key);
                }
            }
            if pointer {
                replace_with_pointer(result, &key.0, &digest, bytes);
                *line = format!("{message}\n");
            }
            return pointer;
        }
        false
    }
}

/// The id and `(tool, canonical arguments)` key of a repeatable read.
fn call_key(request: &JsonRpcRequest) -> Option<(&Value, (String, String))> {
    if request.method != "tools/call" {
        return None;
    }
    let id = request.id.as_ref()?;
    let params = request.params.as_ref()?;
    let tool = params.get("name")?.as_str()?;
    if !REPEATABLE_READS.contains(&tool) {
        return None;
    }
    let arguments = canonical_json_value(params.get("arguments").unwrap_or(&Value::Null)).ok()?;
    Some((id, (tool.to_owned(), arguments)))
}

/// The digest of a successful all-text answer without its trailer blocks and
/// per-call lines, and the content text bytes a pointer would replace.
fn answer_digest(result: &Value) -> Option<(String, usize)> {
    if result.get("isError").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let texts = result
        .get("content")?
        .as_array()?
        .iter()
        .map(|block| block.get("text").and_then(Value::as_str))
        .collect::<Option<Vec<_>>>()?;
    let bytes = texts.iter().map(|text| text.len()).sum();
    let mut answer = texts
        .iter()
        .filter(|text| !is_beside_result_block(text))
        .map(|text| without_per_call_lines(text))
        .collect::<Vec<_>>();
    // A stale-seat trailer is part of what was served, not per-call noise: its
    // presence joins the digest so a fresh/stale transition is served in full,
    // while its exact text stays out so one stale seat can still dedupe.
    if texts.iter().any(|text| is_code_graph_freshness_block(text)) {
        answer.push(CODE_GRAPH_FRESHNESS_TRAILER_PREFIX.to_owned());
    }
    let parts = answer.iter().map(String::as_bytes).collect::<Vec<_>>();
    Some((canonical_framed_sha256(DIGEST_DOMAIN, &parts), bytes))
}

fn replace_with_pointer(result: &mut Value, tool: &str, digest: &str, bytes: usize) {
    let Some(result) = result.as_object_mut() else {
        return;
    };
    let short = digest.get(..12).unwrap_or(digest);
    let text = format!(
        "{POINTER_PREFIX} unchanged. This {tool} call returned the same {bytes} bytes (sha256 \
         {short}) as the previous identical call in this session, apart from request ids and \
         timings, so the content is omitted; use that earlier result. Repeat this exact call \
         to receive the full content, for example if that earlier result is no longer in your \
         context."
    );
    // A stale-seat warning is live serving state, not part of the repeated
    // body: it rides beside the pointer so the warning is never hidden.
    let warnings = result
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|block| {
            block
                .get("text")
                .and_then(Value::as_str)
                .is_some_and(is_code_graph_freshness_block)
        })
        .cloned()
        .collect::<Vec<_>>();
    let mut content = vec![json!({ "type": "text", "text": text })];
    content.extend(warnings);
    result.insert("content".to_owned(), json!(content));
    result.remove("structuredContent");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::tool_surface::parse_env_switch;

    fn call(id: i64, tool: &str, arguments: &Value) -> JsonRpcRequest {
        JsonRpcRequest::decode(
            &json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "tools/call",
                "params": {"name": tool, "arguments": arguments, "_meta": {"progressToken": id}}
            })
            .to_string(),
        )
        .expect("request")
    }

    /// One rendered evidence answer: `payload`, then per-call provenance for
    /// `id`, then a cost trailer block that differs on every call.
    fn answer(id: i64, payload: &str) -> Vec<String> {
        let body = format!(
            "## source\\_body\n\n### Payload\n\n    {payload}\n\n- Status: `success`\n\
             - Provenance: `binding=binding.mcp.source_body.v1; request=request.mcp.{id:032x}; \
             elapsed_us={}`",
            id * 977
        );
        let result = json!({
            "content": [
                {"type": "text", "text": body},
                {"type": "text", "text": format!("\ntracedecay_cost: wall_us={}", id * 31)},
            ],
            "_meta": {"duration_us": id * 7},
        });
        vec![format!(
            "{}\n",
            json!({"jsonrpc": "2.0", "id": id, "result": result})
        )]
    }

    /// `answer` plus the stale-seat trailer the daemon appends, in its real
    /// shape including the per-call `(sealed Ns ago)` age.
    fn stale_answer(id: i64, payload: &str, sealed: &str) -> Vec<String> {
        let mut response = answer(id, payload);
        let mut message: Value = serde_json::from_str(&response[0]).expect("json");
        message["result"]["content"]
            .as_array_mut()
            .expect("content")
            .push(json!({
                "type": "text",
                "text": format!(
                    "\ncode_graph_freshness: stale, serving the last complete generation \
                     generation.fixture.7 (sealed {sealed} ago) while the code index rebuilds; \
                     results may trail the live worktree"
                ),
            }));
        response[0] = format!("{message}\n");
        response
    }

    fn served(reads: &mut RepeatReads, request: &JsonRpcRequest, response: Vec<String>) -> Value {
        let mut response = response;
        let pointed = reads.rewrite(Some(request), &mut response);
        let message = serde_json::from_str(&response[0]).expect("json");
        assert_eq!(pointed, is_pointer(&message), "{message}");
        message
    }

    fn is_pointer(message: &Value) -> bool {
        message["result"]["content"][0]["text"]
            .as_str()
            .is_some_and(|text| text.starts_with(POINTER_PREFIX))
    }

    fn big(fill: char) -> String {
        fill.to_string().repeat(MIN_POINTER_BYTES)
    }

    #[test]
    fn three_identical_reads_are_full_pointer_full() {
        let mut reads = RepeatReads::default();
        let args = json!({"node_id": "n"});
        let tool = "tracedecay_source_body";
        let first = served(&mut reads, &call(1, tool, &args), answer(1, &big('x')));
        let second = served(&mut reads, &call(2, tool, &args), answer(2, &big('x')));
        let third = served(&mut reads, &call(3, tool, &args), answer(3, &big('x')));
        assert!(!is_pointer(&first), "{first}");
        assert!(is_pointer(&second), "{second}");
        assert_eq!(second["id"], json!(2));
        assert_eq!(
            second["result"]["content"].as_array().map(Vec::len),
            Some(1)
        );
        assert_eq!(second["result"]["_meta"]["duration_us"], json!(14));
        let text = second["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default();
        assert!(text.contains(tool), "{text}");
        assert!(
            !is_pointer(&third),
            "the call after a pointer must be full: {third}"
        );
        assert!(
            third["result"]["content"][0]["text"]
                .as_str()
                .is_some_and(|text| text.contains(&big('x'))),
            "{third}"
        );
    }

    #[test]
    fn freshness_transitions_are_full_and_pointers_keep_the_warning() {
        let mut reads = RepeatReads::default();
        let args = json!({"path": "src"});
        let tool = "tracedecay_files";
        served(&mut reads, &call(1, tool, &args), answer(1, &big('x')));
        let to_stale = served(
            &mut reads,
            &call(2, tool, &args),
            stale_answer(2, &big('x'), "5s"),
        );
        assert!(
            !is_pointer(&to_stale),
            "fresh to stale must serve the warning in full: {to_stale}"
        );
        let repeat_stale = served(
            &mut reads,
            &call(3, tool, &args),
            stale_answer(3, &big('x'), "9s"),
        );
        assert!(
            is_pointer(&repeat_stale),
            "the same stale answer still dedupes: {repeat_stale}"
        );
        let texts = repeat_stale["result"]["content"]
            .as_array()
            .expect("content");
        assert!(
            texts.iter().any(|block| {
                block["text"]
                    .as_str()
                    .is_some_and(|text| text.contains("(sealed 9s ago)"))
            }),
            "a pointer carries this call's warning, not an old one: {repeat_stale}"
        );
        let back_to_fresh = served(&mut reads, &call(4, tool, &args), answer(4, &big('x')));
        assert!(
            !is_pointer(&back_to_fresh),
            "stale to fresh must serve in full: {back_to_fresh}"
        );
        let repeat_fresh = served(&mut reads, &call(5, tool, &args), answer(5, &big('x')));
        assert!(
            is_pointer(&repeat_fresh),
            "the fresh baseline still dedupes: {repeat_fresh}"
        );
    }

    #[test]
    fn a_changed_answer_is_served_in_full() {
        let mut reads = RepeatReads::default();
        let args = json!({"file": "src/lib.rs"});
        let tool = "tracedecay_source_outline";
        served(&mut reads, &call(1, tool, &args), answer(1, &big('x')));
        let changed = served(&mut reads, &call(2, tool, &args), answer(2, &big('y')));
        assert!(!is_pointer(&changed), "{changed}");
        let repeat = served(&mut reads, &call(3, tool, &args), answer(3, &big('y')));
        assert!(
            is_pointer(&repeat),
            "the changed answer becomes the new baseline: {repeat}"
        );
    }

    #[test]
    fn a_differently_rendered_answer_is_served_in_full() {
        let mut reads = RepeatReads::default();
        let args = json!({"node_id": "n"});
        let tool = "tracedecay_source_body";
        let trimmed = format!("{}\n    … trimmed", big('x'));
        let full = format!("{}\n    {}", big('x'), big('z'));
        served(&mut reads, &call(1, tool, &args), answer(1, &trimmed));
        let restored = served(&mut reads, &call(2, tool, &args), answer(2, &full));
        assert!(
            !is_pointer(&restored),
            "a full render must never point at a trimmed one: {restored}"
        );
    }

    #[test]
    fn a_line_ending_only_change_is_served_in_full() {
        let mut reads = RepeatReads::default();
        let args = json!({"file": "src/lib.rs", "start": 1, "end": 40});
        let tool = "tracedecay_source_lines";
        // Raw file text as the whole content block, so the final newline is
        // the block's last byte.
        let raw = |id: i64, text: String| {
            let result = json!({"content": [
                {"type": "text", "text": text},
                {"type": "text", "text": format!("\ntracedecay_cost: wall_us={id}")},
            ]});
            vec![format!(
                "{}\n",
                json!({"jsonrpc": "2.0", "id": id, "result": result})
            )]
        };
        let lf = format!("{}\n{}", big('x'), big('y'));
        let crlf = format!("{}\r\n{}", big('x'), big('y'));
        served(&mut reads, &call(1, tool, &args), raw(1, lf.clone()));
        let to_crlf = served(&mut reads, &call(2, tool, &args), raw(2, crlf));
        assert!(!is_pointer(&to_crlf), "LF to CRLF: {to_crlf}");
        served(&mut reads, &call(3, tool, &args), raw(3, lf.clone()));
        let with_newline = served(&mut reads, &call(4, tool, &args), raw(4, format!("{lf}\n")));
        assert!(
            !is_pointer(&with_newline),
            "added final newline: {with_newline}"
        );
    }

    #[test]
    fn small_answers_are_never_pointed_and_reset_the_key() {
        let mut reads = RepeatReads::default();
        let args = json!({"path": "src"});
        let tool = "tracedecay_files";
        served(&mut reads, &call(1, tool, &args), answer(1, "a.rs"));
        let again = served(&mut reads, &call(2, tool, &args), answer(2, "a.rs"));
        assert!(!is_pointer(&again), "{again}");
        let large = served(&mut reads, &call(3, tool, &args), answer(3, &big('x')));
        assert!(
            !is_pointer(&large),
            "a small answer must not seed a pointer: {large}"
        );
    }

    #[test]
    fn tools_outside_the_read_table_pass_through_untouched() {
        let mut reads = RepeatReads::default();
        let grep = json!({"pattern": "x"});
        served(
            &mut reads,
            &call(1, "tracedecay_grep", &grep),
            answer(1, &big('x')),
        );
        let repeat = served(
            &mut reads,
            &call(2, "tracedecay_grep", &grep),
            answer(2, &big('x')),
        );
        assert!(!is_pointer(&repeat), "{repeat}");
    }

    #[test]
    fn failures_pass_through_and_leave_the_ledger_unchanged() {
        let mut reads = RepeatReads::default();
        let args = json!({"node_id": "n"});
        let tool = "tracedecay_source_body";
        served(&mut reads, &call(1, tool, &args), answer(1, &big('x')));

        let mut is_error = answer(2, &big('x'));
        let mut message: Value = serde_json::from_str(&is_error[0]).expect("json");
        message["result"]["isError"] = json!(true);
        is_error[0] = format!("{message}\n");
        let before = is_error.clone();
        reads.rewrite(Some(&call(2, tool, &args)), &mut is_error);
        assert_eq!(is_error, before);

        let mut rpc_error = vec![format!(
            "{}\n",
            json!({"jsonrpc": "2.0", "id": 3, "error": {"code": -32603, "message": "boom"}})
        )];
        let before = rpc_error.clone();
        reads.rewrite(Some(&call(3, tool, &args)), &mut rpc_error);
        assert_eq!(rpc_error, before);

        let after_errors = served(&mut reads, &call(6, tool, &args), answer(6, &big('x')));
        assert!(
            is_pointer(&after_errors),
            "failures leave the ledger unchanged: {after_errors}"
        );
    }

    #[test]
    fn keys_are_tool_and_canonical_arguments() {
        let mut reads = RepeatReads::default();
        let ordered = json!({"file": "src/lib.rs", "format": "json"});
        let reordered: Value =
            serde_json::from_str(r#"{"format":"json","file":"src/lib.rs"}"#).expect("json");
        let outline = "tracedecay_source_outline";
        served(
            &mut reads,
            &call(1, outline, &ordered),
            answer(1, &big('x')),
        );
        let other_file = json!({"file": "src/main.rs", "format": "json"});
        let other_args = served(
            &mut reads,
            &call(2, outline, &other_file),
            answer(2, &big('x')),
        );
        assert!(!is_pointer(&other_args), "{other_args}");
        let body = "tracedecay_source_body";
        let other_tool = served(&mut reads, &call(3, body, &ordered), answer(3, &big('x')));
        assert!(!is_pointer(&other_tool), "{other_tool}");
        let same = served(
            &mut reads,
            &call(4, outline, &reordered),
            answer(4, &big('x')),
        );
        assert!(
            is_pointer(&same),
            "argument key order must not matter: {same}"
        );
    }

    #[test]
    fn repeat_read_pointers_env_is_an_explicit_switch() {
        let parse = |value: &str| parse_env_switch(REPEAT_READ_POINTERS_ENV, Ok(value.to_owned()));
        assert!(matches!(parse("1"), Ok(true)));
        assert!(matches!(parse("true"), Ok(true)));
        assert!(matches!(parse("0"), Ok(false)));
        assert!(matches!(parse(""), Ok(false)));
        assert!(matches!(
            parse_env_switch(
                REPEAT_READ_POINTERS_ENV,
                Err(std::env::VarError::NotPresent)
            ),
            Ok(false)
        ));
        let error = parse("yes").expect_err("an unknown value must be refused");
        assert!(
            error.to_string().contains(REPEAT_READ_POINTERS_ENV),
            "{error}"
        );
    }
}
