// Rust guideline compliant 2026-08-08
//! Where OpenCode's own plugin CLI owns TraceDecay's plugin — and where it
//! does not.
//!
//! Every other host in this crate that owns its plugin lifecycle gets that
//! lifecycle driven through the host's own command (`claude plugin …`,
//! `kiro-cli mcp …`). This module records why OpenCode is the exception, and
//! encodes the parts of that decision that must not rot silently.
//!
//! # What `opencode plugin` actually is
//!
//! `opencode plugin <module>` (alias `plug`, flags `-g/--global`,
//! `-f/--force`) is a *package* installer, verified against the shipped host
//! binary (opencode 1.18.4). Its behavior:
//!
//! 1. It classifies the spec. A spec that starts with `file://` or `.`, or is
//!    absolute, is a **path** spec; anything else is an **npm** spec that the
//!    host installs from the registry. A path spec is resolved to a `file://`
//!    URL, so a local directory is a first-class spelling and TraceDecay would
//!    never have to name a package it does not publish.
//! 2. It reads `package.json` from the resolved target and requires a plugin
//!    entrypoint (`exports["./server"]`, `exports["./tui"]`, or `main`).
//!    A bare `.ts` file with no sibling manifest is rejected.
//! 3. On success it **appends the spec to the `plugin` array** of the
//!    scope's `opencode.json` (`--global` -> the profile config dir,
//!    otherwise `<worktree>/.opencode/opencode.json`). That array is the
//!    entire persistent effect of the command.
//!
//! # Why TraceDecay does not drive it
//!
//! OpenCode's config loader scans **`{plugin,plugins}/*.{ts,js}` in every
//! config directory** it resolves, and those directories are the profile
//! config dir plus each `.opencode` dir found walking up from the project.
//! TraceDecay's deployed `plugins/tracedecay.ts` is therefore loaded by
//! OpenCode's *own* discovery contract, with no registration step at all. The
//! file deployment is not emulation of host-private state — it is the host's
//! documented directory contract, which is precisely the condition under which
//! the host-capability doctrine does **not** demand CLI adoption.
//!
//! Driving `opencode plugin` on top of that would be actively harmful:
//!
//! * **It would double-load the plugin.** The host de-duplicates plugin
//!   origins by resolved `file://` URL. A staged module directory
//!   (`…/tracedecay/index.ts`, needed for the `package.json` entrypoint the
//!   command requires) resolves to a *different* URL than the discovered
//!   `…/plugins/tracedecay.ts`, so both would load and every hook event would
//!   be dispatched to the tracedecay binary twice.
//! * **There is no removal counterpart.** OpenCode ships `plugin` only; there
//!   is no `plugin remove`/`uninstall` subcommand. An adopted install would
//!   leave TraceDecay editing the host-recorded `plugin` array by hand on
//!   uninstall — strictly more emulation than today, not less.
//! * **The project-local scope cannot be targeted anyway.** `opencode plugin`
//!   without `--global` resolves its scope from the process working directory,
//!   and [`super::super::host_cli::run_host_cli`] admits the profile home as
//!   the child working directory. This is the same blocker documented for
//!   Kiro's `--scope workspace`, and it applies here unchanged.
//!
//! So the plugin stays TraceDecay-deployed and `plugin` stays host-owned. The
//! invariants below make both halves of that boundary executable: the deployed
//! path must remain one the host's own loader discovers, and TraceDecay's
//! `opencode.json` merge must never write the key `opencode plugin` owns.
//!
//! # The rest of the OpenCode integration
//!
//! * `opencode mcp add` exists but is an interactive wizard with no
//!   documented non-interactive flags, so the MCP merge stays
//!   TraceDecay-written. See the doc comment on
//!   [`super::install_registration_entries`].
//! * The custom LSP registration has no CLI at all.
//! * The `AGENTS.md` prompt rules have no CLI at all.

use std::path::Path;

use crate::errors::{Result, TraceDecayError};

/// The `opencode.json` key `opencode plugin` writes, and the only key in that
/// file this integration treats as host-recorded rather than TraceDecay-owned.
pub(super) const HOST_OWNED_PLUGIN_KEY: &str = "plugin";

/// Directory names OpenCode's own loader scans for plugin files, in each
/// config directory it resolves. Both spellings are accepted by the host; the
/// deployment uses `plugins`.
const HOST_PLUGIN_DISCOVERY_DIRS: &[&str] = &["plugin", "plugins"];

/// File extensions OpenCode's own loader accepts in those directories.
const HOST_PLUGIN_DISCOVERY_EXTENSIONS: &[&str] = &["ts", "js"];

/// True when `path` is a location OpenCode's own loader discovers without any
/// registration: a `*.ts`/`*.js` file directly inside a `plugin`/`plugins`
/// directory of a config root.
///
/// The host's glob is exactly one level deep, so a file nested in a
/// sub-directory (a staged *module*, which is what `opencode plugin` would
/// need) is deliberately **not** discovered — that asymmetry is the whole
/// reason the CLI is not adopted here, and the tests below pin it.
pub(super) fn is_host_discovered_plugin_path(path: &Path) -> bool {
    let discovered_extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| HOST_PLUGIN_DISCOVERY_EXTENSIONS.contains(&extension));
    let discovered_directory = path
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .is_some_and(|name| HOST_PLUGIN_DISCOVERY_DIRS.contains(&name));
    discovered_extension && discovered_directory
}

/// Snapshot the host-recorded plugin registration in a parsed `opencode.json`.
///
/// Read-only: the value belongs to `opencode plugin`, and TraceDecay's only
/// legitimate interest in it is proving it survived a TraceDecay write.
pub(super) fn host_owned_plugin_registration(
    config: &serde_json::Value,
) -> Option<serde_json::Value> {
    config.get(HOST_OWNED_PLUGIN_KEY).cloned()
}

/// Refuse a TraceDecay write to `opencode.json` that would create, alter, or
/// drop the host-recorded `plugin` registration.
///
/// TraceDecay merges `mcp` and `lsp` into this file because neither has a
/// non-interactive host command. `plugin` is different: it has one, TraceDecay
/// deliberately does not drive it, and therefore TraceDecay must not write its
/// effect either. Emulating the key would be indistinguishable on disk from a
/// real `opencode plugin` install while carrying none of the host's own
/// manifest and engine validation — exactly the half-emulated state the
/// host-capability doctrine forbids. A guard is cheaper than the incident.
pub(super) fn ensure_host_owned_plugin_registration_untouched(
    before: Option<&serde_json::Value>,
    config: &serde_json::Value,
    config_path: &Path,
) -> Result<()> {
    let after = config.get(HOST_OWNED_PLUGIN_KEY);
    if before == after {
        return Ok(());
    }
    Err(TraceDecayError::Config {
        message: format!(
            "refusing to change `{HOST_OWNED_PLUGIN_KEY}` in {}: that registration belongs to \
             `opencode plugin`, which TraceDecay does not drive (the deployed plugin is loaded \
             by OpenCode's own `{{plugin,plugins}}/*.{{ts,js}}` discovery instead)",
            config_path.display()
        ),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    use serde_json::json;

    /// The global deployment path must stay one OpenCode discovers on its own.
    /// If it ever moves out of `plugins/` or stops being a `.ts` file, the
    /// host silently stops loading the plugin and nothing else in this crate
    /// would notice — the config would still validate.
    #[test]
    fn the_deployed_global_plugin_path_is_discovered_by_the_hosts_own_loader() {
        let deployed = Path::new(super::super::OPENCODE_PLUGIN_RELATIVE);

        assert!(
            is_host_discovered_plugin_path(deployed),
            "{} is not a path OpenCode's own loader scans",
            deployed.display()
        );
    }

    /// The project-local deployment path carries the same requirement.
    #[test]
    fn the_project_local_plugin_path_is_discovered_by_the_hosts_own_loader() {
        assert!(is_host_discovered_plugin_path(Path::new(
            ".opencode/plugins/tracedecay.ts"
        )));
    }

    /// The executable form of the adoption decision: a staged *module*
    /// directory — the only shape `opencode plugin` accepts, because it needs
    /// a `package.json` entrypoint — is NOT discovered by the host's own
    /// loader. Driving the CLI would therefore add a second, distinct plugin
    /// origin next to the discovered file rather than replacing it.
    #[test]
    fn a_staged_module_directory_would_not_be_discovered_and_so_would_double_register() {
        assert!(
            !is_host_discovered_plugin_path(Path::new("plugins/tracedecay/index.ts")),
            "a nested module entrypoint must not be mistaken for a discovered plugin file"
        );
        assert!(!is_host_discovered_plugin_path(Path::new(
            "tracedecay/plugin-module/index.ts"
        )));
        assert!(
            !is_host_discovered_plugin_path(Path::new("plugins/tracedecay.json")),
            "only .ts/.js files are discovered"
        );
    }

    /// TraceDecay's own registration merge must leave the key `opencode
    /// plugin` writes exactly as the host left it.
    #[test]
    fn the_registration_merge_preserves_a_host_written_plugin_registration() {
        let home = tempfile::tempdir().unwrap();
        let config_path = home.path().join("opencode.json");
        let host_written = json!(["some-operator-plugin@1.2.3"]);
        std::fs::write(
            &config_path,
            serde_json::to_vec_pretty(&json!({ "plugin": host_written })).unwrap(),
        )
        .unwrap();

        super::super::install_registration_entries(
            &config_path,
            "/usr/bin/tracedecay",
            true,
            true,
            true,
        )
        .unwrap();

        let config = super::super::load_json_file_strict(&config_path).unwrap();
        assert_eq!(config["plugin"], host_written);
        assert!(config.pointer("/mcp/tracedecay").is_some());

        super::super::remove_registration_entries(&config_path, true, true, true).unwrap();

        let config = super::super::load_json_file_strict(&config_path).unwrap();
        assert_eq!(
            config["plugin"], host_written,
            "uninstall must not disturb the host-recorded plugin registration"
        );
    }

    /// And it must not invent one where the host recorded none: writing
    /// `plugin` ourselves would forge an `opencode plugin` install.
    #[test]
    fn the_registration_merge_never_writes_the_host_owned_plugin_key() {
        let home = tempfile::tempdir().unwrap();
        let config_path = home.path().join("opencode.json");

        super::super::install_registration_entries(
            &config_path,
            "/usr/bin/tracedecay",
            true,
            true,
            true,
        )
        .unwrap();

        let config = super::super::load_json_file_strict(&config_path).unwrap();
        assert!(
            config.get(HOST_OWNED_PLUGIN_KEY).is_none(),
            "TraceDecay must leave the plugin registration to `opencode plugin`"
        );
    }

    /// The guard itself refuses rather than writing, and names the host
    /// command that owns the key.
    #[test]
    fn a_write_that_would_forge_the_host_plugin_registration_is_refused() {
        let before = json!(["operator-plugin"]);
        let forged = json!({ "plugin": ["operator-plugin", "tracedecay"] });

        let error = ensure_host_owned_plugin_registration_untouched(
            Some(&before),
            &forged,
            Path::new("/home/example/.config/opencode/opencode.json"),
        )
        .expect_err("a forged plugin registration must refuse, never write");

        let TraceDecayError::Config { message } = error else {
            panic!("the refusal must surface as a config error");
        };
        assert!(
            message.contains("`opencode plugin`"),
            "the refusal must name the host command that owns the key: {message}"
        );
    }

    /// A dropped registration is just as wrong as a forged one.
    #[test]
    fn a_write_that_would_drop_the_host_plugin_registration_is_refused() {
        let before = json!(["operator-plugin"]);

        assert!(
            ensure_host_owned_plugin_registration_untouched(
                Some(&before),
                &json!({ "mcp": {} }),
                Path::new("/home/example/.config/opencode/opencode.json"),
            )
            .is_err()
        );
    }

    /// An untouched key — the steady state — passes.
    #[test]
    fn an_untouched_plugin_registration_passes_the_guard() {
        let before = json!(["operator-plugin"]);
        let config = json!({ "plugin": ["operator-plugin"], "mcp": { "tracedecay": {} } });

        ensure_host_owned_plugin_registration_untouched(
            Some(&before),
            &config,
            Path::new("/home/example/.config/opencode/opencode.json"),
        )
        .expect("an unchanged host registration must pass");

        assert_eq!(host_owned_plugin_registration(&config), Some(before));
    }
}
