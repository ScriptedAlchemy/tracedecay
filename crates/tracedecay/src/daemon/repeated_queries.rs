//! Flags a tool call that nearly repeats a recent call to the same tool.
//!
//! The serve proxy lives exactly as long as one host MCP connection, while it
//! opens a fresh daemon connection per request, so this window is per session
//! without any daemon state. A call whose query words overlap an earlier call
//! to the same tool among the last [`WINDOW`] calls by more than 60% (Jaccard)
//! gets one advisory text block appended to its result. The hint never blocks
//! a call, never changes existing content, and is on only when the profile
//! setting [`USER_REPEATED_QUERY_HINTS_SETTING_KEY`] is true. A call already
//! answered with a repeat-read pointer names its identical earlier call there,
//! so its hint considers only earlier calls with different arguments.

use std::collections::{BTreeSet, VecDeque};
use std::fmt;

use serde_json::{Value, json};
use tokio::sync::watch;
use tracedecay_domain::canonical_json_value;
use tracedecay_domain::configuration::USER_REPEATED_QUERY_HINTS_SETTING_KEY;
use tracedecay_mcp::JsonRpcRequest;

/// How many earlier calls of any tool a new call is compared against.
const WINDOW: usize = 6;
const HINT_PREFIX: &str = "tracedecay_repeated_query:";
const MAX_QUOTED_ARGUMENT_CHARS: usize = 200;

/// One earlier `tools/call` of this session.
struct RecentCall {
    tool: String,
    words: BTreeSet<String>,
    /// Canonical JSON of the call arguments, used only to name the call.
    arguments: String,
}

/// Jaccard overlap of two word sets as the exact ratio `shared / union`.
#[derive(Clone, Copy)]
struct Overlap {
    shared: usize,
    union: usize,
}

impl Overlap {
    fn between(left: &BTreeSet<String>, right: &BTreeSet<String>) -> Self {
        let shared = left.intersection(right).count();
        Self {
            shared,
            union: left.len() + right.len() - shared,
        }
    }

    /// Strictly above 3/5; an empty union never qualifies.
    fn is_repeat(self) -> bool {
        self.shared * 5 > self.union * 3
    }

    fn exceeds(self, other: Self) -> bool {
        self.shared * other.union > other.shared * self.union
    }

    fn percent(self) -> usize {
        (self.shared * 100).checked_div(self.union).unwrap_or(0)
    }
}

/// The earlier call a new call nearly repeats.
struct Hint {
    tool: String,
    arguments: String,
    /// 1 is the immediately previous call.
    calls_back: usize,
    percent: usize,
}

impl fmt::Display for Hint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self {
            tool,
            arguments,
            calls_back,
            percent,
        } = self;
        let calls = if *calls_back == 1 { "call" } else { "calls" };
        let quoted = match arguments.char_indices().nth(MAX_QUOTED_ARGUMENT_CHARS) {
            Some((end, _)) => format!("{}...", &arguments[..end]),
            None => arguments.clone(),
        };
        write!(
            formatter,
            "{HINT_PREFIX} this {tool} call shares {percent}% of its query words with the {tool} \
             call {calls_back} {calls} earlier in this session (arguments {quoted}). If that \
             result did not answer the question, change the query or the approach rather than \
             repeating it. Experimental hint, disable with {USER_REPEATED_QUERY_HINTS_SETTING_KEY}."
        )
    }
}

/// One serve session's window of recent tool calls.
pub(super) struct RepeatedQueries {
    /// The profile setting. Serve reads it beside the proxy rather than before
    /// it, so it can turn on after the session starts.
    enabled: watch::Receiver<bool>,
    recent: VecDeque<RecentCall>,
}

impl RepeatedQueries {
    pub(super) fn new(enabled: watch::Receiver<bool>) -> Self {
        Self {
            enabled,
            recent: VecDeque::new(),
        }
    }

    /// While enabled, records a `tools/call` and appends a hint to its result
    /// when it nearly repeats a recent call to the same tool; `pointed` says
    /// the result is a pointer to the identical earlier call.
    pub(super) fn rewrite(
        &mut self,
        request: Option<&JsonRpcRequest>,
        responses: &mut [String],
        pointed: bool,
    ) {
        if !*self.enabled.borrow() {
            return;
        }
        let Some((id, tool, arguments)) = request.and_then(tool_call) else {
            return;
        };
        let Some(hint) = self.observe(tool, arguments, pointed) else {
            return;
        };
        for line in responses.iter_mut() {
            let Ok(mut message) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if message.get("id") != Some(id) {
                continue;
            }
            // A failed call is not an unanswered query: its result already
            // reports the failure, so a hint would misreport it.
            if message
                .pointer("/result/isError")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                return;
            }
            if let Some(content) = message
                .pointer_mut("/result/content")
                .and_then(Value::as_array_mut)
            {
                content.push(json!({ "type": "text", "text": hint.to_string() }));
                *line = format!("{message}\n");
            }
            return;
        }
    }

    /// An `initialize` re-opens this session, possibly under a new project
    /// route; calls from the previous route must not flag the first queries
    /// of the next.
    pub(super) fn reset_for_initialize(&mut self, request: Option<&JsonRpcRequest>) {
        if request.is_some_and(|request| request.method == "initialize") {
            self.recent.clear();
        }
    }

    fn observe(&mut self, tool: &str, arguments: &Value, pointed: bool) -> Option<Hint> {
        let call = RecentCall {
            tool: tool.to_owned(),
            words: query_words(arguments),
            arguments: canonical_json_value(arguments).ok()?,
        };
        let mut best: Option<(usize, &RecentCall, Overlap)> = None;
        for (calls_back, earlier) in (1..).zip(self.recent.iter().rev()) {
            if earlier.tool != call.tool || (pointed && earlier.arguments == call.arguments) {
                continue;
            }
            let overlap = Overlap::between(&earlier.words, &call.words);
            if overlap.is_repeat() && best.is_none_or(|(_, _, best)| overlap.exceeds(best)) {
                best = Some((calls_back, earlier, overlap));
            }
        }
        let hint = best.map(|(calls_back, earlier, overlap)| Hint {
            tool: earlier.tool.clone(),
            arguments: earlier.arguments.clone(),
            calls_back,
            percent: overlap.percent(),
        });
        self.recent.push_back(call);
        if self.recent.len() > WINDOW {
            self.recent.pop_front();
        }
        hint
    }
}

/// The id, tool name, and arguments of a `tools/call` request.
fn tool_call(request: &JsonRpcRequest) -> Option<(&Value, &str, &Value)> {
    if request.method != "tools/call" {
        return None;
    }
    let id = request.id.as_ref()?;
    let params = request.params.as_ref()?;
    let tool = params.get("name")?.as_str()?;
    Some((id, tool, params.get("arguments").unwrap_or(&Value::Null)))
}

/// Lowercased words of every string and number in `arguments`; object keys
/// name parameters rather than the query, so they are skipped.
fn query_words(arguments: &Value) -> BTreeSet<String> {
    fn insert(text: &str, words: &mut BTreeSet<String>) {
        words.extend(
            text.split(|c: char| !c.is_alphanumeric() && c != '_')
                .filter(|word| !word.is_empty())
                .map(str::to_owned),
        );
    }
    fn collect(value: &Value, words: &mut BTreeSet<String>) {
        match value {
            Value::String(text) => insert(&text.to_lowercase(), words),
            Value::Number(number) => insert(&number.to_string(), words),
            Value::Array(items) => {
                for item in items {
                    collect(item, words);
                }
            }
            Value::Object(fields) => {
                for field in fields.values() {
                    collect(field, words);
                }
            }
            Value::Bool(_) | Value::Null => {}
        }
    }
    let mut words = BTreeSet::new();
    collect(arguments, &mut words);
    words
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enabled() -> RepeatedQueries {
        RepeatedQueries::new(watch::channel(true).1)
    }

    fn call(id: i64, tool: &str, arguments: &Value) -> JsonRpcRequest {
        JsonRpcRequest::decode(
            &json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "tools/call",
                "params": {"name": tool, "arguments": arguments}
            })
            .to_string(),
        )
        .expect("request")
    }

    fn answer(id: i64) -> Vec<String> {
        vec![format!(
            "{}\n",
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "content": [{"type": "text", "text": format!("answer {id}")}],
                    "structuredContent": {"rows": [id, "x"]},
                },
            })
        )]
    }

    /// Runs one call through the session and returns the response lines.
    fn served(queries: &mut RepeatedQueries, request: &JsonRpcRequest, id: i64) -> Vec<String> {
        let mut response = answer(id);
        queries.rewrite(Some(request), &mut response, false);
        response
    }

    fn hints(response: &[String]) -> Vec<String> {
        let message: Value = serde_json::from_str(&response[0]).expect("json");
        message["result"]["content"]
            .as_array()
            .expect("content")
            .iter()
            .filter_map(|block| block["text"].as_str())
            .filter(|text| text.starts_with(HINT_PREFIX))
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn an_identical_repeat_appends_one_hint_after_the_unchanged_result() {
        let mut queries = enabled();
        let args = json!({"query": "where is the proxy", "limit": 5});
        let first = served(&mut queries, &call(1, "tracedecay_search", &args), 1);
        assert_eq!(first, answer(1));
        served(
            &mut queries,
            &call(2, "tracedecay_grep", &json!({"pattern": "x"})),
            2,
        );
        let repeat = served(&mut queries, &call(3, "tracedecay_search", &args), 3);

        let message: Value = serde_json::from_str(&repeat[0]).expect("json");
        let original: Value = serde_json::from_str(&answer(3)[0]).expect("json");
        let content = message["result"]["content"].as_array().expect("content");
        assert_eq!(content.len(), 2, "{message}");
        assert_eq!(
            serde_json::to_string(&content[0]).expect("block"),
            serde_json::to_string(&original["result"]["content"][0]).expect("block")
        );
        assert_eq!(
            serde_json::to_string(&message["result"]["structuredContent"]).expect("structured"),
            serde_json::to_string(&original["result"]["structuredContent"]).expect("structured")
        );
        assert_eq!(message["id"], json!(3));
        assert!(repeat[0].ends_with('\n'));
        let appended = hints(&repeat);
        assert_eq!(appended.len(), 1, "{appended:?}");
        let hint = &appended[0];
        assert_eq!(content[1]["text"].as_str(), Some(hint.as_str()));
        assert!(
            hint.contains("this tracedecay_search call shares 100%"),
            "{hint}"
        );
        assert!(
            hint.contains("with the tracedecay_search call 2 calls earlier"),
            "{hint}"
        );
        assert!(
            hint.contains(r#"(arguments {"limit":5,"query":"where is the proxy"})"#),
            "{hint}"
        );
        let immediate = served(&mut queries, &call(4, "tracedecay_search", &args), 4);
        assert!(
            hints(&immediate)[0].contains("tracedecay_search call 1 call earlier"),
            "{immediate:?}"
        );
    }

    #[test]
    fn a_pointed_call_is_hinted_only_about_calls_with_other_arguments() {
        let mut queries = enabled();
        let tool = "tracedecay_source_body";
        let identical = json!({"query": "a b c d"});
        served(&mut queries, &call(1, tool, &identical), 1);
        let mut pointed = answer(2);
        queries.rewrite(Some(&call(2, tool, &identical)), &mut pointed, true);
        assert_eq!(
            pointed,
            answer(2),
            "the pointer already names the identical earlier call"
        );

        served(
            &mut queries,
            &call(3, tool, &json!({"query": "a b c d e"})),
            3,
        );
        let mut pointed = answer(4);
        queries.rewrite(Some(&call(4, tool, &identical)), &mut pointed, true);
        let hint = &hints(&pointed)[0];
        assert!(hint.contains("80%"), "{hint}");
        assert!(hint.contains("1 call earlier"), "{hint}");
        assert!(hint.contains(r#"{"query":"a b c d e"}"#), "{hint}");
    }

    #[test]
    fn calls_are_neither_hinted_nor_recorded_until_the_setting_turns_on() {
        let (setting, enabled) = watch::channel(false);
        let mut queries = RepeatedQueries::new(enabled);
        let args = json!({"query": "where is the proxy"});
        for id in [1, 2] {
            let response = served(&mut queries, &call(id, "tracedecay_search", &args), id);
            assert_eq!(
                response,
                answer(id),
                "disabled hints must not touch a response"
            );
        }

        setting.send_replace(true);
        let first = served(&mut queries, &call(3, "tracedecay_search", &args), 3);
        assert_eq!(first, answer(3), "calls made while off are not recorded");
        let repeat = served(&mut queries, &call(4, "tracedecay_search", &args), 4);
        assert!(hints(&repeat)[0].contains("1 call earlier"), "{repeat:?}");
    }

    #[test]
    fn a_repeat_is_flagged_within_the_window_and_forgotten_past_it() {
        let search = json!({"query": "proxy session window"});
        for (intervening, expect_hint) in [(5, true), (6, false)] {
            let mut queries = enabled();
            served(&mut queries, &call(0, "tracedecay_search", &search), 0);
            for id in 1..=intervening {
                let other = json!({"pattern": format!("other{id}")});
                served(&mut queries, &call(id, "tracedecay_grep", &other), id);
            }
            let id = intervening + 1;
            let repeat = served(&mut queries, &call(id, "tracedecay_search", &search), id);
            assert_eq!(
                hints(&repeat).len(),
                usize::from(expect_hint),
                "after {intervening} intervening calls: {repeat:?}"
            );
        }
    }

    #[test]
    fn only_overlap_strictly_above_three_fifths_is_flagged() {
        // {a,b,c,d} vs {a,b,c,e}: 3 shared of 5, exactly 60%.
        // {a,b,c,d} vs {a,b,c,d,e}: 4 shared of 5, 80%.
        // {a,b} vs {a,b,c}: 2 shared of 3, 66%.
        for (earlier, later, expect_hint) in [
            ("a b c d", "a b c e", false),
            ("a b", "a b c", true),
            ("a b c d", "a b c d e", true),
        ] {
            let mut queries = enabled();
            let tool = "tracedecay_search";
            served(&mut queries, &call(1, tool, &json!({"query": earlier})), 1);
            let response = served(&mut queries, &call(2, tool, &json!({"query": later})), 2);
            assert_eq!(
                hints(&response).len(),
                usize::from(expect_hint),
                "{earlier:?} then {later:?}: {response:?}"
            );
        }
    }

    #[test]
    fn the_closest_earlier_call_is_named_and_ties_name_the_most_recent() {
        let mut queries = enabled();
        let tool = "tracedecay_search";
        served(
            &mut queries,
            &call(1, tool, &json!({"query": "a b c d e"})),
            1,
        );
        served(
            &mut queries,
            &call(2, tool, &json!({"query": "a b c d x"})),
            2,
        );
        served(
            &mut queries,
            &call(3, tool, &json!({"query": "a b c d y"})),
            3,
        );
        let response = served(
            &mut queries,
            &call(4, tool, &json!({"query": "a b c d e"})),
            4,
        );
        let hint = &hints(&response)[0];
        assert!(hint.contains("100%"), "{hint}");
        assert!(hint.contains("3 calls earlier"), "{hint}");

        let response = served(
            &mut queries,
            &call(5, tool, &json!({"query": "a b c d z"})),
            5,
        );
        let hint = &hints(&response)[0];
        assert!(hint.contains("66%"), "{hint}");
        assert!(hint.contains("1 call earlier"), "{hint}");
    }

    #[test]
    fn a_different_tool_or_an_empty_query_is_never_flagged() {
        let mut queries = enabled();
        let args = json!({"query": "same words here"});
        served(&mut queries, &call(1, "tracedecay_search", &args), 1);
        let other_tool = served(&mut queries, &call(2, "tracedecay_context", &args), 2);
        assert_eq!(other_tool, answer(2));

        let empty = json!({"flag": true, "missing": null, "query": "  ..  "});
        served(&mut queries, &call(3, "tracedecay_files", &empty), 3);
        let repeat = served(&mut queries, &call(4, "tracedecay_files", &empty), 4);
        assert_eq!(repeat, answer(4));
        let no_arguments = JsonRpcRequest::decode(
            r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"tracedecay_files"}}"#,
        )
        .expect("request");
        assert_eq!(served(&mut queries, &no_arguments, 5), answer(5));
    }

    #[test]
    fn query_words_are_lowercased_value_leaves_split_on_non_word_characters() {
        let words = query_words(&json!({
            "Query": "Foo::bar_baz  qux-Quux",
            "paths": ["src/Lib.rs", {"nested_key": 42}],
            "ratio": 1.5,
            "flag": true,
            "none": null,
        }));
        let expected = [
            "1", "42", "5", "bar_baz", "foo", "lib", "quux", "qux", "rs", "src",
        ];
        assert_eq!(
            words.iter().map(String::as_str).collect::<Vec<_>>(),
            expected
        );
    }

    #[test]
    fn a_failed_call_is_recorded_but_its_error_result_is_never_rewritten() {
        let mut queries = enabled();
        let args = json!({"query": "failing search"});
        let failed = |id| {
            vec![format!(
                "{}\n",
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "content": [{"type": "text", "text": "boom"}],
                        "isError": true,
                    },
                })
            )]
        };
        let mut first = failed(1);
        queries.rewrite(
            Some(&call(1, "tracedecay_search", &args)),
            &mut first,
            false,
        );
        assert_eq!(first, failed(1));

        let mut repeat = failed(2);
        queries.rewrite(
            Some(&call(2, "tracedecay_search", &args)),
            &mut repeat,
            false,
        );
        assert_eq!(repeat, failed(2), "an isError result is never rewritten");

        let answered = served(&mut queries, &call(3, "tracedecay_search", &args), 3);
        assert!(
            hints(&answered)[0].contains("1 call earlier"),
            "a failed call still counts as a recent call: {answered:?}"
        );
    }

    #[test]
    fn an_initialize_clears_the_window_but_other_requests_do_not() {
        let mut queries = enabled();
        let args = json!({"query": "first project query"});
        served(&mut queries, &call(1, "tracedecay_search", &args), 1);

        let initialize =
            JsonRpcRequest::decode(r#"{"jsonrpc":"2.0","id":2,"method":"initialize","params":{}}"#)
                .expect("request");
        queries.reset_for_initialize(Some(&initialize));

        let after = served(&mut queries, &call(3, "tracedecay_search", &args), 3);
        assert_eq!(after, answer(3), "calls before initialize are forgotten");

        served(&mut queries, &call(4, "tracedecay_search", &args), 4);
        let ping =
            JsonRpcRequest::decode(r#"{"jsonrpc":"2.0","id":5,"method":"ping"}"#).expect("request");
        queries.reset_for_initialize(Some(&ping));
        let repeat = served(&mut queries, &call(6, "tracedecay_search", &args), 6);
        assert!(
            hints(&repeat)[0].contains("1 call earlier"),
            "a non-initialize request does not clear the window: {repeat:?}"
        );
    }

    #[test]
    fn errors_and_other_methods_pass_through_but_calls_are_still_recorded() {
        let mut queries = enabled();
        let args = json!({"query": "failing search"});
        let mut rpc_error = vec![format!(
            "{}\n",
            json!({"jsonrpc": "2.0", "id": 1, "error": {"code": -32603, "message": "boom"}})
        )];
        let before = rpc_error.clone();
        queries.rewrite(
            Some(&call(1, "tracedecay_search", &args)),
            &mut rpc_error,
            false,
        );
        assert_eq!(rpc_error, before);

        let mut repeated_error = vec![before[0].replace("\"id\":1", "\"id\":2")];
        let before = repeated_error.clone();
        queries.rewrite(
            Some(&call(2, "tracedecay_search", &args)),
            &mut repeated_error,
            false,
        );
        assert_eq!(
            repeated_error, before,
            "a JSON-RPC error is never rewritten"
        );

        let list = JsonRpcRequest::decode(r#"{"jsonrpc":"2.0","id":3,"method":"tools/list"}"#)
            .expect("request");
        let mut listing = answer(3);
        queries.rewrite(Some(&list), &mut listing, false);
        assert_eq!(listing, answer(3));

        let after_errors = served(&mut queries, &call(4, "tracedecay_search", &args), 4);
        assert!(
            hints(&after_errors)[0].contains("1 call earlier"),
            "an errored call is still recorded: {after_errors:?}"
        );
    }

    #[test]
    fn long_earlier_arguments_are_quoted_truncated_on_a_char_boundary() {
        let mut queries = enabled();
        let args = json!({"query": "é".repeat(300)});
        served(&mut queries, &call(1, "tracedecay_search", &args), 1);
        let response = served(&mut queries, &call(2, "tracedecay_search", &args), 2);
        let hint = &hints(&response)[0];
        let quoted = hint
            .split("(arguments ")
            .nth(1)
            .and_then(|rest| rest.split(')').next())
            .expect("quoted arguments");
        assert_eq!(quoted.chars().count(), MAX_QUOTED_ARGUMENT_CHARS + 3);
        assert!(quoted.starts_with(r#"{"query":"éé"#), "{quoted}");
        assert!(quoted.ends_with("é..."), "{quoted}");
    }

    /// What the final call of a corpus case should get: `Useful` and `Missed`
    /// cases are loops, `Quiet` and `KnownFalsePositive` cases are legitimate
    /// work; `Missed` and `KnownFalsePositive` record the heuristic's ceiling.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
    enum Label {
        Useful,
        Quiet,
        KnownFalsePositive,
        Missed,
    }

    impl Label {
        /// The label a case earns from whether its final call drew a hint.
        fn observed(self, hinted: bool) -> Self {
            match (self, hinted) {
                (Self::Useful | Self::Missed, true) => Self::Useful,
                (Self::Useful | Self::Missed, false) => Self::Missed,
                (Self::Quiet | Self::KnownFalsePositive, true) => Self::KnownFalsePositive,
                (Self::Quiet | Self::KnownFalsePositive, false) => Self::Quiet,
            }
        }
    }

    struct Step {
        tool: &'static str,
        arguments: &'static str,
        answer: &'static str,
        is_error: bool,
    }

    const fn ok(tool: &'static str, arguments: &'static str, answer: &'static str) -> Step {
        Step {
            tool,
            arguments,
            answer,
            is_error: false,
        }
    }

    struct Case {
        name: &'static str,
        class: &'static str,
        steps: &'static [Step],
        label: Label,
    }

    const SEARCH: &str = "tracedecay_search";
    const STOCK: &str = "reserve_stock in src/orders.rs holds warehouse stock for each order line";
    const ROLLBACK: &str =
        "release_stock in src/orders.rs returns held stock when an order is cancelled";
    const TOTALS: &str = "compute_total in src/pricing.rs sums line prices then applies tax";
    const CALLERS: &str = "checkout and invoice_preview call compute_total";
    const CALLERS_PAGE_2: &str = "refund_quote and cart_summary call compute_total";

    /// Synthetic sessions over the agent-adoption orders fixture. The label is
    /// what the final call should get; a heuristic change that moves a case
    /// must move its label and the totals below with it.
    const CORPUS: &[Case] = &[
        Case {
            name: "identical_search_same_answer",
            class: "stuck loop",
            steps: &[
                ok(SEARCH, r#"{"query":"where is stock reserved"}"#, STOCK),
                ok(SEARCH, r#"{"query":"where is stock reserved"}"#, STOCK),
            ],
            label: Label::Useful,
        },
        Case {
            name: "rephrased_search_circling",
            class: "stuck loop",
            steps: &[
                ok(SEARCH, r#"{"query":"where is stock reserved"}"#, STOCK),
                ok(SEARCH, r#"{"query":"where is the stock reserved"}"#, STOCK),
            ],
            label: Label::Useful,
        },
        Case {
            name: "identical_grep_with_selector",
            class: "stuck loop",
            steps: &[
                ok(
                    "tracedecay_grep",
                    r#"{"pattern":"reserve_stock","project_selector":{"project_id":"orders-fixture"}}"#,
                    STOCK,
                ),
                ok(
                    "tracedecay_grep",
                    r#"{"pattern":"reserve_stock","project_selector":{"project_id":"orders-fixture"}}"#,
                    STOCK,
                ),
            ],
            label: Label::Useful,
        },
        Case {
            name: "identical_body_three_calls_back",
            class: "stuck loop",
            steps: &[
                ok(
                    "tracedecay_source_body",
                    r#"{"node_id":"fn:src/orders.rs:reserve_stock"}"#,
                    STOCK,
                ),
                ok(
                    "tracedecay_callers",
                    r#"{"node_id":"fn:src/orders.rs:reserve_stock"}"#,
                    "place_order and backorder_retry call reserve_stock",
                ),
                ok("tracedecay_grep", r#"{"pattern":"held_stock"}"#, STOCK),
                ok(
                    "tracedecay_source_body",
                    r#"{"node_id":"fn:src/orders.rs:reserve_stock"}"#,
                    STOCK,
                ),
            ],
            label: Label::Useful,
        },
        Case {
            name: "stemmed_context_rephrase",
            class: "stuck loop",
            steps: &[
                ok(
                    "tracedecay_context",
                    r#"{"task":"explain how orders reserve warehouse stock"}"#,
                    STOCK,
                ),
                ok(
                    "tracedecay_context",
                    r#"{"task":"explain how an order reserves warehouse stock"}"#,
                    STOCK,
                ),
            ],
            label: Label::Missed,
        },
        Case {
            name: "narrow_by_path_glob",
            class: "narrowing",
            steps: &[
                ok("tracedecay_grep", r#"{"pattern":"reserve_stock"}"#, STOCK),
                ok(
                    "tracedecay_grep",
                    r#"{"pattern":"reserve_stock","path_glob":"src/orders/**"}"#,
                    STOCK,
                ),
            ],
            label: Label::Quiet,
        },
        Case {
            name: "narrow_by_added_term",
            class: "narrowing",
            steps: &[
                ok(SEARCH, r#"{"query":"stock reservation"}"#, STOCK),
                ok(
                    SEARCH,
                    r#"{"query":"stock reservation rollback"}"#,
                    ROLLBACK,
                ),
            ],
            label: Label::KnownFalsePositive,
        },
        Case {
            name: "narrow_with_shared_selector_and_format",
            class: "narrowing",
            steps: &[
                ok(
                    SEARCH,
                    r#"{"query":"stock reservation","project_selector":{"project_id":"orders-fixture"},"format":"markdown"}"#,
                    STOCK,
                ),
                ok(
                    SEARCH,
                    r#"{"query":"stock rollback","project_selector":{"project_id":"orders-fixture"},"format":"markdown"}"#,
                    ROLLBACK,
                ),
            ],
            label: Label::Quiet,
        },
        Case {
            name: "next_page_by_cursor",
            class: "pagination",
            steps: &[
                ok(
                    SEARCH,
                    r#"{"query":"order total computation tax","limit":5}"#,
                    TOTALS,
                ),
                ok(
                    SEARCH,
                    r#"{"query":"order total computation tax","limit":5,"cursor":"b3BhcXVlLWN1cnNvci0x"}"#,
                    "apply_tax in src/tax.rs rounds each line before compute_total sums it",
                ),
                ok(
                    SEARCH,
                    r#"{"query":"order total computation tax","limit":5,"cursor":"b3BhcXVlLWN1cnNvci0y"}"#,
                    "tax_rate in src/tax.rs looks up the rate for the shipping region",
                ),
            ],
            label: Label::Quiet,
        },
        Case {
            name: "callers_next_page_meta_cursor",
            class: "pagination",
            steps: &[
                ok(
                    "tracedecay_callers",
                    r#"{"node_id":"fn:src/pricing.rs:compute_total","meta":{"cursor":null,"order":"source_position","projection":"evidence"}}"#,
                    CALLERS,
                ),
                ok(
                    "tracedecay_callers",
                    r#"{"node_id":"fn:src/pricing.rs:compute_total","meta":{"cursor":"Y3Vyc29yLTI","order":"source_position","projection":"evidence"}}"#,
                    CALLERS_PAGE_2,
                ),
            ],
            label: Label::Quiet,
        },
        Case {
            name: "same_query_other_project",
            class: "changed scope",
            steps: &[
                ok(
                    SEARCH,
                    r#"{"query":"who calls compute total","project_selector":{"project_id":"orders"}}"#,
                    CALLERS,
                ),
                ok(
                    SEARCH,
                    r#"{"query":"who calls compute total","project_selector":{"project_id":"billing"}}"#,
                    "invoice_run in src/billing.rs calls compute_total",
                ),
            ],
            label: Label::Quiet,
        },
        Case {
            name: "widened_caller_depth",
            class: "changed scope",
            steps: &[
                ok(
                    "tracedecay_callers",
                    r#"{"node_id":"fn:src/pricing.rs:compute_total","maximum_depth":1}"#,
                    CALLERS,
                ),
                ok(
                    "tracedecay_callers",
                    r#"{"node_id":"fn:src/pricing.rs:compute_total","maximum_depth":3}"#,
                    "checkout, invoice_preview, and their http handlers call compute_total",
                ),
            ],
            label: Label::Quiet,
        },
        Case {
            name: "other_branch_same_query",
            class: "changed generation",
            steps: &[
                ok(
                    "tracedecay_branch_search",
                    r#"{"branch":"main","query":"apply_discount"}"#,
                    "apply_discount in src/pricing.rs takes a percentage",
                ),
                ok(
                    "tracedecay_branch_search",
                    r#"{"branch":"feature/pricing-notes","query":"apply_discount"}"#,
                    "apply_discount in src/pricing.rs takes a discount rule",
                ),
            ],
            label: Label::Quiet,
        },
        Case {
            name: "identical_search_after_reindex",
            class: "changed generation",
            steps: &[
                ok(SEARCH, r#"{"query":"where is stock reserved"}"#, STOCK),
                ok(
                    SEARCH,
                    r#"{"query":"where is stock reserved"}"#,
                    "reserve_stock moved to src/inventory.rs and holds stock per warehouse",
                ),
            ],
            label: Label::Quiet,
        },
        Case {
            name: "diagnostics_after_edit",
            class: "repeated verification",
            steps: &[
                ok(
                    "tracedecay_diagnostics",
                    r#"{"path":"src/orders.rs"}"#,
                    "error E0308 in src/orders.rs line 42 expected u32 found i64",
                ),
                ok(
                    "tracedecay_str_replace",
                    r#"{"path":"src/orders.rs","old_str":"let held: i64","new_str":"let held: u32"}"#,
                    "replaced one occurrence in src/orders.rs",
                ),
                ok(
                    "tracedecay_diagnostics",
                    r#"{"path":"src/orders.rs"}"#,
                    "no diagnostics reported for src/orders.rs",
                ),
            ],
            label: Label::Quiet,
        },
        Case {
            name: "test_results_unchanged",
            class: "repeated verification",
            steps: &[
                ok(
                    "tracedecay_test_results",
                    r#"{"filter":"orders"}"#,
                    "orders tests failed reserve_stock_holds_stock assertion",
                ),
                ok(
                    "tracedecay_test_results",
                    r#"{"filter":"orders"}"#,
                    "orders tests failed reserve_stock_holds_stock assertion",
                ),
            ],
            label: Label::Useful,
        },
        Case {
            name: "retry_after_failure",
            class: "repeated verification",
            steps: &[
                Step {
                    tool: SEARCH,
                    arguments: r#"{"query":"where is stock reserved"}"#,
                    answer: "project orders-fixture is still opening, retry shortly",
                    is_error: true,
                },
                ok(SEARCH, r#"{"query":"where is stock reserved"}"#, STOCK),
            ],
            label: Label::Quiet,
        },
        Case {
            name: "json_after_markdown",
            class: "changed presentation",
            steps: &[
                ok(
                    SEARCH,
                    r#"{"query":"order total computation tax","format":"markdown"}"#,
                    TOTALS,
                ),
                ok(
                    SEARCH,
                    r#"{"query":"order total computation tax","format":"json"}"#,
                    r#"{"results":[{"name":"compute_total","path":"src/pricing.rs"}]}"#,
                ),
            ],
            label: Label::Quiet,
        },
    ];

    const CORPUS_TOTALS: [(Label, usize); 4] = [
        (Label::Useful, 5),
        (Label::Quiet, 11),
        (Label::KnownFalsePositive, 1),
        (Label::Missed, 1),
    ];

    fn response(id: i64, text: &str, is_error: bool) -> Vec<String> {
        vec![format!(
            "{}\n",
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {"content": [{"type": "text", "text": text}], "isError": is_error},
            })
        )]
    }

    #[test]
    fn the_outcome_corpus_matches_every_label() {
        let mut table = String::from("name | class | label | observed | hint\n");
        let mut mismatches = Vec::new();
        let mut totals = std::collections::BTreeMap::<Label, usize>::new();
        for case in CORPUS {
            let mut queries = enabled();
            let mut last = Vec::new();
            for (id, step) in (1..).zip(case.steps) {
                let arguments: Value = serde_json::from_str(step.arguments).expect("arguments");
                let mut lines = response(id, step.answer, step.is_error);
                queries.rewrite(Some(&call(id, step.tool, &arguments)), &mut lines, false);
                last = lines;
            }
            let hint = hints(&last).pop();
            let observed = case.label.observed(hint.is_some());
            let overlap = hint
                .as_deref()
                .and_then(|hint| hint.split(" of its query words").next())
                .and_then(|clause| clause.rsplit(' ').next())
                .unwrap_or("-");
            table.push_str(&format!(
                "{} | {} | {:?} | {observed:?} | {overlap}\n",
                case.name, case.class, case.label
            ));
            if observed != case.label {
                mismatches.push(case.name);
            }
            *totals.entry(observed).or_default() += 1;
        }
        assert!(mismatches.is_empty(), "mismatched {mismatches:?}\n{table}");
        assert_eq!(
            totals.into_iter().collect::<Vec<_>>(),
            CORPUS_TOTALS.to_vec(),
            "\n{table}"
        );
    }
}
