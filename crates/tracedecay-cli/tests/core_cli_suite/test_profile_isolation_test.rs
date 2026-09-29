//! Guards the suite-wide profile isolation configured in `.cargo/config.toml`.
//!
//! Every cargo-launched test process must resolve TraceDecay storage away
//! from the developer's real `~/.tracedecay`; otherwise tests that index
//! temp fixture repos enroll them into the real profile and contend with a
//! live daemon. This binary intentionally never mutates `TRACEDECAY_DATA_DIR`,
//! so it observes exactly what cargo's `[env]` section provided.

use std::path::{Path, PathBuf};

use tracedecay_runtime_core::config::{ProfileRoot, USER_DATA_DIR_ENV};

#[cfg(unix)]
use crate::common::{
    hermetic_path, in_child_test, rerun_test_in_child, tracedecay_command_with_home,
};
#[cfg(unix)]
use tracedecay_runtime_core::test_executable::write_executable_script;

/// Host CLIs the operator really has installed; a fixture child must never
/// reach one of them through the test process's `PATH`.
#[cfg(unix)]
const REAL_HOST_CLIS: [&str; 5] = ["kimi", "kiro-cli", "droid", "codex", "cursor-agent"];

/// Lifecycle commands: Droid and Codex install through their own CLIs, and
/// Kiro, which edits its documented config file, must launch no CLI at all.
#[cfg(unix)]
const HOST_LIFECYCLE_COMMANDS: [&[&str]; 3] = [
    &["install", "--agent", "kiro"],
    &["install", "--agent", "droid"],
    &["install", "--agent", "codex"],
];

#[cfg(unix)]
const SENTINEL_DIR_ENV: &str = "TRACEDECAY_TEST_AMBIENT_HOST_SENTINELS";

/// Writes an executable per host name that appends its own name to `log`.
#[cfg(unix)]
fn write_host_recorders(dir: &Path, log: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    for name in REAL_HOST_CLIS {
        let path = dir.join(name);
        write_executable_script(
            &path,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"${{0##*/}}\" >> '{}'\nexit 1\n",
                log.display()
            ),
        )
        .unwrap();
    }
}

#[cfg(unix)]
fn recorded_hosts(log: &Path) -> Vec<String> {
    let mut hosts: Vec<String> = std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect();
    hosts.sort();
    hosts.dedup();
    hosts
}

/// Host lifecycle commands run through the shared fixture resolve only the
/// fakes a test admits, never a same-named executable on the test process's
/// own `PATH`: the operator's `~/.local/bin/kimi` and `kiro-cli` sit there.
#[cfg(unix)]
#[test]
fn fixture_children_run_only_admitted_fake_hosts_never_ambient_ones() {
    let Some(sentinel_dir) = std::env::var_os(SENTINEL_DIR_ENV).map(PathBuf::from) else {
        let sentinels = tempfile::tempdir().unwrap();
        write_host_recorders(sentinels.path(), &sentinels.path().join("ran.log"));
        let ambient = std::env::var_os("PATH").unwrap_or_default();
        let path = std::env::join_paths(
            std::iter::once(sentinels.path().to_path_buf()).chain(std::env::split_paths(&ambient)),
        )
        .unwrap();
        rerun_test_in_child(
            "test_profile_isolation_test::fixture_children_run_only_admitted_fake_hosts_never_ambient_ones",
            &[
                ("PATH", Some(path.as_os_str())),
                (SENTINEL_DIR_ENV, Some(sentinels.path().as_os_str())),
            ],
        );
        assert_eq!(
            recorded_hosts(&sentinels.path().join("ran.log")),
            Vec::<String>::new(),
            "a fixture child launched a host CLI from the test process's PATH"
        );
        return;
    };
    assert!(in_child_test());

    let home = tempfile::tempdir().unwrap();
    let fakes = home.path().join("fake-bin");
    let fake_log = home.path().join("fake-ran.log");
    write_host_recorders(&fakes, &fake_log);
    for args in HOST_LIFECYCLE_COMMANDS {
        tracedecay_command_with_home(home.path())
            .args(args)
            .output()
            .unwrap();
    }
    assert_eq!(
        recorded_hosts(&sentinel_dir.join("ran.log")),
        Vec::<String>::new(),
        "the fixture's default PATH resolved an ambient host CLI"
    );
    assert_eq!(recorded_hosts(&fake_log), Vec::<String>::new());

    for args in HOST_LIFECYCLE_COMMANDS {
        tracedecay_command_with_home(home.path())
            .args(args)
            .env("PATH", hermetic_path(&[&fakes]))
            .output()
            .unwrap();
    }
    assert_eq!(recorded_hosts(&fake_log), ["codex", "droid"]);
    assert_eq!(
        recorded_hosts(&sentinel_dir.join("ran.log")),
        Vec::<String>::new(),
        "an admitted fake PATH still reached an ambient host CLI"
    );
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

#[test]
fn resolved_data_dir_is_not_the_real_user_profile() {
    let resolved = ProfileRoot::from_env()
        .expect("the test process profile should resolve")
        .data_dir()
        .to_path_buf();
    let Some(real_profile) = dirs::home_dir().map(|home| home.join(".tracedecay")) else {
        return;
    };

    let resolved = canonical(&resolved);
    let real_profile = canonical(&real_profile);
    assert!(
        !resolved.starts_with(&real_profile),
        "tests resolved TraceDecay storage to the real user profile '{}'; \
         the suite must stay isolated (see the {USER_DATA_DIR_ENV} entry in \
         .cargo/config.toml). If {USER_DATA_DIR_ENV} is set in your shell, \
         unset it or point it away from ~/.tracedecay before running tests.",
        real_profile.display()
    );
}
