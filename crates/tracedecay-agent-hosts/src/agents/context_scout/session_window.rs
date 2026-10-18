//! Bounded, transient reader of a session's most recent turns.
//!
//! The decision engine ([`super::decision_engine`]) judges a prompt better
//! with the last few turns of its own session than with the prompt alone. This
//! module reads that tail from the daemon's own LCM raw-message store (the
//! same transcript store every host ingests into) and returns a bounded
//! [`SessionWindowV1`] for model input. A window is never persisted; only its
//! [`SessionWindowV1::window_digest`] is safe to keep as a cache or receipt
//! key.
//!
//! # Store API
//!
//! The reader is an `async fn` over [`RegisteredGlobalDb`] and uses two of its
//! LCM reads, both of which verify and redact like every other transcript
//! read:
//!
//! 1. `lcm_session_replay_slice` with only a tail asks for the newest `K`
//!    store ids of the session. `lcm_load_session` pages forward only, and
//!    store ids interleave across sessions, so the tail is how the first
//!    store id to load from is found without paging the whole session.
//! 2. `lcm_load_session` loads exactly those rows with content and metadata.
//!
//! `K` is the message cap clamped to [`SESSION_WINDOW_FETCH_CEILING`], the
//! store's page size, so a larger `max_messages` is served as that ceiling.
//!
//! # Tool calls
//!
//! The load API returns content and `metadata_json` but not the `tool_names`
//! column, and hosts record tool use differently:
//!
//! * Cursor transcripts carry canonical `metadata_json.tool_calls`
//!   (`{"type":"function","function":{"name","arguments"}}`) and
//!   `tool_events` (`{"type":"tool_use","tool_name",...}`).
//! * Claude, Codex and the other typed-fact hosts keep authored text in
//!   `content` and put tool names only in the `tool_names` column
//!   (comma-joined); their tool-only records are rows of kind
//!   `tool_invocation`/`tool_call`/`tool_dispatch`/`file_edit` whose text is
//!   the argument payload.
//!
//! Assistant rows with no tool metadata are therefore completed from
//! `get_session_message` (one bounded local read each), through
//! [`tracedecay_automation::analytics::split_tool_names`], the splitter hint
//! settlement already uses.

use sha2::{Digest, Sha256};
use tracedecay_automation::analytics::split_tool_names;
use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_lcm::{
    LcmContentSlice, LcmLoadSessionMessage, LcmLoadSessionRequest, LcmSessionReplayRequest,
};

/// Rows one window can load: the LCM store's page size.
pub const SESSION_WINDOW_FETCH_CEILING: usize = 100;

/// Longest argument excerpt kept for one tool call, in characters.
pub const SESSION_WINDOW_ARGUMENT_EXCERPT_CHARS: usize = 160;

/// Stored timestamps at or above this are milliseconds, below it seconds.
const MILLIS_THRESHOLD: i64 = 1_000_000_000_000;

const ELLIPSIS: &str = "…";

/// How much of a session one window may carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionWindowLimitsV1 {
    /// User messages kept, newest first.
    pub max_user_turns: usize,
    /// Messages kept in total, newest first.
    pub max_messages: usize,
    /// Text and tool-call bytes kept in total, newest first.
    pub max_bytes: usize,
    /// A window whose newest message is older than this is stale.
    pub max_age_micros: Option<u64>,
}

#[derive(Debug, Clone, Copy)]
pub struct SessionWindowRequestV1<'a> {
    pub provider: &'a str,
    pub session_id: &'a str,
    pub limits: SessionWindowLimitsV1,
    pub now_micros: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionWindowRoleV1 {
    User,
    Assistant,
    Tool,
    /// System, goal, compaction and any other non-conversational row.
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionWindowToolCallV1 {
    pub name: String,
    pub argument_excerpt: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionWindowMessageV1 {
    pub role: SessionWindowRoleV1,
    pub text: String,
    pub tool_calls: Vec<SessionWindowToolCallV1>,
    pub store_id: i64,
    /// Normalized to microseconds; `None` when the host recorded no time.
    pub timestamp_micros: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionWindowFreshnessV1 {
    Fresh,
    Stale {
        age_micros: u64,
    },
    /// No message carried a timestamp.
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionWindowV1 {
    /// Oldest first.
    pub messages: Vec<SessionWindowMessageV1>,
    pub first_store_id: i64,
    pub last_store_id: i64,
    pub newest_timestamp_micros: Option<i64>,
    /// Digest of the window's identity and content; the only part safe to
    /// persist.
    pub window_digest: [u8; 32],
    pub freshness: SessionWindowFreshnessV1,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionWindowOutcomeV1 {
    Ready(SessionWindowV1),
    /// The session has no usable messages (or the limits allow none).
    Empty,
    /// The store could not be read; the reason is for logs, not for users.
    Unavailable(String),
}

/// Reads the newest turns of one session as a bounded transient window.
///
/// Selection: the last `max_user_turns` user messages and everything after the
/// earliest of them, capped at `max_messages` and `max_bytes` with the oldest
/// messages dropped first. A single newest message larger than `max_bytes`
/// keeps its leading bytes.
pub async fn read_session_window(
    db: &RegisteredGlobalDb,
    request: &SessionWindowRequestV1<'_>,
) -> SessionWindowOutcomeV1 {
    let limits = request.limits;
    if limits.max_user_turns == 0 || limits.max_messages == 0 || limits.max_bytes == 0 {
        return SessionWindowOutcomeV1::Empty;
    }
    let fetch = limits.max_messages.min(SESSION_WINDOW_FETCH_CEILING);

    let tail = match db
        .lcm_session_replay_slice(&LcmSessionReplayRequest {
            provider: request.provider.to_owned(),
            session_id: request.session_id.to_owned(),
            head_limit: 0,
            tail_limit: fetch,
            max_snippet_chars: 1,
            summary_limit: 0,
            max_summary_chars: 0,
        })
        .await
    {
        Ok(slice) => slice.tail,
        Err(error) => return unavailable("tail", &error),
    };
    let Some(oldest) = tail.iter().map(|message| message.store_id).min() else {
        return SessionWindowOutcomeV1::Empty;
    };

    let page = match db
        .lcm_load_session(LcmLoadSessionRequest {
            provider: request.provider.to_owned(),
            session_id: request.session_id.to_owned(),
            after_store_id: Some(oldest.saturating_sub(1)),
            limit: fetch,
            roles: Vec::new(),
            start_time: None,
            end_time: None,
            content_slice: Some(LcmContentSlice {
                offset: 0,
                limit: limits.max_bytes,
            }),
        })
        .await
    {
        Ok(page) => page.messages,
        Err(error) => return unavailable("load", &error),
    };

    let mut messages = Vec::with_capacity(page.len());
    for loaded in page {
        if let Some(message) = window_message(db, loaded).await {
            messages.push(message);
        }
    }
    // A concurrent ingest can add rows after the tail was read; keep the
    // newest `fetch` of what loaded.
    if messages.len() > fetch {
        messages.drain(..messages.len() - fetch);
    }
    match select(messages, limits) {
        Some(messages) => SessionWindowOutcomeV1::Ready(assemble(request, messages)),
        None => SessionWindowOutcomeV1::Empty,
    }
}

fn unavailable(step: &str, error: &dyn std::fmt::Display) -> SessionWindowOutcomeV1 {
    tracing::debug!(step, %error, "session window unavailable");
    SessionWindowOutcomeV1::Unavailable(format!("session window {step} failed: {error}"))
}

async fn window_message(
    db: &RegisteredGlobalDb,
    loaded: LcmLoadSessionMessage,
) -> Option<SessionWindowMessageV1> {
    let role = role_of(&loaded.role);
    let mut text = loaded.content.trim().to_owned();
    let mut tool_calls = loaded
        .metadata_json
        .as_deref()
        .map(tool_calls_from_metadata)
        .unwrap_or_default();
    if tool_calls.is_empty() && role == SessionWindowRoleV1::Assistant {
        // Typed-fact hosts keep tool names only in the `tool_names` column.
        match db
            .get_session_message(&loaded.provider, &loaded.message_id)
            .await
        {
            Ok(Some(record)) => {
                let names = record
                    .tool_names
                    .as_deref()
                    .map(|names| split_tool_names(names).collect::<Vec<_>>())
                    .unwrap_or_default();
                let payload_row = matches!(
                    record.kind.as_deref(),
                    Some("tool_invocation" | "tool_call" | "tool_dispatch" | "file_edit")
                );
                if payload_row && names.len() == 1 {
                    // The row's text is the call's argument payload.
                    tool_calls.push(SessionWindowToolCallV1 {
                        name: names.into_iter().next().unwrap_or_default(),
                        argument_excerpt: excerpt(&text),
                    });
                    text.clear();
                } else {
                    tool_calls.extend(names.into_iter().map(|name| SessionWindowToolCallV1 {
                        name,
                        argument_excerpt: None,
                    }));
                }
            }
            Ok(None) => {}
            Err(error) => {
                tracing::debug!(%error, "session window tool names unavailable");
            }
        }
    }
    if text.is_empty() && tool_calls.is_empty() {
        return None;
    }
    Some(SessionWindowMessageV1 {
        role,
        text,
        tool_calls,
        store_id: loaded.store_id,
        timestamp_micros: loaded.timestamp.map(timestamp_micros),
    })
}

fn role_of(role: &str) -> SessionWindowRoleV1 {
    match role {
        "user" => SessionWindowRoleV1::User,
        "assistant" => SessionWindowRoleV1::Assistant,
        "tool" => SessionWindowRoleV1::Tool,
        _ => SessionWindowRoleV1::Other,
    }
}

/// Hosts store seconds or milliseconds; the window speaks microseconds.
fn timestamp_micros(stored: i64) -> i64 {
    if stored >= MILLIS_THRESHOLD {
        stored.saturating_mul(1_000)
    } else {
        stored.saturating_mul(1_000_000)
    }
}

/// Canonical `tool_calls`, else the names of `tool_events`.
fn tool_calls_from_metadata(metadata: &str) -> Vec<SessionWindowToolCallV1> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(metadata) else {
        return Vec::new();
    };
    let calls = value
        .get("tool_calls")
        .and_then(serde_json::Value::as_array)
        .map(|calls| {
            calls
                .iter()
                .filter_map(|call| {
                    let function = call.get("function")?;
                    let name = function.get("name")?.as_str()?.trim();
                    if name.is_empty() {
                        return None;
                    }
                    let arguments =
                        function
                            .get("arguments")
                            .and_then(|arguments| match arguments {
                                serde_json::Value::Null => None,
                                serde_json::Value::String(text) => excerpt(text),
                                other => excerpt(&other.to_string()),
                            });
                    Some(SessionWindowToolCallV1 {
                        name: name.to_owned(),
                        argument_excerpt: arguments,
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if !calls.is_empty() {
        return calls;
    }
    value
        .get("tool_events")
        .and_then(serde_json::Value::as_array)
        .map(|events| {
            events
                .iter()
                .filter_map(|event| {
                    let name = event.get("tool_name")?.as_str()?.trim();
                    (!name.is_empty()).then(|| SessionWindowToolCallV1 {
                        name: name.to_owned(),
                        argument_excerpt: None,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Whitespace-collapsed, length-bounded one-line excerpt.
fn excerpt(text: &str) -> Option<String> {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return None;
    }
    Some(
        match collapsed
            .char_indices()
            .nth(SESSION_WINDOW_ARGUMENT_EXCERPT_CHARS)
        {
            Some((end, _)) => format!("{}{ELLIPSIS}", &collapsed[..end]),
            None => collapsed,
        },
    )
}

fn message_bytes(message: &SessionWindowMessageV1) -> usize {
    message.text.len()
        + message
            .tool_calls
            .iter()
            .map(|call| call.name.len() + call.argument_excerpt.as_ref().map_or(0, String::len))
            .sum::<usize>()
}

/// Applies the turn, message and byte caps to messages ordered oldest first.
fn select(
    mut messages: Vec<SessionWindowMessageV1>,
    limits: SessionWindowLimitsV1,
) -> Option<Vec<SessionWindowMessageV1>> {
    let user_indices = messages
        .iter()
        .enumerate()
        .filter(|(_, message)| message.role == SessionWindowRoleV1::User)
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let start = user_indices
        .len()
        .checked_sub(limits.max_user_turns)
        .map_or(0, |skipped| user_indices[skipped]);
    messages.drain(..start);
    if messages.len() > limits.max_messages {
        messages.drain(..messages.len() - limits.max_messages);
    }

    // Newest first until the byte budget is spent.
    let mut spent = 0usize;
    let mut keep_from = messages.len();
    for (index, message) in messages.iter().enumerate().rev() {
        let bytes = message_bytes(message);
        if spent.saturating_add(bytes) > limits.max_bytes {
            break;
        }
        spent += bytes;
        keep_from = index;
    }
    if keep_from == messages.len() {
        // Even the newest message alone is over budget: keep its leading bytes.
        let newest = messages.last_mut()?;
        newest.tool_calls.clear();
        newest.text = head_on_char_boundary(&newest.text, limits.max_bytes).to_owned();
        if newest.text.is_empty() {
            return None;
        }
        keep_from = messages.len() - 1;
    }
    messages.drain(..keep_from);
    (!messages.is_empty()).then_some(messages)
}

fn head_on_char_boundary(text: &str, max_bytes: usize) -> &str {
    let mut end = max_bytes.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn tail_on_char_boundary(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut start = text.len() - max_bytes;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

fn assemble(
    request: &SessionWindowRequestV1<'_>,
    messages: Vec<SessionWindowMessageV1>,
) -> SessionWindowV1 {
    let first_store_id = messages.first().map_or(0, |message| message.store_id);
    let last_store_id = messages.last().map_or(0, |message| message.store_id);
    let newest_timestamp_micros = messages
        .iter()
        .filter_map(|message| message.timestamp_micros)
        .max();
    SessionWindowV1 {
        window_digest: window_digest(request.provider, request.session_id, &messages),
        freshness: freshness(
            newest_timestamp_micros,
            request.now_micros,
            request.limits.max_age_micros,
        ),
        messages,
        first_store_id,
        last_store_id,
        newest_timestamp_micros,
    }
}

fn freshness(
    newest_micros: Option<i64>,
    now_micros: u64,
    max_age_micros: Option<u64>,
) -> SessionWindowFreshnessV1 {
    let Some(newest) = newest_micros else {
        return SessionWindowFreshnessV1::Unknown;
    };
    let newest = u64::try_from(newest).unwrap_or(0);
    let age_micros = now_micros.saturating_sub(newest);
    match max_age_micros {
        Some(max_age) if age_micros > max_age => SessionWindowFreshnessV1::Stale { age_micros },
        _ => SessionWindowFreshnessV1::Fresh,
    }
}

fn digest_field(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

/// Length-framed digest over the session identity and, per message, its store
/// id, role, text and tool calls. Timestamps are excluded so the digest names
/// content, not observation time.
fn window_digest(
    provider: &str,
    session_id: &str,
    messages: &[SessionWindowMessageV1],
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    digest_field(&mut hasher, b"tracedecay.session_window.v1");
    digest_field(&mut hasher, provider.as_bytes());
    digest_field(&mut hasher, session_id.as_bytes());
    for message in messages {
        hasher.update(message.store_id.to_le_bytes());
        hasher.update([match message.role {
            SessionWindowRoleV1::User => 0u8,
            SessionWindowRoleV1::Assistant => 1,
            SessionWindowRoleV1::Tool => 2,
            SessionWindowRoleV1::Other => 3,
        }]);
        digest_field(&mut hasher, message.text.as_bytes());
        hasher.update((message.tool_calls.len() as u64).to_le_bytes());
        for call in &message.tool_calls {
            digest_field(&mut hasher, call.name.as_bytes());
            digest_field(
                &mut hasher,
                call.argument_excerpt
                    .as_deref()
                    .unwrap_or_default()
                    .as_bytes(),
            );
        }
    }
    hasher.finalize().into()
}

/// Renders the window as a plain-text transcript, oldest to newest.
///
/// Lines are `User: …`, `Assistant: …`, `Tool result: …`, `Other: …` and, for
/// each recorded call, `Tool <name>(<argument excerpt>)`. `prompt_excerpt`,
/// the turn being decided on, is appended last as a user turn. When the whole
/// exceeds `max_bytes` the front is cut on a character boundary so the newest
/// text survives.
pub fn render_window_state(
    window: &SessionWindowV1,
    prompt_excerpt: Option<&str>,
    max_bytes: usize,
) -> String {
    let mut lines = Vec::new();
    for message in &window.messages {
        let label = match message.role {
            SessionWindowRoleV1::User => "User",
            SessionWindowRoleV1::Assistant => "Assistant",
            SessionWindowRoleV1::Tool => "Tool result",
            SessionWindowRoleV1::Other => "Other",
        };
        if !message.text.is_empty() {
            lines.push(format!("{label}: {}", message.text));
        }
        for call in &message.tool_calls {
            lines.push(format!(
                "Tool {}({})",
                call.name,
                call.argument_excerpt.as_deref().unwrap_or_default()
            ));
        }
    }
    if let Some(prompt) = prompt_excerpt
        .map(str::trim)
        .filter(|prompt| !prompt.is_empty())
    {
        lines.push(format!("User: {prompt}"));
    }
    tail_on_char_boundary(&lines.join("\n"), max_bytes).to_owned()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use serde_json::json;
    use tracedecay_global_db::tests::harness::RegisteredGlobalDbHarness;
    use tracedecay_sessions::runtime::{SessionMessageRecord, SessionRecord};

    use super::*;

    const PROVIDER: &str = "cursor";
    const SESSION: &str = "window-session";
    const NOW_SECS: i64 = 1_800_000_000;

    struct Seed {
        harness: RegisteredGlobalDbHarness,
    }

    impl Seed {
        async fn open(label: &str) -> Self {
            Self {
                harness: RegisteredGlobalDbHarness::open(label).await,
            }
        }

        fn db(&self) -> &RegisteredGlobalDb {
            self.harness.registered.as_ref()
        }

        async fn session(&self, provider: &str, session_id: &str) {
            assert!(
                self.db()
                    .upsert_session(&SessionRecord {
                        provider: provider.to_owned(),
                        session_id: session_id.to_owned(),
                        project_key: "/project".to_owned(),
                        project_path: "/project".to_owned(),
                        title: None,
                        started_at: None,
                        ended_at: None,
                        transcript_path: None,
                        metadata_json: None,
                        parent_session_id: None,
                        is_subagent: false,
                        agent_id: None,
                        parent_tool_use_id: None,
                    })
                    .await
            );
        }

        async fn message(&self, message: SessionMessageRecord) {
            let root = self.db().db_path().parent().unwrap().to_path_buf();
            self.db()
                .lcm_ingest_raw_message(&root, &message)
                .await
                .unwrap();
        }
    }

    fn record(
        provider: &str,
        session_id: &str,
        ordinal: i64,
        role: &str,
        text: &str,
    ) -> SessionMessageRecord {
        SessionMessageRecord {
            provider: provider.to_owned(),
            message_id: format!("{provider}-{session_id}-{ordinal:04}"),
            session_id: session_id.to_owned(),
            role: role.to_owned(),
            timestamp: Some(NOW_SECS - 1_000 + ordinal),
            ordinal,
            text: text.to_owned(),
            kind: Some("message".to_owned()),
            model: None,
            tool_names: None,
            source_path: None,
            source_offset: None,
            metadata_json: None,
        }
    }

    fn limits() -> SessionWindowLimitsV1 {
        SessionWindowLimitsV1 {
            max_user_turns: 2,
            max_messages: 20,
            max_bytes: 4096,
            max_age_micros: Some(600 * 1_000_000),
        }
    }

    fn request(limits: SessionWindowLimitsV1) -> SessionWindowRequestV1<'static> {
        SessionWindowRequestV1 {
            provider: PROVIDER,
            session_id: SESSION,
            limits,
            now_micros: u64::try_from(NOW_SECS).unwrap() * 1_000_000,
        }
    }

    fn ready(outcome: SessionWindowOutcomeV1) -> SessionWindowV1 {
        match outcome {
            SessionWindowOutcomeV1::Ready(window) => window,
            other => panic!("expected a window, got {other:?}"),
        }
    }

    fn texts(window: &SessionWindowV1) -> Vec<&str> {
        window
            .messages
            .iter()
            .map(|message| message.text.as_str())
            .collect()
    }

    /// Three user turns with replies, interleaved with another session so the
    /// store ids of the target session are not contiguous.
    async fn seed_conversation(seed: &Seed) {
        seed.session(PROVIDER, SESSION).await;
        seed.session(PROVIDER, "other-session").await;
        let turns = [
            ("user", "first question"),
            ("assistant", "first answer"),
            ("user", "second question"),
            ("assistant", "second answer"),
            ("user", "third question"),
            ("assistant", "third answer"),
        ];
        for (ordinal, (role, text)) in turns.into_iter().enumerate() {
            let ordinal = i64::try_from(ordinal).unwrap();
            seed.message(record(PROVIDER, SESSION, ordinal, role, text))
                .await;
            seed.message(record(PROVIDER, "other-session", ordinal, "user", "noise"))
                .await;
        }
    }

    #[asupersync::test]
    async fn an_unknown_or_empty_session_yields_an_empty_window() {
        let seed = Seed::open("window-empty").await;
        assert_eq!(
            read_session_window(seed.db(), &request(limits())).await,
            SessionWindowOutcomeV1::Empty
        );
        seed_conversation(&seed).await;
        let mut other = request(limits());
        other.session_id = "never-ingested";
        assert_eq!(
            read_session_window(seed.db(), &other).await,
            SessionWindowOutcomeV1::Empty
        );
        for zero in [
            SessionWindowLimitsV1 {
                max_user_turns: 0,
                ..limits()
            },
            SessionWindowLimitsV1 {
                max_messages: 0,
                ..limits()
            },
            SessionWindowLimitsV1 {
                max_bytes: 0,
                ..limits()
            },
        ] {
            assert_eq!(
                read_session_window(seed.db(), &request(zero)).await,
                SessionWindowOutcomeV1::Empty
            );
        }
    }

    #[asupersync::test]
    async fn the_last_user_turns_and_their_replies_are_kept_oldest_first() {
        let seed = Seed::open("window-turns").await;
        seed_conversation(&seed).await;

        let window = ready(read_session_window(seed.db(), &request(limits())).await);
        assert_eq!(
            texts(&window),
            vec![
                "second question",
                "second answer",
                "third question",
                "third answer"
            ]
        );
        assert_eq!(window.first_store_id, window.messages[0].store_id);
        assert_eq!(window.last_store_id, window.messages[3].store_id);
        assert!(
            window
                .messages
                .windows(2)
                .all(|pair| pair[0].store_id < pair[1].store_id)
        );
        assert_eq!(window.messages[0].role, SessionWindowRoleV1::User);
        assert_eq!(window.messages[1].role, SessionWindowRoleV1::Assistant);

        let one_turn = ready(
            read_session_window(
                seed.db(),
                &request(SessionWindowLimitsV1 {
                    max_user_turns: 1,
                    ..limits()
                }),
            )
            .await,
        );
        assert_eq!(texts(&one_turn), vec!["third question", "third answer"]);

        let everything = ready(
            read_session_window(
                seed.db(),
                &request(SessionWindowLimitsV1 {
                    max_user_turns: 10,
                    ..limits()
                }),
            )
            .await,
        );
        assert_eq!(everything.messages.len(), 6);
    }

    #[asupersync::test]
    async fn the_message_cap_drops_the_oldest_first() {
        let seed = Seed::open("window-message-cap").await;
        seed_conversation(&seed).await;
        let window = ready(
            read_session_window(
                seed.db(),
                &request(SessionWindowLimitsV1 {
                    max_user_turns: 10,
                    max_messages: 3,
                    ..limits()
                }),
            )
            .await,
        );
        assert_eq!(
            texts(&window),
            vec!["second answer", "third question", "third answer"]
        );
    }

    #[asupersync::test]
    async fn the_byte_cap_keeps_the_newest_messages() {
        let seed = Seed::open("window-byte-cap").await;
        seed_conversation(&seed).await;
        // "third question" (14) + "third answer" (12) = 26 bytes.
        let window = ready(
            read_session_window(
                seed.db(),
                &request(SessionWindowLimitsV1 {
                    max_user_turns: 10,
                    max_bytes: 30,
                    ..limits()
                }),
            )
            .await,
        );
        assert_eq!(texts(&window), vec!["third question", "third answer"]);

        // One message over budget keeps its leading bytes rather than nothing.
        let tiny = ready(
            read_session_window(
                seed.db(),
                &request(SessionWindowLimitsV1 {
                    max_user_turns: 10,
                    max_bytes: 5,
                    ..limits()
                }),
            )
            .await,
        );
        assert_eq!(texts(&tiny), vec!["third"]);
    }

    #[asupersync::test]
    async fn cursor_tool_calls_come_from_canonical_metadata() {
        let seed = Seed::open("window-cursor-tools").await;
        seed.session(PROVIDER, SESSION).await;
        seed.message(record(PROVIDER, SESSION, 0, "user", "list the files"))
            .await;
        let mut assistant = record(PROVIDER, SESSION, 1, "assistant", "Running a command.");
        assistant.metadata_json = Some(
            json!({
                "tool_calls": [
                    {"type": "function", "id": "call_1",
                     "function": {"name": "Shell", "arguments": {"command": "ls   -la"}}},
                    {"type": "function",
                     "function": {"name": "Read", "arguments": "path=lib.rs"}},
                ],
                "tool_events": [
                    {"type": "tool_use", "tool_name": "Shell", "call_id": "call_1", "input_bytes": 20},
                ],
            })
            .to_string(),
        );
        seed.message(assistant).await;

        let window = ready(read_session_window(seed.db(), &request(limits())).await);
        let assistant = &window.messages[1];
        assert_eq!(assistant.text, "Running a command.");
        assert_eq!(
            assistant.tool_calls,
            vec![
                SessionWindowToolCallV1 {
                    name: "Shell".to_owned(),
                    argument_excerpt: Some(r#"{"command":"ls -la"}"#.to_owned()),
                },
                SessionWindowToolCallV1 {
                    name: "Read".to_owned(),
                    argument_excerpt: Some("path=lib.rs".to_owned()),
                },
            ]
        );
    }

    #[asupersync::test]
    async fn tool_events_alone_still_name_the_tools() {
        let calls = tool_calls_from_metadata(
            &json!({"tool_events": [
                {"type": "tool_use", "tool_name": "Grep"},
                {"type": "tool_use", "tool_name": ""},
            ]})
            .to_string(),
        );
        assert_eq!(
            calls,
            vec![SessionWindowToolCallV1 {
                name: "Grep".to_owned(),
                argument_excerpt: None
            }]
        );
        assert!(tool_calls_from_metadata("not json").is_empty());
    }

    #[asupersync::test]
    async fn typed_fact_hosts_name_tools_through_the_tool_names_column() {
        let seed = Seed::open("window-claude-tools").await;
        seed.session("claude", SESSION).await;
        seed.message(record("claude", SESSION, 0, "user", "fix the parser"))
            .await;
        let mut reply = record("claude", SESSION, 1, "assistant", "Looking at it.");
        reply.tool_names = Some("Read,Grep".to_owned());
        seed.message(reply).await;
        let mut dispatch = record(
            "claude",
            SESSION,
            2,
            "assistant",
            r#"{"path":"src/lib.rs"}"#,
        );
        dispatch.kind = Some("tool_invocation".to_owned());
        dispatch.tool_names = Some("Read".to_owned());
        seed.message(dispatch).await;

        let mut claude = request(limits());
        claude.provider = "claude";
        let window = ready(read_session_window(seed.db(), &claude).await);
        assert_eq!(window.messages.len(), 3);
        assert_eq!(window.messages[1].text, "Looking at it.");
        assert_eq!(
            window.messages[1]
                .tool_calls
                .iter()
                .map(|call| call.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Read", "Grep"]
        );
        assert_eq!(window.messages[2].text, "");
        assert_eq!(
            window.messages[2].tool_calls,
            vec![SessionWindowToolCallV1 {
                name: "Read".to_owned(),
                argument_excerpt: Some(r#"{"path":"src/lib.rs"}"#.to_owned()),
            }]
        );
    }

    /// Hermes and Codex project a tool-only turn as a `tool_invocation` row
    /// whose text is the argument payload and whose `tool_names` holds the
    /// tool (`terminal` / `exec_command`), as asserted by the ingest suites
    /// `transcript_ingest_suite/hermes.rs` and `codex_response_items.rs`.
    #[asupersync::test]
    async fn hermes_and_codex_tool_invocation_rows_become_tool_calls() {
        for (provider, tool, payload) in [
            ("hermes", "terminal", "cargo test billing"),
            ("codex", "exec_command", "gh pr merge 366 --squash"),
        ] {
            let seed = Seed::open(&format!("window-{provider}-tools")).await;
            seed.session(provider, SESSION).await;
            seed.message(record(provider, SESSION, 0, "user", "run the tests"))
                .await;
            let mut invocation = record(provider, SESSION, 1, "assistant", payload);
            invocation.kind = Some("tool_invocation".to_owned());
            invocation.tool_names = Some(tool.to_owned());
            seed.message(invocation).await;
            seed.message(record(provider, SESSION, 2, "assistant", "Done."))
                .await;

            let mut host = request(limits());
            host.provider = provider;
            let window = ready(read_session_window(seed.db(), &host).await);
            assert_eq!(window.messages.len(), 3, "{provider}");
            assert_eq!(window.messages[1].text, "", "{provider}");
            assert_eq!(
                window.messages[1].tool_calls,
                vec![SessionWindowToolCallV1 {
                    name: tool.to_owned(),
                    argument_excerpt: Some(payload.to_owned()),
                }],
                "{provider}"
            );
            assert!(window.messages[2].tool_calls.is_empty(), "{provider}");
            assert!(
                render_window_state(&window, None, 4096)
                    .contains(&format!("Tool {tool}({payload})")),
                "{provider}"
            );
        }
    }

    #[asupersync::test]
    async fn freshness_follows_the_newest_message_age() {
        let seed = Seed::open("window-freshness").await;
        seed_conversation(&seed).await;

        // Newest message is ~995s old at NOW_SECS.
        let stale = ready(read_session_window(seed.db(), &request(limits())).await);
        let SessionWindowFreshnessV1::Stale { age_micros } = stale.freshness else {
            panic!("expected stale, got {:?}", stale.freshness);
        };
        assert_eq!(age_micros, 995 * 1_000_000);
        assert_eq!(
            stale.newest_timestamp_micros,
            Some((NOW_SECS - 1_000 + 5) * 1_000_000)
        );

        let fresh = ready(
            read_session_window(
                seed.db(),
                &request(SessionWindowLimitsV1 {
                    max_age_micros: Some(2_000 * 1_000_000),
                    ..limits()
                }),
            )
            .await,
        );
        assert_eq!(fresh.freshness, SessionWindowFreshnessV1::Fresh);

        let unbounded = ready(
            read_session_window(
                seed.db(),
                &request(SessionWindowLimitsV1 {
                    max_age_micros: None,
                    ..limits()
                }),
            )
            .await,
        );
        assert_eq!(unbounded.freshness, SessionWindowFreshnessV1::Fresh);
    }

    #[test]
    fn timestamps_normalize_seconds_and_milliseconds_to_micros() {
        assert_eq!(timestamp_micros(1_800_000_000), 1_800_000_000_000_000);
        assert_eq!(timestamp_micros(1_800_000_000_123), 1_800_000_000_123_000);
        assert_eq!(
            freshness(None, 10, Some(1)),
            SessionWindowFreshnessV1::Unknown
        );
        // A timestamp in the future is not stale.
        assert_eq!(
            freshness(Some(2_000_000), 1_000_000, Some(1)),
            SessionWindowFreshnessV1::Fresh
        );
    }

    #[asupersync::test]
    async fn the_digest_is_stable_and_tracks_content_and_identity() {
        let seed = Seed::open("window-digest").await;
        seed_conversation(&seed).await;
        let first = ready(read_session_window(seed.db(), &request(limits())).await);
        let again = ready(read_session_window(seed.db(), &request(limits())).await);
        assert_eq!(first.window_digest, again.window_digest);

        // The clock does not enter the digest.
        let mut later = request(limits());
        later.now_micros += 5_000_000;
        assert_eq!(
            ready(read_session_window(seed.db(), &later).await).window_digest,
            first.window_digest
        );

        // A different window of the same session differs.
        let narrower = ready(
            read_session_window(
                seed.db(),
                &request(SessionWindowLimitsV1 {
                    max_user_turns: 1,
                    ..limits()
                }),
            )
            .await,
        );
        assert_ne!(narrower.window_digest, first.window_digest);

        // A new message changes it.
        seed.message(record(PROVIDER, SESSION, 6, "user", "fourth question"))
            .await;
        let grown = ready(read_session_window(seed.db(), &request(limits())).await);
        assert_ne!(grown.window_digest, first.window_digest);

        // Identity is part of it: the same messages under another session id.
        let messages = first.messages.clone();
        assert_ne!(
            window_digest(PROVIDER, SESSION, &messages),
            window_digest(PROVIDER, "other", &messages)
        );
        assert_ne!(
            window_digest(PROVIDER, SESSION, &messages),
            window_digest("claude", SESSION, &messages)
        );
    }

    fn message(
        role: SessionWindowRoleV1,
        text: &str,
        calls: &[(&str, Option<&str>)],
    ) -> SessionWindowMessageV1 {
        SessionWindowMessageV1 {
            role,
            text: text.to_owned(),
            tool_calls: calls
                .iter()
                .map(|(name, args)| SessionWindowToolCallV1 {
                    name: (*name).to_owned(),
                    argument_excerpt: args.map(str::to_owned),
                })
                .collect(),
            store_id: 1,
            timestamp_micros: None,
        }
    }

    fn window_of(messages: Vec<SessionWindowMessageV1>) -> SessionWindowV1 {
        SessionWindowV1 {
            first_store_id: 1,
            last_store_id: 1,
            newest_timestamp_micros: None,
            window_digest: [0; 32],
            freshness: SessionWindowFreshnessV1::Unknown,
            messages,
        }
    }

    #[test]
    fn the_rendered_state_is_a_plain_transcript_ending_in_the_prompt() {
        let window = window_of(vec![
            message(SessionWindowRoleV1::User, "where is the parser?", &[]),
            message(
                SessionWindowRoleV1::Assistant,
                "Searching.",
                &[("Grep", Some("parse_expr")), ("Read", None)],
            ),
            message(SessionWindowRoleV1::Tool, "src/parse.rs:10", &[]),
        ]);
        assert_eq!(
            render_window_state(&window, Some("  now fix it  "), 4096),
            "User: where is the parser?\n\
             Assistant: Searching.\n\
             Tool Grep(parse_expr)\n\
             Tool Read()\n\
             Tool result: src/parse.rs:10\n\
             User: now fix it"
        );
        assert_eq!(
            render_window_state(&window, None, 4096).lines().last(),
            Some("Tool result: src/parse.rs:10")
        );
        assert_eq!(
            render_window_state(&window, Some("   "), 4096),
            render_window_state(&window, None, 4096)
        );
    }

    #[test]
    fn the_rendered_state_truncates_from_the_front_on_char_boundaries() {
        let window = window_of(vec![
            message(SessionWindowRoleV1::User, "old context é", &[]),
            message(SessionWindowRoleV1::Assistant, "recent reply", &[]),
        ]);
        let full = render_window_state(&window, Some("latest prompt"), usize::MAX);
        let cut = render_window_state(&window, Some("latest prompt"), 30);
        assert!(cut.len() <= 30);
        assert!(full.ends_with(&cut), "{cut:?} must be a suffix of {full:?}");
        assert!(cut.ends_with("User: latest prompt"));

        // A multi-byte character straddling the cut is dropped, not split.
        let accented = window_of(vec![message(SessionWindowRoleV1::User, "ééééé", &[])]);
        let rendered = render_window_state(&accented, None, 7);
        assert_eq!(rendered, "ééé");
        assert_eq!(render_window_state(&accented, None, 0), "");
    }

    #[test]
    fn long_arguments_are_collapsed_and_bounded() {
        assert_eq!(excerpt("  a \n b\t c  ").as_deref(), Some("a b c"));
        assert_eq!(excerpt("   "), None);
        let long = "x".repeat(SESSION_WINDOW_ARGUMENT_EXCERPT_CHARS + 50);
        let bounded = excerpt(&long).unwrap();
        assert_eq!(
            bounded.chars().count(),
            SESSION_WINDOW_ARGUMENT_EXCERPT_CHARS + 1
        );
        assert!(bounded.ends_with(ELLIPSIS));
    }
}
