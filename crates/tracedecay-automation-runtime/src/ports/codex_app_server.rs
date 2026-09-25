//! The Codex app-server prompt runtime, as the automation backend uses it.
//!
//! A **registered port**. `automation::backend`'s `CodexAppServerBackend`
//! drives one-shot prompts through a spawned `codex app-server` JSON-RPC
//! session. Process spawn, handshake, thread lifecycle, and cancellation live
//! in `tracedecay-sessions`, which sits beside this crate rather than beneath
//! it, so the backend states its request and takes the execution as an
//! injected capability.
//!
//! Root wiring: the root registers [`register`] with an adapter over
//! `sessions::codex_app_server::run_prompt_with_codex_app_server`, converting
//! [`SummaryConfig`] to the session runtime's own config type.
//!
//! Unregistered, every run reports the backend as unavailable. That is the
//! same class of failure the backend already handles when the `codex`
//! executable is unconfigured, so an unwired build degrades to "backend
//! unavailable" instead of panicking or silently producing an empty summary.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use serde_json::Value;

/// How to invoke `codex app-server` for one prompt.
///
/// The executable is the exact path the configuration authority bound
/// (`lcm.summarizer_executables.v1`); this port never resolves `codex` from
/// `PATH` or the environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryConfig {
    /// The configured `codex` executable to spawn.
    pub codex_bin: PathBuf,
    /// Model selected for TraceDecay-owned turns.
    pub model: Option<String>,
    /// Hard wall-clock budget for the run.
    pub timeout: Duration,
}

impl SummaryConfig {
    /// Default tuning for an executable the caller resolved through
    /// configuration. Nothing here reads `PATH` or the environment.
    #[must_use]
    pub fn for_executable(codex_bin: &Path) -> Self {
        Self {
            codex_bin: codex_bin.to_path_buf(),
            model: Some("gpt-5.6-sol".to_owned()),
            timeout: Duration::from_mins(2),
        }
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
pub type RunPrompt = fn(&str, &SummaryConfig, &str, Option<&Value>) -> Result<Summary, String>;

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
    response_schema: Option<&Value>,
) -> Result<Summary, String> {
    let Some(run) = RUN_PROMPT.get() else {
        return Err(
            "codex app-server backend is unavailable: no prompt runner is registered".to_string(),
        );
    };
    run(prompt, config, thread_source, response_schema)
}
