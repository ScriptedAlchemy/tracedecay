//! Guards the suite-wide profile isolation configured in `.cargo/config.toml`.
//!
//! Every cargo-launched test process must resolve TraceDecay storage away
//! from the developer's real `~/.tracedecay`; otherwise tests that index
//! temp fixture repos enroll them into the real profile and contend with a
//! live daemon. This binary intentionally never mutates `TRACEDECAY_DATA_DIR`,
//! so it observes exactly what cargo's `[env]` section provided.

use std::path::{Path, PathBuf};

use tracedecay::config::{user_data_dir, USER_DATA_DIR_ENV};

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

#[test]
fn cargo_env_pins_data_dir_for_tests() {
    let value = std::env::var_os(USER_DATA_DIR_ENV).unwrap_or_default();
    assert!(
        !value.is_empty(),
        "{USER_DATA_DIR_ENV} must be set for cargo-launched test processes; \
         expected the [env] entry in .cargo/config.toml to provide it. \
         Run tests through cargo/nextest from the workspace root."
    );
}

#[test]
fn resolved_data_dir_is_not_the_real_user_profile() {
    let resolved = user_data_dir().expect("user_data_dir should resolve in tests");
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
