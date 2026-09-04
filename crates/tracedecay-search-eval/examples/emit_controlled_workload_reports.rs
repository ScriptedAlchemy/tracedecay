//! Emit machine-readable framed-log and cursor-parse reports.
//!
//! Used by the search-eval harness to compare capture/private-fs feature-off
//! vs feature-on durable results. This example does not add production
//! annotations.

use sha2::{Digest, Sha256};

const BUILD_IDENTITY_FILE: &str = "controlled-workload-build-identity.json";

fn main() {
    let Some(dir) = std::env::args().nth(1) else {
        eprintln!("usage: emit_controlled_workload_reports <report-dir>");
        std::process::exit(2);
    };
    if let Err(error) =
        tracedecay_search_eval::write_controlled_workload_reports(std::path::Path::new(&dir))
    {
        eprintln!("{error}");
        std::process::exit(1);
    }
    if let Err(error) = write_build_identity(std::path::Path::new(&dir)) {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn write_build_identity(report_dir: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    let executable = std::env::current_exe()?;
    let executable_sha256 = hex::encode(Sha256::digest(std::fs::read(&executable)?));
    let feature_mode = if cfg!(feature = "controlled-workload-hotpath") {
        "hotpath-on"
    } else {
        "hotpath-off"
    };
    let identity = serde_json::json!({
        "feature_mode": feature_mode,
        "executable_sha256": executable_sha256,
    });
    std::fs::write(
        report_dir.join(BUILD_IDENTITY_FILE),
        serde_json::to_vec(&identity)?,
    )?;
    Ok(())
}
