//! Pi agent integration.
//!
//! Pi (the Earendil Works coding agent) has no MCP route: its extension API
//! registers model-callable tools, lifecycle hooks, and slash commands inside
//! the agent process, and skills provide the prompt-rules surface. TraceDecay
//! therefore deploys a first-party extension plus a companion skill into the
//! Pi agent directory:
//!
//! - `~/.pi/agent/extensions/tracedecay/` — the `Core` component: `index.ts`
//!   (the extension), its `package.json`, and `schemas.json`, the tool
//!   definitions generated from the MCP catalog exactly as the Hermes bridge
//!   renders them. The extension registers one Pi tool per catalog entry,
//!   bridged over `tracedecay tool` through the daemon socket, forwards Pi's
//!   `session_start` and `agent_end` events to `hook-pi-event`, and adds the
//!   `/tracedecay{, -sync, -version}` commands.
//! - `~/.pi/agent/skills/tracedecay-cli/SKILL.md` — the `Agent` component,
//!   the routing skill that tells the model which tool answers which task.
//!
//! The receipt-backed component transaction owns those bytes under
//! `~/.pi/agent`. `PI_CODING_AGENT_DIR` relocates the directory Pi loads; it
//! is honored only for the running process user's home and only when absolute,
//! and activation then mirrors the receipt-owned bytes into it, the same shape
//! OpenCode uses for `$XDG_CONFIG_HOME`.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use tracedecay_domain::errors::{Result, TraceDecayError};

use super::host_bundle::{HostBundleRegistrationStateV1, HostComponentV1};
use super::{
    AgentIntegration, DoctorCounters, HealthcheckContext, InstallContext, safe_remove_host_file,
    safe_write_bytes_file,
};
use crate::ports::mcp_tools::{advertised_tool_schemas_json, advertised_tools};

pub struct PiIntegration;

const PI_EXTENSION_SOURCE: &str = include_str!("../../../../plugin/pi/index.ts");
const PI_PACKAGE_TEMPLATE: &str = include_str!("../../../../plugin/pi/package.json");
const PI_SKILL_SOURCE: &str = include_str!("../../../../plugin/pi/skill/SKILL.md");

const PI_EXTENSION_MARKER: &str = "TraceDecayPiExtension";
const PI_VERSION_PLACEHOLDER: &str = "__TRACEDECAY_VERSION__";
const PI_AGENT_DIR_ENV: &str = "PI_CODING_AGENT_DIR";
/// Home-relative directory Pi loads by default, and the only place the
/// component transaction deploys Pi artifacts.
pub(crate) const PI_AGENT_RELATIVE: &str = ".pi/agent";
const PI_EXTENSION_RELATIVE: &str = "extensions/tracedecay/index.ts";
const PI_PACKAGE_RELATIVE: &str = "extensions/tracedecay/package.json";
const PI_SCHEMAS_RELATIVE: &str = "extensions/tracedecay/schemas.json";
const PI_SKILL_RELATIVE: &str = "skills/tracedecay-cli/SKILL.md";
const PI_CORE_RELATIVE: [&str; 3] = [
    PI_EXTENSION_RELATIVE,
    PI_PACKAGE_RELATIVE,
    PI_SCHEMAS_RELATIVE,
];
const PI_AGENT_COMPONENT_RELATIVE: [&str; 1] = [PI_SKILL_RELATIVE];

/// Render the Pi Core component inventory (agent-directory-relative paths)
/// with the resolved binary path, crate version, and catalog schemas.
pub(crate) fn rendered_core_files(tracedecay_bin: &str) -> Result<Vec<(&'static str, String)>> {
    Ok(vec![
        (
            PI_EXTENSION_RELATIVE,
            rendered_extension_source(tracedecay_bin)?,
        ),
        (PI_PACKAGE_RELATIVE, rendered_package_json()?),
        (
            PI_SCHEMAS_RELATIVE,
            advertised_tool_schemas_json(&advertised_tools()?)?,
        ),
    ])
}

/// The Pi Agent component inventory (agent-directory-relative paths).
pub(crate) fn rendered_agent_files() -> Vec<(&'static str, String)> {
    vec![(PI_SKILL_RELATIVE, PI_SKILL_SOURCE.to_owned())]
}

fn component_relative_paths(components: &[HostComponentV1]) -> Vec<&'static str> {
    let mut paths = Vec::new();
    if components.contains(&HostComponentV1::Core) {
        paths.extend(PI_CORE_RELATIVE);
    }
    if components.contains(&HostComponentV1::Agent) {
        paths.extend(PI_AGENT_COMPONENT_RELATIVE);
    }
    paths
}

/// The agent directory Pi loads for `home`.
fn pi_agent_dir(home: &Path) -> PathBuf {
    pi_agent_dir_for(home, ambient_pi_agent_dir(home).as_deref())
}

/// `PI_CODING_AGENT_DIR` names *this process user's* agent directory, so it
/// only answers for that user's home. A sandbox or `--home` root stays inside
/// the root it named.
fn ambient_pi_agent_dir(home: &Path) -> Option<OsString> {
    if !super::is_process_home(home) {
        return None;
    }
    std::env::var_os(PI_AGENT_DIR_ENV)
}

/// Only an absolute override relocates the directory, so a malformed value
/// cannot quietly deploy next to whatever the working directory happens to be.
fn pi_agent_dir_for(home: &Path, ambient: Option<&OsStr>) -> PathBuf {
    ambient
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| home.join(PI_AGENT_RELATIVE))
}

/// Receipt-owned source and relocated destination for each selected artifact.
/// Empty when Pi loads the receipt-owned directory itself.
fn relocated_assets(
    home: &Path,
    loaded: &Path,
    components: &[HostComponentV1],
) -> Vec<(PathBuf, PathBuf)> {
    let owned = home.join(PI_AGENT_RELATIVE);
    if loaded == owned {
        return Vec::new();
    }
    component_relative_paths(components)
        .into_iter()
        .map(|relative| (owned.join(relative), loaded.join(relative)))
        .collect()
}

fn rendered_package_json() -> Result<String> {
    if !PI_PACKAGE_TEMPLATE.contains(PI_VERSION_PLACEHOLDER) {
        return Err(TraceDecayError::Config {
            message: "Pi plugin package.json template lost its version placeholder".to_owned(),
        });
    }
    Ok(PI_PACKAGE_TEMPLATE.replace(PI_VERSION_PLACEHOLDER, crate::PRODUCT_VERSION))
}

/// Render the extension source with the resolved binary path, exactly the
/// OpenCode plugin rendering contract: the placeholder is a quoted JSON
/// string literal, so the replacement is the JSON-encoded binary path.
fn rendered_extension_source(tracedecay_bin: &str) -> Result<String> {
    let encoded = serde_json::to_string(tracedecay_bin)?;
    let rendered = PI_EXTENSION_SOURCE.replace(
        &format!("\"{}\"", super::plugin_bundle::TRACEDECAY_BIN_PLACEHOLDER),
        &encoded,
    );
    super::plugin_bundle::reject_unresolved_placeholders(&rendered, "Pi plugin")?;
    if !rendered.contains(PI_EXTENSION_MARKER) {
        return Err(TraceDecayError::Config {
            message: "Pi plugin template lost its extension marker".to_owned(),
        });
    }
    Ok(rendered)
}

/// Registration state of one component in the directory Pi loads. A relocated
/// copy is current only while it matches the receipt-owned bytes.
fn component_state(home: &Path, component: HostComponentV1) -> HostBundleRegistrationStateV1 {
    use HostBundleRegistrationStateV1 as State;

    let loaded = pi_agent_dir(home);
    let owned = home.join(PI_AGENT_RELATIVE);
    let relatives = component_relative_paths(&[component]);
    if relatives.is_empty() {
        return State::Missing;
    }
    let present = relatives
        .iter()
        .filter(|relative| loaded.join(relative).is_file())
        .count();
    if present == 0 {
        return State::Missing;
    }
    let current = present == relatives.len()
        && relatives.iter().all(|relative| {
            let Ok(bytes) = std::fs::read(loaded.join(relative)) else {
                return false;
            };
            let marked = match *relative {
                PI_EXTENSION_RELATIVE => contains(&bytes, PI_EXTENSION_MARKER),
                PI_SKILL_RELATIVE => contains(&bytes, "tracedecay_"),
                _ => true,
            };
            marked && (loaded == owned || std::fs::read(owned.join(relative)).ok() == Some(bytes))
        });
    if current {
        State::Current
    } else {
        State::Repairable
    }
}

fn contains(bytes: &[u8], marker: &str) -> bool {
    std::str::from_utf8(bytes).is_ok_and(|text| text.contains(marker))
}

impl AgentIntegration for PiIntegration {
    fn name(&self) -> &'static str {
        "Pi"
    }

    fn id(&self) -> &'static str {
        "pi"
    }

    fn healthcheck(&self, dc: &mut DoctorCounters, ctx: &HealthcheckContext) {
        use HostBundleRegistrationStateV1 as State;

        eprintln!("\n\x1b[1mPi integration\x1b[0m");
        let loaded = pi_agent_dir(&ctx.home);
        for (component, label, relative) in [
            (
                HostComponentV1::Core,
                "TraceDecay extension",
                PI_EXTENSION_RELATIVE,
            ),
            (
                HostComponentV1::Agent,
                "TraceDecay routing skill",
                PI_SKILL_RELATIVE,
            ),
        ] {
            let path = loaded.join(relative);
            match component_state(&ctx.home, component) {
                State::Current => dc.pass(&format!("{label} deployed at {}", path.display())),
                State::Missing => dc.warn(&format!(
                    "{} not found, run `tracedecay install --agent pi` if you use Pi",
                    path.display()
                )),
                _ => dc.fail(&format!(
                    "{label} at {} is incomplete or stale; run `tracedecay install --agent pi`",
                    loaded.display()
                )),
            }
        }
    }

    fn host_component_registration(
        &self,
        component: HostComponentV1,
        ctx: &HealthcheckContext,
    ) -> HostBundleRegistrationStateV1 {
        component_state(&ctx.home, component)
    }

    fn is_detected(&self, home: &Path) -> bool {
        pi_agent_dir(home).is_dir()
    }

    fn detected_host_surface(&self, home: &Path) -> Option<PathBuf> {
        let dir = pi_agent_dir(home);
        dir.is_dir().then_some(dir)
    }

    fn has_tracedecay(&self, home: &Path) -> bool {
        let loaded = pi_agent_dir(home);
        component_relative_paths(&[HostComponentV1::Core, HostComponentV1::Agent])
            .into_iter()
            .any(|relative| loaded.join(relative).exists())
    }

    fn primary_config_path(&self, home: &Path) -> Option<PathBuf> {
        Some(pi_agent_dir(home).join(PI_EXTENSION_RELATIVE))
    }

    fn host_registration_paths(&self, home: &Path) -> Vec<PathBuf> {
        self.host_component_registration_paths(
            &[HostComponentV1::Core, HostComponentV1::Agent],
            home,
        )
    }

    /// Only relocated mirrors are host registration state, snapshotted so a
    /// failed lifecycle restores them. The receipt-owned artifacts under
    /// `~/.pi/agent` belong to the component transaction, so without a
    /// relocation there is nothing to register.
    fn host_component_registration_paths(
        &self,
        components: &[HostComponentV1],
        home: &Path,
    ) -> Vec<PathBuf> {
        relocated_assets(home, &pi_agent_dir(home), components)
            .into_iter()
            .map(|(_, relocated)| relocated)
            .collect()
    }

    fn activate_deployed_host_component_registration(
        &self,
        components: &[HostComponentV1],
        ctx: &InstallContext,
    ) -> Result<()> {
        for (owned, relocated) in relocated_assets(&ctx.home, &pi_agent_dir(&ctx.home), components)
        {
            let bytes = std::fs::read(&owned).map_err(|error| TraceDecayError::Config {
                message: format!(
                    "failed to read deployed Pi artifact {}: {error}",
                    owned.display()
                ),
            })?;
            safe_write_bytes_file(&relocated, &bytes)?;
        }
        Ok(())
    }

    fn deactivate_deployed_host_component_registration(
        &self,
        components: &[HostComponentV1],
        ctx: &InstallContext,
    ) -> Result<()> {
        for (_, relocated) in relocated_assets(&ctx.home, &pi_agent_dir(&ctx.home), components) {
            match safe_remove_host_file(&relocated) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(TraceDecayError::Config {
                        message: format!("failed to remove {}: {error}", relocated.display()),
                    });
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn ambient_agent_dir_is_honored_only_when_absolute() {
        let home = Path::new("/home/operator");
        assert_eq!(pi_agent_dir_for(home, None), home.join(".pi/agent"));
        assert_eq!(
            pi_agent_dir_for(home, Some(OsStr::new("/srv/pi-agent"))),
            PathBuf::from("/srv/pi-agent")
        );
        for relative in ["relocated/agent", "./agent", "~/.pi/other", ""] {
            assert_eq!(
                pi_agent_dir_for(home, Some(OsStr::new(relative))),
                home.join(".pi/agent"),
                "relative override {relative:?} must not relocate the agent directory"
            );
        }
    }

    /// A tempdir is never the process user's home, so whatever the operator
    /// exports as `PI_CODING_AGENT_DIR` cannot redirect a sandboxed home.
    #[test]
    fn a_foreign_home_never_resolves_outside_itself() {
        let home = tempfile::tempdir().unwrap();
        assert!(ambient_pi_agent_dir(home.path()).is_none());
        assert_eq!(pi_agent_dir(home.path()), home.path().join(".pi/agent"));
        let paths = PiIntegration.host_registration_paths(home.path());
        assert!(paths.is_empty(), "{paths:?}");
    }

    #[test]
    fn core_inventory_renders_the_binary_version_and_catalog_schemas() {
        let files = rendered_core_files("/usr/local/bin/tracedecay").unwrap();
        let file = |relative: &str| {
            files
                .iter()
                .find(|(path, _)| *path == relative)
                .map(|(_, body)| body.as_str())
                .unwrap()
        };

        let extension = file(PI_EXTENSION_RELATIVE);
        assert!(extension.contains(PI_EXTENSION_MARKER));
        assert!(extension.contains("\"/usr/local/bin/tracedecay\""));
        assert!(!extension.contains(super::super::plugin_bundle::TRACEDECAY_BIN_PLACEHOLDER));

        let package = file(PI_PACKAGE_RELATIVE);
        assert!(package.contains(crate::PRODUCT_VERSION));
        assert!(!package.contains(PI_VERSION_PLACEHOLDER));

        let schemas: Vec<serde_json::Value> =
            serde_json::from_str(file(PI_SCHEMAS_RELATIVE)).unwrap();
        let catalog = advertised_tools().unwrap();
        assert_eq!(schemas.len(), catalog.len());
        let search = schemas
            .iter()
            .find(|schema| schema["name"] == "tracedecay_search")
            .unwrap();
        assert_eq!(search["read_only"], true);
        let edit = schemas
            .iter()
            .find(|schema| schema["name"] == "tracedecay_str_replace")
            .unwrap();
        assert_eq!(edit["read_only"], false);
    }

    #[test]
    fn relocated_mirror_tracks_the_receipt_owned_bytes() {
        let home = tempfile::tempdir().unwrap();
        let relocated = tempfile::tempdir().unwrap();
        let owned = home.path().join(PI_AGENT_RELATIVE);
        let components = [HostComponentV1::Core, HostComponentV1::Agent];

        assert!(relocated_assets(home.path(), &owned, &components).is_empty());
        let pairs = relocated_assets(home.path(), relocated.path(), &components);
        assert_eq!(pairs.len(), 4);
        for (source, destination) in &pairs {
            assert!(source.starts_with(&owned));
            assert!(destination.starts_with(relocated.path()));
            assert_eq!(
                source.strip_prefix(&owned).unwrap(),
                destination.strip_prefix(relocated.path()).unwrap()
            );
        }
        assert_eq!(
            relocated_assets(home.path(), relocated.path(), &[HostComponentV1::Agent]),
            vec![(
                owned.join(PI_SKILL_RELATIVE),
                relocated.path().join(PI_SKILL_RELATIVE)
            )]
        );
    }

    #[test]
    fn registration_state_follows_the_loaded_directory() {
        use HostBundleRegistrationStateV1 as State;

        let home = tempfile::tempdir().unwrap();
        let owned = home.path().join(PI_AGENT_RELATIVE);
        assert_eq!(
            component_state(home.path(), HostComponentV1::Core),
            State::Missing
        );

        let write = |relative: &str, body: &str| {
            let path = owned.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, body).unwrap();
        };
        for (relative, body) in rendered_core_files("/usr/local/bin/tracedecay").unwrap() {
            write(relative, &body);
        }
        assert_eq!(
            component_state(home.path(), HostComponentV1::Core),
            State::Current
        );
        assert_eq!(
            component_state(home.path(), HostComponentV1::Agent),
            State::Missing
        );

        write(
            PI_EXTENSION_RELATIVE,
            "// operator replaced the extension\n",
        );
        assert_eq!(
            component_state(home.path(), HostComponentV1::Core),
            State::Repairable
        );
        assert_eq!(
            component_state(home.path(), HostComponentV1::ContextMcp),
            State::Missing
        );
    }
}
