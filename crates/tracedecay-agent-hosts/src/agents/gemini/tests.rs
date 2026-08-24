//! Gemini CLI extension-lifecycle tests.
//!
//! Every test runs against a `tempfile::tempdir()` `HOME` and a fake `gemini`
//! shell script, so none of them can observe or touch the operator's real
//! `~/.gemini`. The fake records each invocation's argv, which is how the
//! exact host commands TraceDecay drives are asserted rather than assumed.

use super::extension::{
    EXTENSION_CONTEXT_FILE, EXTENSION_MANIFEST_FILE, TRACEDECAY_BIN_PLACEHOLDER,
    rendered_extension_files,
};
use super::*;

use crate::errors::TraceDecayError;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// Install a fake `gemini` that appends each invocation's argv to `log` and
/// then performs `body`.
#[cfg(unix)]
fn fake_gemini_cli(bin: &Path, log: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {log}\n{body}\n",
        log = shell_single_quote(&log.to_string_lossy()),
    );
    std::fs::write(bin, script).unwrap();
    let mut permissions = std::fs::metadata(bin).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(bin, permissions).unwrap();
}

/// A fake body that behaves like the real CLI far enough to be observable:
/// `install` copies the staged source into the host's extensions directory,
/// `uninstall` removes it. `HOME` is the profile `run_host_cli` admitted, and
/// the coreutils are addressed absolutely because that invocation deliberately
/// clears `PATH`.
#[cfg(unix)]
const FAKE_EXTENSION_LIFECYCLE_BODY: &str = r#"case "$1 $2" in
  "extensions install")
    /bin/mkdir -p "$HOME/.gemini/extensions/tracedecay"
    /bin/cp "$3/gemini-extension.json" "$HOME/.gemini/extensions/tracedecay/gemini-extension.json"
    /bin/cp "$3/GEMINI.md" "$HOME/.gemini/extensions/tracedecay/GEMINI.md"
    ;;
  "extensions uninstall")
    /bin/rm -rf "$HOME/.gemini/extensions/tracedecay"
    ;;
esac
exit 0"#;

#[cfg(unix)]
fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

#[cfg(unix)]
fn recorded_invocations(log: &Path) -> Vec<String> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

#[cfg(unix)]
struct AmbientPathGuard {
    previous: Option<std::ffi::OsString>,
    _lock: std::sync::MutexGuard<'static, ()>,
}

#[cfg(unix)]
impl AmbientPathGuard {
    fn set(path: &Path) -> Self {
        let lock = crate::config::lock_user_data_dir_test_env();
        let previous = std::env::var_os("PATH");
        // SAFETY: the shared profile-discovery lock is held for this guard's
        // lifetime, so sibling tests do not observe the temporary PATH.
        unsafe {
            std::env::set_var("PATH", path);
        }
        Self {
            previous,
            _lock: lock,
        }
    }
}

#[cfg(unix)]
impl Drop for AmbientPathGuard {
    fn drop(&mut self) {
        // SAFETY: see `AmbientPathGuard::set`.
        unsafe {
            match self.previous.take() {
                Some(previous) => std::env::set_var("PATH", previous),
                None => std::env::remove_var("PATH"),
            }
        }
    }
}

fn install_context(home: &Path, tracedecay_bin: &str) -> InstallContext {
    InstallContext {
        home: home.to_path_buf(),
        tracedecay_bin: tracedecay_bin.to_string(),
        tool_permissions: Vec::new(),
        project_root: None,
        dashboard: true,
    }
}

fn read_json(path: &Path) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

/// Write the exact bytes Gemini would have installed from a staged source, so
/// state-dependent behavior can be exercised without a host CLI.
fn simulate_host_install(home: &Path, tracedecay_bin: &str) {
    let installed = installed_extension_dir(home);
    std::fs::create_dir_all(&installed).unwrap();
    for (relative, rendered) in rendered_extension_files(tracedecay_bin).unwrap() {
        std::fs::write(installed.join(relative), rendered).unwrap();
    }
}

// ---------------------------------------------------------------------------
// Staging
// ---------------------------------------------------------------------------

/// The staged manifest is the whole registration: it must name the extension,
/// carry the admitted binary in place of the placeholder, ask for the `serve`
/// transport, and pre-trust the server so Gemini does not prompt per tool call.
#[test]
fn staging_renders_the_manifest_with_the_admitted_binary_serve_args_and_trust() {
    let home = tempfile::tempdir().unwrap();

    let stage_dir = deploy_extension_bundle(home.path(), "/abs/bin/tracedecay").unwrap();

    assert_eq!(stage_dir, extension_stage_dir(home.path()));
    let raw = std::fs::read_to_string(stage_dir.join(EXTENSION_MANIFEST_FILE)).unwrap();
    assert!(
        !raw.contains(TRACEDECAY_BIN_PLACEHOLDER),
        "the binary placeholder must be substituted, never shipped"
    );
    let manifest: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(manifest["name"], EXTENSION_NAME);
    assert_eq!(manifest["version"], crate::PRODUCT_VERSION);
    assert_eq!(manifest["contextFileName"], EXTENSION_CONTEXT_FILE);
    assert_eq!(
        manifest["mcpServers"]["tracedecay"]["command"],
        "/abs/bin/tracedecay"
    );
    assert_eq!(
        manifest["mcpServers"]["tracedecay"]["args"],
        serde_json::json!(["serve"])
    );
    assert_eq!(manifest["mcpServers"]["tracedecay"]["trust"], true);

    // The extension carries its own context file; TraceDecay must not need to
    // append to the operator's ~/.gemini/GEMINI.md for the rules to load.
    let context = std::fs::read_to_string(stage_dir.join(EXTENSION_CONTEXT_FILE)).unwrap();
    assert!(context.contains("tracedecay"));
    assert!(
        !home.path().join(".gemini/GEMINI.md").exists(),
        "staging must not write the operator's own context file"
    );
    assert!(
        !home.path().join(".gemini/settings.json").exists(),
        "staging must not write the host's shared settings file"
    );
}

/// A binary path carrying a JSON-special character must be escaped through
/// serde; a raw string replace into the template would emit invalid JSON.
#[test]
fn staging_escapes_special_chars_in_the_binary_path() {
    let home = tempfile::tempdir().unwrap();
    let weird_bin = "/opt/td \"quote\"/tracedecay";

    let stage_dir = deploy_extension_bundle(home.path(), weird_bin).unwrap();

    let manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(stage_dir.join(EXTENSION_MANIFEST_FILE))
            .expect("the staged manifest must exist"),
    )
    .expect("the staged manifest must stay valid JSON after substitution");
    assert_eq!(manifest["mcpServers"]["tracedecay"]["command"], weird_bin);
}

/// Re-staging is a clean replace: a file an older version staged but this one
/// no longer ships must not survive into the next `gemini extensions install`.
#[test]
fn staging_is_a_clean_replace_dropping_stale_files() {
    let home = tempfile::tempdir().unwrap();
    let stage_dir = deploy_extension_bundle(home.path(), "/bin/tracedecay").unwrap();
    let stale = stage_dir.join("commands/retired.toml");
    std::fs::create_dir_all(stale.parent().unwrap()).unwrap();
    std::fs::write(&stale, "stale command").unwrap();

    deploy_extension_bundle(home.path(), "/bin/tracedecay").unwrap();

    assert!(
        !stale.exists(),
        "a stale staged file must be gone after a clean-replace restage"
    );
    assert!(stage_dir.join(EXTENSION_MANIFEST_FILE).exists());
}

/// The clean replace must refuse a directory TraceDecay does not own, so an
/// operator's own extension source squatting on the path is never deleted.
#[test]
fn staging_refuses_to_replace_a_directory_tracedecay_does_not_own() {
    let home = tempfile::tempdir().unwrap();
    let stage_dir = extension_stage_dir(home.path());
    std::fs::create_dir_all(&stage_dir).unwrap();
    std::fs::write(
        stage_dir.join(EXTENSION_MANIFEST_FILE),
        r#"{"name":"someone-elses-extension"}"#,
    )
    .unwrap();
    std::fs::write(stage_dir.join("user-file.txt"), "keep me").unwrap();

    let error = deploy_extension_bundle(home.path(), "/bin/tracedecay")
        .expect_err("a non-tracedecay extension directory must not be replaced");

    assert!(
        error.to_string().contains("non-tracedecay"),
        "unexpected error: {error}"
    );
    assert!(
        stage_dir.join("user-file.txt").exists(),
        "an unowned directory must be left untouched"
    );
    assert_eq!(
        std::fs::read_to_string(stage_dir.join(EXTENSION_MANIFEST_FILE)).unwrap(),
        r#"{"name":"someone-elses-extension"}"#
    );
}

// ---------------------------------------------------------------------------
// Host-CLI-driven lifecycle
// ---------------------------------------------------------------------------

/// Activation is Gemini's own `extensions install` against the staged source —
/// not a settings.json merge.
#[cfg(unix)]
#[test]
fn activation_drives_the_hosts_own_extension_install_against_the_staged_source() {
    let home = tempfile::tempdir().unwrap();
    let bin_dir = tempfile::tempdir().unwrap();
    let log = bin_dir.path().join("invocations.log");
    let gemini = bin_dir.path().join("gemini");
    let stage_dir = deploy_extension_bundle(home.path(), "/bin/tracedecay").unwrap();
    fake_gemini_cli(&gemini, &log, FAKE_EXTENSION_LIFECYCLE_BODY);

    gemini_extension_activate_with(&gemini, home.path())
        .expect("a clean host CLI run is a completed activation");

    assert_eq!(
        recorded_invocations(&log),
        vec![format!("extensions install {}", stage_dir.display())],
        "activation must hand the staged source to the host's own install command"
    );
    assert!(
        !home.path().join(".gemini/settings.json").exists(),
        "TraceDecay must not write the host's shared settings file"
    );
    // The host's own install is what produced the installed copy.
    assert_eq!(
        read_json(&installed_manifest_path(home.path()))["mcpServers"]["tracedecay"]["command"],
        "/bin/tracedecay"
    );
}

/// `gemini extensions install` refuses to install over an existing extension,
/// so a reinstall drops the old one through the host's own uninstall rather
/// than by deleting the host-owned directory behind its back.
#[cfg(unix)]
#[test]
fn activation_removes_an_existing_extension_through_the_host_before_reinstalling() {
    let home = tempfile::tempdir().unwrap();
    let bin_dir = tempfile::tempdir().unwrap();
    let log = bin_dir.path().join("invocations.log");
    let gemini = bin_dir.path().join("gemini");
    let stage_dir = deploy_extension_bundle(home.path(), "/relocated/tracedecay").unwrap();
    simulate_host_install(home.path(), "/old/tracedecay");
    fake_gemini_cli(&gemini, &log, FAKE_EXTENSION_LIFECYCLE_BODY);

    gemini_extension_activate_with(&gemini, home.path())
        .expect("reinstalling over an existing extension must complete");

    assert_eq!(
        recorded_invocations(&log),
        vec![
            format!("extensions uninstall {EXTENSION_NAME}"),
            format!("extensions install {}", stage_dir.display()),
        ]
    );
    assert_eq!(
        read_json(&installed_manifest_path(home.path()))["mcpServers"]["tracedecay"]["command"],
        "/relocated/tracedecay"
    );
}

/// Activation without a staged source is a TraceDecay bug, not a host failure:
/// refuse before invoking the CLI rather than asking Gemini to install
/// something that is not there.
#[cfg(unix)]
#[test]
fn activation_without_a_staged_source_refuses_before_invoking_the_host() {
    let home = tempfile::tempdir().unwrap();
    let bin_dir = tempfile::tempdir().unwrap();
    let log = bin_dir.path().join("invocations.log");
    let gemini = bin_dir.path().join("gemini");
    fake_gemini_cli(&gemini, &log, FAKE_EXTENSION_LIFECYCLE_BODY);

    let error = gemini_extension_activate_with(&gemini, home.path())
        .expect_err("there is nothing for the host to install");

    assert!(error.to_string().contains("no staged Gemini extension"));
    assert!(
        recorded_invocations(&log).is_empty(),
        "the host CLI must not be invoked without a staged source"
    );
}

/// Deactivation is Gemini's own `extensions uninstall`, addressed by the
/// extension name.
#[cfg(unix)]
#[test]
fn deactivation_drives_the_hosts_own_uninstall_by_extension_name() {
    let home = tempfile::tempdir().unwrap();
    let bin_dir = tempfile::tempdir().unwrap();
    let log = bin_dir.path().join("invocations.log");
    let gemini = bin_dir.path().join("gemini");
    deploy_extension_bundle(home.path(), "/bin/tracedecay").unwrap();
    simulate_host_install(home.path(), "/bin/tracedecay");
    fake_gemini_cli(&gemini, &log, FAKE_EXTENSION_LIFECYCLE_BODY);

    gemini_extension_deactivate_with(&gemini, home.path())
        .expect("a clean host CLI run is a completed removal");

    assert_eq!(
        recorded_invocations(&log),
        vec![format!("extensions uninstall {EXTENSION_NAME}")]
    );
    assert!(
        !installed_extension_dir(home.path()).exists(),
        "the host's own uninstall removed its copy"
    );
    assert!(
        staged_manifest_path(home.path()).exists(),
        "the TraceDecay-owned source is lifecycle input, not host registration state"
    );
}

/// A failing host command must reach the operator as the host's own diagnosis,
/// not as a TraceDecay guess about what went wrong.
#[cfg(unix)]
#[test]
fn a_failing_host_command_reports_the_hosts_own_diagnosis() {
    let home = tempfile::tempdir().unwrap();
    let bin_dir = tempfile::tempdir().unwrap();
    let log = bin_dir.path().join("invocations.log");
    let gemini = bin_dir.path().join("gemini");
    fake_gemini_cli(
        &gemini,
        &log,
        "echo 'extension tracedecay is not installed' >&2\nexit 7",
    );

    let error = gemini_extension_deactivate_with(&gemini, home.path())
        .expect_err("a non-zero host CLI exit must fail the lifecycle");

    let TraceDecayError::Config { message } = error else {
        panic!("a failed host command must surface as a config error");
    };
    assert!(
        message.contains("extension tracedecay is not installed")
            && message.contains("exit code 7"),
        "the host's own stderr and status must reach the operator: {message}"
    );
}

/// The `gemini` binary is a hard requirement. With it absent the lifecycle
/// refuses; it must not fall back to merging `~/.gemini/settings.json` or
/// appending to `~/.gemini/GEMINI.md`.
#[cfg(unix)]
#[test]
fn a_missing_host_binary_refuses_instead_of_editing_host_owned_state() {
    let home = tempfile::tempdir().unwrap();
    let settings = settings_path(home.path());
    std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
    let original = br#"{"mcpServers":{"other":{"command":"other"}},"theme":"Default"}"#;
    std::fs::write(&settings, original).unwrap();
    let user_context = user_context_path(home.path());
    std::fs::write(&user_context, "# my own rules\n").unwrap();

    let empty_path_dir = tempfile::tempdir().unwrap();
    let _path = AmbientPathGuard::set(empty_path_dir.path());

    let error = GeminiIntegration
        .prepare_non_interactive_install(&install_context(home.path(), "/bin/tracedecay"))
        .expect_err("an absent host binary is a hard requirement failure");

    let TraceDecayError::HostCliUnavailable { program, lifecycle } = error else {
        panic!("host CLI absence must surface as a typed requirement");
    };
    assert_eq!(program, "gemini");
    assert_eq!(lifecycle, "gemini extension lifecycle");
    assert_eq!(
        std::fs::read(&settings).unwrap(),
        original,
        "a refused lifecycle must leave the host's settings byte-identical"
    );
    assert_eq!(
        std::fs::read_to_string(&user_context).unwrap(),
        "# my own rules\n",
        "a refused lifecycle must not append rules to the operator's context file"
    );
    assert!(
        !installed_extension_dir(home.path()).exists(),
        "a refused lifecycle must not hand-write the host's extensions directory"
    );
}

// ---------------------------------------------------------------------------
// Observed state and doctor truthfulness
// ---------------------------------------------------------------------------

/// Adoption is a fact about Gemini's installed copy. Staging alone is not an
/// installation, a stale version is repairable, and unreadable JSON is corrupt
/// rather than "missing".
#[test]
fn registration_state_follows_the_hosts_installed_extension() {
    use crate::agents::host_bundle_v2::{HostBundleComponentV1, HostBundleRegistrationStateV1};

    let home = tempfile::tempdir().unwrap();
    let health = HealthcheckContext {
        home: home.path().to_path_buf(),
        project_path: home.path().to_path_buf(),
    };
    deploy_extension_bundle(home.path(), "/bin/tracedecay").unwrap();
    assert!(
        !GeminiIntegration.has_tracedecay(home.path()),
        "a staged source the host never installed is not an installation"
    );
    assert_eq!(
        GeminiIntegration.host_component_registration(HostBundleComponentV1::Core, &health),
        HostBundleRegistrationStateV1::Missing
    );

    simulate_host_install(home.path(), "/bin/tracedecay");
    assert!(GeminiIntegration.has_tracedecay(home.path()));
    assert_eq!(
        GeminiIntegration.host_component_registration(HostBundleComponentV1::ContextMcp, &health),
        HostBundleRegistrationStateV1::Current
    );
    assert!(matches!(
        GeminiIntegration
            .preflight_non_interactive_install(&install_context(home.path(), "/bin/tracedecay"))
            .unwrap(),
        NonInteractiveInstallOutcome::Ready
    ));

    // A relocated binary is only visible to the lifecycle-aware readback.
    assert_eq!(
        GeminiIntegration.host_component_registration_for_lifecycle(
            HostBundleComponentV1::Core,
            &health,
            &install_context(home.path(), "/relocated/tracedecay"),
        ),
        HostBundleRegistrationStateV1::Repairable
    );
    assert!(matches!(
        GeminiIntegration
            .preflight_non_interactive_install(&install_context(
                home.path(),
                "/relocated/tracedecay"
            ))
            .unwrap(),
        NonInteractiveInstallOutcome::DeferredUserAction(_)
    ));

    std::fs::write(installed_manifest_path(home.path()), b"{not json").unwrap();
    assert_eq!(
        GeminiIntegration.host_component_registration(HostBundleComponentV1::Core, &health),
        HostBundleRegistrationStateV1::Corrupt
    );
}

/// The doctor must not fail on the *correct* post-adoption state: under the
/// extension model `~/.gemini/settings.json` carries no tracedecay entry and
/// `~/.gemini/GEMINI.md` carries no managed block, because the extension
/// supplies both.
#[test]
fn doctor_reports_no_issue_when_the_extension_supplies_the_server() {
    let home = tempfile::tempdir().unwrap();
    deploy_extension_bundle(home.path(), "/bin/tracedecay").unwrap();
    simulate_host_install(home.path(), "/bin/tracedecay");

    let mut dc = DoctorCounters::new();
    doctor_check_staged_extension(&mut dc, home.path());
    doctor_check_installed_extension(&mut dc, home.path());
    doctor_check_settings(&mut dc, home.path());
    doctor_check_prompt(&mut dc, home.path());

    assert_eq!(
        dc.issues, 0,
        "an adopted extension with no settings.json entry is a healthy install"
    );
    assert_eq!(dc.warnings, 0);
}

/// Pre-extension state is reported as residue — a warning about duplication —
/// never as the registration the doctor is looking for.
#[test]
fn doctor_reports_pre_extension_state_as_residue() {
    let home = tempfile::tempdir().unwrap();
    let settings = settings_path(home.path());
    std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
    std::fs::write(
        &settings,
        br#"{"mcpServers":{"tracedecay":{"command":"/old/tracedecay","args":["serve"]}}}"#,
    )
    .unwrap();
    std::fs::write(
        user_context_path(home.path()),
        format!(
            "{}\n\nold rules\n",
            super::super::prompt_rules::PROMPT_RULE_MARKER
        ),
    )
    .unwrap();

    let mut dc = DoctorCounters::new();
    doctor_check_settings(&mut dc, home.path());
    doctor_check_prompt(&mut dc, home.path());

    assert_eq!(
        dc.issues, 0,
        "legacy residue is a duplication warning, not a failed registration"
    );
    assert_eq!(dc.warnings, 2, "both residues must be reported");
}

/// A staged source that is missing its manifest is reported as "not staged",
/// and a not-yet-adopted extension is reported as not installed — neither is
/// silently upgraded into a claim that Gemini has the extension.
#[test]
fn doctor_warns_when_nothing_is_staged_or_installed() {
    let home = tempfile::tempdir().unwrap();

    let mut dc = DoctorCounters::new();
    doctor_check_staged_extension(&mut dc, home.path());
    doctor_check_installed_extension(&mut dc, home.path());

    assert_eq!(dc.issues, 0);
    assert_eq!(dc.warnings, 2);
}

#[cfg(unix)]
#[test]
fn doctor_only_treats_an_absent_gemini_cli_as_unobserved_state() {
    let home = tempfile::tempdir().unwrap();
    let empty_path_dir = tempfile::tempdir().unwrap();
    let _path = AmbientPathGuard::set(empty_path_dir.path());

    assert!(
        host_reported_extensions(home.path())
            .expect("an absent Gemini binary is the one optional host-report state")
            .is_none()
    );

    let mut dc = DoctorCounters::new();
    doctor_check_host_reported_extensions(&mut dc, home.path());

    assert_eq!(dc.issues, 0);
    assert_eq!(dc.warnings, 0);
}

#[cfg(unix)]
#[test]
fn doctor_fails_when_a_present_gemini_cli_is_not_executable() {
    let home = tempfile::tempdir().unwrap();
    let bin_dir = tempfile::tempdir().unwrap();
    let candidate = bin_dir.path().join("gemini");
    std::fs::write(&candidate, b"not executable").unwrap();
    let _path = AmbientPathGuard::set(bin_dir.path());

    let error = host_reported_extensions(home.path())
        .expect_err("a present unusable Gemini candidate is not an absent CLI");
    let TraceDecayError::Config { message } = error else {
        panic!("a present unusable Gemini candidate must preserve its typed failure: {error}");
    };
    assert!(
        message.contains(&candidate.display().to_string()),
        "the typed failure must identify the unusable Gemini candidate: {message}"
    );

    let mut dc = DoctorCounters::new();
    doctor_check_host_reported_extensions(&mut dc, home.path());

    assert_eq!(dc.issues, 1);
    assert_eq!(dc.warnings, 0);
}

/// `update_plugin` refreshes only the TraceDecay-owned source and says so:
/// Gemini owns the installed copy, so an unadopted refresh must not be
/// reported as an updated extension.
#[test]
fn update_refreshes_the_staged_source_and_defers_host_adoption() {
    let home = tempfile::tempdir().unwrap();
    assert!(matches!(
        GeminiIntegration
            .update_plugin(&install_context(home.path(), "/bin/tracedecay"))
            .unwrap(),
        UpdatePluginOutcome::NotInstalled
    ));

    deploy_extension_bundle(home.path(), "/old/tracedecay").unwrap();
    let outcome = GeminiIntegration
        .update_plugin(&install_context(home.path(), "/new/tracedecay"))
        .unwrap();

    let UpdatePluginOutcome::DeferredUserAction(deferred) = outcome else {
        panic!("a refreshed source the host has not adopted is a deferred action");
    };
    assert!(deferred.remediation.contains("gemini extensions update"));
    assert_eq!(
        deferred.staged_paths,
        vec![extension_stage_dir(home.path())]
    );
    assert_eq!(
        read_json(&staged_manifest_path(home.path()))["mcpServers"]["tracedecay"]["command"],
        "/new/tracedecay"
    );
}
