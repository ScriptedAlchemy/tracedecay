//! Compile the std-only host-CLI fixture into a real native executable.
//!
//! Unit tests of `tracedecay` cannot rely on a sibling `[[bin]]` being built,
//! so `build.rs` provisions the helper with `rustc` and exports its path
//! through `TRACEDECAY_HOST_CLI_FIXTURE`.

use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const FIXTURE_SOURCE: &str = "build-support/host_cli_fixture.rs";

pub(crate) fn compile_host_cli_fixture(
    out_dir: &Path,
    product_version: &str,
) -> Result<PathBuf, Box<dyn Error>> {
    println!("cargo::rerun-if-changed={FIXTURE_SOURCE}");
    let generated = out_dir.join("host_cli_fixture.rs");
    let source = format!(
        "const PRODUCT_VERSION: &str = {product_version:?};\n{}",
        fs::read_to_string(FIXTURE_SOURCE)?
    );
    if !matches!(fs::read_to_string(&generated), Ok(current) if current == source) {
        fs::write(&generated, source)?;
    }

    let dest = out_dir.join(format!(
        "tracedecay-host-cli-fixture{}",
        std::env::consts::EXE_SUFFIX
    ));
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let mut command = Command::new(rustc);
    command
        .arg("--edition=2024")
        .arg("--crate-name")
        .arg("tracedecay_host_cli_fixture")
        .arg("-C")
        .arg("opt-level=1")
        .arg("-C")
        .arg("debuginfo=0")
        .arg("-o")
        .arg(&dest)
        .arg(&generated);
    let host = std::env::var("HOST").unwrap_or_default();
    let target = std::env::var("TARGET").unwrap_or_default();
    if !target.is_empty() && target != host {
        command.arg("--target").arg(&target);
    }
    let output = command.output()?;
    if !output.status.success() {
        return Err(format!(
            "failed to compile host-CLI fixture with rustc\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    println!(
        "cargo::rustc-env=TRACEDECAY_HOST_CLI_FIXTURE={}",
        dest.display()
    );
    Ok(dest)
}
