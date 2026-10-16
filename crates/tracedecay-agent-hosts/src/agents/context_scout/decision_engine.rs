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
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use tracedecay_application::http_agent::http_agent;
use tracedecay_runtime_core::native_runtime;

/// Path of the typed-question endpoint under the configured base URL.
const SYSTEMONE_PATH: &str = "/v1/systemone";

/// Tolerance when checking a `score` against the rubric's last level index.
const SCORE_EPSILON: f64 = 1e-6;

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
    /// every probability the server reported for a requested option.
    Choice {
        chosen: Option<String>,
        probabilities: BTreeMap<String, f64>,
    },
    /// Expected level index on the rubric, in `[0, levels - 1]`.
    Score(f64),
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
        let body = request_body(
            request,
            self.identity.model.as_deref(),
            self.max_state_bytes,
        );
        let mut response = match self.agent.post(&self.endpoint).send_json(&body) {
            Ok(response) => response,
            Err(error) => return transport_failure(&error),
        };
        let status = response.status().as_u16();
        if status != 200 {
            tracing::debug!(status, "decision engine refused");
            return DecisionEngineOutcomeV1::Refused { status };
        }
        let value = match response.body_mut().read_json::<Value>() {
            Ok(value) => value,
            Err(error) => {
                return if is_timeout(&error) {
                    tracing::debug!(%error, "decision engine timed out reading its answer");
                    DecisionEngineOutcomeV1::TimedOut
                } else {
                    tracing::debug!(%error, "decision engine answer unreadable");
                    DecisionEngineOutcomeV1::Malformed
                };
            }
        };
        let latency_micros = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
        match parse_answers(&value, request) {
            Some((answers, model)) => DecisionEngineOutcomeV1::Answered(DecisionAnswersV1 {
                answers,
                model,
                latency_micros,
            }),
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
        ureq::Error::Io(io) => {
            matches!(
                io.kind(),
                std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
            ) || io.to_string().to_ascii_lowercase().contains("timed out")
        }
        other => other.to_string().to_ascii_lowercase().contains("timeout"),
    }
}

type ParsedAnswers = (BTreeMap<String, DecisionAnswerV1>, Option<String>);

/// Every requested question must be answered with the requested kind; extra
/// answers are ignored.
fn parse_answers(value: &Value, request: &DecisionRequestV1) -> Option<ParsedAnswers> {
    let answers = value.get("answers")?.as_object()?;
    let mut parsed = BTreeMap::new();
    for question in &request.questions {
        let answer = answers.get(&question.key)?.as_object()?;
        parsed.insert(question.key.clone(), parse_answer(answer, &question.kind)?);
    }
    let model = value
        .get("model")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .filter(|model| !model.is_empty());
    Some((parsed, model))
}

fn parse_answer(
    answer: &Map<String, Value>,
    kind: &DecisionQuestionKindV1,
) -> Option<DecisionAnswerV1> {
    let declared = answer.get("type").and_then(Value::as_str);
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
                Some(value) => Some(label_text(value)?),
            };
            if chosen
                .as_ref()
                .is_some_and(|chosen| !options.contains(chosen))
            {
                return None;
            }
            let reported = ["probs", "probabilities", "options"]
                .iter()
                .find_map(|key| answer.get(*key))
                .map(|value| value.as_object())
                .map(|map| map.map(|map| choice_probabilities(map, options)));
            let probabilities = match reported {
                Some(Some(Some(probabilities))) => probabilities,
                // A reported map that is not an object, or that carries an
                // out-of-range value, poisons the whole answer.
                Some(_) => return None,
                None => BTreeMap::new(),
            };
            if chosen.is_none() && probabilities.is_empty() {
                return None;
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

/// Probabilities for the requested options; keys the request did not name are
/// ignored, a requested option with a bad value voids the map.
fn choice_probabilities(
    reported: &Map<String, Value>,
    options: &[String],
) -> Option<BTreeMap<String, f64>> {
    let mut probabilities = BTreeMap::new();
    for (label, value) in reported {
        if !options.contains(label) {
            continue;
        }
        probabilities.insert(label.clone(), unit_interval(value)?);
    }
    Some(probabilities)
}

fn unit_interval(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .filter(|probability| (0.0..=1.0).contains(probability))
}

fn label_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    }
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
    fn a_noul_answer_is_read_as_a_probability() {
        let server = fake_decision_engine(200, &single_noul_body("recall", 0.73), Duration::ZERO);
        let answers =
            answered(client(&server.url).ask_blocking(&request("state", vec![noul("recall")])));
        assert_eq!(answers.answers["recall"], DecisionAnswerV1::Noul(0.73),);
        assert_eq!(answers.model, None);
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
                ("unrequested", noul_answer(0.1)),
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
        assert_eq!(answers.answers.len(), 3, "unrequested answers are dropped");
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
    fn an_unreachable_engine_is_unreachable() {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let mut configured = config(&format!("http://127.0.0.1:{port}"));
        configured.timeout = Duration::from_millis(500);
        assert_eq!(
            DecisionEngineClientV1::new(configured)
                .unwrap()
                .ask_blocking(&request("s", vec![noul("q")])),
            DecisionEngineOutcomeV1::Unreachable
        );
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
