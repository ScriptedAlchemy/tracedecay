//! Shared MCP server registration for JSON/JSONC-configured hosts.
//!
//! Every such host registers tracedecay the same way: one entry named
//! `tracedecay` under a root key (`mcpServers` for the Cline family, Devin,
//! Kimi, Kiro and Antigravity, `context_servers` for Zed, `mcp` for Kilo).
//! Only the config path, the root key, the entry
//! shape, and the config dialect differ, so install, uninstall, and the doctor
//! check live here rather than once per host, alongside the advertised MCP
//! tool allowlists hosts embed into their permission config.

use std::path::Path;

use tracedecay_domain::errors::{Result, TraceDecayError};

use super::DoctorCounters;
use super::host_bundle;
use super::host_config_io::{
    JsonConfigDialect, JsonConfigMutation, update_json_config_transactionally,
};
use crate::ports::mcp_tools::advertised_tools;

/// Register the tracedecay MCP entry under `root_key` in a host config.
///
/// The config is read, transformed, and published under the host-file write
/// lock; a config that exists but cannot be parsed for `dialect` is a typed
/// error rather than a silent overwrite. `agent_label` names the host in the
/// directory-creation error.
#[hotpath::measure(label = "agent_hosts.agents.mcp.install")]
pub fn install_mcp_server_entry(
    config_path: &Path,
    root_key: &str,
    entry: serde_json::Value,
    agent_label: &str,
    dialect: JsonConfigDialect,
) -> Result<()> {
    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| TraceDecayError::Config {
            message: format!(
                "cannot create {agent_label} config directory {}: {error}",
                parent.display()
            ),
        })?;
    }

    let outcome = update_json_config_transactionally(config_path, dialect, |mut settings| {
        let outcome = set_mcp_server_entry(config_path, &mut settings, root_key, entry)?;
        if outcome == McpRegistrationOutcome::Unchanged {
            return Ok((outcome, JsonConfigMutation::Unchanged));
        }
        Ok((outcome, JsonConfigMutation::Write(settings)))
    })?;
    outcome.report(config_path);
    Ok(())
}

/// Set the tracedecay entry under `root_key` in a config parsed from
/// `config_path`, refusing a document or root key that is not an object.
pub(crate) fn set_mcp_server_entry(
    config_path: &Path,
    settings: &mut serde_json::Value,
    root_key: &str,
    entry: serde_json::Value,
) -> Result<McpRegistrationOutcome> {
    if !settings.is_object() {
        return Err(TraceDecayError::Config {
            message: format!("{} must contain a JSON object", config_path.display()),
        });
    }
    if settings
        .get(root_key)
        .is_some_and(|value| !value.is_object())
    {
        return Err(TraceDecayError::Config {
            message: format!("{}.{root_key} must be a JSON object", config_path.display()),
        });
    }
    let outcome = McpRegistrationOutcome::between(
        settings
            .get(root_key)
            .and_then(|servers| servers.get("tracedecay")),
        &entry,
    );
    if outcome != McpRegistrationOutcome::Unchanged {
        settings[root_key]["tracedecay"] = entry;
    }
    Ok(outcome)
}

/// Remove the tracedecay entry under `root_key`; true when one was there.
pub(crate) fn remove_mcp_server_entry(settings: &mut serde_json::Value, root_key: &str) -> bool {
    settings
        .get_mut(root_key)
        .and_then(|servers| servers.as_object_mut())
        .is_some_and(|servers| servers.remove("tracedecay").is_some())
}

/// What registering tracedecay did to a host config, reported as it happened
/// so a refresh that finds its entry in place never claims to have added it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum McpRegistrationOutcome {
    Added,
    Updated,
    Unchanged,
}

impl McpRegistrationOutcome {
    /// The outcome of replacing the registration `previous` with `next`.
    pub(crate) fn between(previous: Option<&serde_json::Value>, next: &serde_json::Value) -> Self {
        match previous {
            None => Self::Added,
            Some(previous) if previous == next => Self::Unchanged,
            Some(_) => Self::Updated,
        }
    }

    pub(crate) fn report(self, config_path: &Path) {
        let path = config_path.display();
        match self {
            Self::Added => eprintln!("\x1b[32m✔\x1b[0m Added tracedecay MCP server to {path}"),
            Self::Updated => eprintln!("\x1b[32m✔\x1b[0m Updated tracedecay MCP server in {path}"),
            Self::Unchanged => eprintln!("  tracedecay MCP server unchanged in {path}"),
        }
    }
}

/// Remove the tracedecay MCP entry under `root_key` from a host config.
///
/// Runs under the host-file write lock like [`install_mcp_server_entry`]. A
/// config that exists but cannot be parsed is a typed error, reporting a
/// clean uninstall over a corrupt config would fabricate state, and callers
/// decide whether to keep going across the remaining hosts. Only the entry is
/// removed here; the root key and the file go too exactly when the install
/// this lifecycle records created them.
#[hotpath::measure(label = "agent_hosts.agents.mcp.uninstall")]
pub fn uninstall_mcp_server_entry(
    config_path: &Path,
    root_key: &str,
    dialect: JsonConfigDialect,
) -> Result<()> {
    if !config_path.exists() {
        eprintln!("  {} not found, skipping", config_path.display());
        return Ok(());
    }

    let removed = update_json_config_transactionally(config_path, dialect, |mut settings| {
        let removed = remove_mcp_server_entry(&mut settings, root_key);
        if removed {
            Ok((true, JsonConfigMutation::Write(settings)))
        } else {
            Ok((false, JsonConfigMutation::Unchanged))
        }
    })?;
    if removed {
        eprintln!(
            "\x1b[32m✔\x1b[0m Removed tracedecay MCP server from {}",
            config_path.display()
        );
    } else {
        eprintln!(
            "  No tracedecay MCP server in {}, skipping",
            config_path.display()
        );
    }
    Ok(())
}

/// Bundle registration state for hosts that store the tracedecay MCP server
/// under a `mcpServers.tracedecay` object with `disabled` and `args` fields.
///
/// A registration only counts as current when it is explicitly enabled and
/// still launches `tracedecay serve`.
pub fn mcp_servers_registration_state(
    settings_path: &Path,
) -> host_bundle::HostBundleRegistrationStateV1 {
    use host_bundle::HostBundleRegistrationStateV1 as State;

    let Ok(bytes) = std::fs::read(settings_path) else {
        return State::Missing;
    };
    let Ok(settings) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return State::Corrupt;
    };
    if settings
        .pointer("/mcpServers/tracedecay/disabled")
        .and_then(serde_json::Value::as_bool)
        == Some(false)
        && settings
            .pointer("/mcpServers/tracedecay/args")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|args| args.iter().any(|arg| arg.as_str() == Some("serve")))
    {
        State::Current
    } else {
        State::Missing
    }
}

/// Host-specific wording for [`doctor_check_mcp_registration`].
pub struct McpDoctorLabels<'a> {
    /// Agent id used in the ``run `tracedecay install --agent <id>` `` hint.
    pub agent_id: &'a str,
    /// Product name used in the "if you use ..." warning for a missing file.
    pub product: &'a str,
    /// Subject of the pass line, rendered as "{registered} in {path}".
    pub registered: &'a str,
    /// Subject of the fail line, rendered as "{missing} in {path}, run ...".
    pub missing: &'a str,
}

/// Look up the tracedecay MCP server entry a host config stores under
/// `root_key`, without judging its shape.
///
/// Hosts that only need a yes/no answer (or that accept non-object entries)
/// can call this directly; [`doctor_check_mcp_registration`] layers the
/// object-shape filter and doctor reporting on top.
pub fn mcp_registration_entry(
    config_path: &Path,
    root_key: &str,
    load: fn(&Path) -> serde_json::Value,
) -> Option<serde_json::Value> {
    load(config_path)
        .get(root_key)
        .and_then(|servers| servers.get("tracedecay"))
        .cloned()
}

/// True when `config_path` already names a tracedecay entry under `root_key`.
///
/// Missing or unreadable files are `false` because `load` is the host's
/// lenient loader. Shared by project-local Claude/Kimi checks and the
/// host `has_tracedecay` implementations that only need a yes/no.
pub fn mcp_config_has_tracedecay(
    config_path: &Path,
    root_key: &str,
    load: fn(&Path) -> serde_json::Value,
) -> bool {
    mcp_registration_entry(config_path, root_key, load).is_some()
}

/// Emit the standard doctor pass/fail line for a host MCP registration.
///
/// Split out from [`doctor_check_mcp_registration`] so hosts with their own
/// missing-file control flow or registration-state readers still share the
/// wording.
pub fn report_mcp_registration(
    dc: &mut DoctorCounters,
    config_path: &Path,
    registered: bool,
    labels: &McpDoctorLabels<'_>,
) {
    if registered {
        dc.pass(&format!(
            "{} in {}",
            labels.registered,
            config_path.display()
        ));
    } else {
        dc.fail(&format!(
            "{} in {}, run `tracedecay install --agent {}`",
            labels.missing,
            config_path.display(),
            labels.agent_id
        ));
    }
}

/// Report whether a host config registers tracedecay under `root_key`.
///
/// Returns the registered server object so callers can keep checking
/// host-specific fields (Gemini's `args`/`trust`, for example).
pub fn doctor_check_mcp_registration(
    dc: &mut DoctorCounters,
    config_path: &Path,
    root_key: &str,
    load: fn(&Path) -> serde_json::Value,
    labels: &McpDoctorLabels<'_>,
) -> Option<serde_json::Value> {
    if !config_path.exists() {
        dc.warn(&format!(
            "{} not found, run `tracedecay install --agent {}` if you use {}",
            config_path.display(),
            labels.agent_id,
            labels.product
        ));
        return None;
    }

    let server =
        mcp_registration_entry(config_path, root_key, load).filter(serde_json::Value::is_object);
    report_mcp_registration(dc, config_path, server.is_some(), labels);
    server
}

/// Shared doctor for host prompt files that must contain the word `tracedecay`.
///
/// Vibe (`prompts/cli.md`) and OpenCode (`AGENTS.md`) use the same
/// exists → contains → pass/fail/warn shape.
pub(crate) fn doctor_check_prompt_contains_tracedecay(
    dc: &mut DoctorCounters,
    prompt_path: &Path,
    subject: &str,
    agent_id: &str,
) {
    if !prompt_path.exists() {
        dc.warn(&format!("{subject} does not exist"));
        return;
    }
    let has_rules = std::fs::read_to_string(prompt_path)
        .unwrap_or_default()
        .contains("tracedecay");
    if has_rules {
        dc.pass(&format!("{subject} contains tracedecay rules"));
    } else {
        dc.fail(&format!(
            "{subject} missing tracedecay rules, run `tracedecay install --agent {agent_id}`"
        ));
    }
}

/// Every advertised tool name. Errors when the catalog is unavailable, so no
/// caller can mistake a broken catalog read for "this host advertises nothing".
pub fn tool_names() -> tracedecay_domain::errors::Result<Vec<String>> {
    Ok(advertised_tools()?
        .into_iter()
        .map(|tool| tool.name)
        .collect())
}

/// The read-only subset of [`tool_names`].
pub fn read_only_tool_names() -> tracedecay_domain::errors::Result<Vec<String>> {
    Ok(advertised_tools()?
        .into_iter()
        .filter(|tool| tool.read_only)
        .map(|tool| tool.name)
        .collect())
}
