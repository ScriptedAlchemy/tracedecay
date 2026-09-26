//! Gemini CLI agent integration.
//!
//! Gemini CLI has a first-party extension lifecycle, `gemini extensions
//! install <path|url>`, `uninstall <name>`, `list`, `update <name>`, and an
//! extension natively bundles everything this integration needs: the MCP
//! server entry, a context file, and commands. TraceDecay therefore **adopts
//! that lifecycle** instead of configuring Gemini by hand:
//!
//! 1. The receipt-backed component transaction deploys an extension source
//!    TraceDecay owns outright under
//!    `~/.gemini/tracedecay-extension/`, a `gemini-extension.json` manifest
//!    naming the tracedecay MCP server (`args: ["serve"]`, `trust: true`, the
//!    resolved binary substituted through a placeholder) plus the extension's
//!    own `GEMINI.md` context file.
//! 2. TraceDecay drives `gemini extensions install <staged dir>` to activate
//!    and `gemini extensions uninstall tracedecay` to deactivate.
//!
//! What this integration deliberately no longer does is merge
//! `~/.gemini/settings.json` or splice a managed block into
//! `~/.gemini/GEMINI.md`. The extension supplies both, so writing them as well
//! would register two tracedecay servers and duplicate the rules, and
//! emulating host-owned registration next to a CLI that owns it is exactly
//! what the host-capability doctrine forbids. The `gemini` binary is a hard
//! requirement for the lifecycle for the same reason: there is no
//! config-editing fallback, only a typed refusal naming the missing binary.
//!
//! The doctor therefore checks what the extension model makes true: the
//! staged source, the host's installed extension copy, and, when the binary
//! is present, Gemini's own `gemini extensions list`. It never requires an
//! `mcpServers.tracedecay` entry in `~/.gemini/settings.json`.

use std::path::{Path, PathBuf};
use tracedecay_runtime_core::config::ProfileRoot;

use tracedecay_domain::errors::Result;

use super::host_bundle::HostComponentV1;
use super::{AgentIntegration, DoctorCounters, HealthcheckContext, InstallContext, load_json_file};

mod extension;

/// The first-party component catalog deploys the staged extension source as
/// this host's one receipt-owned component, then `gemini extensions install`
/// adopts it. Both halves render from the same function, so the catalog's
/// artifacts and the staged directory can never describe different extensions.
pub(crate) use extension::{GEMINI_STAGED_EXTENSION_RELATIVE, rendered_extension_files};

use extension::{
    EXTENSION_NAME, InstalledExtensionV1, MCP_SERVER_NAME, extension_stage_dir,
    gemini_extension_activate_with, gemini_extension_deactivate_with, host_reported_extensions,
    installed_extension_is_current, installed_extension_is_present, installed_manifest_path,
    manifest_declares_current_server, read_installed_extension, require_gemini_cli, settings_path,
    stage_dir_is_tracedecay, staged_context_path, staged_manifest_path,
};

pub struct GeminiIntegration;

impl AgentIntegration for GeminiIntegration {
    fn name(&self) -> &'static str {
        "Gemini CLI"
    }

    fn id(&self) -> &'static str {
        "gemini"
    }

    /// Project-local installs are not supported under the extension model.
    ///
    /// A workspace-scoped extension lives in `<project>/.gemini/extensions/`
    /// and is installed by running the host CLI *inside that workspace*, but
    /// `host_cli::run_host_cli` admits the profile home as the child working
    /// directory, so this integration cannot target an arbitrary project yet.
    /// The honest answer is "not supported", the alternative would be
    /// hand-writing `<project>/.gemini/settings.json`, which is the emulation
    /// the adopted lifecycle exists to eliminate.
    fn supports_local_install(&self) -> bool {
        false
    }

    fn activate_deployed_host_registration(&self, ctx: &InstallContext) -> Result<()> {
        if installed_extension_is_current(&ctx.home, Some(&ctx.tracedecay_bin)) {
            return Ok(());
        }
        let gemini = require_gemini_cli()?;
        gemini_extension_activate_with(&gemini, &ctx.home)
    }

    fn deactivate_deployed_host_registration(&self, ctx: &InstallContext) -> Result<()> {
        // Nothing installed where Gemini keeps extensions: there is no host
        // registration to drop, and inventing an `uninstall` here would report
        // a removal that never happened.
        if !installed_extension_is_present(&ctx.home) {
            return Ok(());
        }
        let gemini = require_gemini_cli()?;
        gemini_extension_deactivate_with(&gemini, &ctx.home)
    }

    /// The canonical set is the lone context-MCP component, and the extension
    /// it installs is also the core registration, so either selects the
    /// extension lifecycle. The trait default forwards only `Core`.
    fn activate_deployed_host_component_registration(
        &self,
        components: &[HostComponentV1],
        ctx: &InstallContext,
    ) -> Result<()> {
        if selects_extension(components) {
            self.activate_deployed_host_registration(ctx)
        } else {
            Ok(())
        }
    }

    fn deactivate_deployed_host_component_registration(
        &self,
        components: &[HostComponentV1],
        ctx: &InstallContext,
    ) -> Result<()> {
        if selects_extension(components) {
            self.deactivate_deployed_host_registration(ctx)
        } else {
            Ok(())
        }
    }

    fn healthcheck(&self, dc: &mut DoctorCounters, ctx: &HealthcheckContext) {
        eprintln!("\n\x1b[1mGemini CLI integration\x1b[0m");
        doctor_check_staged_extension(dc, &ctx.home);
        doctor_check_installed_extension(dc, &ctx.home);
        doctor_check_host_reported_extensions(dc, &ctx.home);
    }

    /// Read-only registration state, observed from the host's installed
    /// extension. The extension carries the MCP server, so the context-MCP
    /// component and the core registration are the same fact here.
    fn host_component_registration(
        &self,
        component: super::host_bundle::HostComponentV1,
        ctx: &HealthcheckContext,
    ) -> super::host_bundle::HostBundleRegistrationStateV1 {
        if !selects_extension(&[component]) {
            return super::host_bundle::HostBundleRegistrationStateV1::Missing;
        }
        gemini_extension_registration_state(&ctx.home, None)
    }

    /// The lifecycle-aware twin: with an `InstallContext` the binary path in
    /// the installed manifest can be compared too, so a relocated tracedecay
    /// binary reports `Repairable` instead of a stale `Current`.
    fn host_component_registration_for_lifecycle(
        &self,
        component: super::host_bundle::HostComponentV1,
        ctx: &HealthcheckContext,
        install: &InstallContext,
    ) -> super::host_bundle::HostBundleRegistrationStateV1 {
        use super::host_bundle::HostBundleRegistrationStateV1 as State;

        match self.host_component_registration(component, ctx) {
            State::Current => gemini_extension_registration_state(
                &ctx.home,
                Some(install.tracedecay_bin.as_str()),
            ),
            state => state,
        }
    }

    fn is_detected(&self, home: &Path) -> bool {
        home.join(".gemini").is_dir()
    }

    /// The staged manifest, not `~/.gemini/settings.json`: the manifest is the
    /// one native config file this integration's projection owns and writes.
    fn primary_config_path(&self, home: &Path, _profile: &ProfileRoot) -> Option<PathBuf> {
        Some(staged_manifest_path(home))
    }

    /// Everything a lifecycle transaction must be able to restore: the staged
    /// source manifest TraceDecay writes, the installed manifest Gemini writes
    /// from it, and the shared settings file the host CLI may touch while
    /// enabling or disabling the extension.
    fn host_registration_paths(&self, home: &Path, _profile: &ProfileRoot) -> Vec<PathBuf> {
        vec![
            staged_manifest_path(home),
            installed_manifest_path(home),
            settings_path(home),
        ]
    }

    /// Adoption is a fact about Gemini, not about TraceDecay's staging: a
    /// staged source that the host never installed is not an installation.
    fn has_tracedecay(&self, home: &Path, _profile: &ProfileRoot) -> bool {
        match read_installed_extension(home) {
            InstalledExtensionV1::Present(manifest) => {
                manifest.get("name").and_then(serde_json::Value::as_str) == Some(EXTENSION_NAME)
            }
            InstalledExtensionV1::Missing | InstalledExtensionV1::Unreadable => false,
        }
    }
}

// ---------------------------------------------------------------------------
// Lifecycle state
// ---------------------------------------------------------------------------

fn selects_extension(components: &[HostComponentV1]) -> bool {
    components.iter().any(|component| {
        matches!(
            component,
            HostComponentV1::Core | HostComponentV1::ContextMcp
        )
    })
}

/// Registration state from the host's installed extension alone.
fn gemini_extension_registration_state(
    home: &Path,
    tracedecay_bin: Option<&str>,
) -> super::host_bundle::HostBundleRegistrationStateV1 {
    use super::host_bundle::HostBundleRegistrationStateV1 as State;

    match read_installed_extension(home) {
        InstalledExtensionV1::Missing => State::Missing,
        InstalledExtensionV1::Unreadable => State::Corrupt,
        InstalledExtensionV1::Present(manifest) => {
            if manifest_declares_current_server(&manifest, tracedecay_bin) {
                State::Current
            } else {
                State::Repairable
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Healthcheck helpers
// ---------------------------------------------------------------------------

fn doctor_check_staged_extension(dc: &mut DoctorCounters, home: &Path) {
    let stage_dir = extension_stage_dir(home);
    let manifest_path = staged_manifest_path(home);
    if !manifest_path.exists() {
        dc.warn(&format!(
            "{} not found, run `tracedecay install --agent gemini` if you use Gemini CLI",
            manifest_path.display()
        ));
        return;
    }
    if !stage_dir_is_tracedecay(&stage_dir) {
        dc.fail(&format!(
            "{} does not name the tracedecay extension, it is not a TraceDecay-owned source; \
             move it aside and run `tracedecay install --agent gemini`",
            manifest_path.display()
        ));
        return;
    }
    dc.pass(&format!(
        "Extension source staged at {}",
        stage_dir.display()
    ));

    let manifest = load_json_file(&manifest_path);
    match manifest.get("version").and_then(|v| v.as_str()) {
        Some(crate::PRODUCT_VERSION) => dc.pass("Staged extension version matches tracedecay"),
        Some(version) => dc.warn(&format!(
            "Staged extension version {version} does not match tracedecay {}, run `tracedecay update-plugin`",
            crate::PRODUCT_VERSION
        )),
        None => dc.warn("Staged gemini-extension.json does not contain a version"),
    }
    report_manifest_server(dc, &manifest, "Staged extension");

    let context_path = staged_context_path(home);
    if std::fs::read_to_string(&context_path).is_ok_and(|contents| contents.contains("tracedecay"))
    {
        dc.pass(&format!(
            "Staged extension ships its own context file ({})",
            context_path.display()
        ));
    } else {
        dc.fail(&format!(
            "{} is missing or carries no tracedecay rules, run `tracedecay install --agent gemini`",
            context_path.display()
        ));
    }
}

/// Check the copy Gemini CLI installed from that source. This is the check
/// that replaces the old `mcpServers.tracedecay` assertion: under the
/// extension model the server lives here, so this is where its `serve` args
/// and `trust: true` are now true or false.
fn doctor_check_installed_extension(dc: &mut DoctorCounters, home: &Path) {
    let manifest_path = installed_manifest_path(home);
    match read_installed_extension(home) {
        InstalledExtensionV1::Missing => dc.warn(&format!(
            "No tracedecay extension at {}. `tracedecay install --agent gemini` drives \
             `gemini extensions install`",
            manifest_path.display()
        )),
        InstalledExtensionV1::Unreadable => dc.fail(&format!(
            "{} exists but could not be read as JSON, remove it with \
             `gemini extensions uninstall {EXTENSION_NAME}` and reinstall",
            manifest_path.display()
        )),
        InstalledExtensionV1::Present(manifest) => {
            dc.pass(&format!(
                "Gemini CLI extension installed at {}",
                manifest_path.display()
            ));
            match manifest.get("version").and_then(|v| v.as_str()) {
                Some(crate::PRODUCT_VERSION) => {
                    dc.pass("Installed extension version matches tracedecay");
                }
                Some(version) => dc.warn(&format!(
                    "Installed extension version {version} does not match tracedecay {}, run \
                     `gemini extensions update {EXTENSION_NAME}`",
                    crate::PRODUCT_VERSION
                )),
                None => dc.warn("Installed gemini-extension.json does not contain a version"),
            }
            report_manifest_server(dc, &manifest, "Installed extension");
        }
    }
}

/// Report the MCP-server shape of one manifest. Shared by the staged and
/// installed checks so both judge the extension by the same contract.
fn report_manifest_server(dc: &mut DoctorCounters, manifest: &serde_json::Value, subject: &str) {
    let Some(server) = manifest
        .pointer(&format!("/mcpServers/{MCP_SERVER_NAME}"))
        .and_then(serde_json::Value::as_object)
    else {
        dc.fail(&format!(
            "{subject} does not declare the tracedecay MCP server, run \
             `tracedecay install --agent gemini`"
        ));
        return;
    };

    if server
        .get("command")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|command| !command.is_empty())
    {
        dc.pass(&format!("{subject} declares an MCP server command"));
    } else {
        dc.fail(&format!(
            "{subject} MCP server has no command, run `tracedecay install --agent gemini`"
        ));
    }

    let has_serve = server
        .get("args")
        .and_then(|v| v.as_array())
        .is_some_and(|args| args.iter().any(|arg| arg.as_str() == Some("serve")));
    if has_serve {
        dc.pass(&format!("{subject} MCP server args include \"serve\""));
    } else {
        dc.fail(&format!(
            "{subject} MCP server args missing \"serve\", run `tracedecay install --agent gemini`"
        ));
    }

    if server.get("trust").and_then(serde_json::Value::as_bool) == Some(true) {
        dc.pass(&format!(
            "{subject} MCP server has trust: true (tools auto-approved)"
        ));
    } else {
        dc.warn(&format!(
            "{subject} MCP server missing trust: true. Gemini will prompt for each tool call"
        ));
    }
}

/// Ask Gemini CLI itself. This is the only check that can report *Gemini's*
/// view of its extensions; when its binary is absent the doctor says the state
/// was not observed rather than inferring adoption from TraceDecay's own
/// staged files.
fn doctor_check_host_reported_extensions(dc: &mut DoctorCounters, home: &Path) {
    let outcome = match host_reported_extensions(home) {
        Ok(Some(outcome)) => outcome,
        Ok(None) => {
            dc.info(
                "`gemini` is not on PATH, could not ask Gemini CLI which extensions it has \
                 (the extension lifecycle requires that binary)",
            );
            return;
        }
        Err(error) => {
            dc.fail(&format!(
                "could not inspect Gemini CLI extension state: {error}"
            ));
            return;
        }
    };
    if !outcome.succeeded() {
        dc.warn(&format!(
            "could not verify with Gemini CLI itself: {}",
            outcome.failure_message()
        ));
        return;
    }
    if outcome.stdout.contains(EXTENSION_NAME) {
        dc.pass("`gemini extensions list` reports the tracedecay extension");
    } else {
        dc.fail(
            "`gemini extensions list` does not report a tracedecay extension, run \
             `tracedecay install --agent gemini`",
        );
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests;
