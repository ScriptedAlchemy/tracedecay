//! `bazel run //tools/storage/parity:regen` rewrites the golden receipt trace.
//!
//! It runs the golden test itself in regeneration mode, so goldens always come
//! from the exact code the test asserts with. The test launcher expects the
//! variables `bazel test` sets, which `bazel run` does not, so they are
//! supplied here.

use std::{env, path::PathBuf, process::Command};

const PACKAGE: &str = "crates/tracedecay-storage-parity";

fn main() {
    let workspace = env::var_os("BUILD_WORKSPACE_DIRECTORY")
        .map(PathBuf::from)
        .expect("run through `bazel run`, which sets BUILD_WORKSPACE_DIRECTORY");
    let runfiles = env::var_os("RUNFILES_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let mut path = env::current_exe().expect("regen executable path");
            path.as_mut_os_string().push(".runfiles");
            path
        });
    let scratch = tempfile_dir();
    let status = Command::new(
        runfiles
            .join("_main")
            .join(PACKAGE)
            .join("golden_receipt_trace"),
    )
    .env("TEST_SRCDIR", &runfiles)
    .env("TEST_WORKSPACE", "_main")
    .env("TEST_TMPDIR", &scratch)
    .env("TMPDIR", &scratch)
    .env(
        "TRACEDECAY_PARITY_REGEN_DIR",
        workspace.join(PACKAGE).join("fixtures/golden"),
    )
    .status()
    .expect("run the golden receipt trace test");
    let _ = std::fs::remove_dir_all(&scratch);
    assert!(status.success(), "golden regeneration failed: {status}");
}

fn tempfile_dir() -> PathBuf {
    let path = env::temp_dir().join(format!("tracedecay-parity-regen-{}", std::process::id()));
    std::fs::create_dir_all(&path).expect("regen scratch directory");
    path
}
