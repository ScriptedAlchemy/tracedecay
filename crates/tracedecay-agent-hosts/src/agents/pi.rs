//! Pi agent integration.
//!
//! Pi (the Earendil Works coding agent) has no MCP route: its extension API
//! registers model-callable tools, lifecycle hooks, and slash commands inside
//! the agent process, and skills provide the prompt-rules surface. TraceDecay
//! therefore deploys a first-party extension plus a companion skill into the
//! Pi agent directory:
//!
//! - `~/.pi/agent/extensions/tracedecay/index.ts` — the extension that
//!   registers the `tracedecay_*` graph tools (bridged over `tracedecay tool`
//!   through the daemon socket) and the `/tracedecay{, -sync, -version}`
//!   commands. Tool schemas come from the generated `schemas.json` rendered
//!   beside it from the same MCP catalog authority Hermes consumes, so the
//!   extension never carries hand-copied parameter schemas. Owned by the
//!   `Core` component together with its `package.json` and `schemas.json`.
//! - `~/.pi/agent/skills/tracedecay-cli/SKILL.md` — the routing skill that
//!   tells the model which tool answers which task. Owned by the `Agent`
//!   component.
//!
//! The agent directory is always `~/.pi/agent`. Ambient relocation
//! (`PI_CODING_AGENT_DIR`) is deliberately not honored: the receipt-backed
//! catalog artifacts are pinned to the home-relative paths, and honoring an
//! ambient override would let `tracedecay install --home <other>` escape that
//! home or let a unit test overwrite the operator's real install.

use std::path::{Path, PathBuf};

use tracedecay_domain::errors::{Result, TraceDecayError};

use super::host_bundle::{HostBundleRegistrationStateV1, HostComponentV1};
use super::{
    AgentIntegration, DoctorCounters, HealthcheckContext, InstallContext, safe_remove_host_file,
    safe_write_text_file,
};

pub struct PiIntegration;

const PI_EXTENSION_SOURCE: &str = include_str!("../../../../plugin/pi/index.ts");
const PI_LIB_SOURCE: &str = include_str!("../../../../plugin/pi/lib.ts");
const PI_PACKAGE_TEMPLATE: &str = include_str!("../../../../plugin/pi/package.json");

const PI_EXTENSION_MARKER: &str = "TraceDecayPiExtension";
const PI_VERSION_PLACEHOLDER: &str = "__TRACEDECAY_VERSION__";
const PI_EXTENSION_RELATIVE: &str = "extensions/tracedecay/index.ts";
const PI_LIB_RELATIVE: &str = "extensions/tracedecay/lib.ts";
const PI_PACKAGE_RELATIVE: &str = "extensions/tracedecay/package.json";
const PI_SCHEMAS_RELATIVE: &str = "extensions/tracedecay/schemas.json";
const PI_SKILL_RELATIVE: &str = "skills/tracedecay-cli/SKILL.md";
pub(crate) const PI_SKILL_SOURCE: &str = include_str!("../../../../plugin/pi/skill/SKILL.md");

/// Render the Pi Core component inventory with the resolved binary path and
/// crate version, byte-identical to what `activate` deploys so the catalog
/// transaction and the activation cannot disagree.
pub(crate) fn rendered_plugin_files(tracedecay_bin: &str) -> Result<Vec<(&'static str, String)>> {
    Ok(vec![
        (
            PI_EXTENSION_RELATIVE,
            rendered_extension_source(tracedecay_bin)?,
        ),
        (PI_LIB_RELATIVE, PI_LIB_SOURCE.to_owned()),
        (PI_PACKAGE_RELATIVE, rendered_package_json()?),
        (PI_SCHEMAS_RELATIVE, rendered_schemas_json()?),
    ])
}

/// The agent directory for this home. Deliberately home-relative only: the
/// receipt-owned catalog artifacts are pinned to these paths, so an ambient
/// `PI_CODING_AGENT_DIR` override would split the receipts from the files Pi
/// loads (see the module documentation).
fn pi_agent_dir(home: &Path) -> PathBuf {
    home.join(".pi").join("agent")
}

fn pi_extension_path(home: &Path) -> PathBuf {
    pi_agent_dir(home).join(PI_EXTENSION_RELATIVE)
}

fn pi_package_path(home: &Path) -> PathBuf {
    pi_agent_dir(home).join(PI_PACKAGE_RELATIVE)
}

fn pi_lib_path(home: &Path) -> PathBuf {
    pi_agent_dir(home).join(PI_LIB_RELATIVE)
}

fn pi_schemas_path(home: &Path) -> PathBuf {
    pi_agent_dir(home).join(PI_SCHEMAS_RELATIVE)
}

/// Render the generated tool catalog for the extension, the same authority
/// Hermes renders into its plugin: name, description, JSON-Schema parameters,
/// and the read-only annotation the passthrough gate consumes.
fn rendered_schemas_json() -> Result<String> {
    let defs = crate::ports::mcp_tools::advertised_tools()?
        .into_iter()
        .map(|tool| {
            serde_json::json!({
                "name": tool.name,
                "description": tool.description,
                "parameters": tool.input_schema,
                "read_only": tool.read_only,
            })
        })
        .collect::<Vec<_>>();
    serde_json::to_string_pretty(&defs)
        .map(|json| format!("{json}\n"))
        .map_err(|error| TraceDecayError::Config {
            message: format!("failed to serialize Pi schemas.json: {error}"),
        })
}

fn pi_skill_path(home: &Path) -> PathBuf {
    pi_agent_dir(home).join(PI_SKILL_RELATIVE)
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

impl AgentIntegration for PiIntegration {
    fn name(&self) -> &'static str {
        "Pi"
    }

    fn id(&self) -> &'static str {
        "pi"
    }

    fn healthcheck(&self, dc: &mut DoctorCounters, ctx: &HealthcheckContext) {
        eprintln!("\n\x1b[1mPi integration\x1b[0m");
        let extension_path = pi_extension_path(&ctx.home);
        let package_path = pi_package_path(&ctx.home);
        let lib_path = pi_lib_path(&ctx.home);
        let schemas_path = pi_schemas_path(&ctx.home);
        let skill_path = pi_skill_path(&ctx.home);
        let extension_current = std::fs::read_to_string(&extension_path)
            .is_ok_and(|contents| contents.contains(PI_EXTENSION_MARKER));
        let package_current = package_path.is_file();
        let lib_current = lib_path.is_file();
        let schemas_current = std::fs::read_to_string(&schemas_path)
            .is_ok_and(|contents| contents.contains("\"read_only\""));
        let skill_current = std::fs::read_to_string(&skill_path)
            .is_ok_and(|contents| contents.contains("tracedecay_"));
        if extension_current && package_current && lib_current && schemas_current {
            dc.pass(&format!(
                "TraceDecay extension deployed at {}",
                extension_path.display()
            ));
        } else if extension_path.exists()
            || package_path.exists()
            || lib_path.exists()
            || schemas_path.exists()
        {
            dc.fail(&format!(
                "TraceDecay Pi extension is incomplete at {}; run `tracedecay install --agent pi`",
                pi_agent_dir(&ctx.home).display()
            ));
        } else {
            dc.warn(&format!(
                "{} not found, run `tracedecay install --agent pi` if you use Pi",
                extension_path.display()
            ));
        }
        if skill_current {
            dc.pass(&format!(
                "TraceDecay routing skill deployed at {}",
                skill_path.display()
            ));
        } else {
            dc.warn(&format!(
                "{} not found, run `tracedecay install --agent pi` if you use Pi",
                skill_path.display()
            ));
        }
    }

    fn host_component_registration(
        &self,
        component: HostComponentV1,
        ctx: &HealthcheckContext,
    ) -> HostBundleRegistrationStateV1 {
        use HostBundleRegistrationStateV1 as State;

        let extension_path = pi_extension_path(&ctx.home);
        let package_path = pi_package_path(&ctx.home);
        let lib_path = pi_lib_path(&ctx.home);
        let schemas_path = pi_schemas_path(&ctx.home);
        let skill_path = pi_skill_path(&ctx.home);
        let extension_current = std::fs::read_to_string(&extension_path)
            .is_ok_and(|contents| contents.contains(PI_EXTENSION_MARKER));
        let package_current = package_path.is_file();
        let lib_current = lib_path.is_file();
        let schemas_current = schemas_path.is_file();
        let skill_current = std::fs::read_to_string(&skill_path)
            .is_ok_and(|contents| contents.contains("tracedecay_"));

        match component {
            HostComponentV1::Core => {
                if extension_current && package_current && lib_current && schemas_current {
                    State::Current
                } else if extension_path.exists()
                    || package_path.exists()
                    || lib_path.exists()
                    || schemas_path.exists()
                {
                    State::Repairable
                } else {
                    State::Missing
                }
            }
            HostComponentV1::Agent => {
                if skill_current {
                    State::Current
                } else if skill_path.exists() {
                    State::Repairable
                } else {
                    State::Missing
                }
            }
            HostComponentV1::ContextMcp | HostComponentV1::OperatorMcp => State::Missing,
        }
    }

    fn is_detected(&self, home: &Path) -> bool {
        pi_agent_dir(home).is_dir()
    }

    fn detected_host_surface(&self, home: &Path) -> Option<PathBuf> {
        pi_agent_dir(home).is_dir().then(|| pi_agent_dir(home))
    }

    fn has_tracedecay(&self, home: &Path) -> bool {
        pi_extension_path(home).exists()
            || pi_package_path(home).exists()
            || pi_skill_path(home).exists()
    }

    fn primary_config_path(&self, home: &Path) -> Option<PathBuf> {
        Some(pi_extension_path(home))
    }

    fn host_registration_paths(&self, home: &Path) -> Vec<PathBuf> {
        vec![
            pi_extension_path(home),
            pi_lib_path(home),
            pi_package_path(home),
            pi_schemas_path(home),
            pi_skill_path(home),
        ]
    }

    fn host_component_registration_paths(
        &self,
        components: &[HostComponentV1],
        home: &Path,
    ) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        if components.contains(&HostComponentV1::Core) {
            paths.push(pi_extension_path(home));
            paths.push(pi_lib_path(home));
            paths.push(pi_package_path(home));
            paths.push(pi_schemas_path(home));
        }
        if components.contains(&HostComponentV1::Agent) {
            paths.push(pi_skill_path(home));
        }
        paths
    }

    fn activate_deployed_host_component_registration(
        &self,
        components: &[HostComponentV1],
        ctx: &InstallContext,
    ) -> Result<()> {
        if components.contains(&HostComponentV1::Core) {
            let agent_dir = pi_agent_dir(&ctx.home);
            let extension_dir = agent_dir.join("extensions").join("tracedecay");
            std::fs::create_dir_all(&extension_dir).map_err(|error| TraceDecayError::Config {
                message: format!(
                    "failed to create Pi extension directory {}: {error}",
                    extension_dir.display()
                ),
            })?;
            safe_write_text_file(
                &pi_extension_path(&ctx.home),
                &rendered_extension_source(&ctx.tracedecay_bin)?,
            )?;
            safe_write_text_file(&pi_lib_path(&ctx.home), PI_LIB_SOURCE)?;
            safe_write_text_file(&pi_package_path(&ctx.home), &rendered_package_json()?)?;
            safe_write_text_file(&pi_schemas_path(&ctx.home), &rendered_schemas_json()?)?;
        }
        if components.contains(&HostComponentV1::Agent) {
            let skill_dir = pi_agent_dir(&ctx.home)
                .join("skills")
                .join("tracedecay-cli");
            std::fs::create_dir_all(&skill_dir).map_err(|error| TraceDecayError::Config {
                message: format!(
                    "failed to create Pi skill directory {}: {error}",
                    skill_dir.display()
                ),
            })?;
            safe_write_text_file(&pi_skill_path(&ctx.home), PI_SKILL_SOURCE)?;
        }
        Ok(())
    }

    fn deactivate_deployed_host_component_registration(
        &self,
        components: &[HostComponentV1],
        ctx: &InstallContext,
    ) -> Result<()> {
        if components.contains(&HostComponentV1::Core) {
            remove_owned_file(&pi_extension_path(&ctx.home), PI_EXTENSION_MARKER)?;
            remove_owned_file(&pi_lib_path(&ctx.home), "resolvePassthroughTool")?;
            remove_owned_file(&pi_package_path(&ctx.home), "tracedecay-pi-extension")?;
            remove_owned_file(&pi_schemas_path(&ctx.home), "\"read_only\"")?;
            let extension_dir = pi_agent_dir(&ctx.home)
                .join("extensions")
                .join("tracedecay");
            if extension_dir.exists()
                && std::fs::read_dir(&extension_dir)
                    .is_ok_and(|mut entries| entries.next().is_none())
            {
                std::fs::remove_dir(&extension_dir).map_err(|error| TraceDecayError::Config {
                    message: format!(
                        "failed to remove empty Pi extension directory {}: {error}",
                        extension_dir.display()
                    ),
                })?;
            }
        }
        if components.contains(&HostComponentV1::Agent) {
            remove_owned_file(&pi_skill_path(&ctx.home), "tracedecay_")?;
            let skill_dir = pi_agent_dir(&ctx.home)
                .join("skills")
                .join("tracedecay-cli");
            if skill_dir.exists()
                && std::fs::read_dir(&skill_dir).is_ok_and(|mut entries| entries.next().is_none())
            {
                std::fs::remove_dir(&skill_dir).map_err(|error| TraceDecayError::Config {
                    message: format!(
                        "failed to remove empty Pi skill directory {}: {error}",
                        skill_dir.display()
                    ),
                })?;
            }
        }
        Ok(())
    }
}

/// Remove a TraceDecay-owned file after verifying its ownership marker.
/// A file whose bytes were edited by the operator still carries the marker and
/// is removed; a foreign file at the deploy path is refused untouched.
fn remove_owned_file(path: &Path, marker: &str) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let contents = std::fs::read_to_string(path).map_err(|error| TraceDecayError::Config {
        message: format!("failed to read {}: {error}", path.display()),
    })?;
    if !contents.contains(marker) {
        return Err(TraceDecayError::Config {
            message: format!(
                "refusing to remove non-TraceDecay Pi artifact {}",
                path.display()
            ),
        });
    }
    safe_remove_host_file(path).map_err(|error| TraceDecayError::Config {
        message: format!("failed to remove {}: {error}", path.display()),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn install_context(home: &Path) -> InstallContext {
        InstallContext {
            home: home.to_path_buf(),
            tracedecay_bin: "/usr/local/bin/tracedecay".to_owned(),
            project_root: None,
            dashboard: true,
        }
    }

    #[test]
    fn core_and_agent_components_deploy_the_full_pi_surface() {
        let home = tempfile::tempdir().unwrap();
        let ctx = install_context(home.path());
        let integration = PiIntegration;

        integration
            .activate_deployed_host_component_registration(
                &[HostComponentV1::Core, HostComponentV1::Agent],
                &ctx,
            )
            .unwrap();

        let rendered = std::fs::read_to_string(pi_extension_path(home.path())).unwrap();
        assert!(rendered.contains(PI_EXTENSION_MARKER));
        assert!(rendered.contains("\"/usr/local/bin/tracedecay\""));
        assert!(!rendered.contains(super::super::plugin_bundle::TRACEDECAY_BIN_PLACEHOLDER));

        let package = std::fs::read_to_string(pi_package_path(home.path())).unwrap();
        assert!(package.contains(crate::PRODUCT_VERSION));
        assert!(!package.contains(PI_VERSION_PLACEHOLDER));

        let skill = std::fs::read_to_string(pi_skill_path(home.path())).unwrap();
        assert!(skill.contains("tracedecay_context"));

        let health = HealthcheckContext {
            home: home.path().to_path_buf(),
            project_path: home.path().to_path_buf(),
        };
        assert_eq!(
            integration.host_component_registration(HostComponentV1::Core, &health),
            HostBundleRegistrationStateV1::Current
        );
        assert_eq!(
            integration.host_component_registration(HostComponentV1::Agent, &health),
            HostBundleRegistrationStateV1::Current
        );
        assert_eq!(
            integration.host_component_registration(HostComponentV1::ContextMcp, &health),
            HostBundleRegistrationStateV1::Missing
        );
    }

    #[test]
    fn deactivate_removes_owned_files_and_refuses_foreign_bytes() {
        let home = tempfile::tempdir().unwrap();
        let ctx = install_context(home.path());
        let integration = PiIntegration;
        let components = [HostComponentV1::Core, HostComponentV1::Agent];

        integration
            .activate_deployed_host_component_registration(&components, &ctx)
            .unwrap();

        // A foreign file at the deploy path is refused, not deleted. The
        // installed artifact is read-only, so replace it after removing it.
        std::fs::remove_file(pi_skill_path(home.path())).unwrap();
        std::fs::write(pi_skill_path(home.path()), "operator-owned\n").unwrap();
        let refused = integration
            .deactivate_deployed_host_component_registration(&[HostComponentV1::Agent], &ctx);
        assert!(refused.is_err());
        assert!(pi_skill_path(home.path()).exists());

        // Owned files are removed together with their emptied directories.
        integration
            .deactivate_deployed_host_component_registration(&[HostComponentV1::Core], &ctx)
            .unwrap();
        assert!(!pi_extension_path(home.path()).exists());
        assert!(!pi_package_path(home.path()).exists());
        assert!(!pi_schemas_path(home.path()).exists());
        assert!(
            !pi_agent_dir(home.path())
                .join("extensions")
                .join("tracedecay")
                .exists()
        );
    }

    #[test]
    fn schemas_json_is_rendered_from_the_catalog_with_read_only_annotations() {
        let home = tempfile::tempdir().unwrap();
        let ctx = install_context(home.path());
        let integration = PiIntegration;

        integration
            .activate_deployed_host_component_registration(&[HostComponentV1::Core], &ctx)
            .unwrap();

        let schemas = std::fs::read_to_string(pi_schemas_path(home.path())).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&schemas).unwrap();
        let entries = parsed.as_array().expect("schemas.json is an array");
        assert!(entries.len() > 100, "the full catalog is rendered");
        let find_exact = entries
            .iter()
            .find(|entry| entry["name"] == "tracedecay_search")
            .expect("tracedecay_search is in the catalog");
        assert!(find_exact["parameters"].get("required").is_some());
        assert_eq!(find_exact["read_only"], true);
    }

    #[test]
    fn the_agent_directory_is_always_home_relative() {
        // A foreign home never escapes to an ambient agent directory: the
        // receipt-backed catalog artifacts are pinned to the home-relative
        // paths, so relocation must not exist until it is receipt-backed.
        let home = tempfile::tempdir().unwrap();
        assert_eq!(pi_agent_dir(home.path()), home.path().join(".pi/agent"));
        assert!(pi_extension_path(home.path()).starts_with(home.path()));
        assert!(pi_schemas_path(home.path()).starts_with(home.path()));
        assert!(pi_skill_path(home.path()).starts_with(home.path()));
    }
}
