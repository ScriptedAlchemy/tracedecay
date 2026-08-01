//! The Codex app-server prompt runtime, as the automation backend uses it.
//!
//! A **registered port**. `automation::backend`'s `CodexAppServerBackend`
//! drives one-shot prompts through a spawned `codex app-server` JSON-RPC
//! session. That transport — process spawn, handshake, thread lifecycle,
//! cancellation — lives in `tracedecay-sessions`, which sits beside this crate
//! rather than beneath it, so the backend states its request and takes the
//! execution as an injected capability.
//!
//! Root wiring: the root registers [`register`] with an adapter over
//! `sessions::codex_app_server::run_prompt_with_codex_app_server`, converting
//! [`SummaryConfig`] to the session runtime's own config type.
//!
//! Unregistered, every run reports the backend as unavailable. That is the
//! same class of failure the backend already handles when the `codex` binary
//! is missing, so an unwired build degrades to "backend unavailable" instead
//! of panicking or silently producing an empty summary.

use std::sync::OnceLock;
use std::time::Duration;

/// How to invoke `codex app-server` for one prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryConfig {
    /// The `codex` executable to spawn.
    pub codex_bin: String,
    /// Model override, or `None` to accept the host default.
    pub model: Option<String>,
    /// Hard wall-clock budget for the run.
    pub timeout: Duration,
}

impl Default for SummaryConfig {
    fn default() -> Self {
        Self {
            codex_bin: "codex".to_string(),
            model: None,
            timeout: Duration::from_secs(120),
        }
    }
}

impl SummaryConfig {
    /// Reads the operator overrides from the environment.
    ///
    /// The timeout is clamped to 5..=300 seconds: below that a real model turn
    /// cannot finish, and above it a stuck backend would outlive the
    /// automation run that is waiting on it.
    #[must_use]
    pub fn from_env() -> Self {
        fn non_empty_env(key: &str) -> Option<String> {
            std::env::var(key)
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        }

        let mut config = Self::default();
        if let Some(bin) = non_empty_env("TRACEDECAY_CODEX_BIN") {
            config.codex_bin = bin;
        }
        if let Some(model) = non_empty_env("TRACEDECAY_CODEX_SUMMARY_MODEL") {
            config.model = Some(model);
        }
        if let Some(secs) = non_empty_env("TRACEDECAY_CODEX_SUMMARY_TIMEOUT_SECS")
            .and_then(|secs| secs.parse::<u64>().ok())
        {
            config.timeout = Duration::from_secs(secs.clamp(5, 300));
        }
        config
    }
}

/// What one prompt run produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    /// The assistant's final text.
    pub text: String,
    /// The model that actually served the turn, when the host reported it.
    pub model: Option<String>,
}

/// Runs one prompt to completion. Arguments are `(prompt, config,
/// thread_source)`; the error is already rendered for an automation port
/// failure.
pub type RunPrompt = fn(&str, &SummaryConfig, &str) -> Result<Summary, String>;

static RUN_PROMPT: OnceLock<RunPrompt> = OnceLock::new();

/// Registers the root crate's Codex app-server prompt runner.
///
/// Idempotent: the first registration wins.
pub fn register(run_prompt: RunPrompt) {
    let _ = RUN_PROMPT.set(run_prompt);
}

/// Runs one prompt, or reports the backend unavailable when the root never
/// registered a runner.
pub fn run_prompt(
    prompt: &str,
    config: &SummaryConfig,
    thread_source: &str,
) -> Result<Summary, String> {
    let Some(run) = RUN_PROMPT.get() else {
        return Err(
            "codex app-server backend is unavailable: no prompt runner is registered".to_string(),
        );
    };
    run(prompt, config, thread_source)
}
