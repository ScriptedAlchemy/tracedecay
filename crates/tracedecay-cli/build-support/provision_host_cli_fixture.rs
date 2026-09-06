//! Shared installer for the compiled host-CLI fixture.
//!
//! Included from CLI unit tests and integration tests via `#[path]`. The
//! binary itself is provisioned by `build.rs`; this module only places a
//! copy on a test-local PATH under the host program name.

use std::path::{Path, PathBuf};

pub fn compiled_host_cli_fixture() -> PathBuf {
    PathBuf::from(env!("TRACEDECAY_HOST_CLI_FIXTURE"))
}

pub fn install_compiled_host_cli_fixture(dir: &Path, program: &str) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let dest = dir.join(format!("{program}{}", std::env::consts::EXE_SUFFIX));
    let src = compiled_host_cli_fixture();
    let _ = std::fs::remove_file(&dest);
    if std::fs::hard_link(&src, &dest).is_err() {
        std::fs::copy(&src, &dest).unwrap();
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(&dest).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&dest, permissions).unwrap();
    }
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
