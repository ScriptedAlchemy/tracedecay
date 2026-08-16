//! Host-CLI-driven Kiro MCP registry lifecycle.
//!
//! Kiro owns `~/.kiro/settings/mcp.json` through `kiro-cli mcp`, so TraceDecay
//! drives that CLI rather than merging the file. These tests stand a fake
//! `kiro-cli` in an isolated HOME, assert the exact argv TraceDecay issues,
//! and assert that an absent binary refuses instead of falling back to config
//! surgery. The fake host preserves a known peer server so the lifecycle's
//! preservation guard is exercised on both add and remove.
//!
//! The fake CLI also emulates the registry's own effect (add writes the
//! server entry, remove drops it) so removal can be shown to reverse
//! installation rather than merely being spelled correctly.

use super::*;

/// Install a fake `kiro-cli` that appends each invocation's argv to `log` and
/// then performs `body`.
#[cfg(unix)]
fn fake_kiro_cli(bin: &Path, log: &Path, body: &str) {
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

/// Body for a fake `kiro-cli` that emulates the registry's own writes, so a
/// test can observe that TraceDecay's removal really reverses its install.
#[cfg(unix)]
const FAKE_REGISTRY_BODY: &str = r#"case "$1 $2" in
  "mcp add")
    [ "${11-}" = "--force" ] || { echo 'missing --force' >&2; exit 64; }
    command="$6"
    /bin/mkdir -p "$HOME/.kiro/settings"
    if [ -f "$HOME/.kiro/settings/mcp.json" ] && /bin/grep -q '"other"' "$HOME/.kiro/settings/mcp.json"; then
      printf '{"mcpServers":{"other":{"command":"other","args":[]},"tracedecay":{"command":"%s","args":["serve"],"disabled":false}}}\n' "$command" > "$HOME/.kiro/settings/mcp.json"
    else
      printf '{"mcpServers":{"tracedecay":{"command":"%s","args":["serve"],"disabled":false}}}\n' "$command" > "$HOME/.kiro/settings/mcp.json"
    fi
    ;;
  "mcp remove")
    if [ -f "$HOME/.kiro/settings/mcp.json" ] && /bin/grep -q '"other"' "$HOME/.kiro/settings/mcp.json"; then
      printf '%s\n' '{"mcpServers":{"other":{"command":"other","args":[]}}}' > "$HOME/.kiro/settings/mcp.json"
    else
      /bin/rm -f "$HOME/.kiro/settings/mcp.json"
    fi
    ;;
esac
exit 0"#;

/// A host command can mutate its registry and still return a failure (for
/// example after a post-write validation error). The component-set transaction
/// must restore the exact pre-command bytes in that case.
#[cfg(unix)]
const FAKE_FAIL_AFTER_WRITE_BODY: &str = r#"case "$1 $2" in
  "mcp add")
    /bin/mkdir -p "$HOME/.kiro/settings"
    printf '%s\n' '{"mcpServers":{"other":{"command":"other","args":[]},"tracedecay":{"command":"/bin/tracedecay","args":["serve"],"disabled":false}}}' > "$HOME/.kiro/settings/mcp.json"
    ;;
esac
echo 'Kiro rejected the registry after writing it' >&2
exit 7"#;

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
#[test]
fn activation_drives_the_hosts_own_mcp_add_with_the_registered_server_contract() {
    let home = tempfile::tempdir().unwrap();
    let bin_dir = tempfile::tempdir().unwrap();
    let log = bin_dir.path().join("invocations.log");
    let kiro_cli = bin_dir.path().join("kiro-cli");
    fake_kiro_cli(&kiro_cli, &log, FAKE_REGISTRY_BODY);

    kiro_mcp_add_with(&kiro_cli, home.path(), "/bin/tracedecay")
        .expect("a clean host CLI run is a completed registration");

    assert_eq!(
        recorded_invocations(&log),
        vec![
            "mcp add --name tracedecay --command /bin/tracedecay --args serve --scope global --force"
                .to_string(),
        ],
        "activation must add the server through Kiro's own registry, naming it and \
         passing each launch argument as Kiro's raw `--args` value at global scope"
    );
    assert!(
        mcp_config_path(home.path()).exists(),
        "the host's own registry write must be what lands the entry"
    );
}

#[cfg(unix)]
#[test]
fn removal_drives_the_hosts_own_mcp_remove_and_reverses_the_registration() {
    let home = tempfile::tempdir().unwrap();
    let bin_dir = tempfile::tempdir().unwrap();
    let log = bin_dir.path().join("invocations.log");
    let kiro_cli = bin_dir.path().join("kiro-cli");
    fake_kiro_cli(&kiro_cli, &log, FAKE_REGISTRY_BODY);
    let mcp_path = mcp_config_path(home.path());
    assert!(!mcp_path.exists(), "precondition: nothing registered yet");

    kiro_mcp_add_with(&kiro_cli, home.path(), "/bin/tracedecay").unwrap();
    kiro_mcp_remove_with(&kiro_cli, home.path())
        .expect("a clean host CLI run is a completed removal");

    assert_eq!(
        recorded_invocations(&log),
        vec![
            "mcp add --name tracedecay --command /bin/tracedecay --args serve --scope global --force"
                .to_string(),
            "mcp remove --name tracedecay --scope global".to_string(),
        ],
        "removal must address the server by the same registry name the add used"
    );
    assert!(
        !mcp_path.exists(),
        "removal must fully reverse installation, leaving no tracedecay entry behind"
    );
}

#[cfg(unix)]
#[test]
fn add_and_remove_preserve_an_operator_owned_peer_server() {
    let home = tempfile::tempdir().unwrap();
    let bin_dir = tempfile::tempdir().unwrap();
    let log = bin_dir.path().join("invocations.log");
    let kiro_cli = bin_dir.path().join("kiro-cli");
    fake_kiro_cli(&kiro_cli, &log, FAKE_REGISTRY_BODY);
    let mcp_path = mcp_config_path(home.path());
    std::fs::create_dir_all(mcp_path.parent().unwrap()).unwrap();
    std::fs::write(
        &mcp_path,
        br#"{"mcpServers":{"other":{"command":"other","args":[]},"tracedecay":{"command":"/old/tracedecay","args":["serve"]}}}"#,
    )
    .unwrap();

    kiro_mcp_add_with(&kiro_cli, home.path(), "/new/tracedecay")
        .expect("host add must force-update tracedecay while preserving the peer");
    let added: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&mcp_path).unwrap()).unwrap();
    assert_eq!(added["mcpServers"]["other"]["command"], "other");
    assert_eq!(
        added["mcpServers"]["tracedecay"]["command"],
        "/new/tracedecay"
    );

    kiro_mcp_remove_with(&kiro_cli, home.path())
        .expect("host remove must preserve the peer while dropping tracedecay");
    let removed: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&mcp_path).unwrap()).unwrap();
    assert_eq!(removed["mcpServers"]["other"]["command"], "other");
    assert!(removed["mcpServers"].get("tracedecay").is_none());
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

#[cfg(unix)]
fn kiro_component_set() -> crate::agents::host_bundle_registry::VerifiedEmbeddedHostComponentSetV1 {
    crate::agents::host_bundle_registry::verified_embedded_host_component_set_with_tracedecay_bin(
        crate::agents::host_bundle_v2::HostKindV1::Kiro,
        &[crate::agents::host_bundle_v2::HostBundleComponentV1::ContextMcp],
        0,
        "/bin/tracedecay",
    )
    .expect("the embedded Kiro component set must verify")
}

#[cfg(unix)]
fn kiro_component_request(
    operation: crate::agents::host_bundle_v2::HostBundleLifecycleOpV1,
    operation_id: [u8; 16],
) -> crate::agents::host_bundle_v2::HostComponentSetExecutionRequestV1 {
    crate::agents::host_bundle_v2::HostComponentSetExecutionRequestV1 {
        lifecycle: crate::agents::host_bundle_v2::HostComponentSetLifecycleRequestV1 {
            operation,
            expected_host: crate::agents::host_bundle_v2::HostKindV1::Kiro,
            expected_components: vec![
                crate::agents::host_bundle_v2::HostBundleComponentV1::ContextMcp,
            ],
            explicit_confirmation: true,
            hermes_profile_bindings: 0,
        },
        operation_id,
    }
}

#[cfg(unix)]
#[test]
fn failed_kiro_cli_effect_rolls_back_the_peer_containing_registry() {
    use crate::agents::host_bundle_v2::{
        HostBundleLifecycleOpV1, HostBundleWriterV1, HostComponentSetTransactionV1,
    };

    let home = tempfile::tempdir().unwrap();
    let lifecycle = tempfile::tempdir().unwrap();
    let bin_dir = tempfile::tempdir().unwrap();
    let kiro_cli = bin_dir.path().join("kiro-cli");
    let log = bin_dir.path().join("invocations.log");
    fake_kiro_cli(&kiro_cli, &log, FAKE_FAIL_AFTER_WRITE_BODY);
    let _path = AmbientPathGuard::set(bin_dir.path());

    let mcp_path = mcp_config_path(home.path());
    std::fs::create_dir_all(mcp_path.parent().unwrap()).unwrap();
    let original = br#"{"mcpServers":{"other":{"command":"other","args":[]}}}"#;
    std::fs::write(&mcp_path, original).unwrap();

    let component_set = kiro_component_set();
    let request = kiro_component_request(HostBundleLifecycleOpV1::Install, [31; 16]);
    let mut writer = HostBundleWriterV1::open_with_lifecycle_root(home.path(), lifecycle.path())
        .expect("host bundle writer must open for an isolated profile");
    let mut registration = crate::agents::host_component_registration::CatalogHostComponentRegistrationAuthority::new_with_tracedecay_bin(
        "kiro",
        home.path(),
        lifecycle.path(),
        request.lifecycle.operation,
        "/bin/tracedecay".to_string(),
    )
    .unwrap();
    let mut transaction = HostComponentSetTransactionV1::new(&mut writer);
    let preview = transaction
        .preview(
            &component_set.component_set,
            &request,
            &component_set,
            &mut registration,
        )
        .expect("the isolated peer-containing registry must preview");
    let _error = transaction
        .execute_confirmed(
            &component_set.component_set,
            &request,
            &preview,
            &component_set,
            &mut registration,
        )
        .expect_err("a failing native command must fail the lifecycle");
    assert_eq!(
        std::fs::read(&mcp_path).unwrap(),
        original,
        "registration rollback must restore the exact peer-containing document"
    );
}

#[cfg(unix)]
#[test]
fn rollback_refuses_a_foreign_registry_write_after_cli_apply() {
    use crate::agents::host_bundle_v2::{
        HostBundleLifecycleOpV1, HostBundleWriterV1, HostComponentSetRegistrationV1,
        HostComponentSetTransactionV1,
    };

    let home = tempfile::tempdir().unwrap();
    let lifecycle = tempfile::tempdir().unwrap();
    let bin_dir = tempfile::tempdir().unwrap();
    let kiro_cli = bin_dir.path().join("kiro-cli");
    let log = bin_dir.path().join("invocations.log");
    fake_kiro_cli(&kiro_cli, &log, FAKE_REGISTRY_BODY);
    let _path = AmbientPathGuard::set(bin_dir.path());

    let mcp_path = mcp_config_path(home.path());
    std::fs::create_dir_all(mcp_path.parent().unwrap()).unwrap();
    let original = br#"{"mcpServers":{"other":{"command":"other","args":[]}}}"#;
    std::fs::write(&mcp_path, original).unwrap();

    let component_set = kiro_component_set();
    let request = kiro_component_request(HostBundleLifecycleOpV1::Install, [32; 16]);
    let mut registration = crate::agents::host_component_registration::CatalogHostComponentRegistrationAuthority::new_with_tracedecay_bin(
        "kiro",
        home.path(),
        lifecycle.path(),
        request.lifecycle.operation,
        "/bin/tracedecay".to_string(),
    )
    .unwrap();
    let mut writer =
        HostBundleWriterV1::open_with_lifecycle_root(home.path(), lifecycle.path()).unwrap();
    let mut transaction = HostComponentSetTransactionV1::new(&mut writer);
    let preview = transaction
        .preview(
            &component_set.component_set,
            &request,
            &component_set,
            &mut registration,
        )
        .unwrap();
    drop(transaction);

    registration
        .confirm_preview(&component_set.component_set, &request, &preview)
        .unwrap();
    registration
        .declare_artifact_writes(&component_set.component_set, &request, &[])
        .unwrap();
    registration
        .preflight(&component_set.component_set, &request)
        .unwrap();
    registration
        .stage(&component_set.component_set, &request)
        .unwrap();
    registration
        .apply(&component_set.component_set, &request)
        .expect("the fake native add must apply");

    let foreign = br#"{"mcpServers":{"foreign":{"command":"operator"}}}"#;
    std::fs::write(&mcp_path, foreign).unwrap();
    let error = registration
        .rollback(&component_set.component_set, &request)
        .expect_err("rollback must refuse to overwrite a later foreign edit");
    assert!(
        matches!(
            error,
            crate::agents::host_bundle_v2::HostBundleError::StalePreview(_)
        ),
        "foreign drift must be typed stale preview: {error}"
    );
    assert_eq!(
        std::fs::read(&mcp_path).unwrap(),
        foreign,
        "a refused rollback must leave the later foreign bytes untouched"
    );
}

#[cfg(unix)]
#[test]
fn a_failing_kiro_registry_command_reports_the_hosts_own_diagnosis() {
    let home = tempfile::tempdir().unwrap();
    let bin_dir = tempfile::tempdir().unwrap();
    let log = bin_dir.path().join("invocations.log");
    let kiro_cli = bin_dir.path().join("kiro-cli");
    fake_kiro_cli(
        &kiro_cli,
        &log,
        "echo 'mcp server tracedecay is not configured' >&2\nexit 7",
    );

    let error = kiro_mcp_remove_with(&kiro_cli, home.path())
        .expect_err("a non-zero host CLI exit must fail the lifecycle");

    let TraceDecayError::Config { message } = error else {
        panic!("a failed host command must surface as a config error");
    };
    assert!(
        message.contains("mcp server tracedecay is not configured")
            && message.contains("exit code 7"),
        "the host's own stderr and status must reach the operator: {message}"
    );
}

#[test]
fn a_missing_kiro_binary_refuses_instead_of_editing_host_owned_state() {
    let home = tempfile::tempdir().unwrap();
    let mcp_path = mcp_config_path(home.path());
    std::fs::create_dir_all(mcp_path.parent().unwrap()).unwrap();
    let operator_owned = br#"{"mcpServers":{"someone-elses":{"command":"other"}}}"#;
    std::fs::write(&mcp_path, operator_owned).unwrap();

    let error =
        crate::agents::host_cli::require_host_cli("kiro-cli-definitely-absent", KIRO_CLI_LIFECYCLE)
            .expect_err("an absent host binary is a hard requirement failure");

    let TraceDecayError::HostCliUnavailable { program, lifecycle } = error else {
        panic!("host CLI absence must surface as a typed requirement");
    };
    assert_eq!(program, "kiro-cli-definitely-absent");
    assert_eq!(lifecycle, KIRO_CLI_LIFECYCLE);
    assert_eq!(
        std::fs::read(&mcp_path).unwrap(),
        operator_owned,
        "a refused lifecycle must not have touched host-owned registry state"
    );
    assert!(
        !config_backup_path(&mcp_path).exists(),
        "a refused lifecycle must not have staged a backup of host-owned registry state"
    );
}

#[test]
fn detected_kiro_without_a_tracedecay_server_is_a_single_optional_warning() {
    let home = tempfile::tempdir().unwrap();
    let mcp_path = mcp_config_path(home.path());
    std::fs::create_dir_all(mcp_path.parent().unwrap()).unwrap();
    std::fs::write(
        &mcp_path,
        br#"{"mcpServers":{"operator":{"command":"other","args":[]}}}"#,
    )
    .unwrap();

    let mut counters = DoctorCounters::new();
    KiroIntegration.healthcheck(
        &mut counters,
        &HealthcheckContext {
            home: home.path().to_path_buf(),
            project_path: home.path().to_path_buf(),
        },
    );

    assert_eq!(counters.issues, 0);
    assert_eq!(counters.warnings, 1);
}

#[test]
fn malformed_kiro_mcp_config_remains_a_doctor_failure() {
    let home = tempfile::tempdir().unwrap();
    let mcp_path = mcp_config_path(home.path());
    std::fs::create_dir_all(mcp_path.parent().unwrap()).unwrap();
    std::fs::write(&mcp_path, "{ not valid JSON").unwrap();

    let mut counters = DoctorCounters::new();
    KiroIntegration.healthcheck(
        &mut counters,
        &HealthcheckContext {
            home: home.path().to_path_buf(),
            project_path: home.path().to_path_buf(),
        },
    );

    assert_eq!(counters.issues, 1);
    assert_eq!(counters.warnings, 0);
}

#[test]
fn an_ambient_kiro_home_never_redirects_an_admitted_profile() {
    struct AmbientKiroHomeGuard {
        previous: Option<std::ffi::OsString>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl AmbientKiroHomeGuard {
        fn set(value: &Path) -> Self {
            let lock = crate::config::lock_user_data_dir_test_env();
            let previous = std::env::var_os("KIRO_HOME");
            // SAFETY: the shared profile-discovery lock is held for the
            // guard's lifetime, so no sibling profile test observes this
            // temporary ambient value.
            unsafe {
                std::env::set_var("KIRO_HOME", value);
            }
            Self {
                previous,
                _lock: lock,
            }
        }
    }

    impl Drop for AmbientKiroHomeGuard {
        fn drop(&mut self) {
            // SAFETY: see `AmbientKiroHomeGuard::set`.
            unsafe {
                match self.previous.take() {
                    Some(previous) => std::env::set_var("KIRO_HOME", previous),
                    None => std::env::remove_var("KIRO_HOME"),
                }
            }
        }
    }

    let home = tempfile::tempdir().unwrap();
    let ambient = tempfile::tempdir().unwrap();
    let _ambient = AmbientKiroHomeGuard::set(ambient.path());
    assert_eq!(
        mcp_config_path(home.path()),
        home.path().join(".kiro/settings/mcp.json")
    );
    assert_ne!(
        mcp_config_path(home.path()),
        ambient.path().join("settings/mcp.json")
    );
}

#[cfg(unix)]
#[test]
fn cli_lifecycle_leaves_an_ambient_kiro_home_sentinel_untouched() {
    let home = tempfile::tempdir().unwrap();
    let ambient = tempfile::tempdir().unwrap();
    let bin_dir = tempfile::tempdir().unwrap();
    let log = bin_dir.path().join("invocations.log");
    let kiro_cli = bin_dir.path().join("kiro-cli");
    fake_kiro_cli(&kiro_cli, &log, FAKE_REGISTRY_BODY);
    let ambient_mcp = ambient.path().join("settings/mcp.json");
    std::fs::create_dir_all(ambient_mcp.parent().unwrap()).unwrap();
    let sentinel = br#"{"mcpServers":{"operator-sentinel":{"command":"keep"}}}"#;
    std::fs::write(&ambient_mcp, sentinel).unwrap();
    let _ambient = {
        struct AmbientKiroHomeGuard {
            previous: Option<std::ffi::OsString>,
            _lock: std::sync::MutexGuard<'static, ()>,
        }

        impl AmbientKiroHomeGuard {
            fn set(value: &Path) -> Self {
                let lock = crate::config::lock_user_data_dir_test_env();
                let previous = std::env::var_os("KIRO_HOME");
                // SAFETY: the shared profile-discovery lock serializes this
                // process-global test environment mutation.
                unsafe { std::env::set_var("KIRO_HOME", value) };
                Self {
                    previous,
                    _lock: lock,
                }
            }
        }

        impl Drop for AmbientKiroHomeGuard {
            fn drop(&mut self) {
                // SAFETY: see `AmbientKiroHomeGuard::set`.
                unsafe {
                    match self.previous.take() {
                        Some(previous) => std::env::set_var("KIRO_HOME", previous),
                        None => std::env::remove_var("KIRO_HOME"),
                    }
                }
            }
        }

        AmbientKiroHomeGuard::set(ambient.path())
    };
    kiro_mcp_add_with(&kiro_cli, home.path(), "/bin/tracedecay")
        .expect("the admitted profile must drive the native CLI");
    assert_eq!(std::fs::read(&ambient_mcp).unwrap(), sentinel);
    assert!(mcp_config_path(home.path()).is_file());
}

#[test]
fn the_cli_raw_args_match_the_config_writers_launch_arguments() {
    let entry = mcp_server_entry("/bin/tracedecay");
    let expected = serde_json::to_value(MCP_SERVER_ARGS).unwrap();
    assert_eq!(
        &expected,
        entry.get("args").unwrap(),
        "the CLI-driven global registration's raw --args values and the workspace-local config \
         writer must launch the same server with the same arguments"
    );
}
