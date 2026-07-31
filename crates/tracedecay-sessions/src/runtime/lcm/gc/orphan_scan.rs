use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::db::engine::Executor;

use super::super::util;
use super::{LcmError, LcmGcConfig, LcmGcReport, payload, stage_payload_delete};

pub(super) fn preview_orphan_files(
    dir: &Path,
    metadata_refs: &BTreeSet<String>,
    now: i64,
    cfg: &LcmGcConfig,
    remaining: &mut usize,
    report: &mut LcmGcReport,
) -> Result<(), LcmError> {
    let candidates = orphan_file_candidates(dir, metadata_refs, now, cfg, report)?;
    for (payload_ref, bytes) in candidates {
        if *remaining == 0 {
            report.batch_cap(1);
            continue;
        }
        report.orphans.add(&payload_ref, bytes);
        *remaining -= 1;
    }
    Ok(())
}

fn orphan_file_candidates(
    dir: &Path,
    metadata_refs: &BTreeSet<String>,
    now: i64,
    cfg: &LcmGcConfig,
    report: &mut LcmGcReport,
) -> Result<Vec<(String, u64)>, LcmError> {
    let entries = fs::read_dir(dir)
        .map_err(|err| LcmError::Io(err.to_string()))?
        .map(|entry| entry.map(|entry| (entry.file_name(), entry.path())));
    Ok(orphan_file_candidates_from_entries(
        dir,
        metadata_refs,
        now,
        cfg,
        report,
        entries,
        |path| fs::symlink_metadata(path),
    ))
}

fn orphan_file_candidates_from_entries<I, M>(
    dir: &Path,
    metadata_refs: &BTreeSet<String>,
    now: i64,
    cfg: &LcmGcConfig,
    report: &mut LcmGcReport,
    entries: I,
    mut metadata_for: M,
) -> Vec<(String, u64)>
where
    I: IntoIterator<Item = std::io::Result<(std::ffi::OsString, PathBuf)>>,
    M: FnMut(&Path) -> std::io::Result<fs::Metadata>,
{
    let mut candidates = Vec::new();
    for entry in entries {
        let (file_name, path) = match entry {
            Ok(entry) => entry,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => {
                report.add_error("<directory-entry>", "orphan_entry_failed", err.to_string());
                continue;
            }
        };
        let name = file_name.to_string_lossy().to_string();
        if !is_payload_filename(&name) || payload::validate_payload_ref(&name).is_err() {
            continue;
        }
        if metadata_refs.contains(&name) {
            continue;
        }
        if let Err(err) = payload::ensure_contained(dir, &path) {
            report.add_error(&name, "orphan_path_invalid", err.to_string());
            continue;
        }
        let metadata = match metadata_for(&path) {
            Ok(metadata) => metadata,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => {
                report.add_error(&name, "orphan_stat_failed", err.to_string());
                continue;
            }
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            continue;
        }
        let age = now.saturating_sub(util::file_mtime_seconds(&metadata));
        if age < cfg.grace_seconds as i64 {
            report.deferred.count += 1;
            report
                .deferred
                .reason
                .get_or_insert_with(|| "within_grace".to_string());
            continue;
        }
        candidates.push((name, metadata.len()));
    }
    candidates
}

pub(super) async fn stage_orphan_files(
    conn: &(impl Executor + ?Sized),
    dir: &Path,
    metadata_refs: &BTreeSet<String>,
    now: i64,
    cfg: &LcmGcConfig,
    remaining: &mut usize,
    report: &mut LcmGcReport,
) -> Result<(), LcmError> {
    let candidates = orphan_file_candidates(dir, metadata_refs, now, cfg, report)?;
    for (payload_ref, _bytes) in candidates {
        if *remaining == 0 {
            report.batch_cap(1);
            continue;
        }
        let (content_hash, verified_bytes, verified_chars) =
            match payload::payload_file_fingerprint(dir, &payload_ref) {
                Ok(fingerprint) => fingerprint,
                Err(err) => {
                    report.add_error(&payload_ref, "orphan_stage_failed", err.to_string());
                    continue;
                }
            };
        stage_payload_delete(
            conn,
            &payload_ref,
            Some(&content_hash),
            verified_bytes,
            verified_chars,
        )
        .await?;
        report.orphans.add(&payload_ref, verified_bytes);
        *remaining -= 1;
    }
    Ok(())
}

/// True when the payload file exists as a regular (non-symlink) file; a
/// missing payload directory means no payload file can exist.
pub(super) fn payload_file_present(
    dir: Option<&Path>,
    payload_ref: &str,
) -> Result<bool, LcmError> {
    payload_file_present_with(dir, payload_ref, |path| fs::symlink_metadata(path))
}

fn payload_file_present_with<M>(
    dir: Option<&Path>,
    payload_ref: &str,
    metadata_for: M,
) -> Result<bool, LcmError>
where
    M: FnOnce(&Path) -> std::io::Result<fs::Metadata>,
{
    let Some(dir) = dir else {
        return Ok(false);
    };
    let path = dir.join(payload_ref);
    payload::ensure_contained(dir, &path)?;
    match metadata_for(&path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => Ok(true),
        Ok(_) => Err(LcmError::InvalidPayloadRef),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(LcmError::Io(err.to_string())),
    }
}

fn is_payload_filename(name: &str) -> bool {
    name.len() == "payload_".len() + 64 + ".payload".len()
        && name.starts_with("payload_")
        && name.ends_with(".payload")
        && name["payload_".len().."payload_".len() + 64]
            .chars()
            .all(|ch| ch.is_ascii_hexdigit() && !ch.is_ascii_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROVIDER: &str = "codex";
    const PRIMARY_REF: &str =
        "payload_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.payload";
    const SECONDARY_REF: &str =
        "payload_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb.payload";

    #[test]
    fn payload_file_present_propagates_permission_denied() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|err| err.to_string())?;
        let error = payload_file_present_with(Some(temp.path()), PRIMARY_REF, |_| {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "injected unreadable payload",
            ))
        })
        .unwrap_err();
        assert!(matches!(error, LcmError::Io(detail) if detail.contains("injected unreadable")));
        Ok(())
    }

    #[test]
    fn orphan_scan_isolates_entry_errors_and_stat_races() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|err| err.to_string())?;
        let healthy_path = temp.path().join(SECONDARY_REF);
        fs::write(&healthy_path, b"healthy orphan").map_err(|err| err.to_string())?;
        let mtime = util::file_mtime_seconds(
            &fs::symlink_metadata(&healthy_path).map_err(|err| err.to_string())?,
        );
        let cfg = LcmGcConfig {
            grace_seconds: LcmGcConfig::MIN_GRACE_SECONDS,
            ..Default::default()
        }
        .normalized();
        let mut report = LcmGcReport::new(PROVIDER, None, &cfg, false, mtime);
        let entries = vec![
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "injected unreadable entry",
            )),
            Ok((PRIMARY_REF.into(), temp.path().join(PRIMARY_REF))),
            Ok((SECONDARY_REF.into(), healthy_path)),
        ];

        let candidates = orphan_file_candidates_from_entries(
            temp.path(),
            &BTreeSet::new(),
            mtime + LcmGcConfig::MIN_GRACE_SECONDS as i64,
            &cfg,
            &mut report,
            entries,
            |path| fs::symlink_metadata(path),
        );

        assert_eq!(candidates, [(SECONDARY_REF.to_string(), 14)]);
        assert_eq!(report.errors.len(), 1);
        assert_eq!(report.errors[0].kind, "orphan_entry_failed");
        Ok(())
    }
}
