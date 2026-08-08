// Rust guideline compliant 2026-08-08
//! Staging and host-CLI driving for the tracedecay Gemini CLI extension.
//!
//! Gemini CLI owns extension registration, enablement, and the installed copy
//! under `~/.gemini/extensions/<name>/` through `gemini extensions
//! install|uninstall|list|update`. TraceDecay therefore does exactly two
//! things here:
//!
//! 1. **Stage** a complete extension source directory it owns outright — the
//!    `gemini-extension.json` manifest (naming the tracedecay MCP server with
//!    `args: ["serve"]` and `trust: true`) plus the extension's own context
//!    file. The manifest carries the resolved tracedecay binary through the
//!    [`TRACEDECAY_BIN_PLACEHOLDER`] substitution rather than a hardcoded
//!    path, exactly as the Claude plugin bundle does.
//! 2. **Drive** the host's own commands against that staged directory.
//!
//! What it must never do is merge `~/.gemini/settings.json` or append to
//! `~/.gemini/GEMINI.md`. A Gemini extension natively bundles the MCP server
//! entry *and* the context file, so both of those writes are now the host's
//! job; emulating them alongside a CLI-driven install would produce two
//! tracedecay servers and two copies of the rules, and is precisely the
//! emulation the host-capability doctrine forbids.
//!
//! The staging directory is deliberately **not** `~/.gemini/extensions/
//! tracedecay`: that path is the host's install target, and staging into it
//! would mean TraceDecay hand-writing the very state `gemini extensions
//! install` claims to own.

use std::path::{Path, PathBuf};

use serde_json::json;

use crate::errors::{Result, TraceDecayError};

use crate::agents::{
    host_cli, load_json_file, record_host_config_observation_bytes, safe_write_text_file,
};

/// Name of Gemini CLI's lifecycle binary.
pub(super) const GEMINI_CLI: &str = "gemini";

/// What the binary is required *for*, used in the typed absence error.
pub(super) const GEMINI_CLI_LIFECYCLE: &str = "gemini extension lifecycle";

/// Name Gemini CLI selects the extension by (`gemini extensions uninstall
/// <name>`, `gemini extensions update <name>`) and the `name` field of the
/// manifest. Gemini derives the install directory from this same name, so the
/// three cannot be allowed to drift.
pub(super) const EXTENSION_NAME: &str = "tracedecay";

/// Key the MCP server lands under inside the extension manifest.
pub(super) const MCP_SERVER_NAME: &str = "tracedecay";

/// Arguments the tracedecay MCP server is launched with.
pub(super) const MCP_SERVER_ARGS: &[&str] = &["serve"];

/// Manifest file name Gemini CLI requires at the root of an extension.
pub(super) const EXTENSION_MANIFEST_FILE: &str = "gemini-extension.json";

/// Context file the extension ships. Gemini loads it because the manifest's
/// `contextFileName` names it; TraceDecay never appends to the operator's own
/// `~/.gemini/GEMINI.md` for this.
pub(super) const EXTENSION_CONTEXT_FILE: &str = "GEMINI.md";

/// Placeholder in the manifest replaced with the resolved absolute tracedecay
/// binary path at staging time. Substituted through serde so a path carrying a
/// JSON-special character is escaped instead of corrupting the manifest.
pub(super) const TRACEDECAY_BIN_PLACEHOLDER: &str = "__TRACEDECAY_BIN__";

/// The staged manifest template.
///
/// `trust: true` is what makes Gemini auto-approve tracedecay tool calls
/// instead of prompting per call; `args: ["serve"]` is the MCP transport the
/// binary speaks. Both live in the extension now — not in
/// `~/.gemini/settings.json`.
const EXTENSION_MANIFEST_TEMPLATE: &str = r#"{
  "name": "tracedecay",
  "version": "0.0.0",
  "description": "TraceDecay code graph: MCP tools plus mandatory tool-routing context for Gemini CLI.",
  "contextFileName": "GEMINI.md",
  "mcpServers": {
    "tracedecay": {
      "command": "__TRACEDECAY_BIN__",
      "args": ["serve"],
      "trust": true
    }
  }
}
"#;

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

/// TraceDecay's own Gemini profile root. Every path below is derived from the
/// admitted `home` rather than an ambient environment variable, because the
/// host CLI is invoked with a cleared environment and resolves its profile
/// from the same admitted `HOME`.
fn gemini_home(home: &Path) -> PathBuf {
    home.join(".gemini")
}

/// The staged extension source relative to the profile home.
///
/// The first-party component catalog deploys the extension source at exactly
/// this prefix, so [`extension_stage_dir`] and the receipt-owned artifact paths
/// are one definition and cannot drift into two directories.
pub(crate) const GEMINI_STAGED_EXTENSION_RELATIVE: &str = ".gemini/tracedecay-extension";

/// The TraceDecay-owned extension *source*. `gemini extensions install` copies
/// from here into the host's own extensions directory.
pub(super) fn extension_stage_dir(home: &Path) -> PathBuf {
    home.join(GEMINI_STAGED_EXTENSION_RELATIVE)
}

/// The staged manifest — presence is the signal that TraceDecay has rendered
/// an extension source for this profile.
pub(super) fn staged_manifest_path(home: &Path) -> PathBuf {
    extension_stage_dir(home).join(EXTENSION_MANIFEST_FILE)
}

/// The staged context file.
pub(super) fn staged_context_path(home: &Path) -> PathBuf {
    extension_stage_dir(home).join(EXTENSION_CONTEXT_FILE)
}

/// Where Gemini CLI keeps an installed extension. Host-owned: TraceDecay reads
/// it to report state and never writes it.
pub(super) fn installed_extension_dir(home: &Path) -> PathBuf {
    gemini_home(home).join("extensions").join(EXTENSION_NAME)
}

/// The installed manifest Gemini CLI wrote when it adopted the staged source.
pub(super) fn installed_manifest_path(home: &Path) -> PathBuf {
    installed_extension_dir(home).join(EXTENSION_MANIFEST_FILE)
}

/// Gemini CLI's shared settings file. Host-owned, and under the extension
/// model no longer a TraceDecay write target — only an observation target, so
/// a lifecycle transaction can roll back whatever the host CLI changed there.
pub(super) fn settings_path(home: &Path) -> PathBuf {
    gemini_home(home).join("settings.json")
}

/// The operator's own global context file. Read-only for this integration; a
/// tracedecay block here is legacy residue from the pre-extension model.
pub(super) fn user_context_path(home: &Path) -> PathBuf {
    gemini_home(home).join(EXTENSION_CONTEXT_FILE)
}

// ---------------------------------------------------------------------------
// Staging
// ---------------------------------------------------------------------------

/// Every file the staged extension consists of, already rendered.
///
/// One renderer for staging, doctor, and tests, so the bytes that are staged
/// are the same bytes every other reader reasons about.
pub(crate) fn rendered_extension_files(
    tracedecay_bin: &str,
) -> Result<Vec<(&'static str, String)>> {
    Ok(vec![
        (
            EXTENSION_MANIFEST_FILE,
            render_extension_file(
                EXTENSION_MANIFEST_FILE,
                EXTENSION_MANIFEST_TEMPLATE,
                tracedecay_bin,
            )?,
        ),
        (EXTENSION_CONTEXT_FILE, context_file_text()),
    ])
}

/// Apply staging-time substitutions to one extension file:
/// - `gemini-extension.json`: stamp `version` from the crate version and
///   replace the `__TRACEDECAY_BIN__` placeholder with the resolved binary.
/// - everything else is shipped verbatim.
fn render_extension_file(relative: &str, contents: &str, tracedecay_bin: &str) -> Result<String> {
    match relative {
        EXTENSION_MANIFEST_FILE => render_manifest(contents, tracedecay_bin),
        _ => Ok(contents.to_string()),
    }
}

/// Stamp the version and substitute the binary path in a single parse /
/// serialize round-trip. Assigning a `serde_json::Value` escapes any
/// JSON-special character in the path, which a raw string replace would not.
fn render_manifest(raw: &str, tracedecay_bin: &str) -> Result<String> {
    crate::agents::plugin_bundle::stamp_manifest_version_with(raw, |manifest| {
        if let Some(server) = manifest
            .pointer_mut("/mcpServers/tracedecay")
            .and_then(serde_json::Value::as_object_mut)
            && server.get("command").and_then(serde_json::Value::as_str)
                == Some(TRACEDECAY_BIN_PLACEHOLDER)
        {
            server.insert("command".to_string(), json!(tracedecay_bin));
        }
    })
}

/// The extension's context file: the shared tracedecay routing rules every
/// standard host carries, shipped *inside* the extension so Gemini loads it
/// through `contextFileName` instead of TraceDecay splicing a managed block
/// into the operator's `~/.gemini/GEMINI.md`.
fn context_file_text() -> String {
    format!(
        "{}\n",
        crate::agents::prompt_rules::standard_prompt_rules(
            crate::agents::prompt_rules::PROMPT_RULE_MARKER,
            &crate::agents::prompt_rules::PromptRulesOptions {
                extra_paragraphs: &[],
            },
        )
    )
}

/// Render the extension source into its stable staging directory and report
/// that directory. A clean replace, so a file a previous version staged but
/// this one no longer ships cannot linger into the next `gemini extensions
/// install`.
pub(super) fn deploy_extension_bundle(home: &Path, tracedecay_bin: &str) -> Result<PathBuf> {
    let stage_dir = extension_stage_dir(home);
    clean_replace_owned_stage_dir(&stage_dir)?;
    for (relative, rendered) in rendered_extension_files(tracedecay_bin)? {
        safe_write_text_file(&stage_dir.join(relative), &rendered, None)?;
    }
    eprintln!(
        "\x1b[32m✔\x1b[0m Staged tracedecay Gemini extension in {}",
        stage_dir.display()
    );
    Ok(stage_dir)
}

/// True when a staging directory is tracedecay-owned: its manifest names the
/// tracedecay extension. A missing directory is trivially safe to write into.
pub(super) fn stage_dir_is_tracedecay(stage_dir: &Path) -> bool {
    load_json_file(&stage_dir.join(EXTENSION_MANIFEST_FILE))
        .get("name")
        .and_then(serde_json::Value::as_str)
        == Some(EXTENSION_NAME)
}

/// Remove the tracedecay-owned staging directory so the next write is a clean
/// replace. No-op when it is missing; refuses when it exists but is not
/// tracedecay-owned, so a directory squatting on the path — an operator's own
/// hand-written extension source, say — is never deleted.
fn clean_replace_owned_stage_dir(stage_dir: &Path) -> Result<()> {
    if !stage_dir.exists() {
        return Ok(());
    }
    if !stage_dir_is_tracedecay(stage_dir) {
        return Err(TraceDecayError::Config {
            message: format!(
                "refusing to replace non-tracedecay Gemini extension directory {}",
                stage_dir.display()
            ),
        });
    }
    std::fs::remove_dir_all(stage_dir).map_err(|error| TraceDecayError::Config {
        message: format!("failed to remove {}: {error}", stage_dir.display()),
    })
}

// ---------------------------------------------------------------------------
// Installed-extension observation
// ---------------------------------------------------------------------------

/// What TraceDecay can actually see of the host's installed extension.
///
/// Deliberately a three-way answer: "the manifest is unreadable" is not the
/// same fact as "there is no extension", and reporting either as the other
/// would be a doctor that describes a state it did not observe.
pub(super) enum InstalledExtensionV1 {
    /// No manifest at the host's install path for this extension name.
    Missing,
    /// A manifest exists but could not be read or parsed.
    Unreadable,
    /// The parsed installed manifest.
    Present(serde_json::Value),
}

/// Read the host-installed extension manifest without judging its contents.
pub(super) fn read_installed_extension(home: &Path) -> InstalledExtensionV1 {
    match std::fs::read(installed_manifest_path(home)) {
        Ok(bytes) => match serde_json::from_slice::<serde_json::Value>(&bytes) {
            Ok(manifest) => InstalledExtensionV1::Present(manifest),
            Err(_) => InstalledExtensionV1::Unreadable,
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => InstalledExtensionV1::Missing,
        Err(_) => InstalledExtensionV1::Unreadable,
    }
}

/// True when a manifest declares the tracedecay MCP server the way this
/// version stages it: the current product version, a non-empty command
/// (matching `tracedecay_bin` when the caller knows it), `serve` in `args`,
/// and `trust: true`.
///
/// `tracedecay_bin` is optional because the doctor and the read-only
/// registration inspector have no `InstallContext`; they check everything they
/// *can* observe and leave the binary-path comparison to the lifecycle-aware
/// caller rather than guessing at it.
pub(super) fn manifest_declares_current_server(
    manifest: &serde_json::Value,
    tracedecay_bin: Option<&str>,
) -> bool {
    manifest.get("name").and_then(serde_json::Value::as_str) == Some(EXTENSION_NAME)
        && manifest.get("version").and_then(serde_json::Value::as_str)
            == Some(crate::PRODUCT_VERSION)
        && manifest_server_is_current(manifest, tracedecay_bin)
}

/// The MCP-server half of [`manifest_declares_current_server`], without the
/// manifest identity/version checks.
pub(super) fn manifest_server_is_current(
    manifest: &serde_json::Value,
    tracedecay_bin: Option<&str>,
) -> bool {
    let Some(server) = manifest
        .pointer(&format!("/mcpServers/{MCP_SERVER_NAME}"))
        .and_then(serde_json::Value::as_object)
    else {
        return false;
    };
    let command_ok = server
        .get("command")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|command| {
            !command.is_empty() && tracedecay_bin.is_none_or(|expected| command == expected)
        });
    let args_ok = server
        .get("args")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|args| {
            MCP_SERVER_ARGS
                .iter()
                .all(|expected| args.iter().any(|arg| arg.as_str() == Some(*expected)))
        });
    let trust_ok = server.get("trust").and_then(serde_json::Value::as_bool) == Some(true);
    command_ok && args_ok && trust_ok
}

/// True when the host's installed extension is present and current.
pub(super) fn installed_extension_is_current(home: &Path, tracedecay_bin: Option<&str>) -> bool {
    match read_installed_extension(home) {
        InstalledExtensionV1::Present(manifest) => {
            manifest_declares_current_server(&manifest, tracedecay_bin)
        }
        InstalledExtensionV1::Missing | InstalledExtensionV1::Unreadable => false,
    }
}

/// True when *something* tracedecay-shaped is installed at the host's path,
/// current or not. Drives the "is there anything to remove" decision.
pub(super) fn installed_extension_is_present(home: &Path) -> bool {
    installed_extension_dir(home).exists()
}

// ---------------------------------------------------------------------------
// Host CLI lifecycle
// ---------------------------------------------------------------------------

/// Resolve Gemini CLI's own binary, or fail with the typed requirement.
///
/// Gemini CLI owns extension registration, the installed copy, and enablement
/// through `gemini extensions`. Its binary is therefore a hard requirement for
/// this lifecycle, not a preference with a config-editing fallback: falling
/// back to merging `~/.gemini/settings.json` and appending to
/// `~/.gemini/GEMINI.md` would re-create by hand the state the host believes
/// it owns, and a half-emulated install is indistinguishable on disk from a
/// corrupt one.
pub(super) fn require_gemini_cli() -> Result<PathBuf> {
    host_cli::require_host_cli(GEMINI_CLI, GEMINI_CLI_LIFECYCLE)
}

/// Drive Gemini CLI's own command to adopt the staged extension source.
///
/// Split from the trait method so tests can supply a fake launcher and an
/// isolated `HOME` without mutating the process environment.
///
/// When the host already carries an installed tracedecay extension the host's
/// own `uninstall` runs first: `gemini extensions install` refuses to install
/// over an existing extension, and removing it through the host — rather than
/// deleting the host-owned directory ourselves — keeps every write to that
/// state on Gemini's side of the boundary.
pub(super) fn gemini_extension_activate_with(gemini: &Path, home: &Path) -> Result<()> {
    let stage_dir = extension_stage_dir(home);
    if !staged_manifest_path(home).exists() {
        return Err(TraceDecayError::Config {
            message: format!(
                "no staged Gemini extension at {}; stage it before driving `gemini extensions install`",
                stage_dir.display()
            ),
        });
    }
    if installed_extension_is_present(home) {
        run_gemini_extension_step(gemini, &["extensions", "uninstall", EXTENSION_NAME], home)?;
    }
    let stage_arg = stage_dir.to_string_lossy().into_owned();
    run_gemini_extension_step(gemini, &["extensions", "install", stage_arg.as_str()], home)
}

/// Drive Gemini CLI's own command to drop the tracedecay extension.
///
/// The staged source is left in place: it is TraceDecay-owned input to the
/// host lifecycle, not host registration state, and the deployed-asset
/// lifecycle — not this registration boundary — owns removing it.
pub(super) fn gemini_extension_deactivate_with(gemini: &Path, home: &Path) -> Result<()> {
    run_gemini_extension_step(gemini, &["extensions", "uninstall", EXTENSION_NAME], home)
}

/// Ask Gemini CLI itself what extensions it has, or `None` when its binary is
/// not on `PATH`.
///
/// Used only by the doctor, which must never *claim* an activation state it
/// could not observe: when this returns `None` the doctor says so instead of
/// inferring adoption from files TraceDecay staged itself.
pub(super) fn host_reported_extensions(home: &Path) -> Option<host_cli::HostCliOutcomeV1> {
    let gemini = host_cli::require_host_cli(GEMINI_CLI, GEMINI_CLI_LIFECYCLE).ok()?;
    host_cli::run_host_cli(&gemini, &["extensions", "list"], home).ok()
}

/// Run one `gemini extensions ...` step, converting a failed invocation into
/// the host's own diagnosis.
///
/// The post-command bytes of `~/.gemini/settings.json` are recorded through
/// the active host transaction — read exactly once, after the child exits — so
/// the transaction's existing rollback authority can restore the pre-command
/// document if the command fails or a later verification rejects its effect.
/// Reading again after recording would let a foreign writer be absorbed into
/// the transaction's intended state.
fn run_gemini_extension_step(gemini: &Path, args: &[&str], home: &Path) -> Result<()> {
    let settings = settings_path(home);
    let outcome = host_cli::run_host_cli(gemini, args, home)?;
    let observed = read_optional_bytes(&settings)?;
    record_host_config_observation_bytes(&settings, observed.as_deref())?;
    if outcome.succeeded() {
        return Ok(());
    }
    Err(TraceDecayError::Config {
        message: outcome.failure_message(),
    })
}

fn read_optional_bytes(path: &Path) -> Result<Option<Vec<u8>>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(TraceDecayError::Config {
            message: format!(
                "failed to read {} after the Gemini CLI: {error}",
                path.display()
            ),
        }),
    }
}
