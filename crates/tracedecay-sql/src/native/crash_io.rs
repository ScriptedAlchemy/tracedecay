//! Crash injection under the native engine, for the crash harness
//! (`crates/tracedecay-rusqlite-runtime/tests/crash`).
//!
//! [`arm`] installs one [`CrashPlan`] for the process. Every native database
//! opened afterwards routes its file I/O through [`CrashIo`], which counts the
//! writes and syncs of the targeted file and, on the `nth` one, crashes the
//! process before that operation runs:
//!
//! - [`CrashEffect::Kill`] aborts. What the OS already accepted stays, as after
//!   a killed process.
//! - [`CrashEffect::PowerLoss`] first rolls every tracked file back to its
//!   state at its last completed sync, then aborts, as after a power cut.

use std::{
    fs::{File, OpenOptions},
    os::unix::fs::FileExt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, PoisonError},
};

use turso_core::{
    Buffer, Completion, IO, OpenFlags,
    io::{
        Clock, FileId, FileSyncType,
        clock::{MonotonicInstant, WallClockInstant},
    },
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CrashTarget {
    Main,
    Wal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CrashOp {
    Write,
    Sync,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CrashEffect {
    Kill,
    PowerLoss,
    /// The `nth` operation fails with an I/O error and the process lives on.
    Fail,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CrashPlan {
    pub target: CrashTarget,
    pub op: CrashOp,
    /// 1-based count of matching operations; the crash replaces the `nth`.
    pub nth: u64,
    pub effect: CrashEffect,
}

struct Armed {
    plan: CrashPlan,
    seen: u64,
    files: Vec<Arc<Tracked>>,
}

static ARMED: Mutex<Option<Armed>> = Mutex::new(None);

/// Arms `plan` for every native database this process opens from now on.
pub fn arm(plan: CrashPlan) {
    assert!(plan.nth >= 1, "crash plan armed for operation 0");
    *ARMED.lock().unwrap_or_else(PoisonError::into_inner) = Some(Armed {
        plan,
        seen: 0,
        files: Vec::new(),
    });
}

fn armed() -> bool {
    ARMED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .is_some()
}

pub(crate) fn wrap(io: Arc<dyn IO>) -> Arc<dyn IO> {
    if armed() {
        Arc::new(CrashIo { delegate: io })
    } else {
        io
    }
}

/// Undo state since the last completed sync of one file.
struct Tracked {
    path: PathBuf,
    target: CrashTarget,
    shadow: File,
    state: Mutex<Unsynced>,
}

struct Unsynced {
    synced_len: u64,
    /// `(offset, bytes before the overwrite)`, oldest first.
    undo: Vec<(u64, Vec<u8>)>,
}

impl Tracked {
    fn save(&self, offset: u64, len: usize) {
        let mut old = vec![0; len];
        let mut read = 0;
        while read < len {
            match self.shadow.read_at(&mut old[read..], offset + read as u64) {
                Ok(0) => break,
                Ok(n) => read += n,
                Err(error) => panic!("crash io: read {}: {error}", self.path.display()),
            }
        }
        old.truncate(read);
        self.lock().undo.push((offset, old));
    }

    fn synced(&self) {
        let len = self.current_len();
        let mut state = self.lock();
        state.synced_len = len;
        state.undo.clear();
    }

    fn lose_unsynced(&self) {
        let state = self.lock();
        for (offset, old) in state.undo.iter().rev() {
            self.shadow
                .write_all_at(old, *offset)
                .unwrap_or_else(|error| {
                    panic!("crash io: restore {}: {error}", self.path.display())
                });
        }
        self.shadow
            .set_len(state.synced_len)
            .and_then(|()| self.shadow.sync_all())
            .unwrap_or_else(|error| panic!("crash io: cut {}: {error}", self.path.display()));
    }

    fn current_len(&self) -> u64 {
        self.shadow
            .metadata()
            .unwrap_or_else(|error| panic!("crash io: stat {}: {error}", self.path.display()))
            .len()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Unsynced> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Counts `op` on `tracked` and crashes the process when it is the armed one.
/// Under [`CrashEffect::Fail`] the armed operation instead fails.
fn before(tracked: &Tracked, op: CrashOp) -> turso_core::Result<()> {
    let mut guard = ARMED.lock().unwrap_or_else(PoisonError::into_inner);
    let Some(armed) = guard.as_mut() else {
        return Ok(());
    };
    if armed.plan.target != tracked.target || armed.plan.op != op {
        return Ok(());
    }
    armed.seen += 1;
    if armed.seen != armed.plan.nth {
        return Ok(());
    }
    if armed.plan.effect == CrashEffect::Fail {
        return Err(turso_core::LimboError::InternalError(format!(
            "crash io: injected {op:?} failure"
        )));
    }
    if armed.plan.effect == CrashEffect::PowerLoss {
        for file in &armed.files {
            file.lose_unsynced();
        }
    }
    eprintln!("TD_CRASH crash_io {:?}", armed.plan);
    std::process::abort();
}

struct CrashIo {
    delegate: Arc<dyn IO>,
}

struct CrashFile {
    inner: Arc<dyn turso_core::File>,
    tracked: Arc<Tracked>,
}

impl CrashFile {
    fn track(path: &Path, inner: Arc<dyn turso_core::File>) -> Arc<dyn turso_core::File> {
        let shadow = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .unwrap_or_else(|error| panic!("crash io: shadow {}: {error}", path.display()));
        let synced_len = shadow
            .metadata()
            .unwrap_or_else(|error| panic!("crash io: stat {}: {error}", path.display()))
            .len();
        let target = if path.to_string_lossy().ends_with("-wal") {
            CrashTarget::Wal
        } else {
            CrashTarget::Main
        };
        let tracked = Arc::new(Tracked {
            path: path.to_owned(),
            target,
            shadow,
            state: Mutex::new(Unsynced {
                synced_len,
                undo: Vec::new(),
            }),
        });
        if let Some(armed) = ARMED
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_mut()
        {
            armed.files.push(Arc::clone(&tracked));
        }
        Arc::new(Self { inner, tracked })
    }

    fn write(&self, pos: u64, len: usize) -> turso_core::Result<()> {
        before(&self.tracked, CrashOp::Write)?;
        self.tracked.save(pos, len);
        Ok(())
    }
}

impl Clock for CrashIo {
    fn current_time_monotonic(&self) -> MonotonicInstant {
        self.delegate.current_time_monotonic()
    }
    fn current_time_wall_clock(&self) -> WallClockInstant {
        self.delegate.current_time_wall_clock()
    }
}

impl IO for CrashIo {
    fn open_file(
        &self,
        path: &str,
        flags: OpenFlags,
        direct: bool,
    ) -> turso_core::Result<Arc<dyn turso_core::File>> {
        let file = self.delegate.open_file(path, flags, direct)?;
        Ok(CrashFile::track(Path::new(path), file))
    }
    fn open_shared_wal_file(&self, path: &str) -> turso_core::Result<Arc<dyn turso_core::File>> {
        let file = self.delegate.open_shared_wal_file(path)?;
        Ok(CrashFile::track(Path::new(path), file))
    }
    fn remove_file(&self, path: &str) -> turso_core::Result<()> {
        self.delegate.remove_file(path)
    }
    fn supports_shared_wal_coordination(&self) -> bool {
        self.delegate.supports_shared_wal_coordination()
    }
    fn step(&self) -> turso_core::Result<()> {
        self.delegate.step()
    }
    fn cancel(&self, completions: &[Completion]) -> turso_core::Result<()> {
        self.delegate.cancel(completions)
    }
    fn register_fixed_buffer(
        &self,
        pointer: std::ptr::NonNull<u8>,
        length: usize,
    ) -> turso_core::Result<u32> {
        self.delegate.register_fixed_buffer(pointer, length)
    }
    fn file_id(&self, path: &str) -> turso_core::Result<FileId> {
        self.delegate.file_id(path)
    }

    fn temp_dir(&self) -> Option<PathBuf> {
        self.delegate.temp_dir()
    }
}

impl turso_core::File for CrashFile {
    fn lock_file(&self, exclusive: bool) -> turso_core::Result<()> {
        self.inner.lock_file(exclusive)
    }
    fn unlock_file(&self) -> turso_core::Result<()> {
        self.inner.unlock_file()
    }
    fn pread(&self, pos: u64, c: Completion) -> turso_core::Result<Completion> {
        self.inner.pread(pos, c)
    }
    fn pwrite(
        &self,
        pos: u64,
        buffer: Arc<Buffer>,
        c: Completion,
    ) -> turso_core::Result<Completion> {
        self.write(pos, buffer.len())?;
        self.inner.pwrite(pos, buffer, c)
    }
    fn pwritev(
        &self,
        pos: u64,
        buffers: Vec<Arc<Buffer>>,
        c: Completion,
    ) -> turso_core::Result<Completion> {
        self.write(pos, buffers.iter().map(|buffer| buffer.len()).sum())?;
        self.inner.pwritev(pos, buffers, c)
    }
    fn sync(&self, c: Completion, sync_type: FileSyncType) -> turso_core::Result<Completion> {
        before(&self.tracked, CrashOp::Sync)?;
        // The native Unix backend fsyncs before `sync` returns.
        let completion = self.inner.sync(c, sync_type)?;
        self.tracked.synced();
        Ok(completion)
    }
    fn size(&self) -> turso_core::Result<u64> {
        self.inner.size()
    }
    fn truncate(&self, len: u64, c: Completion) -> turso_core::Result<Completion> {
        let current = self.tracked.current_len();
        if len < current {
            self.tracked
                .save(len, usize::try_from(current - len).unwrap_or(usize::MAX));
        }
        self.inner.truncate(len, c)
    }
}
