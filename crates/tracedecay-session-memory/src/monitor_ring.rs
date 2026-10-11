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
/// Nonzero stamp the first writer puts on a fresh file, so a reader can tell
/// a replaced ring from the one it was draining.
const OFF_GENERATION: usize = 8;
// bytes 16..32 reserved

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

    let timestamp = tracedecay_runtime_core::tracedecay::unix_secs();
    mmap[off + EOFF_TIMESTAMP..off + EOFF_TIMESTAMP + 8].copy_from_slice(&timestamp.to_le_bytes());

    if read_u64(&mmap, OFF_GENERATION) == 0 {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(1, |elapsed| elapsed.as_nanos() as u64 | 1);
        mmap[OFF_GENERATION..OFF_GENERATION + 8].copy_from_slice(&stamp.to_le_bytes());
    }

    // Increment write_idx (reader sees this last).
    let new_idx = write_idx + 1;
    mmap[OFF_WRITE_IDX..OFF_WRITE_IDX + 8].copy_from_slice(&new_idx.to_le_bytes());

    // No msync: readers map the same page-cache pages and see the entry as
    // soon as the lock is released, and this display ring needs no
    // durability. A per-call flush cost 22-53 ms on every tool call.
    file.release()?;
    Ok(())
}

// ── Reader (used by monitor TUI and tests) ──────────────────────────

/// Stable identity of the file behind a mapping. A replaced ring carries no
/// reliable header witness (a legacy file's generation field stays zero until
/// a writer stamps it in place), so identity comes from the filesystem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RingFileId {
    device: u64,
    node: u64,
}

#[cfg(unix)]
fn ring_file_id(file: &std::fs::File) -> Option<RingFileId> {
    use std::os::unix::fs::MetadataExt;

    file.metadata().ok().map(|metadata| RingFileId {
        device: metadata.dev(),
        node: metadata.ino(),
    })
}

#[cfg(windows)]
fn ring_file_id(file: &std::fs::File) -> Option<RingFileId> {
    tracedecay_private_fs::windows_file::information(file)
        .ok()
        .map(|information| RingFileId {
            device: u64::from(information.volume_serial_number),
            node: information.file_index,
        })
}

#[cfg(not(any(unix, windows)))]
fn ring_file_id(file: &std::fs::File) -> Option<RingFileId> {
    let nanos = file
        .metadata()
        .and_then(|metadata| metadata.created())
        .ok()
        .and_then(|created| created.duration_since(std::time::UNIX_EPOCH).ok())?
        .as_nanos();
    Some(RingFileId {
        device: (nanos >> 64) as u64,
        node: nanos as u64,
    })
}

/// Read-only view of the global monitor mmap.
pub struct MmapReader {
    mmap: memmap2::Mmap,
    dir: PathBuf,
    file_id: Option<RingFileId>,
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    bytes
        .get(offset..offset + 8)
        .and_then(|field| field.try_into().ok())
        .map_or(0, u64::from_le_bytes)
}

/// How far a reader has drained one ring file.
#[derive(Debug, Clone, Copy, Default)]
pub struct RingCursor {
    file_id: Option<RingFileId>,
    generation: u64,
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
        let file_id = ring_file_id(&file);
        let mmap = unsafe { memmap2::Mmap::map(&file)? };
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

    /// Read every entry written after `cursor` that the ring still holds, and
    /// advance `cursor` past them.
    ///
    /// A replaced ring file restarts the cursor at its first entry. Entries
    /// lost to overwrite are counted in [`RingDrain::dropped`], never reread.
    pub fn drain(&self, cursor: &mut RingCursor) -> RingDrain {
        let capacity = RING_CAPACITY as u64;
        let generation = read_u64(&self.mmap, OFF_GENERATION);
        let head = self.write_idx();
        // A nonzero generation alone cannot witness replacement: writers stamp
        // a legacy ring in place, so 0 -> G is also a continuation. The file
        // identity distinguishes a new inode at the same path, and an
        // unobserved identity neither restarts nor rebinds the cursor.
        let replaced = self
            .file_id
            .is_some_and(|file_id| Some(file_id) != cursor.file_id)
            || (generation != 0 && cursor.generation != 0 && generation != cursor.generation);
        if replaced || head < cursor.next_idx {
            cursor.next_idx = 0;
        }
        if self.file_id.is_some() {
            cursor.file_id = self.file_id;
        }
        if generation != 0 {
            cursor.generation = generation;
        }

        // The writer of index `i + capacity` reuses the slot of `i` before it
        // publishes `i + capacity + 1`, so without the writer lock only the
        // newest `capacity - 1` entries are safe to read.
        let unread = head.saturating_sub(cursor.next_idx);
        let start = cursor.next_idx.max((head + 1).saturating_sub(capacity));
        let mut read: Vec<(u64, MonitorEntry)> = (start..head)
            .filter_map(|idx| Some((idx, self.entry((idx as usize) % RING_CAPACITY)?)))
            .collect();
        let settled = self.write_idx();
        read.retain(|(idx, _)| idx + capacity > settled);

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
        self.file_id = ring_file_id(&file);
        self.mmap = unsafe { memmap2::Mmap::map(&file)? };
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ENTRY_SIZE, EOFF_DELTA, EOFF_PREFIX, EOFF_PROJECT, EOFF_TIMESTAMP, EOFF_TOOL, FILE_SIZE,
        HEADER_SIZE, MMAP_FILENAME, MmapReader, OFF_WRITE_IDX, RING_CAPACITY, RingCursor,
        RingDrain, read_u64, write_entry_to, write_str,
    };
    use std::path::Path;

    fn write_values(dir: &Path, values: std::ops::RangeInclusive<u64>) {
        let project = Path::new("/projects/alpha");
        for value in values {
            write_entry_to(dir, project, "tracedecay", "search", value, 0);
        }
    }

    /// Write entries the way a pre-stamp writer did: `OFF_GENERATION` stays
    /// zero, so the file looks like a legacy ring to a reader.
    fn write_unstamped(dir: &Path, values: std::ops::RangeInclusive<u64>) {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(dir.join(MMAP_FILENAME))
            .expect("open ring");
        file.set_len(FILE_SIZE as u64).expect("size ring");
        let mut mmap = unsafe { memmap2::MmapMut::map_mut(&file).expect("map ring") };
        let mut write_idx = read_u64(&mmap, OFF_WRITE_IDX);
        for value in values {
            let off = HEADER_SIZE + (write_idx as usize % RING_CAPACITY) * ENTRY_SIZE;
            write_str(&mut mmap, off + EOFF_PREFIX, "tracedecay");
            write_str(&mut mmap, off + EOFF_PROJECT, "alpha");
            write_str(&mut mmap, off + EOFF_TOOL, "search");
            mmap[off + EOFF_DELTA..off + EOFF_DELTA + 8].copy_from_slice(&value.to_le_bytes());
            mmap[off + EOFF_TIMESTAMP..off + EOFF_TIMESTAMP + 8]
                .copy_from_slice(&1_u64.to_le_bytes());
            write_idx += 1;
        }
        mmap[OFF_WRITE_IDX..OFF_WRITE_IDX + 8].copy_from_slice(&write_idx.to_le_bytes());
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

    /// A ring written before the generation stamp keeps `cursor.generation`
    /// at zero, so the header cannot witness its replacement. The file
    /// identity restarts the cursor at the new ring's first entry.
    #[test]
    fn drain_restarts_when_a_legacy_ring_is_replaced() {
        let dir = tempfile::tempdir().expect("monitor dir");
        write_unstamped(dir.path(), 1..=10);
        let mut reader = MmapReader::open_at(dir.path()).expect("open monitor");
        let mut cursor = RingCursor::default();
        assert_eq!(
            deltas(&reader.drain(&mut cursor)),
            (1..=10).collect::<Vec<_>>()
        );

        std::fs::remove_file(dir.path().join(MMAP_FILENAME)).expect("remove ring");
        write_values(dir.path(), 101..=120);
        reader.refresh().expect("remap replaced ring");
        let drain = reader.drain(&mut cursor);
        assert_eq!(deltas(&drain), (101..=120).collect::<Vec<_>>());
        assert_eq!(drain.dropped, 0);
    }

    /// Two legacy rings share no header witness at all; only the file
    /// identity distinguishes a replacement from a continued ring.
    #[test]
    fn drain_restarts_when_a_legacy_ring_is_replaced_by_another_legacy_ring() {
        let dir = tempfile::tempdir().expect("monitor dir");
        write_unstamped(dir.path(), 1..=10);
        let mut reader = MmapReader::open_at(dir.path()).expect("open monitor");
        let mut cursor = RingCursor::default();
        assert_eq!(
            deltas(&reader.drain(&mut cursor)),
            (1..=10).collect::<Vec<_>>()
        );

        std::fs::remove_file(dir.path().join(MMAP_FILENAME)).expect("remove ring");
        write_unstamped(dir.path(), 101..=120);
        reader.refresh().expect("remap replaced ring");
        let drain = reader.drain(&mut cursor);
        assert_eq!(deltas(&drain), (101..=120).collect::<Vec<_>>());
        assert_eq!(drain.dropped, 0);
    }

    /// A writer that meets a legacy ring stamps its generation in place: the
    /// file continues, so the cursor must continue rather than restart.
    #[test]
    fn drain_continues_when_a_legacy_ring_is_stamped_in_place() {
        let dir = tempfile::tempdir().expect("monitor dir");
        write_unstamped(dir.path(), 1..=10);
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
