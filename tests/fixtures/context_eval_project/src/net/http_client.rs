//! A minimal HTTP client that wraps every request in the retry-with-backoff
//! policy from `net::retry`.

use crate::net::retry::{RetryPolicy, retry_with_backoff};

/// Sends HTTP requests, retrying transient failures via `RetryPolicy`.
pub struct HttpClient {
    base_url: String,
    retry_policy: RetryPolicy,
}

impl HttpClient {
    pub fn new(base_url: &str) -> Self {
        HttpClient {
            base_url: base_url.to_string(),
            retry_policy: RetryPolicy::new(3, 100),
        }
    }

    /// Sends a GET request to `path`, retrying on failure according to the
    /// client's retry policy.
    pub fn send_request(&self, path: &str) -> Result<String, String> {
        let url = format!("{}{}", self.base_url, path);
        retry_with_backoff(&self.retry_policy, || fake_send(&url))
    }
}

fn fake_send(url: &str) -> Result<String, String> {
    Ok(format!("response from {url}"))
}
