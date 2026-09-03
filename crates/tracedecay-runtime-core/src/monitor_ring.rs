//! Global memory-mapped ring buffer for live token-savings monitoring.
//!
//! The mmap lives at `monitor.mmap` inside the user-level data dir
//! (`~/.tracedecay/` by default) so a single TUI can show activity from every
//! project on the machine. Multiple MCP
//! server instances (one per project) write concurrently using file locking.
//!
//! Entry format is generic: each entry carries a **prefix** (tool suite
//! name, e.g. "tracedecay"), a **project** (folder name), and a
//! **`tool_name`** (the specific MCP call).

use std::path::{Path, PathBuf};

// ── Layout constants ────────────────────────────────────────────────
const HEADER_SIZE: usize = 32;
const ENTRY_SIZE: usize = 128;
/// Number of ring-buffer slots in the monitor mmap.
pub const RING_CAPACITY: usize = 256;
/// Total on-disk size of the monitor mmap file.
pub const FILE_SIZE: usize = HEADER_SIZE + ENTRY_SIZE * RING_CAPACITY;

const FIELD_LEN: usize = 32; // null-padded UTF-8 per string field

// Header offsets
const OFF_WRITE_IDX: usize = 0;
// bytes 8..32 reserved

// Entry field offsets (relative to entry start)
const EOFF_PREFIX: usize = 0;
const EOFF_PROJECT: usize = 32;
const EOFF_TOOL: usize = 64;
const EOFF_DELTA: usize = 96;
const EOFF_BEFORE: usize = 104;
const EOFF_TIMESTAMP: usize = 112;
// bytes 120..128 padding

/// File name of the global monitor mmap inside the user data dir.
pub const MMAP_FILENAME: &str = "monitor.mmap";
/// File name of the single-instance TUI lock inside the user data dir.
pub const LOCK_FILENAME: &str = "monitor.lock";

/// Resolve the user-level data directory (`~/.tracedecay/` by default).
fn global_tracedecay_dir() -> Option<PathBuf> {
    crate::config::user_data_dir()
}

/// A single ring-buffer entry read from the mmap.
#[derive(Debug, Clone)]
pub struct MonitorEntry {
    pub prefix: String,
    pub project: String,
    pub tool_name: String,
    pub delta: u64,
    pub before: u64,
    pub timestamp: u64,
}

impl MonitorEntry {
    /// Display label: `prefix - project - tool_name`
    pub fn label(&self) -> String {
        format!("{} - {} - {}", self.prefix, self.project, self.tool_name)
    }
}

// ── Writer (called by MCP server) ───────────────────────────────────

/// Write a tool-call entry to the global monitor mmap.
///
/// `project_root` is used to derive the folder name. `prefix` identifies
/// the tool suite (e.g. "tracedecay"). Best-effort: silently returns on
/// any failure.
pub fn write_entry(project_root: &Path, prefix: &str, tool_name: &str, delta: u64, before: u64) {
    let Some(dir) = global_tracedecay_dir() else {
        return;
    };
    let _ = std::fs::create_dir_all(&dir);
    let mmap_path = dir.join(MMAP_FILENAME);
    let project = project_root
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let _ = write_entry_inner(&mmap_path, prefix, &project, tool_name, delta, before);
}

/// Write a tool-call entry to a specific mmap directory (for testing).
pub fn write_entry_to(
    dir: &Path,
    project_root: &Path,
    prefix: &str,
    tool_name: &str,
    delta: u64,
    before: u64,
) {
    let _ = std::fs::create_dir_all(dir);
    let mmap_path = dir.join(MMAP_FILENAME);
    let project = project_root
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let _ = write_entry_inner(&mmap_path, prefix, &project, tool_name, delta, before);
}

fn write_str(mmap: &mut memmap2::MmapMut, offset: usize, value: &str) {
    let bytes = value.as_bytes();
    let copy_len = bytes.len().min(FIELD_LEN - 1);
    mmap[offset..offset + FIELD_LEN].fill(0);
    mmap[offset..offset + copy_len].copy_from_slice(&bytes[..copy_len]);
}

#[hotpath::measure(label = "runtime_core.monitor_ring.write")]
fn write_entry_inner(
    mmap_path: &Path,
    prefix: &str,
    project: &str,
    tool_name: &str,
    delta: u64,
    before: u64,
) -> std::io::Result<()> {
    // Exclusive lock for concurrent writer safety. This mmap handle is itself
    // the lock file; the shared helper supplies the cross-platform r/w open and
    // lock semantics without introducing a second sidecar.
    let file = crate::storage::acquire_sidecar_lock_blocking(mmap_path)?;

    let len = file.metadata()?.len() as usize;
    if len < FILE_SIZE {
        file.set_len(FILE_SIZE as u64)?;
    }

    let mut mmap = unsafe { memmap2::MmapMut::map_mut(&file)? };

    let write_idx = u64::from_le_bytes(
        mmap[OFF_WRITE_IDX..OFF_WRITE_IDX + 8]
            .try_into()
            .unwrap_or([0; 8]),
    );
    let slot = (write_idx as usize) % RING_CAPACITY;
    let off = HEADER_SIZE + slot * ENTRY_SIZE;

    write_str(&mut mmap, off + EOFF_PREFIX, prefix);
    write_str(&mut mmap, off + EOFF_PROJECT, project);
    write_str(&mut mmap, off + EOFF_TOOL, tool_name);

    mmap[off + EOFF_DELTA..off + EOFF_DELTA + 8].copy_from_slice(&delta.to_le_bytes());
    mmap[off + EOFF_BEFORE..off + EOFF_BEFORE + 8].copy_from_slice(&before.to_le_bytes());

    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    mmap[off + EOFF_TIMESTAMP..off + EOFF_TIMESTAMP + 8].copy_from_slice(&timestamp.to_le_bytes());

    // Increment write_idx (reader sees this last).
    let new_idx = write_idx + 1;
    mmap[OFF_WRITE_IDX..OFF_WRITE_IDX + 8].copy_from_slice(&new_idx.to_le_bytes());

    mmap.flush()?;
    file.unlock()?;
    Ok(())
}

// ── Reader (used by monitor TUI and tests) ──────────────────────────

/// Read-only view of the global monitor mmap.
pub struct MmapReader {
    mmap: memmap2::Mmap,
    dir: PathBuf,
}

fn read_str(mmap: &memmap2::Mmap, offset: usize) -> String {
    let bytes = &mmap[offset..offset + FIELD_LEN];
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(FIELD_LEN);
    String::from_utf8_lossy(&bytes[..end]).to_string()
}

impl MmapReader {
    /// Open the global monitor mmap for reading.
    pub fn open() -> std::io::Result<Self> {
        let dir = global_tracedecay_dir().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "cannot resolve home directory",
            )
        })?;
        Self::open_at(&dir)
    }

    /// Open a monitor mmap at an explicit directory (for testing).
    #[hotpath::measure(label = "runtime_core.monitor_ring.open")]
    pub fn open_at(dir: &Path) -> std::io::Result<Self> {
        let mmap_path = dir.join(MMAP_FILENAME);
        let file = std::fs::OpenOptions::new().read(true).open(&mmap_path)?;
        let mmap = unsafe { memmap2::Mmap::map(&file)? };
        Ok(Self {
            mmap,
            dir: dir.to_path_buf(),
        })
    }

    /// Current write index (number of entries ever written).
    pub fn write_idx(&self) -> u64 {
        if self.mmap.len() < HEADER_SIZE {
            return 0;
        }
        u64::from_le_bytes(
            self.mmap[OFF_WRITE_IDX..OFF_WRITE_IDX + 8]
                .try_into()
                .unwrap_or([0; 8]),
        )
    }

    /// Read the entry at the given ring-buffer slot (0..255).
    pub fn entry(&self, slot: usize) -> Option<MonitorEntry> {
        if slot >= RING_CAPACITY {
            return None;
        }
        let off = HEADER_SIZE + slot * ENTRY_SIZE;
        if self.mmap.len() < off + ENTRY_SIZE {
            return None;
        }

        let prefix = read_str(&self.mmap, off + EOFF_PREFIX);
        let project = read_str(&self.mmap, off + EOFF_PROJECT);
        let tool_name = read_str(&self.mmap, off + EOFF_TOOL);

        let delta = u64::from_le_bytes(
            self.mmap[off + EOFF_DELTA..off + EOFF_DELTA + 8]
                .try_into()
                .unwrap_or([0; 8]),
        );
        let before = u64::from_le_bytes(
            self.mmap[off + EOFF_BEFORE..off + EOFF_BEFORE + 8]
                .try_into()
                .unwrap_or([0; 8]),
        );
        let timestamp = u64::from_le_bytes(
            self.mmap[off + EOFF_TIMESTAMP..off + EOFF_TIMESTAMP + 8]
                .try_into()
                .unwrap_or([0; 8]),
        );

        Some(MonitorEntry {
            prefix,
            project,
            tool_name,
            delta,
            before,
            timestamp,
        })
    }

    /// Re-read the mmap to pick up new writes.
    #[hotpath::measure(label = "runtime_core.monitor_ring.refresh")]
    pub fn refresh(&mut self) -> std::io::Result<()> {
        let mmap_path = self.dir.join(MMAP_FILENAME);
        let file = std::fs::OpenOptions::new().read(true).open(&mmap_path)?;
        self.mmap = unsafe { memmap2::Mmap::map(&file)? };
        Ok(())
    }
}
