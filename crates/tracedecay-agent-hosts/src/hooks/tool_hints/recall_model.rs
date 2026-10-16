//! Optional learned second opinion for session-recall hints.
//!
//! [`super::classifiers::asks_for_session_recall`] matches only the phrasings
//! it lists, so most ways of asking for earlier context ("pull up our earlier
//! discussion", "have we hit this panic before?") stay silent. When
//! `TRACEDECAY_HINT_RECALL_MODEL_URL` names a running Laya decision server
//! (`laya-serve`), a prompt that no keyword rule classified is asked one
//! calibrated yes/no question. Unset, unreachable, slow, or malformed answers
//! leave the keyword result standing; the model never overrides a keyword
//! category and never touches delivery budget or dedupe.
//!
//! The question, the threshold, and the measured effect live with the
//! held-out set in `evals/hint_recall_model/`.

use std::time::Duration;

use serde_json::{Value, json};
use tracedecay_application::http_agent::http_agent;

/// Base URL of a `laya-serve` instance, e.g. `http://127.0.0.1:8000`.
pub(super) const RECALL_MODEL_URL_ENV: &str = "TRACEDECAY_HINT_RECALL_MODEL_URL";
/// Whole-request budget in milliseconds; the hook answers without the model
/// once it elapses.
pub(super) const RECALL_MODEL_TIMEOUT_MS_ENV: &str = "TRACEDECAY_HINT_RECALL_MODEL_TIMEOUT_MS";

const DEFAULT_TIMEOUT: Duration = Duration::from_millis(250);

/// Calibrated probability at or above which the model's "yes" yields a
/// session-recall hint. Chosen on `evals/hint_recall_model/heldout.json`.
pub(super) const RECALL_PROBABILITY_THRESHOLD: f64 = 0.6;

const RECALL_QUESTION_KEY: &str = "recall";
const RECALL_QUESTION: &str = "Is the user asking to look up or reuse something from an earlier \
     conversation, a previous session, or another agent's transcript?";

pub(super) struct RecallModel {
    endpoint: String,
    agent: ureq::Agent,
}

impl RecallModel {
    pub(super) fn from_env() -> Option<Self> {
        Self::from_values(
            std::env::var(RECALL_MODEL_URL_ENV).ok().as_deref(),
            std::env::var(RECALL_MODEL_TIMEOUT_MS_ENV).ok().as_deref(),
        )
    }

    pub(super) fn from_values(url: Option<&str>, timeout_ms: Option<&str>) -> Option<Self> {
        let base = url.map(str::trim).filter(|url| !url.is_empty())?;
        let timeout = timeout_ms
            .and_then(|raw| raw.trim().parse::<u64>().ok())
            .filter(|ms| *ms > 0)
            .map_or(DEFAULT_TIMEOUT, Duration::from_millis);
        // A loopback sidecar: environment proxies would only add a hop.
        let agent = http_agent(
            ureq::Agent::config_builder()
                .timeout_global(Some(timeout))
                .http_status_as_error(false)
                .proxy(None)
                .max_redirects(0)
                .build(),
        );
        Some(Self {
            endpoint: format!("{}/v1/systemone", base.trim_end_matches('/')),
            agent,
        })
    }

    /// `Some(true)` when the model is at least [`RECALL_PROBABILITY_THRESHOLD`]
    /// sure the prompt asks for earlier context, `Some(false)` below it, and
    /// `None` when no usable answer arrived.
    pub(super) fn asks_for_recall(&self, prompt: &str) -> Option<bool> {
        let body = json!({
            "state": prompt,
            "questions": {
                RECALL_QUESTION_KEY: { "type": "noul", "instructions": RECALL_QUESTION },
            },
        });
        let mut response = match self.agent.post(&self.endpoint).send_json(&body) {
            Ok(response) => response,
            Err(error) => {
                tracing::debug!(%error, "hint recall model unreachable");
                return None;
            }
        };
        if response.status().as_u16() != 200 {
            tracing::debug!(
                status = response.status().as_u16(),
                "hint recall model refused"
            );
            return None;
        }
        let answer = response.body_mut().read_json::<Value>().ok()?;
        recall_probability(&answer).map(|probability| probability >= RECALL_PROBABILITY_THRESHOLD)
    }
}

fn recall_probability(answer: &Value) -> Option<f64> {
    answer
        .pointer(&format!("/answers/{RECALL_QUESTION_KEY}/noul"))
        .and_then(Value::as_f64)
        .filter(|probability| (0.0..=1.0).contains(probability))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
pub(super) mod test_server {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{Ipv4Addr, TcpListener};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    /// A loopback server answering every request with `status` and `body`
    /// after `delay`, counting the requests it saw.
    pub(in super::super) struct FakeLaya {
        pub(in super::super) url: String,
        pub(in super::super) requests: Arc<AtomicUsize>,
        pub(in super::super) last_body: Arc<std::sync::Mutex<String>>,
    }

    pub(in super::super) fn fake_laya(status: u16, body: &str, delay: Duration) -> FakeLaya {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(AtomicUsize::new(0));
        let last_body = Arc::new(std::sync::Mutex::new(String::new()));
        let body = body.to_string();
        let (seen, captured) = (Arc::clone(&requests), Arc::clone(&last_body));
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut content_length = 0;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
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
            }
        });
        FakeLaya {
            url,
            requests,
            last_body,
        }
    }

    pub(in super::super) fn recall_answer(probability: f64) -> String {
        format!(r#"{{"answers":{{"recall":{{"noul":{probability}}}}}}}"#)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::time::{Duration, Instant};

    use super::test_server::{fake_laya, recall_answer};
    use super::*;

    fn model(url: &str, timeout_ms: &str) -> RecallModel {
        RecallModel::from_values(Some(url), Some(timeout_ms)).unwrap()
    }

    #[test]
    fn an_unset_or_blank_url_disables_the_model() {
        assert!(RecallModel::from_values(None, None).is_none());
        assert!(RecallModel::from_values(Some("  "), Some("100")).is_none());
    }

    #[test]
    fn the_endpoint_joins_the_base_url_once() {
        let model = RecallModel::from_values(Some("http://127.0.0.1:8000/"), Some("x")).unwrap();
        assert_eq!(model.endpoint, "http://127.0.0.1:8000/v1/systemone");
    }

    #[test]
    fn the_threshold_splits_yes_from_no() {
        let yes = fake_laya(
            200,
            &recall_answer(RECALL_PROBABILITY_THRESHOLD),
            Duration::ZERO,
        );
        assert_eq!(
            model(&yes.url, "2000").asks_for_recall("have we hit this before?"),
            Some(true)
        );
        let no = fake_laya(200, &recall_answer(0.59), Duration::ZERO);
        assert_eq!(
            model(&no.url, "2000").asks_for_recall("add a session table"),
            Some(false)
        );
    }

    #[test]
    fn the_request_asks_one_noul_question_about_the_prompt() {
        let server = fake_laya(200, &recall_answer(0.9), Duration::ZERO);
        model(&server.url, "2000").asks_for_recall("recap our earlier decisions");
        let sent: Value = serde_json::from_str(&server.last_body.lock().unwrap()).unwrap();
        assert_eq!(sent["state"], "recap our earlier decisions");
        assert_eq!(sent["questions"]["recall"]["type"], "noul");
        assert_eq!(sent["questions"].as_object().unwrap().len(), 1);
    }

    #[test]
    fn unusable_answers_yield_no_opinion() {
        for (status, body) in [
            (500, r#"{"detail":"inference failed"}"#.to_string()),
            (200, "not json".to_string()),
            (200, r#"{"answers":{}}"#.to_string()),
            (200, recall_answer(1.5)),
        ] {
            let server = fake_laya(status, &body, Duration::ZERO);
            assert_eq!(
                model(&server.url, "2000").asks_for_recall("anything"),
                None,
                "{body}"
            );
        }
    }

    #[test]
    fn a_slow_model_is_abandoned_at_the_timeout() {
        let server = fake_laya(200, &recall_answer(0.99), Duration::from_secs(5));
        let started = Instant::now();
        assert_eq!(model(&server.url, "100").asks_for_recall("anything"), None);
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn an_unreachable_model_yields_no_opinion() {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        assert_eq!(
            model(&format!("http://127.0.0.1:{port}"), "500").asks_for_recall("anything"),
            None
        );
    }
}
