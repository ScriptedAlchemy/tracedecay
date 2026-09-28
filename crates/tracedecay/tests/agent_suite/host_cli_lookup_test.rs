//! In-process host lifecycles resolve host CLIs only through what a
//! `HostProgramSearchPathGuard` admits, never through the test process's own
//! `PATH`, which carries the operator's real host CLIs.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use tracedecay_agent_hosts::agents::host_bundle::{
    HostBundleError, HostBundleLifecycleOpV1, HostBundleWriterV1,
    HostComponentSetExecutionRequestV1, HostComponentSetLifecycleRequestV1,
    HostComponentSetTransactionV1, HostComponentV1, HostKindV1,
};
use tracedecay_agent_hosts::agents::host_bundle_registry::verified_embedded_host_component_set_with_tracedecay_bin;
use tracedecay_agent_hosts::agents::host_component_registration::CatalogHostComponentRegistrationAuthority;
use tracedecay_domain::errors::HostAbsence;
use tracedecay_runtime_core::config::{HostProgramSearchPathGuard, ProfileRoot};
use tracedecay_runtime_core::test_executable::write_executable_script;

use crate::common::{in_child_test, rerun_test_in_child};

const SENTINEL_DIR_ENV: &str = "TRACEDECAY_TEST_KIRO_SENTINEL_DIR";
const TRACEDECAY_BIN: &str = "/bin/tracedecay";

/// A `kiro-cli` that records its arguments to `launches.log` and exits 0.
fn write_sentinel_kiro_cli(dir: &Path) {
    let path = dir.join("kiro-cli");
    write_executable_script(
        &path,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n",
            dir.join("launches.log").display()
        ),
    )
    .unwrap();
}

/// The canonical Kiro install transaction `tracedecay install --agent kiro`
/// drives, against an isolated home.
fn install_kiro(home: &Path) -> Result<(), HostBundleError> {
    let lifecycle = home.join("lifecycle");
    std::fs::create_dir_all(home.join(".kiro")).unwrap();
    std::fs::create_dir_all(&lifecycle).unwrap();
    let component_set = verified_embedded_host_component_set_with_tracedecay_bin(
        HostKindV1::Kiro,
        &[HostComponentV1::ContextMcp],
        0,
        TRACEDECAY_BIN,
        "0123456789abcdef0123456789abcdef01234567",
    )
    .unwrap();
    let request = HostComponentSetExecutionRequestV1 {
        lifecycle: HostComponentSetLifecycleRequestV1 {
            operation: HostBundleLifecycleOpV1::Install,
            expected_host: HostKindV1::Kiro,
            expected_components: vec![HostComponentV1::ContextMcp],
            explicit_confirmation: true,
            hermes_profile_bindings: 0,
            explicit_adoption: false,
        },
        operation_id: [7; 16],
    };
    let mut writer = HostBundleWriterV1::open_with_lifecycle_root(home, &lifecycle)?;
    let mut registration = CatalogHostComponentRegistrationAuthority::new_with_tracedecay_bin(
        &ProfileRoot::under_home(home),
        "kiro",
        home,
        request.lifecycle.operation,
        TRACEDECAY_BIN.to_string(),
    )
    .unwrap();
    let mut transaction = HostComponentSetTransactionV1::new(&mut writer);
    let preview = transaction.preview(
        &component_set.component_set,
        &request,
        &component_set,
        &mut registration,
    )?;
    transaction
        .execute_confirmed(
            &component_set.component_set,
            &request,
            &preview,
            &component_set,
            &mut registration,
        )
        .map(drop)
}

fn launches(sentinel: &Path) -> String {
    std::fs::read_to_string(sentinel.join("launches.log")).unwrap_or_default()
}

#[test]
fn kiro_install_never_launches_a_kiro_cli_found_only_on_the_test_process_path() {
    if !in_child_test() {
        let sentinel = tempfile::tempdir().unwrap();
        write_sentinel_kiro_cli(sentinel.path());
        let mut path = OsString::from(sentinel.path());
        if let Some(ambient) = std::env::var_os("PATH") {
            path.push(":");
            path.push(ambient);
        }
        rerun_test_in_child(
            "host_cli_lookup_test::kiro_install_never_launches_a_kiro_cli_found_only_on_the_test_process_path",
            &[
                ("PATH", Some(path.as_os_str())),
                (SENTINEL_DIR_ENV, Some(sentinel.path().as_os_str())),
            ],
        );
        return;
    }
    let sentinel = PathBuf::from(std::env::var_os(SENTINEL_DIR_ENV).unwrap());
    let home = tempfile::tempdir().unwrap();

    let outcome = install_kiro(home.path());
    assert_eq!(
        launches(&sentinel),
        "",
        "an in-process test launched the kiro-cli on the process PATH"
    );
    assert_eq!(
        outcome,
        Err(HostBundleError::HostAbsent {
            host: HostKindV1::Kiro,
            absence: HostAbsence::NotInstalled,
            detail: "host CLI `kiro-cli` is unavailable for kiro MCP registry lifecycle; \
                     install it or add it to PATH and retry"
                .to_string(),
        }),
        "a kiro-cli reachable only through the process PATH is not installed for an in-process test"
    );

    let admitted_home = tempfile::tempdir().unwrap();
    let _guard = HostProgramSearchPathGuard::set(&sentinel);
    let _ = install_kiro(admitted_home.path());
    assert_eq!(
        launches(&sentinel),
        "mcp list\nmcp add --name tracedecay --command /bin/tracedecay --args serve --scope global --force\n",
        "the same sentinel runs once a guard admits its directory"
    );
}
