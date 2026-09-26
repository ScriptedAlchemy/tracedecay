//! Stable macOS ad-hoc code-signing identity for the tracedecay executable.
//!
//! rustc's linker ad-hoc signs each binary using the hashed deps filename
//! (`tracedecay-<hash>`). macOS TCC treats that identifier as the app, so
//! every local rebuild re-prompts for removable volumes and files, and the
//! daemon blocks in `open()` until the prompt is answered.
//!
//! [`stabilize_installed_executable`] replaces an unsigned or ad-hoc signature
//! with [`STABLE_SIGNING_IDENTIFIER`]. A Developer ID or other team signature
//! is left in place. Release workflows do not Apple-sign; this runs only for
//! the installed file after its archive checksum has matched.

use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::process::Command;

use tracedecay_domain::errors::{Result, TraceDecayError};

/// Bundle id shared with the install script and the macOS link wrapper.
pub(crate) const STABLE_SIGNING_IDENTIFIER: &str = "dev.tracedecay.cli";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MacosSignaturePlan {
    /// Already `dev.tracedecay.cli`, or signed by a team.
    Preserve,
    /// Unsigned or ad-hoc with an unstable identifier.
    ApplyStableAdhoc,
}

/// Decide from `codesign -dv` output. `report` is the combined stderr and stdout.
pub(crate) fn plan_macos_signature(report: &str) -> MacosSignaturePlan {
    let mut identifier = None;
    let mut foreign = false;
    for line in report.lines() {
        if let Some(value) = line.strip_prefix("Identifier=") {
            identifier = Some(value.trim());
        } else if line.starts_with("Authority=") {
            foreign = true;
        } else if let Some(team) = line.strip_prefix("TeamIdentifier=") {
            let team = team.trim();
            if !team.is_empty() && team != "not set" {
                foreign = true;
            }
        }
    }
    if foreign || identifier == Some(STABLE_SIGNING_IDENTIFIER) {
        MacosSignaturePlan::Preserve
    } else {
        MacosSignaturePlan::ApplyStableAdhoc
    }
}

pub(crate) fn is_macho_header(header: &[u8]) -> bool {
    matches!(
        header,
        [0xfe, 0xed, 0xfa, 0xce]
            | [0xce, 0xfa, 0xed, 0xfe]
            | [0xfe, 0xed, 0xfa, 0xcf]
            | [0xcf, 0xfa, 0xed, 0xfe]
            | [0xca, 0xfe, 0xba, 0xbe]
            | [0xbe, 0xba, 0xfe, 0xca]
            | [0xca, 0xfe, 0xba, 0xbf]
            | [0xbf, 0xba, 0xfe, 0xca]
    )
}

/// Give an installed Mach-O the stable ad-hoc identifier on macOS.
///
/// `enabled` is split from `cfg!` so this body stays reachable on every host
/// the crate is type-checked on. Production passes `cfg!(target_os = "macos")`.
pub(crate) fn stabilize_installed_executable(path: &Path) -> Result<()> {
    apply_stable_adhoc_signature(path, cfg!(target_os = "macos"))
}

fn apply_stable_adhoc_signature(path: &Path, enabled: bool) -> Result<()> {
    if !enabled {
        return Ok(());
    }
    let header = read_header(path)?;
    if !is_macho_header(&header) {
        return Ok(());
    }
    let report = codesign_report(path)?;
    if plan_macos_signature(&report) == MacosSignaturePlan::Preserve {
        return Ok(());
    }
    let output = Command::new("codesign")
        .args([
            "--force",
            "--sign",
            "-",
            "--identifier",
            STABLE_SIGNING_IDENTIFIER,
        ])
        .arg(path)
        .output()
        .map_err(|error| TraceDecayError::Config {
            message: format!(
                "cannot ad-hoc sign {} as {STABLE_SIGNING_IDENTIFIER}: {error}",
                path.display()
            ),
        })?;
    if output.status.success() {
        return Ok(());
    }
    let detail = String::from_utf8_lossy(&output.stderr);
    let detail = detail.trim();
    Err(TraceDecayError::Config {
        message: format!(
            "cannot ad-hoc sign {} as {STABLE_SIGNING_IDENTIFIER}: {detail}",
            path.display()
        ),
    })
}

fn read_header(path: &Path) -> Result<[u8; 4]> {
    let mut file = File::open(path).map_err(|error| TraceDecayError::Config {
        message: format!(
            "cannot read {} to inspect its code signature: {error}",
            path.display()
        ),
    })?;
    let mut header = [0_u8; 4];
    let read = file
        .read(&mut header)
        .map_err(|error| TraceDecayError::Config {
            message: format!(
                "cannot read {} to inspect its code signature: {error}",
                path.display()
            ),
        })?;
    if read < header.len() {
        return Ok([0; 4]);
    }
    Ok(header)
}

fn codesign_report(path: &Path) -> Result<String> {
    let output = Command::new("codesign")
        .args(["-dv", "--verbose=2"])
        .arg(path)
        .output()
        .map_err(|error| TraceDecayError::Config {
            message: format!(
                "cannot read the code signature of {}: {error}",
                path.display()
            ),
        })?;
    let mut report = String::from_utf8_lossy(&output.stderr).into_owned();
    if !report.is_empty() && !report.ends_with('\n') {
        report.push('\n');
    }
    report.push_str(&String::from_utf8_lossy(&output.stdout));
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::{
        MacosSignaturePlan, STABLE_SIGNING_IDENTIFIER, is_macho_header, plan_macos_signature,
    };

    #[test]
    fn hashed_adhoc_identifier_is_replaced() {
        let report = "\
Identifier=tracedecay-3d1e6be7cae777a9
Format=Mach-O thin (arm64)
Signature=adhoc
TeamIdentifier=not set
";
        assert_eq!(
            plan_macos_signature(report),
            MacosSignaturePlan::ApplyStableAdhoc
        );
    }

    #[test]
    fn unsigned_binary_is_signed() {
        let report = "/tmp/tracedecay: code object is not signed at all\n";
        assert_eq!(
            plan_macos_signature(report),
            MacosSignaturePlan::ApplyStableAdhoc
        );
    }

    #[test]
    fn stable_identifier_is_preserved() {
        let report = format!("Identifier={STABLE_SIGNING_IDENTIFIER}\nSignature=adhoc\n");
        assert_eq!(plan_macos_signature(&report), MacosSignaturePlan::Preserve);
    }

    #[test]
    fn developer_id_authority_is_preserved() {
        let report = "\
Identifier=tracedecay-3d1e6be7cae777a9
Authority=Developer ID Application: Example (TEAMID1234)
TeamIdentifier=TEAMID1234
";
        assert_eq!(plan_macos_signature(report), MacosSignaturePlan::Preserve);
    }

    #[test]
    fn team_identifier_without_an_authority_line_is_preserved() {
        let report = "\
Identifier=com.example.legacy
TeamIdentifier=TEAMID1234
";
        assert_eq!(plan_macos_signature(report), MacosSignaturePlan::Preserve);
    }

    #[test]
    fn macho_magic_covers_thin_and_fat_headers() {
        assert!(is_macho_header(&[0xcf, 0xfa, 0xed, 0xfe]));
        assert!(is_macho_header(&[0xfe, 0xed, 0xfa, 0xcf]));
        assert!(is_macho_header(&[0xca, 0xfe, 0xba, 0xbe]));
        assert!(is_macho_header(&[0xca, 0xfe, 0xba, 0xbf]));
        assert!(!is_macho_header(b"#!/b"));
        assert!(!is_macho_header(&[0x7f, 0x45, 0x4c, 0x46]));
    }
}
