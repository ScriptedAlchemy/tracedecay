//! Shared installer for the compiled host-CLI fixture.
//!
//! Included from CLI unit tests and integration tests via `#[path]`. The
//! binary itself is a Cargo example built by the test workflow; this module
//! resolves it relative to the executing test and copies it onto a test-local
//! PATH under the host program name.

#[cfg(unix)]
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

pub fn compiled_host_cli_fixture() -> PathBuf {
    let test_executable = std::env::current_exe().expect("test binary has a current_exe path");
    let profile_dir = test_executable
        .parent()
        .and_then(Path::parent)
        .expect("test binary sits under a Cargo target profile directory");
    profile_dir.join("examples").join(format!(
        "tracedecay-host-cli-fixture{}",
        std::env::consts::EXE_SUFFIX
    ))
}

pub fn install_compiled_host_cli_fixture(dir: &Path, program: &str) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let dest = dir.join(format!("{program}{}", std::env::consts::EXE_SUFFIX));
    let src = compiled_host_cli_fixture();
    let _ = std::fs::remove_file(&dest);
    #[cfg(unix)]
    symlink(&src, &dest).unwrap_or_else(|error| {
        panic!(
            "link compiled host-CLI fixture from {} to {}: {error}; \
             build it with `cargo build -p tracedecay-cli --example \
             tracedecay-host-cli-fixture`",
            src.display(),
            dest.display()
        )
    });
    #[cfg(not(unix))]
    std::fs::copy(&src, &dest).unwrap_or_else(|error| {
        panic!(
            "copy compiled host-CLI fixture from {} to {}: {error}; \
             build it with `cargo build -p tracedecay-cli --example \
             tracedecay-host-cli-fixture`",
            src.display(),
            dest.display()
        )
    });
    dest
}

pub fn looks_like_native_executable(bytes: &[u8]) -> bool {
    if bytes.starts_with(b"#!") || bytes.starts_with(b"@echo") {
        return false;
    }
    bytes.starts_with(&[0x7f, b'E', b'L', b'F'])
        || bytes.starts_with(&[0x4d, 0x5a])
        || bytes.starts_with(&[0xfe, 0xed, 0xfa, 0xce])
        || bytes.starts_with(&[0xfe, 0xed, 0xfa, 0xcf])
        || bytes.starts_with(&[0xce, 0xfa, 0xed, 0xfe])
        || bytes.starts_with(&[0xcf, 0xfa, 0xed, 0xfe])
        || bytes.starts_with(&[0xca, 0xfe, 0xba, 0xbe])
}
