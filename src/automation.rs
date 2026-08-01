//! Compatibility shim for the extracted `automation` subsystem.
//!
//! The implementation moved to `crates/tracedecay-agent-hosts` (together with
//! `agents`, which it is mutually recursive with). This glob re-export keeps
//! every previously public path resolving unchanged — both leaf items
//! (`crate::automation::runner::…` entry points) and the submodules
//! themselves, since a `pub mod` is itself a re-exportable item.
//!
//! Items that were `pub(crate)` in the old tree are deliberately NOT covered:
//! they are now private to `tracedecay-agent-hosts`. Root call sites that
//! reached them are cataloged in
//! `crates/tracedecay-agent-hosts/SEAMS.md`.
pub use tracedecay_agent_hosts::automation::*;

fn run_codex_app_server_prompt(
    prompt: &str,
    config: &tracedecay_agent_hosts::ports::codex_app_server::SummaryConfig,
    thread_source: &str,
) -> Result<tracedecay_agent_hosts::ports::codex_app_server::Summary, String> {
    let config = crate::sessions::codex_app_server::CodexAppServerSummaryConfig {
        codex_bin: config.codex_bin.clone(),
        model: config.model.clone(),
        timeout: config.timeout,
    };
    crate::sessions::codex_app_server::run_prompt_with_codex_app_server(
        prompt,
        &config,
        thread_source,
    )
    .map(
        |summary| tracedecay_agent_hosts::ports::codex_app_server::Summary {
            text: summary.text,
            model: summary.model,
        },
    )
    .map_err(|error| error.to_string())
}

/// Installs the root-owned runtime capabilities required by agent-hosts.
pub(crate) fn register_runtime_ports() {
    tracedecay_agent_hosts::ports::codex_app_server::register(run_codex_app_server_prompt);
    tracedecay_agent_hosts::ports::session_store::register_canonical_project_key(
        crate::global_db::RegisteredGlobalDb::canonical_project_key,
    );
}
