//! ChatGPT host integration.
//!
//! TraceDecay owns the receipt-backed staged source. ChatGPT owns interactive
//! activation and exposes no supported local registration readback. Successful
//! staging is terminal; registration is reported as unverifiable, never current.

use std::path::{Path, PathBuf};

use tracedecay_runtime_core::config::ProfileRoot;

use tracedecay_domain::errors::{Result, TraceDecayError};

use super::{
    AgentIntegration, DoctorCounters, HealthcheckContext, InstallContext,
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
        _ctx: &InstallContext,
    ) -> Result<NonInteractiveInstallOutcome> {
        Ok(NonInteractiveInstallOutcome::Ready)
    }

    fn interactive_activation_guidance(&self) -> Option<String> {
        Some("ChatGPT registration is unverifiable locally; install the staged bundle inside ChatGPT or follow its README to connect an MCP endpoint".to_string())
    }

    fn interactive_removal_guidance(&self) -> Option<String> {
        Some("remove the TraceDecay plugin or connector inside ChatGPT; TraceDecay removes only its staged source".to_string())
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
        super::host_bundle::HostBundleRegistrationStateV1::Unverifiable
    }

    fn is_detected(&self, home: &Path) -> bool {
        chatgpt_host_surfaces(home).iter().any(|dir| dir.is_dir())
    }

    fn has_tracedecay(&self, home: &Path, _profile: &ProfileRoot) -> bool {
        // Any residue under the staged root is evidence this integration ran;
        // a manifest-less remnant is a broken stage doctor must see, not
        // proof that nothing was deployed.
        chatgpt_staged_plugin_dir(home)
            .read_dir()
            .map(|mut entries| entries.next().is_some())
            .unwrap_or(false)
    }

    fn detected_host_surface(&self, home: &Path, _profile: &ProfileRoot) -> Option<PathBuf> {
        chatgpt_host_surfaces(home)
            .into_iter()
            .find(|dir| dir.is_dir())
    }

    fn activate_deployed_host_registration(&self, _ctx: &InstallContext) -> Result<()> {
        Err(TraceDecayError::Config {
            message: "ChatGPT activation is available only inside the host; registration is unverifiable locally".to_string(),
        })
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

// ---------------------------------------------------------------------------
// Healthcheck helpers
// ---------------------------------------------------------------------------

/// Check the staged ChatGPT bundle: its manifest parses, every file the
/// rendered inventory names is present, and host registration is reported as
/// unverifiable. An absent bundle warns (not every machine runs
/// ChatGPT); a partial one fails. Whether ChatGPT has consumed the bundle is
/// never claimed, no local surface reports it. Byte equality is not asserted
/// here: `mcp.json` embeds the installing machine's resolved tracedecay path,
/// which the receipt digest already pins.
fn doctor_check_plugin(dc: &mut DoctorCounters, home: &Path) {
    let staged_dir = chatgpt_staged_plugin_dir(home);
    let manifest_path = staged_dir.join(CHATGPT_PLUGIN_MANIFEST_RELATIVE);
    if !manifest_path.is_file() {
        // A staged tree without its manifest is a partial stage, not an
        // absent one: the lifecycle committed files it owns, so the check
        // fails rather than warning as if nothing were staged.
        let has_residue = staged_dir
            .read_dir()
            .map(|mut entries| entries.next().is_some())
            .unwrap_or(false);
        if has_residue {
            dc.fail(&format!(
                "ChatGPT staged bundle at {} is incomplete: {} missing, run `tracedecay reinstall --agent chatgpt`",
                staged_dir.display(),
                CHATGPT_PLUGIN_MANIFEST_RELATIVE
            ));
        } else {
            dc.warn(&format!(
                "no ChatGPT plugin bundle staged at {}, run `tracedecay install --agent chatgpt` if you use ChatGPT",
                staged_dir.display()
            ));
        }
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

    let missing: Vec<&'static str> = super::plugin_bundle::chatgpt_files()
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

    dc.info("ChatGPT registration is unverifiable locally; install the staged bundle inside ChatGPT or follow its README to connect an MCP endpoint");
}
