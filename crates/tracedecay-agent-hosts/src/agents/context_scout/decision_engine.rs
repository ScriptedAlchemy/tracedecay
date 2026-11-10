//! Pluggable typed-question client for the shared decision-model API.
//!
//! One `POST {base_url}/v1/systemone` carries a transient `state` string and a
//! set of typed questions (`noul` yes/no probability, `choice` over named
//! options, `score` over an ordered rubric) and returns one calibrated answer
//! per question key. The wire contract is served by `laya-serve`, Kev's
//! server, Ollama (0.35+) and llama.cpp, so response parsing is deliberately
//! tolerant about key names (`probs` / `probabilities` / `options`) and
//! strict about values: anything missing or out of range is
//! [`DecisionEngineOutcomeV1::Malformed`], never a guess.
//!
//! This module owns its request, answer, limit and identity types so that
//! configuration wiring only has to map operator settings onto
//! [`DecisionEngineConfigV1`]. It never persists the `state`: callers hand it
//! transient text, it truncates that text to a byte budget (keeping the most
//! recent tail) and sends it once.
//!
//! Failure is always a typed outcome, never a panic and never an error the
//! caller must interpret: an unreachable server, a blown timeout, a non-200,
//! an unparseable body and local back-pressure each leave the caller free to
//! fall back to its non-model path.

use std::collections::BTreeMap;
use std::io::Read as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use tracedecay_application::http_agent::http_agent;
use tracedecay_runtime_core::native_runtime;

/// Path of the typed-question endpoint under the configured base URL.
const SYSTEMONE_PATH: &str = "/v1/systemone";

/// Bound decoded HTTP answer bytes before JSON parsing, matching qualified replay.
const MAX_DECISION_RESPONSE_BYTES: usize = 1024 * 1024;

/// Tolerance when checking a `score` against the rubric's last level index.
const SCORE_EPSILON: f64 = 1e-6;

/// Decision servers return a probability distribution, not independent scores.
/// Allow rounding error in the distribution total, not a conflicting chosen option.
const CHOICE_EPSILON: f64 = 1e-3;

/// What one question asks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecisionQuestionKindV1 {
    /// A calibrated yes/no probability.
    Noul,
    /// One of `options`, with a probability per option.
    Choice { options: Vec<String> },
    /// A position on an ordered rubric; `levels[0]` is the lowest level.
    Score { levels: Vec<String> },
}

/// One typed question, addressed by `key` in the answer map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionQuestionV1 {
    pub key: String,
    pub kind: DecisionQuestionKindV1,
    pub instructions: String,
}

/// One request: a transient `state` and the questions asked about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionRequestV1 {
    pub state: String,
    pub questions: Vec<DecisionQuestionV1>,
}

/// One parsed answer, matching the kind of the question it answers.
#[derive(Debug, Clone, PartialEq)]
pub enum DecisionAnswerV1 {
    /// Probability in `[0, 1]`.
    Noul(f64),
    /// `chosen` is the server's pick when it named one; `probabilities` holds
    /// exactly one bounded probability for every requested option. Missing
    /// values are never inferred, and unrequested options are rejected.
    Choice {
        chosen: Option<String>,
        probabilities: BTreeMap<String, f64>,
    },
    /// Expected level index on the rubric, in `[0, levels - 1]`.
    Score(f64),
}

/// Byte-exact identity of the request prepared by the configured Rust client.
/// Carries no transcript content, only the bytes and digest of the JSON body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecisionRequestCommitmentV1 {
    pub wire_sha256: [u8; 32],
    pub wire_bytes: usize,
}

/// Every answer of one successful exchange.
#[derive(Debug, Clone, PartialEq)]
pub struct DecisionAnswersV1 {
    pub answers: BTreeMap<String, DecisionAnswerV1>,
    /// The model name the server reported, when it reported one.
    pub model: Option<String>,
    pub latency_micros: u64,
}

/// Result of one ask. Every non-`Answered` variant means "no model opinion".
#[derive(Debug, Clone, PartialEq)]
pub enum DecisionEngineOutcomeV1 {
    Answered(DecisionAnswersV1),
    /// Connection, DNS, TLS or transport failure.
    Unreachable,
    /// The whole-request budget elapsed.
    TimedOut,
    /// The server answered with a non-200 status.
    Refused {
        status: u16,
    },
    /// The body was not the expected shape, a requested answer was missing or
    /// out of range, or the request itself was not well formed (no questions,
    /// duplicate keys, an empty option list); invalid requests never reach
    /// the network.
    Malformed,
    /// Every concurrency permit was in use; nothing was sent.
    Backpressured,
}

/// Client settings. Construct through [`DecisionEngineClientV1::new`], which
/// validates them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionEngineConfigV1 {
    /// Base URL of the server, e.g. `http://127.0.0.1:8000`.
    pub base_url: String,
    /// Sent as `"model"` only when set.
    pub model: Option<String>,
    /// Whole-request budget.
    pub timeout: Duration,
    /// Simultaneous in-flight requests this client allows.
    pub max_concurrent: usize,
    /// Byte budget for `state`; longer text keeps its newest tail.
    pub max_state_bytes: usize,
}

/// Why a [`DecisionEngineConfigV1`] was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DecisionEngineConfigErrorV1 {
    #[error("decision engine base URL is empty")]
    EmptyBaseUrl,
    #[error("decision engine base URL must start with http:// or https://")]
    InvalidBaseUrl,
    #[error("decision engine timeout must be positive")]
    ZeroTimeout,
    #[error("decision engine needs at least one concurrent request")]
    ZeroConcurrency,
    #[error("decision engine state budget must be positive")]
    ZeroStateBytes,
}

/// Stable identity of the configured engine: which server and which model a
/// decision came from. The URL is digested so receipts never carry it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionEngineIdentityV1 {
    pub base_url_digest: [u8; 32],
    pub model: Option<String>,
}

/// Blocking entry point of an engine, so callers and tests can substitute it.
pub trait DecisionEnginePortV1: Send + Sync {
    fn ask_blocking(&self, request: &DecisionRequestV1) -> DecisionEngineOutcomeV1;
    fn identity(&self) -> DecisionEngineIdentityV1;

    /// Test doubles cannot claim a production HTTP wire commitment.
    fn request_commitment(
        &self,
        _request: &DecisionRequestV1,
    ) -> Option<DecisionRequestCommitmentV1> {
        None
    }
}

/// HTTP client for the decision-model API.
#[derive(Clone)]
pub struct DecisionEngineClientV1 {
    endpoint: String,
    identity: DecisionEngineIdentityV1,
    agent: ureq::Agent,
    in_flight: Arc<AtomicUsize>,
    max_concurrent: usize,
    timeout: Duration,
    max_state_bytes: usize,
}

impl std::fmt::Debug for DecisionEngineClientV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DecisionEngineClientV1")
            .field("model", &self.identity.model)
            .field("timeout", &self.timeout)
            .field("max_concurrent", &self.max_concurrent)
            .finish_non_exhaustive()
    }
}

/// Releases one concurrency permit when dropped.
struct Permit(Arc<AtomicUsize>);

impl Drop for Permit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

impl DecisionEngineClientV1 {
    pub fn new(config: DecisionEngineConfigV1) -> Result<Self, DecisionEngineConfigErrorV1> {
        let base = config.base_url.trim().trim_end_matches('/');
        if base.is_empty() {
            return Err(DecisionEngineConfigErrorV1::EmptyBaseUrl);
        }
        if !(base.starts_with("http://") || base.starts_with("https://")) {
            return Err(DecisionEngineConfigErrorV1::InvalidBaseUrl);
        }
        if config.timeout.is_zero() {
            return Err(DecisionEngineConfigErrorV1::ZeroTimeout);
        }
        if config.max_concurrent == 0 {
            return Err(DecisionEngineConfigErrorV1::ZeroConcurrency);
        }
        if config.max_state_bytes == 0 {
            return Err(DecisionEngineConfigErrorV1::ZeroStateBytes);
        }
        let model = config
            .model
            .map(|model| model.trim().to_owned())
            .filter(|model| !model.is_empty());
        // The decision server is a sidecar the operator named explicitly:
        // environment proxies would only add a hop, and redirects would let
        // it send the transcript window somewhere it did not name.
        let agent = http_agent(
            ureq::Agent::config_builder()
                .timeout_global(Some(config.timeout))
                .http_status_as_error(false)
                .proxy(None)
                .max_redirects(0)
                .build(),
        );
        Ok(Self {
            endpoint: format!("{base}{SYSTEMONE_PATH}"),
            identity: DecisionEngineIdentityV1 {
                base_url_digest: Sha256::digest(base.as_bytes()).into(),
                model,
            },
            agent,
            in_flight: Arc::new(AtomicUsize::new(0)),
            max_concurrent: config.max_concurrent,
            timeout: config.timeout,
            max_state_bytes: config.max_state_bytes,
        })
    }

    pub fn identity(&self) -> DecisionEngineIdentityV1 {
        self.identity.clone()
    }

    /// Render the pretty UTF-8 JSON that is both sent over HTTP and committed.
    /// Sharing the bytes prevents receipt/transport serializer drift.
    fn request_wire(&self, request: &DecisionRequestV1) -> Option<Vec<u8>> {
        if !request_is_well_formed(request) {
            return None;
        }
        let body = request_body(
            request,
            self.identity.model.as_deref(),
            self.max_state_bytes,
        );
        serde_json::to_vec_pretty(&body).ok()
    }

    pub fn request_commitment(
        &self,
        request: &DecisionRequestV1,
    ) -> Option<DecisionRequestCommitmentV1> {
        let wire = self.request_wire(request)?;
        Some(DecisionRequestCommitmentV1 {
            wire_sha256: Sha256::digest(&wire).into(),
            wire_bytes: wire.len(),
        })
    }

    fn try_acquire(&self) -> Option<Permit> {
        let mut current = self.in_flight.load(Ordering::Acquire);
        loop {
            if current >= self.max_concurrent {
                return None;
            }
            match self.in_flight.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Some(Permit(Arc::clone(&self.in_flight))),
                Err(observed) => current = observed,
            }
        }
    }

    /// Asks every question of `request` in one exchange.
    ///
    /// Blocks the calling thread for at most the configured timeout; call it
    /// off an async executor (see [`Self::ask`]).
    pub fn ask_blocking(&self, request: &DecisionRequestV1) -> DecisionEngineOutcomeV1 {
        if !request_is_well_formed(request) {
            tracing::debug!("decision engine request is not well formed");
            return DecisionEngineOutcomeV1::Malformed;
        }
        let Some(_permit) = self.try_acquire() else {
            tracing::debug!("decision engine back-pressured");
            return DecisionEngineOutcomeV1::Backpressured;
        };
        let started = Instant::now();
        let Some(wire) = self.request_wire(request) else {
            tracing::debug!("decision engine request could not be serialized");
            return DecisionEngineOutcomeV1::Malformed;
        };
        let mut response = match self
            .agent
            .post(&self.endpoint)
            .header("content-type", "application/json")
            .send(wire.as_slice())
        {
            Ok(response) => response,
            Err(error) => return transport_failure(&error),
        };
        let status = response.status().as_u16();
        if status != 200 {
            tracing::debug!(status, "decision engine refused");
            return DecisionEngineOutcomeV1::Refused { status };
        }
        // The server is independently configured and may return an unlimited
        // response, including one without Content-Length. Never deserialize
        // unbounded bytes from a transcript-bearing decision exchange.
        let mut answer_bytes = Vec::new();
        if let Err(error) = response
            .body_mut()
            .as_reader()
            .take((MAX_DECISION_RESPONSE_BYTES + 1) as u64)
            .read_to_end(&mut answer_bytes)
        {
            return if is_io_timeout(&error) {
                tracing::debug!(%error, "decision engine timed out reading its answer");
                DecisionEngineOutcomeV1::TimedOut
            } else {
                tracing::debug!(%error, "decision engine answer unreadable");
                DecisionEngineOutcomeV1::Malformed
            };
        }
        if answer_bytes.len() > MAX_DECISION_RESPONSE_BYTES {
            tracing::debug!("decision engine answer exceeds the byte limit");
            return DecisionEngineOutcomeV1::Malformed;
        }
        let value = match serde_json::from_slice::<UnambiguousValue>(&answer_bytes) {
            Ok(value) => value,
            Err(error) => {
                tracing::debug!(%error, "decision engine answer is not JSON");
                return DecisionEngineOutcomeV1::Malformed;
            }
        };
        let latency_micros = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
        match parse_answers(&value.0, request) {
            Some((answers, model)) => {
                // The server's declared model must not contradict the
                // explicitly selected model used to prepare this request.
                if let (Some(requested), Some(reported)) =
                    (self.identity.model.as_deref(), model.as_deref())
                {
                    if requested != reported {
                        tracing::debug!("decision engine reported a different model");
                        return DecisionEngineOutcomeV1::Malformed;
                    }
                }
                DecisionEngineOutcomeV1::Answered(DecisionAnswersV1 {
                    answers,
                    model,
                    latency_micros,
                })
            }
            None => {
                tracing::debug!("decision engine answer malformed");
                DecisionEngineOutcomeV1::Malformed
            }
        }
    }

    /// [`Self::ask_blocking`] on the runtime's blocking pool. A lost worker
    /// reads as an unreachable engine.
    pub async fn ask(&self, request: DecisionRequestV1) -> DecisionEngineOutcomeV1 {
        let client = self.clone();
        match native_runtime::spawn_blocking(move || client.ask_blocking(&request)).await {
            Ok(outcome) => outcome,
            Err(error) => {
                tracing::debug!(%error, "decision engine worker lost");
                DecisionEngineOutcomeV1::Unreachable
            }
        }
    }
}

impl DecisionEnginePortV1 for DecisionEngineClientV1 {
    fn ask_blocking(&self, request: &DecisionRequestV1) -> DecisionEngineOutcomeV1 {
        Self::ask_blocking(self, request)
    }

    fn identity(&self) -> DecisionEngineIdentityV1 {
        Self::identity(self)
    }

    fn request_commitment(
        &self,
        request: &DecisionRequestV1,
    ) -> Option<DecisionRequestCommitmentV1> {
        Self::request_commitment(self, request)
    }
}

fn request_is_well_formed(request: &DecisionRequestV1) -> bool {
    if request.questions.is_empty() {
        return false;
    }
    let mut keys = std::collections::BTreeSet::new();
    request.questions.iter().all(|question| {
        let kind_ok = match &question.kind {
            DecisionQuestionKindV1::Noul => true,
            DecisionQuestionKindV1::Choice { options } => {
                let unique = options.iter().collect::<std::collections::BTreeSet<_>>();
                !options.is_empty() && unique.len() == options.len()
            }
            DecisionQuestionKindV1::Score { levels } => !levels.is_empty(),
        };
        kind_ok && !question.key.is_empty() && keys.insert(question.key.as_str())
    })
}

/// The newest `max_bytes` of `state`, cut on a character boundary.
pub(crate) fn tail_on_char_boundary(state: &str, max_bytes: usize) -> &str {
    if state.len() <= max_bytes {
        return state;
    }
    let mut start = state.len() - max_bytes;
    while !state.is_char_boundary(start) {
        start += 1;
    }
    &state[start..]
}

fn request_body(request: &DecisionRequestV1, model: Option<&str>, max_state_bytes: usize) -> Value {
    let mut questions = Map::new();
    for question in &request.questions {
        let mut entry = Map::new();
        match &question.kind {
            DecisionQuestionKindV1::Noul => {
                entry.insert("type".to_owned(), json!("noul"));
            }
            DecisionQuestionKindV1::Choice { options } => {
                entry.insert("type".to_owned(), json!("choice"));
                entry.insert("criteria".to_owned(), json!(options));
            }
            DecisionQuestionKindV1::Score { levels } => {
                entry.insert("type".to_owned(), json!("score"));
                entry.insert("criteria".to_owned(), json!(levels));
            }
        }
        entry.insert("instructions".to_owned(), json!(question.instructions));
        questions.insert(question.key.clone(), Value::Object(entry));
    }
    let mut body = Map::new();
    body.insert(
        "state".to_owned(),
        json!(tail_on_char_boundary(&request.state, max_state_bytes)),
    );
    body.insert("questions".to_owned(), Value::Object(questions));
    if let Some(model) = model {
        body.insert("model".to_owned(), json!(model));
    }
    Value::Object(body)
}

fn transport_failure(error: &ureq::Error) -> DecisionEngineOutcomeV1 {
    if is_timeout(error) {
        tracing::debug!(%error, "decision engine timed out");
        DecisionEngineOutcomeV1::TimedOut
    } else {
        tracing::debug!(%error, "decision engine unreachable");
        DecisionEngineOutcomeV1::Unreachable
    }
}

fn is_timeout(error: &ureq::Error) -> bool {
    match error {
        ureq::Error::Timeout(_) => true,
        ureq::Error::Io(io) => is_io_timeout(io),
        other => other.to_string().to_ascii_lowercase().contains("timeout"),
    }
}

fn is_io_timeout(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
    ) || error.to_string().to_ascii_lowercase().contains("timed out")
}

/// Refuse ambiguous response objects before interpreting model decisions.
/// serde_json::Value alone silently keeps the last occurrence of a JSON key,
/// while the qualified HTTP evaluator rejects all duplicate keys.
struct UnambiguousValue(Value);

impl<'de> Deserialize<'de> for UnambiguousValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(UnambiguousValueVisitor)
    }
}

struct UnambiguousValueVisitor;

impl<'de> Visitor<'de> for UnambiguousValueVisitor {
    type Value = UnambiguousValue;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("JSON without duplicate object keys")
    }

    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(UnambiguousValue(Value::Null))
    }

    fn visit_bool<E: de::Error>(self, value: bool) -> Result<Self::Value, E> {
        Ok(UnambiguousValue(Value::Bool(value)))
    }

    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Self::Value, E> {
        Ok(UnambiguousValue(Value::Number(value.into())))
    }

    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Self::Value, E> {
        Ok(UnambiguousValue(Value::Number(value.into())))
    }

    fn visit_f64<E: de::Error>(self, value: f64) -> Result<Self::Value, E> {
        let number = serde_json::Number::from_f64(value)
            .ok_or_else(|| E::custom("nonfinite decision JSON number"))?;
        Ok(UnambiguousValue(Value::Number(number)))
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
        Ok(UnambiguousValue(Value::String(value.to_owned())))
    }

    fn visit_string<E: de::Error>(self, value: String) -> Result<Self::Value, E> {
        Ok(UnambiguousValue(Value::String(value)))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Self::Value, A::Error> {
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element::<UnambiguousValue>()? {
            values.push(value.0);
        }
        Ok(UnambiguousValue(Value::Array(values)))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut entries: A) -> Result<Self::Value, A::Error> {
        let mut values = Map::new();
        while let Some((key, value)) = entries.next_entry::<String, UnambiguousValue>()? {
            if values.contains_key(&key) {
                return Err(de::Error::custom(format!("duplicate decision JSON key: {key}")));
            }
            values.insert(key, value.0);
        }
        Ok(UnambiguousValue(Value::Object(values)))
    }
}

type ParsedAnswers = (BTreeMap<String, DecisionAnswerV1>, Option<String>);

/// Every requested question must be answered with the requested kind.
/// Unexpected answers are malformed, matching the qualified HTTP evaluator's
/// exact-request contract rather than silently accepting a different batch.
fn parse_answers(value: &Value, request: &DecisionRequestV1) -> Option<ParsedAnswers> {
    let answers = value.get("answers")?.as_object()?;
    if answers.len() != request.questions.len() {
        return None;
    }
    let mut parsed = BTreeMap::new();
    for question in &request.questions {
        let answer = answers.get(&question.key)?.as_object()?;
        parsed.insert(question.key.clone(), parse_answer(answer, &question.kind)?);
    }
    // Omission is permitted by the runtime contract; an explicitly present
    // but malformed model is not the same as an omitted identity.
    let model = match value.get("model") {
        None => None,
        Some(Value::String(model)) if !model.trim().is_empty() => Some(model.clone()),
        _ => return None,
    };
    Some((parsed, model))
}

fn parse_answer(
    answer: &Map<String, Value>,
    kind: &DecisionQuestionKindV1,
) -> Option<DecisionAnswerV1> {
    // An omitted tag is valid, but a non-string tag must not also become
    // `None` and skip kind validation.
    let declared = match answer.get("type") {
        None => None,
        Some(declared) => Some(declared.as_str()?),
    };
    match kind {
        DecisionQuestionKindV1::Noul => {
            if declared.is_some_and(|declared| declared != "noul") {
                return None;
            }
            unit_interval(answer.get("noul")?).map(DecisionAnswerV1::Noul)
        }
        DecisionQuestionKindV1::Choice { options } => {
            if declared.is_some_and(|declared| declared != "choice") {
                return None;
            }
            let chosen = match answer.get("choice") {
                None | Some(Value::Null) => None,
                Some(value) => Some(value.as_str()?.to_owned()),
            };
            if chosen
                .as_ref()
                .is_some_and(|chosen| !options.contains(chosen))
            {
                return None;
            }
            let mut reported = ["probs", "probabilities", "options"]
                .into_iter()
                .filter_map(|key| answer.get(key));
            let probabilities = choice_probabilities(reported.next()?.as_object()?, options)?;
            // Aliases describe the same answer. A second spelling cannot
            // hide a malformed map or contradict the first one's values.
            for alias in reported {
                if choice_probabilities(alias.as_object()?, options)? != probabilities {
                    return None;
                }
            }
            if let Some(chosen) = &chosen {
                let selected_probability = *probabilities.get(chosen)?;
                if probabilities
                    .values()
                    .any(|probability| *probability > selected_probability)
                {
                    return None;
                }
            }
            Some(DecisionAnswerV1::Choice {
                chosen,
                probabilities,
            })
        }
        DecisionQuestionKindV1::Score { levels } => {
            if declared.is_some_and(|declared| declared != "score") {
                return None;
            }
            let score = answer.get("score")?.as_f64()?;
            let highest = (levels.len() - 1) as f64;
            (score.is_finite() && score >= -SCORE_EPSILON && score <= highest + SCORE_EPSILON)
                .then(|| DecisionAnswerV1::Score(score.clamp(0.0, highest)))
        }
    }
}

/// Exactly the requested options, each with a probability in `[0, 1]`.
/// Missing, extra or invalid entries void the answer rather than being
/// dropped or filled with an invented probability.
fn choice_probabilities(
    reported: &Map<String, Value>,
    options: &[String],
) -> Option<BTreeMap<String, f64>> {
    if reported.len() != options.len() {
        return None;
    }
    let mut probabilities = BTreeMap::new();
    for label in options {
        probabilities.insert(label.clone(), unit_interval(reported.get(label)?)?);
    }
    // A map containing every label is still malformed if it is not a
    // distribution. In particular, two individually valid 0.8 scores are
    // not a valid two-way choice.
    // A three-decimal response can deviate by up to 0.0005 per option.
    // Scale the sum tolerance with cardinality, but cap it at one percent
    // so a large choice set cannot excuse an unnormalized answer.
    let rounding_tolerance = (CHOICE_EPSILON * probabilities.len() as f64 / 2.0).min(0.01);
    if (probabilities.values().sum::<f64>() - 1.0).abs() > rounding_tolerance {
        return None;
    }
    Some(probabilities)
}

fn unit_interval(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .filter(|probability| (0.0..=1.0).contains(probability))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
pub mod test_server {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{Ipv4Addr, TcpListener};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    /// A loopback server answering every request with `status` and `body`
    /// after `delay`, counting the requests it saw and keeping the last body.
    pub struct FakeDecisionEngine {
        pub url: String,
        pub requests: Arc<AtomicUsize>,
        pub last_body: Arc<Mutex<String>>,
    }

    pub fn fake_decision_engine(status: u16, body: &str, delay: Duration) -> FakeDecisionEngine {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(AtomicUsize::new(0));
        let last_body = Arc::new(Mutex::new(String::new()));
        let body = body.to_string();
        let (seen, captured) = (Arc::clone(&requests), Arc::clone(&last_body));
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let body = body.clone();
                let (seen, captured) = (Arc::clone(&seen), Arc::clone(&captured));
                std::thread::spawn(move || {
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut content_length = 0;
                    loop {
                        let mut line = String::new();
                        if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                            break;
                        }
                        if let Some(value) =
                            line.to_ascii_lowercase().strip_prefix("content-length:")
                        {
                            content_length = value.trim().parse().unwrap_or(0);
                        }
                    }
                    let mut request_body = vec![0; content_length];
                    let _ = reader.read_exact(&mut request_body);
                    *captured.lock().unwrap() = String::from_utf8_lossy(&request_body).into_owned();
                    seen.fetch_add(1, Ordering::SeqCst);
                    std::thread::sleep(delay);
                    let _ = write!(
                        stream,
                        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                });
            }
        });
        FakeDecisionEngine {
            url,
            requests,
            last_body,
        }
    }

    /// A `noul` answer object.
    pub fn noul_answer(probability: f64) -> String {
        format!(r#"{{"type":"noul","noul":{probability}}}"#)
    }

    /// A `choice` answer object with the server's pick and per-option probabilities
    /// under `probabilities_key` (`probs`, `probabilities` or `options`).
    pub fn choice_answer(
        chosen: Option<&str>,
        probabilities_key: &str,
        probabilities: &[(&str, f64)],
    ) -> String {
        let map = probabilities
            .iter()
            .map(|(label, probability)| format!(r#""{label}":{probability}"#))
            .collect::<Vec<_>>()
            .join(",");
        let chosen = chosen.map_or_else(String::new, |chosen| format!(r#""choice":"{chosen}","#));
        format!(r#"{{"type":"choice",{chosen}"{probabilities_key}":{{{map}}}}}"#)
    }

    /// A `score` answer object.
    pub fn score_answer(score: f64) -> String {
        format!(r#"{{"type":"score","score":{score},"legend":{{"0":"low","1":"high"}}}}"#)
    }

    /// A full response body from `(key, answer object)` pairs.
    pub fn answers_body(model: Option<&str>, answers: &[(&str, String)]) -> String {
        let answers = answers
            .iter()
            .map(|(key, answer)| format!(r#""{key}":{answer}"#))
            .collect::<Vec<_>>()
            .join(",");
        let model = model.map_or_else(String::new, |model| format!(r#""model":"{model}","#));
        format!(r#"{{{model}"answers":{{{answers}}}}}"#)
    }

    /// The single-question body of the session-recall style `noul` probe.
    pub fn single_noul_body(key: &str, probability: f64) -> String {
        answers_body(None, &[(key, noul_answer(probability))])
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::io::{BufRead as _, BufReader, Write as _};
    use std::time::{Duration, Instant};

    use super::test_server::{
        answers_body, choice_answer, fake_decision_engine, noul_answer, score_answer,
        single_noul_body,
    };
    use super::*;

    fn config(url: &str) -> DecisionEngineConfigV1 {
        DecisionEngineConfigV1 {
            base_url: url.to_owned(),
            model: None,
            timeout: Duration::from_secs(2),
            max_concurrent: 2,
            max_state_bytes: 4096,
        }
    }

    fn client(url: &str) -> DecisionEngineClientV1 {
        DecisionEngineClientV1::new(config(url)).unwrap()
    }

    fn noul(key: &str) -> DecisionQuestionV1 {
        DecisionQuestionV1 {
            key: key.to_owned(),
            kind: DecisionQuestionKindV1::Noul,
            instructions: format!("is {key} true?"),
        }
    }

    fn choice(key: &str, options: &[&str]) -> DecisionQuestionV1 {
        DecisionQuestionV1 {
            key: key.to_owned(),
            kind: DecisionQuestionKindV1::Choice {
                options: options.iter().map(|option| (*option).to_owned()).collect(),
            },
            instructions: format!("which {key}?"),
        }
    }

    fn score(key: &str, levels: &[&str]) -> DecisionQuestionV1 {
        DecisionQuestionV1 {
            key: key.to_owned(),
            kind: DecisionQuestionKindV1::Score {
                levels: levels.iter().map(|level| (*level).to_owned()).collect(),
            },
            instructions: format!("how much {key}?"),
        }
    }

    fn request(state: &str, questions: Vec<DecisionQuestionV1>) -> DecisionRequestV1 {
        DecisionRequestV1 {
            state: state.to_owned(),
            questions,
        }
    }

    fn answered(outcome: DecisionEngineOutcomeV1) -> DecisionAnswersV1 {
        match outcome {
            DecisionEngineOutcomeV1::Answered(answers) => answers,
            other => panic!("expected an answer, got {other:?}"),
        }
    }

    #[test]
    fn invalid_configuration_is_rejected() {
        let invalid = |edit: fn(&mut DecisionEngineConfigV1)| {
            let mut config = config("http://127.0.0.1:1");
            edit(&mut config);
            DecisionEngineClientV1::new(config).unwrap_err()
        };
        assert_eq!(
            invalid(|config| config.base_url = "   ".to_owned()),
            DecisionEngineConfigErrorV1::EmptyBaseUrl
        );
        assert_eq!(
            invalid(|config| config.base_url = "127.0.0.1:8000".to_owned()),
            DecisionEngineConfigErrorV1::InvalidBaseUrl
        );
        assert_eq!(
            invalid(|config| config.timeout = Duration::ZERO),
            DecisionEngineConfigErrorV1::ZeroTimeout
        );
        assert_eq!(
            invalid(|config| config.max_concurrent = 0),
            DecisionEngineConfigErrorV1::ZeroConcurrency
        );
        assert_eq!(
            invalid(|config| config.max_state_bytes = 0),
            DecisionEngineConfigErrorV1::ZeroStateBytes
        );
    }

    #[test]
    fn the_endpoint_joins_the_base_url_once() {
        let client = client(" http://127.0.0.1:8000/ ");
        assert_eq!(client.endpoint, "http://127.0.0.1:8000/v1/systemone");
        let same = DecisionEngineClientV1::new(config("http://127.0.0.1:8000")).unwrap();
        assert_eq!(client.identity(), same.identity());
        assert_eq!(
            client.identity().base_url_digest,
            <[u8; 32]>::from(Sha256::digest(b"http://127.0.0.1:8000"))
        );
    }

    #[test]
    fn identity_carries_the_trimmed_model_only_when_set() {
        let mut with_model = config("http://127.0.0.1:8000");
        with_model.model = Some(" laya-1 ".to_owned());
        assert_eq!(
            DecisionEngineClientV1::new(with_model)
                .unwrap()
                .identity()
                .model
                .as_deref(),
            Some("laya-1")
        );
        let mut blank = config("http://127.0.0.1:8000");
        blank.model = Some("  ".to_owned());
        assert_eq!(
            DecisionEngineClientV1::new(blank).unwrap().identity().model,
            None
        );
    }

    #[test]
    fn the_prepared_request_commitment_matches_the_actual_utf8_wire() {
        let server = fake_decision_engine(200, &single_noul_body("q", 0.5), Duration::ZERO);
        let mut configured = config(&server.url);
        configured.model = Some("kev-4b".to_owned());
        configured.max_state_bytes = 9;
        let client = DecisionEngineClientV1::new(configured.clone()).unwrap();
        let original = request("old private context éé latest", vec![noul("q")]);
        let commitment = client.request_commitment(&original).unwrap();
        let _ = answered(client.ask_blocking(&original));
        let actual = server.last_body.lock().unwrap().clone();
        assert_eq!(server.requests.load(Ordering::SeqCst), 1);
        assert_eq!(commitment.wire_bytes, actual.len());
        assert_eq!(
            commitment.wire_sha256,
            <[u8; 32]>::from(Sha256::digest(actual.as_bytes()))
        );
        let sent: Value = serde_json::from_str(&actual).unwrap();
        assert_eq!(actual, serde_json::to_string_pretty(&sent).unwrap());
        assert_eq!(sent["model"], "kev-4b");
        assert_eq!(sent["state"], tail_on_char_boundary(&original.state, 9));
        assert!(!actual.contains("old private context"));

        configured.model = Some("different-model".to_owned());
        let changed_model = DecisionEngineClientV1::new(configured).unwrap();
        assert_ne!(
            changed_model.request_commitment(&original),
            Some(commitment)
        );
        let mut changed_question = original.clone();
        changed_question.questions[0]
            .instructions
            .push_str(" Be precise.");
        assert_ne!(
            client.request_commitment(&changed_question),
            Some(commitment)
        );
        // The prefix intentionally discarded by the configured byte cap
        // does not affect the sent request, but changed visible text does.
        let alternate_prefix = request("different private context éé latest", vec![noul("q")]);
        assert_eq!(
            client.request_commitment(&alternate_prefix),
            Some(commitment)
        );
        let changed_visible = request("old private context éé latest!", vec![noul("q")]);
        assert_ne!(
            client.request_commitment(&changed_visible),
            Some(commitment)
        );
        assert!(
            client
                .request_commitment(&request("state", vec![]))
                .is_none()
        );
    }

    #[test]
    fn duplicate_json_keys_cannot_change_the_model_decision() {
        // Every response below is otherwise a valid typed answer. The ordinary
        // serde_json::Value decoder accepts the last duplicate key; it must
        // not let an ambiguous server response qualify as Scout evidence.
        for (body, questions) in [
            (
                r#"{"answers":{"q":{"type":"noul","noul":0.1,"noul":0.9}}}"#,
                vec![noul("q")],
            ),
            (
                r#"{"model":"untrusted","model":"kev-4b","answers":{"q":{"type":"noul","noul":0.9}}}"#,
                vec![noul("q")],
            ),
            (
                r#"{"answers":{"route":{"type":"choice","choice":"code","probs":{"code":0.7,"code":0.7,"memory":0.3}}}}"#,
                vec![choice("route", &["code", "memory"])],
            ),
        ] {
            let server = fake_decision_engine(200, body, Duration::ZERO);
            assert_eq!(
                client(&server.url).ask_blocking(&request("state", questions)),
                DecisionEngineOutcomeV1::Malformed,
                "{body}"
            );
            assert_eq!(server.requests.load(Ordering::SeqCst), 1);
        }
        // Non-ambiguous JSON remains a usable answer.
        let server = fake_decision_engine(200, &single_noul_body("q", 0.9), Duration::ZERO);
        let answers =
            answered(client(&server.url).ask_blocking(&request("state", vec![noul("q")])));
        assert_eq!(answers.answers["q"], DecisionAnswerV1::Noul(0.9));
    }

    #[test]
    fn reported_model_must_be_valid_and_match_explicit_selection() {
        let questions = vec![noul("q")];
        let body = answers_body(Some("kev-4b"), &[("q", noul_answer(0.8))]);

        // An explicitly selected model and a matching server declaration
        // preserve an ordinary usable decision.
        let server = fake_decision_engine(200, &body, Duration::ZERO);
        let mut selected = config(&server.url);
        selected.model = Some("kev-4b".to_owned());
        let observed = answered(
            DecisionEngineClientV1::new(selected)
                .unwrap()
                .ask_blocking(&request("state", questions.clone())),
        );
        assert_eq!(observed.model.as_deref(), Some("kev-4b"));
        assert_eq!(observed.answers["q"], DecisionAnswerV1::Noul(0.8));
        assert_eq!(server.requests.load(Ordering::SeqCst), 1);

        // Each response contains an otherwise valid answer. Previously the
        // mismatched declaration was trusted, and malformed declarations
        // silently became None even though the server supplied a model key.
        for model in [
            json!("other-model"),
            Value::Null,
            json!(true),
            json!(17),
            json!("  "),
        ] {
            let body = json!({"model": model, "answers": {
                "q": {"type": "noul", "noul": 0.8}
            }})
            .to_string();
            let server = fake_decision_engine(200, &body, Duration::ZERO);
            let mut selected = config(&server.url);
            selected.model = Some("kev-4b".to_owned());
            assert_eq!(
                DecisionEngineClientV1::new(selected)
                    .unwrap()
                    .ask_blocking(&request("state", questions.clone())),
                DecisionEngineOutcomeV1::Malformed,
                "{body}"
            );
            assert_eq!(server.requests.load(Ordering::SeqCst), 1);
        }

        // A server that omits its model remains usable as an unverified
        // runtime response, rather than inventing a reported identity.
        let server = fake_decision_engine(
            200,
            &single_noul_body("q", 0.8),
            Duration::ZERO,
        );
        let mut selected = config(&server.url);
        selected.model = Some("kev-4b".to_owned());
        let observed = answered(
            DecisionEngineClientV1::new(selected)
                .unwrap()
                .ask_blocking(&request("state", questions)),
        );
        assert_eq!(observed.model, None);
    }

    #[test]
    fn a_noul_answer_is_read_as_a_probability() {
        let server = fake_decision_engine(200, &single_noul_body("recall", 0.73), Duration::ZERO);
        let answers =
            answered(client(&server.url).ask_blocking(&request("state", vec![noul("recall")])));
        assert_eq!(answers.answers["recall"], DecisionAnswerV1::Noul(0.73),);
        assert_eq!(answers.model, None);
    }

    #[test]
    fn unsolicited_batch_answers_are_malformed_even_when_requested_answers_are_valid() {
        // The frozen evaluator rejects extra response keys. An unrequested
        // Noul or Score must not turn an otherwise unrelated batch into a
        // successful production Scout decision.
        for body in [
            answers_body(
                None,
                &[("q", noul_answer(0.8)), ("unexpected", noul_answer(0.9))],
            ),
            answers_body(
                None,
                &[("q", noul_answer(0.8)), ("unexpected", score_answer(0.5))],
            ),
        ] {
            let server = fake_decision_engine(200, &body, Duration::ZERO);
            assert_eq!(
                client(&server.url).ask_blocking(&request("state", vec![noul("q")])),
                DecisionEngineOutcomeV1::Malformed,
                "{body}"
            );
        }

        // Exact two-question batches remain valid, rather than rejecting
        // batching itself or dropping the second requested answer.
        let body = answers_body(
            None,
            &[("q", noul_answer(0.8)), ("other", noul_answer(0.3))],
        );
        let server = fake_decision_engine(200, &body, Duration::ZERO);
        let answers = answered(
            client(&server.url).ask_blocking(&request("state", vec![noul("q"), noul("other")])),
        );
        assert_eq!(answers.answers.len(), 2);
        assert_eq!(answers.answers["q"], DecisionAnswerV1::Noul(0.8));
        assert_eq!(answers.answers["other"], DecisionAnswerV1::Noul(0.3));
    }

    #[test]
    fn a_choice_answer_reads_the_pick_and_every_probability_spelling() {
        for key in ["probs", "probabilities", "options"] {
            let body = answers_body(
                Some("laya-test"),
                &[(
                    "route",
                    choice_answer(Some("code"), key, &[("code", 0.7), ("memory", 0.3)]),
                )],
            );
            let server = fake_decision_engine(200, &body, Duration::ZERO);
            let answers = answered(client(&server.url).ask_blocking(&request(
                "state",
                vec![choice("route", &["code", "memory"])],
            )));
            assert_eq!(answers.model.as_deref(), Some("laya-test"), "{key}");
            assert_eq!(
                answers.answers["route"],
                DecisionAnswerV1::Choice {
                    chosen: Some("code".to_owned()),
                    probabilities: BTreeMap::from([
                        ("code".to_owned(), 0.7),
                        ("memory".to_owned(), 0.3)
                    ]),
                },
                "{key}"
            );
        }
    }

    #[test]
    fn a_choice_without_a_pick_still_answers_from_its_probabilities() {
        let body = answers_body(
            None,
            &[(
                "route",
                choice_answer(None, "probabilities", &[("code", 0.4), ("memory", 0.6)]),
            )],
        );
        let server = fake_decision_engine(200, &body, Duration::ZERO);
        let answers = answered(client(&server.url).ask_blocking(&request(
            "state",
            vec![choice("route", &["code", "memory"])],
        )));
        let DecisionAnswerV1::Choice {
            chosen,
            probabilities,
        } = &answers.answers["route"]
        else {
            panic!("choice expected");
        };
        assert_eq!(*chosen, None);
        assert_eq!(probabilities.len(), 2);
    }

    #[test]
    fn choice_answers_require_exact_option_coverage() {
        let cases = [
            json!({"choice": "code"}),
            json!({"choice": "code", "probs": {}}),
            json!({"choice": "code", "probs": {"code": 0.7}}),
            json!({"probabilities": {"memory": 0.3}}),
            json!({"options": {"code": 0.7, "other": 0.3}}),
            json!({"probs": {"code": 0.7, "memory": 0.3, "other": 0.0}}),
            json!({"probs": {"code": "0.7", "memory": 0.3}}),
            json!({"choice": "code", "probs": {"code": 0.8, "memory": 0.8}}),
            json!({"choice": "code", "probs": {"code": 0.1, "memory": 0.9}}),
            json!({"probs": {"code": 0.0, "memory": 0.0}}),
            json!({"probs": {"code": 0.4, "memory": 0.4}}),
        ];
        for answer in cases {
            let body = json!({"answers": {"route": answer}}).to_string();
            let server = fake_decision_engine(200, &body, Duration::ZERO);
            assert_eq!(
                client(&server.url).ask_blocking(&request(
                    "state",
                    vec![choice("route", &["code", "memory"])],
                )),
                DecisionEngineOutcomeV1::Malformed,
                "{body}"
            );
        }
    }

    #[test]
    fn choice_pick_requires_string_label_and_maximum_probability() {
        // A server Boolean or number is not the caller's string option,
        // even when stringifying it would happen to match an offered label.
        // Similarly, a declared pick must be an actual maximum; the
        // distribution-total rounding tolerance cannot excuse a wrong pick.
        for answer in [
            json!({"choice": true, "probs": {"true": 0.5, "1": 0.5}}),
            json!({"choice": 1, "probs": {"true": 0.5, "1": 0.5}}),
            json!({"choice": "true", "probs": {"true": 0.4997, "1": 0.5003}}),
        ] {
            let body = json!({"answers": {"route": answer}}).to_string();
            let server = fake_decision_engine(200, &body, Duration::ZERO);
            assert_eq!(
                client(&server.url).ask_blocking(&request(
                    "state",
                    vec![choice("route", &["true", "1"])],
                )),
                DecisionEngineOutcomeV1::Malformed,
                "{body}"
            );
        }

        // A genuine string label remains admissible on an exact tie.
        let body = json!({"answers": {"route": {
            "choice": "true",
            "probs": {"true": 0.5, "1": 0.5}
        }}})
        .to_string();
        let server = fake_decision_engine(200, &body, Duration::ZERO);
        let result = answered(client(&server.url).ask_blocking(&request(
            "state",
            vec![choice("route", &["true", "1"])],
        )));
        assert_eq!(
            result.answers["route"],
            DecisionAnswerV1::Choice {
                chosen: Some("true".to_owned()),
                probabilities: BTreeMap::from([
                    ("1".to_owned(), 0.5),
                    ("true".to_owned(), 0.5),
                ]),
            }
        );
    }

    #[test]
    fn choice_probability_aliases_must_agree_and_be_well_formed() {
        let complete = json!({"code": 0.7, "memory": 0.3});
        let body = json!({"answers": {"route": {
            "probs": complete,
            "probabilities": complete,
            "options": complete
        }}})
        .to_string();
        let server = fake_decision_engine(200, &body, Duration::ZERO);
        let answers = answered(client(&server.url).ask_blocking(&request(
            "state",
            vec![choice("route", &["memory", "code"])],
        )));
        assert_eq!(
            answers.answers["route"],
            DecisionAnswerV1::Choice {
                chosen: None,
                probabilities: BTreeMap::from([
                    ("code".to_owned(), 0.7),
                    ("memory".to_owned(), 0.3)
                ]),
            }
        );

        for alias in [
            json!({"code": 0.2, "memory": 0.8}),
            json!({"code": 0.7}),
            json!({"code": 0.7, "memory": 0.3, "other": 0.0}),
            json!({"code": 1.2, "memory": 0.3}),
            Value::Null,
            json!([0.7, 0.3]),
        ] {
            let body = json!({"answers": {"route": {
                "probs": complete,
                "probabilities": alias
            }}})
            .to_string();
            let server = fake_decision_engine(200, &body, Duration::ZERO);
            assert_eq!(
                client(&server.url).ask_blocking(&request(
                    "state",
                    vec![choice("route", &["code", "memory"])],
                )),
                DecisionEngineOutcomeV1::Malformed,
                "{body}"
            );
        }
    }

    #[test]
    fn an_explicit_none_option_preserves_boundary_probabilities() {
        let body = answers_body(
            None,
            &[(
                "route",
                choice_answer(Some("none"), "options", &[("code", 0.0), ("none", 1.0)]),
            )],
        );
        let server = fake_decision_engine(200, &body, Duration::ZERO);
        let answers = answered(
            client(&server.url)
                .ask_blocking(&request("state", vec![choice("route", &["none", "code"])])),
        );
        assert_eq!(
            answers.answers["route"],
            DecisionAnswerV1::Choice {
                chosen: Some("none".to_owned()),
                probabilities: BTreeMap::from([("code".to_owned(), 0.0), ("none".to_owned(), 1.0)]),
            }
        );
    }

    #[test]
    fn rounded_choice_distribution_is_accepted_without_exact_float_equality() {
        let body = answers_body(
            None,
            &[(
                "route",
                choice_answer(
                    None,
                    "probabilities",
                    &[("code", 0.3333), ("memory", 0.3333), ("none", 0.3333)],
                ),
            )],
        );
        let server = fake_decision_engine(200, &body, Duration::ZERO);
        let answers = answered(client(&server.url).ask_blocking(&request(
            "state",
            vec![choice("route", &["code", "memory", "none"])],
        )));
        assert_eq!(
            answers.answers["route"],
            DecisionAnswerV1::Choice {
                chosen: None,
                probabilities: BTreeMap::from([
                    ("code".to_owned(), 0.3333),
                    ("memory".to_owned(), 0.3333),
                    ("none".to_owned(), 0.3333),
                ]),
            }
        );
    }

    #[test]
    fn six_way_choice_accepts_decimal_rounding_but_not_invalid_totals() {
        for (probability, valid) in [(0.167, true), (0.166, false), (0.17, false)] {
            let body = answers_body(
                None,
                &[(
                    "route",
                    choice_answer(
                        None,
                        "probabilities",
                        &[
                            ("a", probability),
                            ("b", probability),
                            ("c", probability),
                            ("d", probability),
                            ("e", probability),
                            ("f", probability),
                        ],
                    ),
                )],
            );
            let server = fake_decision_engine(200, &body, Duration::ZERO);
            let result = client(&server.url).ask_blocking(&request(
                "state",
                vec![choice("route", &["a", "b", "c", "d", "e", "f"])],
            ));
            if valid {
                assert_eq!(
                    answered(result).answers["route"],
                    DecisionAnswerV1::Choice {
                        chosen: None,
                        probabilities: BTreeMap::from([
                            ("a".to_owned(), probability),
                            ("b".to_owned(), probability),
                            ("c".to_owned(), probability),
                            ("d".to_owned(), probability),
                            ("e".to_owned(), probability),
                            ("f".to_owned(), probability),
                        ]),
                    }
                );
            } else {
                assert_eq!(result, DecisionEngineOutcomeV1::Malformed, "{body}");
            }
        }
    }

    #[test]
    fn a_score_answer_is_read_as_a_level_position() {
        let body = answers_body(None, &[("urgency", score_answer(1.4))]);
        let server = fake_decision_engine(200, &body, Duration::ZERO);
        let answers = answered(client(&server.url).ask_blocking(&request(
            "state",
            vec![score("urgency", &["low", "mid", "high"])],
        )));
        assert_eq!(answers.answers["urgency"], DecisionAnswerV1::Score(1.4));
    }

    #[test]
    fn several_questions_are_answered_in_one_exchange() {
        let body = answers_body(
            Some("m"),
            &[
                ("recall", noul_answer(0.9)),
                (
                    "route",
                    choice_answer(Some("memory"), "probs", &[("code", 0.1), ("memory", 0.9)]),
                ),
                ("urgency", score_answer(0.5)),
            ],
        );
        let server = fake_decision_engine(200, &body, Duration::ZERO);
        let answers = answered(client(&server.url).ask_blocking(&request(
            "state",
            vec![
                noul("recall"),
                choice("route", &["code", "memory"]),
                score("urgency", &["low", "high"]),
            ],
        )));
        assert_eq!(answers.answers.len(), 3, "each requested answer is returned");
        assert_eq!(answers.answers["recall"], DecisionAnswerV1::Noul(0.9));
        assert_eq!(answers.answers["urgency"], DecisionAnswerV1::Score(0.5));
        assert_eq!(server.requests.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn the_request_asks_each_question_by_key_and_sends_model_only_when_set() {
        let body = answers_body(
            None,
            &[
                ("recall", noul_answer(0.9)),
                (
                    "route",
                    choice_answer(Some("code"), "probs", &[("code", 1.0), ("memory", 0.0)]),
                ),
                ("urgency", score_answer(0.0)),
            ],
        );
        let questions = || {
            vec![
                noul("recall"),
                choice("route", &["code", "memory"]),
                score("urgency", &["low", "high"]),
            ]
        };

        let server = fake_decision_engine(200, &body, Duration::ZERO);
        client(&server.url).ask_blocking(&request("the prompt", questions()));
        let sent: Value = serde_json::from_str(&server.last_body.lock().unwrap()).unwrap();
        assert_eq!(sent["state"], "the prompt");
        assert!(sent.get("model").is_none());
        let asked = sent["questions"].as_object().unwrap();
        assert_eq!(asked.len(), 3);
        assert_eq!(asked["recall"]["type"], "noul");
        assert_eq!(asked["recall"]["instructions"], "is recall true?");
        assert!(asked["recall"].get("criteria").is_none());
        assert_eq!(asked["route"]["type"], "choice");
        assert_eq!(asked["route"]["criteria"], json!(["code", "memory"]));
        assert_eq!(asked["urgency"]["type"], "score");
        assert_eq!(asked["urgency"]["criteria"], json!(["low", "high"]));

        let with_model = fake_decision_engine(200, &body, Duration::ZERO);
        let mut configured = config(&with_model.url);
        configured.model = Some("laya-1".to_owned());
        DecisionEngineClientV1::new(configured)
            .unwrap()
            .ask_blocking(&request("the prompt", questions()));
        let sent: Value = serde_json::from_str(&with_model.last_body.lock().unwrap()).unwrap();
        assert_eq!(sent["model"], "laya-1");
    }

    #[test]
    fn an_oversized_state_keeps_its_newest_tail_on_a_char_boundary() {
        assert_eq!(tail_on_char_boundary("abcdef", 4), "cdef");
        assert_eq!(tail_on_char_boundary("abc", 10), "abc");
        // Cutting mid-character advances to the next boundary.
        assert_eq!(tail_on_char_boundary("aéz", 2), "z");
        assert_eq!(tail_on_char_boundary("aéz", 3), "éz");

        let server = fake_decision_engine(200, &single_noul_body("q", 0.5), Duration::ZERO);
        let mut configured = config(&server.url);
        configured.max_state_bytes = 8;
        let client = DecisionEngineClientV1::new(configured).unwrap();
        client.ask_blocking(&request("0123456789 newest", vec![noul("q")]));
        let sent: Value = serde_json::from_str(&server.last_body.lock().unwrap()).unwrap();
        assert_eq!(sent["state"], "9 newest");
    }

    #[test]
    fn response_byte_limit_accepts_exact_boundary_and_refuses_one_extra_byte() {
        let valid = single_noul_body("q", 0.5);
        assert!(valid.len() < MAX_DECISION_RESPONSE_BYTES);
        let exact = format!(
            "{valid}{}",
            " ".repeat(MAX_DECISION_RESPONSE_BYTES - valid.len())
        );
        assert_eq!(exact.len(), MAX_DECISION_RESPONSE_BYTES);
        let accepted = fake_decision_engine(200, &exact, Duration::ZERO);
        let result = client(&accepted.url).ask_blocking(&request("state", vec![noul("q")]));
        assert_eq!(answered(result).answers["q"], DecisionAnswerV1::Noul(0.5));
        assert_eq!(accepted.requests.load(Ordering::SeqCst), 1);

        let oversized = format!("{exact} ");
        assert_eq!(oversized.len(), MAX_DECISION_RESPONSE_BYTES + 1);
        let rejected = fake_decision_engine(200, &oversized, Duration::ZERO);
        assert_eq!(
            client(&rejected.url).ask_blocking(&request("state", vec![noul("q")])),
            DecisionEngineOutcomeV1::Malformed
        );
        assert_eq!(rejected.requests.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn unknown_length_response_is_bounded_without_waiting_for_eof() {
        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let sender = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(socket.try_clone().unwrap());
            let mut length = 0;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n",
                )
                .unwrap();
            // Without a declared length or EOF, an unbounded parser would
            // wait for the server and return TimedOut rather than Malformed.
            let _ = socket.write_all(&vec![b' '; MAX_DECISION_RESPONSE_BYTES + 1]);
            std::thread::sleep(Duration::from_millis(1500));
        });
        let mut settings = config(&url);
        settings.timeout = Duration::from_secs(1);
        let result = DecisionEngineClientV1::new(settings)
            .unwrap()
            .ask_blocking(&request("state", vec![noul("q")]));
        assert_eq!(result, DecisionEngineOutcomeV1::Malformed);
        sender.join().unwrap();
    }

    #[test]
    fn unusable_answers_are_malformed_or_refused() {
        let cases: Vec<(u16, String, DecisionEngineOutcomeV1)> = vec![
            (
                500,
                r#"{"detail":"inference failed"}"#.to_owned(),
                DecisionEngineOutcomeV1::Refused { status: 500 },
            ),
            (
                404,
                String::new(),
                DecisionEngineOutcomeV1::Refused { status: 404 },
            ),
            (
                200,
                "not json".to_owned(),
                DecisionEngineOutcomeV1::Malformed,
            ),
            (
                200,
                r#"{"answers":{}}"#.to_owned(),
                DecisionEngineOutcomeV1::Malformed,
            ),
            (200, r#"{}"#.to_owned(), DecisionEngineOutcomeV1::Malformed),
            (
                200,
                answers_body(None, &[("q", noul_answer(1.5))]),
                DecisionEngineOutcomeV1::Malformed,
            ),
            (
                200,
                answers_body(None, &[("q", noul_answer(-0.1))]),
                DecisionEngineOutcomeV1::Malformed,
            ),
            (
                200,
                answers_body(None, &[("q", r#"{"type":"noul"}"#.to_owned())]),
                DecisionEngineOutcomeV1::Malformed,
            ),
            (
                200,
                answers_body(None, &[("q", r#"{"type":"noul","noul":"0.5"}"#.to_owned())]),
                DecisionEngineOutcomeV1::Malformed,
            ),
            (
                200,
                answers_body(None, &[("q", score_answer(0.5))]),
                DecisionEngineOutcomeV1::Malformed,
            ),
        ];
        for (status, body, expected) in cases {
            let server = fake_decision_engine(status, &body, Duration::ZERO);
            assert_eq!(
                client(&server.url).ask_blocking(&request("anything", vec![noul("q")])),
                expected,
                "{status} {body}"
            );
        }
    }

    #[test]
    fn explicit_answer_types_must_not_be_treated_as_omitted_when_malformed() {
        for (question, answer) in [
            (noul("q"), json!({"noul": 0.5})),
            (
                choice("q", &["code", "memory"]),
                json!({"probs": {"code": 0.7, "memory": 0.3}}),
            ),
            (score("q", &["low", "high"]), json!({"score": 0.5})),
        ] {
            // An omitted tag is valid for every kind.
            let body = json!({"answers": {"q": &answer}}).to_string();
            let server = fake_decision_engine(200, &body, Duration::ZERO);
            let result = answered(
                client(&server.url).ask_blocking(&request("state", vec![question.clone()])),
            );
            assert_eq!(result.answers.len(), 1);

            for declared in [
                Value::Null,
                json!(false),
                json!(1),
                json!(["noul"]),
                json!({"type": "noul"}),
            ] {
                let mut malformed = answer.clone();
                malformed["type"] = declared;
                let body = json!({"answers": {"q": malformed}}).to_string();
                let server = fake_decision_engine(200, &body, Duration::ZERO);
                assert_eq!(
                    client(&server.url).ask_blocking(&request("state", vec![question.clone()])),
                    DecisionEngineOutcomeV1::Malformed,
                    "{body}"
                );
            }
        }
    }

    #[test]
    fn choice_and_score_answers_out_of_range_are_malformed() {
        let choice_cases = [
            // Chosen option the request never offered.
            choice_answer(Some("other"), "probs", &[("code", 0.5), ("memory", 0.5)]),
            // Probability outside [0, 1].
            choice_answer(Some("code"), "probs", &[("code", 1.2), ("memory", 0.0)]),
            // Neither a pick nor probabilities.
            r#"{"type":"choice"}"#.to_owned(),
            // Wrong answer kind for the question.
            noul_answer(0.5),
        ];
        for answer in choice_cases {
            let server = fake_decision_engine(
                200,
                &answers_body(None, &[("q", answer.clone())]),
                Duration::ZERO,
            );
            assert_eq!(
                client(&server.url)
                    .ask_blocking(&request("s", vec![choice("q", &["code", "memory"])])),
                DecisionEngineOutcomeV1::Malformed,
                "{answer}"
            );
        }
        for value in [-0.5, 2.5, 99.0] {
            let server = fake_decision_engine(
                200,
                &answers_body(None, &[("q", score_answer(value))]),
                Duration::ZERO,
            );
            assert_eq!(
                client(&server.url).ask_blocking(&request("s", vec![score("q", &["a", "b"])])),
                DecisionEngineOutcomeV1::Malformed,
                "{value}"
            );
        }
    }

    #[test]
    fn invalid_requests_never_reach_the_network() {
        let server = fake_decision_engine(200, &single_noul_body("q", 0.5), Duration::ZERO);
        let client = client(&server.url);
        for bad in [
            request("s", vec![]),
            request("s", vec![noul("q"), noul("q")]),
            request("s", vec![noul("")]),
            request("s", vec![choice("q", &[])]),
            request("s", vec![choice("q", &["a", "a"])]),
            request("s", vec![score("q", &[])]),
        ] {
            assert_eq!(
                client.ask_blocking(&bad),
                DecisionEngineOutcomeV1::Malformed
            );
        }
        assert_eq!(server.requests.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_slow_engine_is_abandoned_at_the_timeout() {
        let server =
            fake_decision_engine(200, &single_noul_body("q", 0.99), Duration::from_secs(5));
        let mut configured = config(&server.url);
        configured.timeout = Duration::from_millis(150);
        let client = DecisionEngineClientV1::new(configured).unwrap();
        let started = Instant::now();
        assert_eq!(
            client.ask_blocking(&request("s", vec![noul("q")])),
            DecisionEngineOutcomeV1::TimedOut
        );
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn a_disconnected_engine_is_unreachable() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        // A closed port can spend the entire request deadline in Windows'
        // connect retries. Close an established request to observe transport
        // loss independently of that platform's refusal timing.
        let peer = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.read_exact(&mut [0_u8; 1]).unwrap();
        });
        let mut configured = config(&format!("http://127.0.0.1:{port}"));
        configured.timeout = Duration::from_millis(500);
        assert_eq!(
            DecisionEngineClientV1::new(configured)
                .unwrap()
                .ask_blocking(&request("s", vec![noul("q")])),
            DecisionEngineOutcomeV1::Unreachable
        );
        peer.join().unwrap();
    }

    #[test]
    fn exhausted_permits_back_pressure_instead_of_queueing() {
        let server =
            fake_decision_engine(200, &single_noul_body("q", 0.5), Duration::from_millis(700));
        let mut configured = config(&server.url);
        configured.max_concurrent = 1;
        configured.timeout = Duration::from_secs(5);
        let client = DecisionEngineClientV1::new(configured).unwrap();

        let holder = {
            let client = client.clone();
            std::thread::spawn(move || client.ask_blocking(&request("s", vec![noul("q")])))
        };
        let deadline = Instant::now() + Duration::from_secs(3);
        while server.requests.load(Ordering::SeqCst) == 0 {
            assert!(Instant::now() < deadline, "the first request never arrived");
            std::thread::sleep(Duration::from_millis(5));
        }

        assert_eq!(
            client.ask_blocking(&request("s", vec![noul("q")])),
            DecisionEngineOutcomeV1::Backpressured
        );
        assert_eq!(server.requests.load(Ordering::SeqCst), 1);

        assert!(matches!(
            holder.join().unwrap(),
            DecisionEngineOutcomeV1::Answered(_)
        ));
        // The permit is returned once the holder finishes.
        assert!(matches!(
            client.ask_blocking(&request("s", vec![noul("q")])),
            DecisionEngineOutcomeV1::Answered(_)
        ));
    }

    #[asupersync::test]
    async fn the_async_entry_point_answers_off_the_executor() {
        let server = fake_decision_engine(200, &single_noul_body("q", 0.25), Duration::ZERO);
        let outcome = client(&server.url).ask(request("s", vec![noul("q")])).await;
        assert_eq!(answered(outcome).answers["q"], DecisionAnswerV1::Noul(0.25));
    }

    #[test]
    fn the_client_is_usable_as_a_port() {
        let server = fake_decision_engine(200, &single_noul_body("q", 0.5), Duration::ZERO);
        let port: Box<dyn DecisionEnginePortV1> = Box::new(client(&server.url));
        assert!(matches!(
            port.ask_blocking(&request("s", vec![noul("q")])),
            DecisionEngineOutcomeV1::Answered(_)
        ));
        assert_eq!(port.identity().model, None);
    }

    #[test]
    fn answer_arithmetic_helpers_reject_non_numbers() {
        assert_eq!(unit_interval(&json!(0.0)), Some(0.0));
        assert_eq!(unit_interval(&json!(1)), Some(1.0));
        assert_eq!(unit_interval(&json!("0.5")), None);
        assert_eq!(unit_interval(&Value::Null), None);
    }
}
