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

/// Write a tool-call entry to the monitor mmap in the profile data directory
/// `dir`.
///
/// `project_root` is used to derive the folder name. `prefix` identifies
/// the tool suite (e.g. "tracedecay"). Best-effort: silently returns on
/// any failure.
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

#[tracing::instrument(name = "runtime_core.monitor_ring.write", level = "trace", skip_all)]
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
    let file = tracedecay_runtime_core::storage::acquire_sidecar_lock_blocking(mmap_path)?;

    let len = file.metadata()?.len() as usize;
    if len < FILE_SIZE {
        file.set_len(FILE_SIZE as u64)?;
    }

    let mut mmap = unsafe { memmap2::MmapMut::map_mut(&*file)? };

    let write_idx = read_u64(&mmap, OFF_WRITE_IDX);
    let new_idx = write_idx.checked_add(1).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "monitor write index exhausted",
        )
    })?;
    let slot = (write_idx as usize) % RING_CAPACITY;
    let off = HEADER_SIZE + slot * ENTRY_SIZE;

    write_str(&mut mmap, off + EOFF_PREFIX, prefix);
    write_str(&mut mmap, off + EOFF_PROJECT, project);
    write_str(&mut mmap, off + EOFF_TOOL, tool_name);

    mmap[off + EOFF_DELTA..off + EOFF_DELTA + 8].copy_from_slice(&delta.to_le_bytes());
    mmap[off + EOFF_BEFORE..off + EOFF_BEFORE + 8].copy_from_slice(&before.to_le_bytes());

    let timestamp = tracedecay_runtime_core::tracedecay::unix_secs();
    mmap[off + EOFF_TIMESTAMP..off + EOFF_TIMESTAMP + 8].copy_from_slice(&timestamp.to_le_bytes());

    // Publish the index after the entry is complete.
    mmap[OFF_WRITE_IDX..OFF_WRITE_IDX + 8].copy_from_slice(&new_idx.to_le_bytes());

    // No msync: readers map the same page-cache pages and see the entry as
    // soon as the lock is released, and this display ring needs no
    // durability. A per-call flush cost 22-53 ms on every tool call.
    file.release()?;
    Ok(())
}

// ── Reader (used by monitor TUI and tests) ──────────────────────────

/// Stable filesystem identity distinguishes a replacement from an in-place write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RingFileId {
    device: u64,
    node: u64,
}

#[cfg(unix)]
fn ring_file_id(file: &std::fs::File) -> std::io::Result<RingFileId> {
    use std::os::unix::fs::MetadataExt;

    let metadata = file.metadata()?;
    Ok(RingFileId {
        device: metadata.dev(),
        node: metadata.ino(),
    })
}

#[cfg(windows)]
fn ring_file_id(file: &std::fs::File) -> std::io::Result<RingFileId> {
    let information = tracedecay_private_fs::windows_file::information(file)?;
    Ok(RingFileId {
        device: u64::from(information.volume_serial_number),
        node: information.file_index,
    })
}

#[cfg(not(any(unix, windows)))]
fn ring_file_id(_: &std::fs::File) -> std::io::Result<RingFileId> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "monitor filesystem identity is unavailable on this platform",
    ))
}

fn map_ring(file: &std::fs::File) -> std::io::Result<memmap2::Mmap> {
    if file.metadata()?.len() < FILE_SIZE as u64 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "monitor ring is shorter than its fixed layout",
        ));
    }
    unsafe { memmap2::Mmap::map(file) }
}

/// Read-only view of the global monitor mmap.
pub struct MmapReader {
    mmap: memmap2::Mmap,
    dir: PathBuf,
    file_id: RingFileId,
}

// Every caller has sized or validated the mapping against FILE_SIZE.
fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    let mut field = [0; 8];
    field.copy_from_slice(&bytes[offset..offset + 8]);
    u64::from_le_bytes(field)
}

/// How far a reader has drained one ring file.
#[derive(Debug, Clone, Copy, Default)]
pub struct RingCursor {
    file_id: Option<RingFileId>,
    next_idx: u64,
}

/// Entries one [`MmapReader::drain`] read, oldest first.
#[derive(Debug, Default)]
pub struct RingDrain {
    pub entries: Vec<MonitorEntry>,
    /// Entries written since the cursor that writers overwrote, or may have
    /// been overwriting, before they could be read.
    pub dropped: u64,
}

fn read_str(mmap: &memmap2::Mmap, offset: usize) -> String {
    let bytes = &mmap[offset..offset + FIELD_LEN];
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(FIELD_LEN);
    String::from_utf8_lossy(&bytes[..end]).to_string()
}

impl MmapReader {
    /// Open the monitor mmap in the profile data directory `dir`.
    #[tracing::instrument(name = "runtime_core.monitor_ring.open", level = "trace", skip_all)]
    pub fn open_at(dir: &Path) -> std::io::Result<Self> {
        let mmap_path = dir.join(MMAP_FILENAME);
        let file = std::fs::OpenOptions::new().read(true).open(&mmap_path)?;
        let file_id = ring_file_id(&file)?;
        let mmap = map_ring(&file)?;
        Ok(Self {
            mmap,
            dir: dir.to_path_buf(),
            file_id,
        })
    }

    /// Current write index (number of entries ever written).
    pub fn write_idx(&self) -> u64 {
        read_u64(&self.mmap, OFF_WRITE_IDX)
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

        let delta = read_u64(&self.mmap, off + EOFF_DELTA);
        let before = read_u64(&self.mmap, off + EOFF_BEFORE);
        let timestamp = read_u64(&self.mmap, off + EOFF_TIMESTAMP);

        Some(MonitorEntry {
            prefix,
            project,
            tool_name,
            delta,
            before,
            timestamp,
        })
    }

    /// Read every entry written after `cursor` that the ring still holds, and
    /// advance `cursor` past them.
    ///
    /// A replaced ring file restarts the cursor at its first entry. Entries
    /// lost to overwrite are counted in [`RingDrain::dropped`], never reread.
    pub fn drain(&self, cursor: &mut RingCursor) -> RingDrain {
        let capacity = RING_CAPACITY as u64;
        let head = self.write_idx();
        if cursor.file_id != Some(self.file_id) || head < cursor.next_idx {
            cursor.next_idx = 0;
        }
        cursor.file_id = Some(self.file_id);

        // The writer of index `i + capacity` reuses the slot of `i` before it
        // publishes `i + capacity + 1`, so without the writer lock only the
        // newest `capacity - 1` entries are safe to read.
        let unread = head.saturating_sub(cursor.next_idx);
        let start = cursor
            .next_idx
            .max(head.saturating_add(1).saturating_sub(capacity));
        let mut read: Vec<(u64, MonitorEntry)> = (start..head)
            .filter_map(|idx| Some((idx, self.entry((idx as usize) % RING_CAPACITY)?)))
            .collect();
        let settled = self.write_idx();
        read.retain(|(idx, _)| idx.saturating_add(capacity) > settled);

        cursor.next_idx = head;
        RingDrain {
            dropped: unread - read.len() as u64,
            entries: read.into_iter().map(|(_, entry)| entry).collect(),
        }
    }

    /// Re-read the mmap to pick up new writes.
    #[tracing::instrument(name = "runtime_core.monitor_ring.refresh", level = "trace", skip_all)]
    pub fn refresh(&mut self) -> std::io::Result<()> {
        let mmap_path = self.dir.join(MMAP_FILENAME);
        let file = std::fs::OpenOptions::new().read(true).open(&mmap_path)?;
        let file_id = ring_file_id(&file)?;
        let mmap = map_ring(&file)?;
        self.file_id = file_id;
        self.mmap = mmap;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        FILE_SIZE, MMAP_FILENAME, MmapReader, RING_CAPACITY, RingCursor, RingDrain, write_entry_to,
    };
    use std::path::Path;

    fn write_values(dir: &Path, values: std::ops::RangeInclusive<u64>) {
        let project = Path::new("/projects/alpha");
        for value in values {
            write_entry_to(dir, project, "tracedecay", "search", value, 0);
        }
    }

    fn deltas(drain: &RingDrain) -> Vec<u64> {
        drain.entries.iter().map(|entry| entry.delta).collect()
    }

    #[test]
    fn drain_reads_each_retained_entry_once_and_counts_overwritten_ones() {
        let capacity = RING_CAPACITY as u64;
        for burst in [0_u64, capacity, capacity + 1, 300] {
            let dir = tempfile::tempdir().expect("monitor dir");
            std::fs::File::create(dir.path().join(MMAP_FILENAME))
                .and_then(|file| file.set_len(FILE_SIZE as u64))
                .expect("create ring");
            let reader = MmapReader::open_at(dir.path()).expect("open monitor");
            let mut cursor = RingCursor::default();

            write_values(dir.path(), 1..=5);
            assert_eq!(deltas(&reader.drain(&mut cursor)), vec![1, 2, 3, 4, 5]);

            write_values(dir.path(), 6..=5 + burst);
            let drain = reader.drain(&mut cursor);
            let lost = burst.saturating_sub(capacity - 1);
            assert_eq!(drain.dropped, lost, "burst {burst}");
            assert_eq!(
                deltas(&drain),
                (6 + lost..=5 + burst).collect::<Vec<_>>(),
                "burst {burst}"
            );

            let idle = reader.drain(&mut cursor);
            assert!(idle.entries.is_empty(), "burst {burst}: idle poll rereads");
            assert_eq!(idle.dropped, 0, "burst {burst}");
        }
    }

    /// Walking every index modulo the capacity rereads lapped slots: these
    /// 300 calls once totaled 56,414 instead of at most the true 45,150.
    #[test]
    fn drain_totals_only_the_retained_window_after_a_lapped_ring() {
        let dir = tempfile::tempdir().expect("monitor dir");
        write_values(dir.path(), 1..=300);
        let reader = MmapReader::open_at(dir.path()).expect("open monitor");
        let drain = reader.drain(&mut RingCursor::default());
        assert_eq!(
            drain.entries.iter().map(|entry| entry.delta).sum::<u64>(),
            44_115
        );
        assert_eq!(drain.dropped, 45);
    }

    #[test]
    fn drain_restarts_at_the_first_entry_of_a_replaced_ring() {
        let dir = tempfile::tempdir().expect("monitor dir");
        write_values(dir.path(), 1..=10);
        let mut reader = MmapReader::open_at(dir.path()).expect("open monitor");
        let mut cursor = RingCursor::default();
        assert_eq!(reader.drain(&mut cursor).entries.len(), 10);

        std::fs::remove_file(dir.path().join(MMAP_FILENAME)).expect("remove ring");
        write_values(dir.path(), 101..=120);
        reader.refresh().expect("remap replaced ring");
        let drain = reader.drain(&mut cursor);
        assert_eq!(deltas(&drain), (101..=120).collect::<Vec<_>>());
        assert_eq!(drain.dropped, 0);
    }

    #[test]
    fn drain_continues_when_the_same_file_receives_more_entries() {
        let dir = tempfile::tempdir().expect("monitor dir");
        write_values(dir.path(), 1..=10);
        let reader = MmapReader::open_at(dir.path()).expect("open monitor");
        let mut cursor = RingCursor::default();
        assert_eq!(
            deltas(&reader.drain(&mut cursor)),
            (1..=10).collect::<Vec<_>>()
        );

        write_values(dir.path(), 11..=15);
        let drain = reader.drain(&mut cursor);
        assert_eq!(deltas(&drain), (11..=15).collect::<Vec<_>>());
        assert_eq!(drain.dropped, 0);
    }

    #[test]
    fn incomplete_ring_is_refused_without_rebinding_an_existing_reader() {
        let dir = tempfile::tempdir().expect("monitor dir");
        write_values(dir.path(), 1..=10);
        let mut reader = MmapReader::open_at(dir.path()).expect("open monitor");
        let mut cursor = RingCursor::default();
        assert_eq!(reader.drain(&mut cursor).entries.len(), 10);
        let identity = reader.file_id;

        std::fs::remove_file(dir.path().join(MMAP_FILENAME)).expect("remove ring");
        std::fs::write(dir.path().join(MMAP_FILENAME), [0; 8]).expect("write incomplete ring");
        assert_eq!(
            reader
                .refresh()
                .expect_err("refresh refuses incomplete ring")
                .kind(),
            std::io::ErrorKind::InvalidData
        );
        assert_eq!(reader.file_id, identity);
        assert_eq!(reader.write_idx(), 10);
        assert!(reader.drain(&mut cursor).entries.is_empty());
        assert_eq!(
            MmapReader::open_at(dir.path())
                .err()
                .expect("open refuses incomplete ring")
                .kind(),
            std::io::ErrorKind::InvalidData
        );
    }

    /// Writers no longer msync, so an already-open reader must still observe
    /// every entry through its existing mapping, without a refresh.
    #[test]
    fn open_reader_observes_every_unflushed_entry() {
        let dir = tempfile::tempdir().expect("monitor dir");
        let project = tempfile::tempdir().expect("project dir");
        write_entry_to(dir.path(), project.path(), "tracedecay", "first", 1, 2);
        let reader = MmapReader::open_at(dir.path()).expect("open monitor");
        for call in 1..40_u64 {
            write_entry_to(dir.path(), project.path(), "tracedecay", "poll", call, 2);
        }
        assert_eq!(reader.write_idx(), 40);
        let last = reader.entry(39).expect("last slot");
        assert_eq!((last.tool_name.as_str(), last.delta), ("poll", 39));
    }
}
