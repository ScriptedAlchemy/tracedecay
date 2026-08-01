//! Bounded filesystem discovery for transcript enumeration.
//!
//! Limits are derived from existing PR6 host-admission / ingest constants so
//! discovery cannot invent tighter arbitrary caps. Bounds are enforced before
//! a candidate path is retained, and directory symlinks are not followed.

use std::path::{Path, PathBuf};

use crate::admission::{
    DEFAULT_MAX_RECORD_BYTES, DEFAULT_MAX_RECORDS, DEFAULT_MAX_SPOOL_BYTES,
};

/// Fixed metadata charge per directory entry (filetype/stat bookkeeping).
///
/// Discovery never materializes full file contents; this charges a stable
/// upper bound for the `Metadata` / `FileType` values touched per entry.
const ENTRY_METADATA_CHARGE_BYTES: u64 = std::mem::size_of::<std::fs::Metadata>() as u64;

/// Allocation bounds for one transcript discovery walk or path-list filter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TranscriptDiscoveryBounds {
    /// Maximum retained file paths (units).
    pub max_files: usize,
    /// Maximum native path bytes for one retained candidate.
    pub max_path_bytes: usize,
    /// Maximum metadata bytes charged for one directory entry.
    pub max_metadata_bytes: u64,
    /// Cumulative path + metadata budget for the whole walk / filter.
    pub max_discovery_bytes: u64,
}

impl TranscriptDiscoveryBounds {
    /// Derive walk bounds from an ingest discovery unit cap.
    ///
    /// - `max_files` ← ingest `discovered_units` (default family: [`DEFAULT_MAX_RECORDS`])
    /// - `max_path_bytes` ← host-admission record bytes (1 MiB)
    /// - `max_metadata_bytes` ← host-admission record bytes (1 MiB)
    /// - `max_discovery_bytes` ← min(`max_files * max_path_bytes`, [`DEFAULT_MAX_SPOOL_BYTES`])
    pub fn from_discovered_units(discovered_units: usize) -> Self {
        let max_files = discovered_units.max(1);
        let max_path_bytes = DEFAULT_MAX_RECORD_BYTES;
        let max_metadata_bytes = u64::try_from(DEFAULT_MAX_RECORD_BYTES).unwrap_or(u64::MAX);
        let path_budget =
            u64::try_from(max_files.saturating_mul(max_path_bytes)).unwrap_or(u64::MAX);
        let spool_budget = u64::try_from(DEFAULT_MAX_SPOOL_BYTES).unwrap_or(u64::MAX);
        Self {
            max_files,
            max_path_bytes,
            max_metadata_bytes,
            max_discovery_bytes: path_budget.min(spool_budget),
        }
    }

    /// Default walk bounds aligned with production ingest discovery (4096 units).
    pub fn default_walk() -> Self {
        Self::from_discovered_units(DEFAULT_MAX_RECORDS)
    }
}

/// Which discovery budget stopped retention.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileDiscoveryLimit {
    FileCount,
    PathBytes,
    MetadataBytes,
    DiscoveryBytes,
}

/// Typed discovery result without embedding oversized path payloads in errors.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileDiscoveryReport {
    pub paths: Vec<PathBuf>,
    /// Set when a hard bound stopped further retention (typed backpressure).
    pub truncated: Option<FileDiscoveryLimit>,
    /// Entries skipped because a single path/metadata charge exceeded its cap.
    pub skipped_oversized_entries: u64,
    /// Cumulative path + metadata bytes charged for retained + skipped decisions.
    pub bytes_charged: u64,
}

impl FileDiscoveryReport {
    pub fn is_truncated(&self) -> bool {
        self.truncated.is_some()
    }
}

/// Native path byte length without allocating a lossy `String`.
pub fn path_byte_len(path: &Path) -> usize {
    os_str_byte_len(path.as_os_str())
}

pub fn os_str_byte_len(value: &std::ffi::OsStr) -> usize {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        value.as_bytes().len()
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        value
            .encode_wide()
            .count()
            .saturating_mul(std::mem::size_of::<u16>())
    }
    #[cfg(not(any(unix, windows)))]
    {
        value.len()
    }
}

/// Apply discovery bounds to an already-materialized path list.
///
/// Prefer [`collect_files_with_ext_bounded`] for filesystem walks so bounds are
/// enforced before collection. This helper exists for trait defaults and tests.
pub fn bound_path_list(
    paths: impl IntoIterator<Item = PathBuf>,
    bounds: TranscriptDiscoveryBounds,
) -> FileDiscoveryReport {
    let mut out = Vec::new();
    let mut truncated = None;
    let mut skipped_oversized_entries = 0u64;
    let mut bytes_charged = 0u64;

    for path in paths {
        if out.len() >= bounds.max_files {
            truncated = Some(FileDiscoveryLimit::FileCount);
            break;
        }
        let path_bytes = path_byte_len(&path);
        if path_bytes > bounds.max_path_bytes {
            skipped_oversized_entries = skipped_oversized_entries.saturating_add(1);
            truncated = truncated.or(Some(FileDiscoveryLimit::PathBytes));
            continue;
        }
        let path_charge = u64::try_from(path_bytes).unwrap_or(u64::MAX);
        let meta_charge = ENTRY_METADATA_CHARGE_BYTES;
        if meta_charge > bounds.max_metadata_bytes {
            skipped_oversized_entries = skipped_oversized_entries.saturating_add(1);
            truncated = truncated.or(Some(FileDiscoveryLimit::MetadataBytes));
            continue;
        }
        let entry_charge = path_charge.saturating_add(meta_charge);
        if bytes_charged.saturating_add(entry_charge) > bounds.max_discovery_bytes {
            truncated = Some(FileDiscoveryLimit::DiscoveryBytes);
            break;
        }
        bytes_charged = bytes_charged.saturating_add(entry_charge);
        out.push(path);
    }

    FileDiscoveryReport {
        paths: out,
        truncated,
        skipped_oversized_entries,
        bytes_charged,
    }
}

/// Recursively collect files with `ext` under `dir`, enforcing discovery bounds
/// before retaining each path. Directory symlinks are not followed.
pub fn collect_files_with_ext_bounded(
    dir: &Path,
    ext: &str,
    max_depth: u8,
    bounds: TranscriptDiscoveryBounds,
) -> FileDiscoveryReport {
    let mut state = WalkState {
        bounds,
        ext,
        max_depth,
        paths: Vec::new(),
        truncated: None,
        skipped_oversized_entries: 0,
        bytes_charged: 0,
    };
    state.walk(dir, 0);
    FileDiscoveryReport {
        paths: state.paths,
        truncated: state.truncated,
        skipped_oversized_entries: state.skipped_oversized_entries,
        bytes_charged: state.bytes_charged,
    }
}

struct WalkState<'a> {
    bounds: TranscriptDiscoveryBounds,
    ext: &'a str,
    max_depth: u8,
    paths: Vec<PathBuf>,
    truncated: Option<FileDiscoveryLimit>,
    skipped_oversized_entries: u64,
    bytes_charged: u64,
}

impl WalkState<'_> {
    fn walk(&mut self, dir: &Path, depth: u8) {
        if self.truncated.is_some() {
            return;
        }
        if depth > self.max_depth {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        // Stream DirEntry values; never collect the whole directory into a Vec.
        for entry in entries.flatten() {
            if self.truncated.is_some() {
                return;
            }
            let file_name = entry.file_name();
            let name_bytes = os_str_byte_len(&file_name);
            if name_bytes > self.bounds.max_path_bytes {
                // Reject before joining/materializing an oversized path component.
                self.skipped_oversized_entries = self.skipped_oversized_entries.saturating_add(1);
                continue;
            }
            let meta_charge = ENTRY_METADATA_CHARGE_BYTES;
            if meta_charge > self.bounds.max_metadata_bytes {
                self.skipped_oversized_entries = self.skipped_oversized_entries.saturating_add(1);
                self.truncated = Some(FileDiscoveryLimit::MetadataBytes);
                return;
            }
            if self.bytes_charged.saturating_add(meta_charge) > self.bounds.max_discovery_bytes {
                self.truncated = Some(FileDiscoveryLimit::DiscoveryBytes);
                return;
            }
            // Charge metadata for every examined entry before materializing a
            // retained path (and before recursing).
            self.bytes_charged = self.bytes_charged.saturating_add(meta_charge);

            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            // Rebuild from the caller's root spelling. On Windows, DirEntry::path
            // may add an extended-length prefix, which would create a second
            // durable cursor key for the same transcript.
            let path = dir.join(&file_name);

            if file_type.is_symlink() {
                // Do not follow directory symlinks (cycles / tree escape).
                // Symlink-to-file candidates are included by link-path extension
                // only when path/discovery budgets still allow retention.
                if path.extension().and_then(|e| e.to_str()) == Some(self.ext) {
                    self.try_retain(path);
                }
                continue;
            }
            if file_type.is_dir() {
                self.walk(&path, depth.saturating_add(1));
                continue;
            }
            if file_type.is_file() && path.extension().and_then(|e| e.to_str()) == Some(self.ext) {
                self.try_retain(path);
            }
        }
    }

    fn try_retain(&mut self, path: PathBuf) {
        if self.truncated.is_some() {
            return;
        }
        if self.paths.len() >= self.bounds.max_files {
            self.truncated = Some(FileDiscoveryLimit::FileCount);
            return;
        }
        let path_bytes = path_byte_len(&path);
        if path_bytes > self.bounds.max_path_bytes {
            self.skipped_oversized_entries = self.skipped_oversized_entries.saturating_add(1);
            // Oversized single paths are non-durable skips; keep walking peers.
            return;
        }
        let path_charge = u64::try_from(path_bytes).unwrap_or(u64::MAX);
        if self.bytes_charged.saturating_add(path_charge) > self.bounds.max_discovery_bytes {
            self.truncated = Some(FileDiscoveryLimit::DiscoveryBytes);
            return;
        }
        self.bytes_charged = self.bytes_charged.saturating_add(path_charge);
        self.paths.push(path);
    }
}
