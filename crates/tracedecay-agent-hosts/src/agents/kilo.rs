//! Kilo CLI agent integration.
//!
//! Handles registration of the tracedecay MCP server in Kilo CLI config files.
//! Kilo uses the `mcp` key (not `mcpServers`) with entries having `type`,
//! `command` (as array), and `enabled` fields.
//!
//! ## Why this host is not driven through its own CLI (verified 2026-08-08)
//!
//! The owner policy is CLI-first: a host's own command is the preferred
//! install/uninstall mechanism and writing its config by hand is a fallback
//! that must be justified. Kilo's `kilo mcp` surface was probed against its
//! published reference and fails that bar on two independent counts:
//!
//! 1. **It cannot carry this registration.** `kilo mcp add` accepts only
//!    `--url`, `--env`, and `--header`; there is no `--command`/`--args` flag,
//!    so an arbitrary local stdio server — which is exactly what `tracedecay
//!    serve` is — has no representation on the command line. `--env` only
//!    decorates a server resolved some other way.
//! 2. **There is no removal counterpart.** The documented subcommands are
//!    `add | list | auth | logout | debug`. `logout` clears credentials, not a
//!    registration, so an adopted install could never be uninstalled through
//!    the same authority that created it.
//!
//! Adopting a command that can register a server we cannot describe and can
//! never remove would be strictly worse than the config write below, which is
//! ownership-marked, backed up, and reversible.
//!
//! Sources:
//! - <https://kilo.ai/docs/code-with-ai/platforms/cli-reference> (flag set)
//! - <https://kilo.ai/docs/automate/mcp/using-in-cli> (no remove/delete; the
//!   documented way to stop a server is `enabled: false` in the config file)
//! - <https://github.com/Kilo-Org/kilocode/issues/7079> (`kilo mcp add` writes
//!   the OpenCode-derived config; closed "not planned")
//!
//! Re-open this decision if Kilo publishes a `--command`/`--args` form *and* a
//! `mcp remove`; both are required, not either.

use std::path::Path;

use super::{
    AgentIntegration, DoctorCounters, HealthcheckContext, McpDoctorLabels,
    doctor_check_mcp_registration, load_jsonc_file,
};

/// Kilo CLI agent.
pub struct KiloIntegration;

fn kilo_config_dir(home: &Path) -> std::path::PathBuf {
    home.join(".config/kilo")
}

fn kilo_config_path(home: &Path) -> std::path::PathBuf {
    kilo_config_dir(home).join("kilo.jsonc")
}

impl AgentIntegration for KiloIntegration {
    fn name(&self) -> &'static str {
        "Kilo CLI"
    }

    fn id(&self) -> &'static str {
        "kilo"
    }

    fn supports_local_install(&self) -> bool {
        true
    }

    fn healthcheck(&self, dc: &mut DoctorCounters, ctx: &HealthcheckContext) {
        eprintln!("\n\x1b[1mKilo CLI integration\x1b[0m");
        doctor_check_settings(dc, &ctx.home);
    }

    fn host_component_registration(
        &self,
        _component: super::host_bundle_v2::HostBundleComponentV1,
        ctx: &HealthcheckContext,
    ) -> super::host_bundle_v2::HostBundleRegistrationStateV1 {
        use super::host_bundle_v2::HostBundleRegistrationStateV1 as State;

        let path = kilo_config_path(&ctx.home);
        let Ok(bytes) = std::fs::read_to_string(path) else {
            return State::Missing;
        };
        let settings = super::parse_jsonc(&bytes);
        if settings
            .pointer("/mcp/tracedecay/enabled")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
            && settings
                .pointer("/mcp/tracedecay/command")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|args| args.iter().any(|arg| arg.as_str() == Some("serve")))
        {
            State::Current
        } else {
            State::Missing
        }
    }

    fn is_detected(&self, home: &Path) -> bool {
        kilo_config_dir(home).is_dir()
    }

    fn primary_config_path(&self, home: &Path) -> Option<std::path::PathBuf> {
        Some(kilo_config_path(home))
    }

    fn has_tracedecay(&self, home: &Path) -> bool {
        let config_path = kilo_config_path(home);
        if !config_path.exists() {
            return false;
        }
        let json = load_jsonc_file(&config_path);
        let servers = json.get("mcp");
        servers.and_then(|v| v.get("tracedecay")).is_some()
    }
}

// ---------------------------------------------------------------------------
// Healthcheck helpers
// ---------------------------------------------------------------------------

fn doctor_check_settings(dc: &mut DoctorCounters, home: &Path) {
    doctor_check_mcp_registration(
        dc,
        &kilo_config_path(home),
        "mcp",
        load_jsonc_file,
        &McpDoctorLabels {
            agent_id: "kilo",
            product: "Kilo CLI",
            registered: "MCP server registered",
            missing: "MCP server NOT registered",
        },
    );
}
