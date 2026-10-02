//! ChatGPT host integration.
//!
//! `TraceDecay` stages its portable Agent Plugins bundle, `plugin.json`,
//! `mcp.json`, and the compiled ChatGPT code explorer, under its own profile
//! through the receipt-backed component transaction. ChatGPT exposes no
//! non-interactive registration surface: connectors are added through
//! developer mode and plugins through the app's own interactive flow, and no
//! host CLI or local registry file exists for TraceDecay to drive. Every
//! lifecycle command therefore commits the staged source it owns and reports
//! the remaining host-side step as a typed pending action that cannot be
//! verified locally. `uninstall` removes the staged source; the connector or
//! plugin the operator created in ChatGPT is theirs to remove. There is no
//! project-local route.
//!
//! ChatGPT owns connector activation; TraceDecay owns only its staged source.

use std::path::{Path, PathBuf};

use tracedecay_runtime_core::config::ProfileRoot;

use tracedecay_domain::errors::{Result, TraceDecayError};

use super::{
    AgentIntegration, DeferredUserAction, DoctorCounters, HealthcheckContext, InstallContext,
    NonInteractiveInstallOutcome,
};

/// ChatGPT's plugin manifest inside its staged source, the bundle's own
/// registration payload.
const CHATGPT_PLUGIN_MANIFEST_RELATIVE: &str = "plugin.json";

/// The MCP config inside the staged bundle. It launches `graph`
/// (`tracedecay serve`) and `tracedecay-explorer` (`node
/// ${PLUGIN_ROOT}/chatgpt-extension/embedded/server.mjs`).
const CHATGPT_MCP_RELATIVE: &str = "mcp.json";

/// Profile-relative source directory the receipt-backed lifecycle owns.
/// ChatGPT consumes it through its own interactive install; TraceDecay never
/// writes a ChatGPT-owned directory.
pub(crate) const CHATGPT_STAGED_PLUGIN_RELATIVE: &str =
    ".tracedecay/host-bundle-stage/chatgpt/tracedecay";

/// Application-data directories proving the ChatGPT desktop app ran on this
/// machine. There is no documented stable config file inside either of them
/// that a registration readback could trust, and neither is written here.
fn chatgpt_host_surfaces(home: &Path) -> [PathBuf; 2] {
    [
        home.join("Library/Application Support/ChatGPT"),
        home.join("AppData/Roaming/ChatGPT"),
    ]
}

pub struct ChatGptIntegration;

impl AgentIntegration for ChatGptIntegration {
    fn name(&self) -> &'static str {
        "ChatGPT"
    }

    fn id(&self) -> &'static str {
        "chatgpt"
    }

    fn require_host(&self, _home: &Path) -> Result<super::HostPresence> {
        Ok(super::HostPresence::NoHostCli)
    }

    fn preflight_non_interactive_install(
        &self,
        ctx: &InstallContext,
    ) -> Result<NonInteractiveInstallOutcome> {
        // The component transaction deploys the staged source; ChatGPT's own
        // interactive install must then register it. There is no state to
        // probe for `Ready`: ChatGPT keeps no locally readable registry, so
        // activation is a standing deferral rather than a recoverable one.
        Ok(NonInteractiveInstallOutcome::DeferredUserAction(
            chatgpt_lifecycle_unavailable("install", Some(&chatgpt_staged_plugin_dir(&ctx.home))),
        ))
    }

    fn interactive_activation_guidance(&self) -> Option<String> {
        Some(chatgpt_lifecycle_unavailable("install", None).remediation)
    }

    fn interactive_removal_guidance(&self) -> Option<String> {
        Some(chatgpt_lifecycle_unavailable("remove", None).remediation)
    }

    fn healthcheck(&self, dc: &mut DoctorCounters, ctx: &HealthcheckContext) {
        eprintln!("\n\x1b[1mChatGPT integration\x1b[0m");
        doctor_check_plugin(dc, &ctx.home);
    }

    fn host_component_registration(
        &self,
        _component: super::host_bundle::HostComponentV1,
        _ctx: &HealthcheckContext,
    ) -> super::host_bundle::HostBundleRegistrationStateV1 {
        // ChatGPT keeps no locally readable registration surface: connector
        // state lives host-side and the desktop app's plugin store is not a
        // documented readable registry. Reporting anything beyond Missing
        // would assert state this integration cannot observe.
        super::host_bundle::HostBundleRegistrationStateV1::Missing
    }

    fn is_detected(&self, home: &Path) -> bool {
        chatgpt_host_surfaces(home).iter().any(|dir| dir.is_dir())
    }

    fn has_tracedecay(&self, home: &Path, _profile: &ProfileRoot) -> bool {
        // The staged bundle's manifest is the only integration payload this
        // host owns; its presence is all the readback can truthfully claim.
        chatgpt_staged_plugin_dir(home)
            .join(CHATGPT_PLUGIN_MANIFEST_RELATIVE)
            .is_file()
    }

    fn detected_host_surface(&self, home: &Path, _profile: &ProfileRoot) -> Option<PathBuf> {
        chatgpt_host_surfaces(home)
            .into_iter()
            .find(|dir| dir.is_dir())
    }

    fn activate_deployed_host_registration(&self, ctx: &InstallContext) -> Result<()> {
        Err(deferred_user_action_error(chatgpt_lifecycle_unavailable(
            "install",
            Some(&chatgpt_staged_plugin_dir(&ctx.home)),
        )))
    }

    fn deactivate_deployed_host_registration(&self, _ctx: &InstallContext) -> Result<()> {
        // ChatGPT-side connector removal is the operator's interactive step;
        // nothing locally readable belongs to this registration.
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Staged source helpers
// ---------------------------------------------------------------------------

/// The staged portable bundle the ChatGPT host flow consumes.
pub(crate) fn chatgpt_staged_plugin_dir(home: &Path) -> PathBuf {
    home.join(CHATGPT_STAGED_PLUGIN_RELATIVE)
}

/// Canonical rendered ChatGPT bundle inventory shared by staged-source
/// staging and the receipt-backed first-party catalog, so the bytes the
/// transaction deploys are byte-identical to the ones doctor verifies.
///
/// The `graph` server entry in `mcp.json` is pointed at the resolved
/// tracedecay binary; `tracedecay-explorer` keeps its `node` launch, the
/// extension requires a Node runtime the host supplies.
pub(crate) fn rendered_plugin_files(tracedecay_bin: &str) -> Result<Vec<(&'static str, String)>> {
    super::plugin_bundle::chatgpt_files()
        .into_iter()
        .map(|(relative, contents)| {
            let rendered = match relative {
                CHATGPT_PLUGIN_MANIFEST_RELATIVE => {
                    super::plugin_bundle::stamp_manifest_version(contents)?
                }
                CHATGPT_MCP_RELATIVE => {
                    super::plugin_bundle::set_mcp_command(contents, tracedecay_bin)?
                }
                _ => contents.to_string(),
            };
            super::plugin_bundle::reject_unresolved_placeholders(&rendered, relative)?;
            Ok((relative, rendered))
        })
        .collect()
}

fn deferred_user_action_error(action: DeferredUserAction) -> TraceDecayError {
    TraceDecayError::Config {
        message: action.remediation,
    }
}

fn chatgpt_lifecycle_unavailable(action: &str, staged_dir: Option<&Path>) -> DeferredUserAction {
    // The one operator step that is itself a command: serving the staged
    // bundle's MCP adapter on loopback for a developer-mode connector.
    let command = staged_dir.map_or_else(
        || format!("finish the {action} inside ChatGPT"),
        |dir| {
            format!(
                "node {} --http 127.0.0.1:8787",
                dir.join("chatgpt-extension/embedded/server.mjs").display()
            )
        },
    );
    let remediation = match action {
        "install" => match staged_dir {
            Some(dir) => format!(
                "ChatGPT registers plugins and connectors only through its own interactive \
                 surfaces (developer-mode connector setup, the desktop app); there is no host \
                 CLI or local registry for TraceDecay to drive. TraceDecay committed the \
                 staged bundle it owns at {}; install it inside ChatGPT, or point a connector \
                 at its MCP endpoint (`{command}`)",
                dir.display()
            ),
            None => "register the TraceDecay plugin or connector inside ChatGPT (developer-mode \
                     connector setup or the app's plugin flow); ChatGPT keeps no local registry \
                     for TraceDecay to drive"
                .to_string(),
        },
        _ => "remove the TraceDecay plugin or connector inside ChatGPT through its own \
              interactive surfaces; TraceDecay removes only the staged bundle it owns"
            .to_string(),
    };
    DeferredUserAction {
        remediation,
        command,
    }
}

/// Doctor's line for a staged bundle ChatGPT has not observably activated.
/// ChatGPT keeps no locally readable registry, so the host-side step stays
/// pending from TraceDecay's vantage rather than silently converging.
fn pending_activation_notice(home: &Path) -> String {
    let action = chatgpt_lifecycle_unavailable("install", Some(&chatgpt_staged_plugin_dir(home)));
    format!("pending operator action: {}", action.command)
}

// ---------------------------------------------------------------------------
// Healthcheck helpers
// ---------------------------------------------------------------------------

/// Check the staged ChatGPT bundle: its manifest parses, every file the
/// rendered inventory names is present, and the host-side activation step is
/// reported as pending. An absent bundle warns (not every machine runs
/// ChatGPT); a partial one fails. Whether ChatGPT has consumed the bundle is
/// never claimed, no local surface reports it. Byte equality is not asserted
/// here: `mcp.json` embeds the installing machine's resolved tracedecay path,
/// which the receipt digest already pins.
fn doctor_check_plugin(dc: &mut DoctorCounters, home: &Path) {
    let staged_dir = chatgpt_staged_plugin_dir(home);
    let manifest_path = staged_dir.join(CHATGPT_PLUGIN_MANIFEST_RELATIVE);
    if !manifest_path.is_file() {
        dc.warn(&format!(
            "no ChatGPT plugin bundle staged at {}, run `tracedecay install --agent chatgpt` if you use ChatGPT",
            staged_dir.display()
        ));
        return;
    }
    dc.pass(&format!(
        "ChatGPT plugin bundle staged at {}",
        staged_dir.display()
    ));

    let manifest = std::fs::read_to_string(&manifest_path)
        .ok()
        .and_then(|contents| serde_json::from_str::<serde_json::Value>(&contents).ok());
    match manifest {
        Some(manifest) => {
            dc.pass(&format!(
                "ChatGPT plugin manifest parses at {}",
                manifest_path.display()
            ));
            if manifest
                .get("extensions")
                .and_then(|extensions| extensions.get("com.openai"))
                .is_some()
            {
                dc.pass("ChatGPT plugin manifest declares the com.openai extension block");
            } else {
                dc.fail("ChatGPT plugin manifest is missing its com.openai extension block");
            }
        }
        None => {
            dc.fail(&format!(
                "ChatGPT plugin manifest missing or invalid at {}, run `tracedecay install --agent chatgpt`",
                manifest_path.display()
            ));
            return;
        }
    }

    let missing: Vec<&'static str> = rendered_plugin_files("tracedecay")
        .unwrap_or_default()
        .into_iter()
        .map(|(relative, _)| relative)
        .filter(|relative| !staged_dir.join(relative).is_file())
        .collect();
    if missing.is_empty() {
        dc.pass("ChatGPT staged bundle contains every rendered file");
    } else {
        dc.fail(&format!(
            "ChatGPT staged bundle at {} is missing {} file(s) ({}), run `tracedecay reinstall --agent chatgpt`",
            staged_dir.display(),
            missing.len(),
            missing.join(", ")
        ));
    }

    dc.pending(&pending_activation_notice(home));
}
