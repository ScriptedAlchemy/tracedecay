//! Real filesystem evidence for bounded, descriptor-owned durability batches.
use std::ffi::CString;
use std::fs::{self, File};
use std::io::{ErrorKind, Read, Write};
use std::mem::size_of;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[cfg(feature = "test-helpers")]
use super::sync_latency;
use super::{DURABLE_BATCH_CONCURRENT_SYNCS, DurableFileBatch};

/// Count kernel open events, not calls to an observer unrelated to the opener.
struct OpenEvents(File);

impl OpenEvents {
    fn watch(root: &Path) -> Self {
        // SAFETY: inotify_init1 has no pointer arguments.
        let descriptor = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        assert!(
            descriptor >= 0,
            "inotify: {}",
            std::io::Error::last_os_error()
        );
        // SAFETY: the successful descriptor is owned exactly once by this File.
        let file = unsafe { File::from_raw_fd(descriptor) };
        let path = CString::new(root.as_os_str().as_bytes()).expect("fixture path");
        // SAFETY: file is live and path is NUL-terminated for the whole call.
        let watch =
            unsafe { libc::inotify_add_watch(file.as_raw_fd(), path.as_ptr(), libc::IN_OPEN) };
        assert!(watch >= 0, "watch: {}", std::io::Error::last_os_error());
        Self(file)
    }

    fn drain(&mut self) -> usize {
        let mut opens = 0;
        let mut buffer = [0_u8; 8192];
        loop {
            let length = match self.0.read(&mut buffer) {
                Ok(length) => length,
                Err(error) if error.kind() == ErrorKind::WouldBlock => return opens,
                Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                Err(error) => panic!("read inotify events: {error}"),
            };
            assert!(length > 0, "inotify unexpectedly reached EOF");
            let mut offset = 0;
            while offset < length {
                assert!(offset + size_of::<libc::inotify_event>() <= length);
                // SAFETY: the complete header is in bounds; read_unaligned
                // handles the byte buffer's alignment. No borrowed name escapes.
                let event = unsafe {
                    buffer
                        .as_ptr()
                        .add(offset)
                        .cast::<libc::inotify_event>()
                        .read_unaligned()
                };
                assert_eq!(event.mask & libc::IN_Q_OVERFLOW, 0, "lost open events");
                if event.mask & libc::IN_OPEN != 0 {
                    opens += 1;
                }
                offset += size_of::<libc::inotify_event>()
                    + usize::try_from(event.len).expect("event length");
                assert!(offset <= length, "truncated inotify name");
            }
        }
    }
}

fn write_member(batch: &mut DurableFileBatch, root: &Path, index: usize) -> PathBuf {
    let path = root.join(format!("segment-{index}.tmp"));
    let mut file = File::create(&path).expect("member writer");
    file.write_all(format!("sealed segment {index}").as_bytes())
        .expect("member bytes");
    batch.written(&path, &file).expect("record member");
    path
}

#[test]
fn durable_batch_flushes_multiple_windows_without_pathname_reopens() {
    let root = tempfile::tempdir().expect("batch root");
    let mut events = OpenEvents::watch(root.path());
    let probe = root.path().join("open-probe");
    drop(File::create(&probe).expect("real probe open"));
    assert_eq!(
        events.drain(),
        1,
        "the kernel observer must actually see opens"
    );
    fs::remove_file(&probe).expect("remove probe");

    let mut batch = DurableFileBatch::new();
    let members = DURABLE_BATCH_CONCURRENT_SYNCS * 2 + 3;
    for index in 0..members {
        write_member(&mut batch, root.path(), index);
        assert!(batch.pending.len() <= DURABLE_BATCH_CONCURRENT_SYNCS);
    }
    assert_eq!(batch.synced.len(), DURABLE_BATCH_CONCURRENT_SYNCS * 2);
    assert_eq!(batch.pending.len(), 3);
    batch.sync().expect("flush final window");
    assert_eq!(
        events.drain(),
        members,
        "only the original writer opens are allowed; flushing must not reopen members"
    );
    batch.sync().expect("repeat final barrier");
    assert_eq!(events.drain(), 0, "a repeated sync must not reopen paths");
    drop(batch);
    for index in 0..members {
        assert_eq!(
            fs::read(root.path().join(format!("segment-{index}.tmp")))
                .expect("exact durable readback"),
            format!("sealed segment {index}").as_bytes()
        );
    }
}

#[cfg(feature = "test-helpers")]
#[test]
fn durable_batch_flushes_each_window_before_releasing_its_handles() {
    let root = tempfile::tempdir().expect("batch root");
    let barriers = sync_latency::inject(root.path(), Duration::ZERO);
    let mut batch = DurableFileBatch::new();
    let members = DURABLE_BATCH_CONCURRENT_SYNCS * 2 + 3;
    for index in 0..members {
        write_member(&mut batch, root.path(), index);
    }
    assert_eq!(
        barriers.syncs(),
        u64::try_from(DURABLE_BATCH_CONCURRENT_SYNCS * 2).expect("flushed count"),
        "both completed windows must have issued real file barriers"
    );
    batch.sync().expect("flush remaining members");
    assert_eq!(
        barriers.syncs(),
        u64::try_from(members).expect("member count"),
        "no member may be omitted or flushed as unrelated filesystem data"
    );
}

#[test]
fn durable_batch_rejects_a_path_not_owned_by_the_writer() {
    let root = tempfile::tempdir().expect("batch root");
    let path = root.path().join("member.tmp");
    let other = root.path().join("other.tmp");
    let mut writer = File::create(&path).expect("writer");
    writer.write_all(b"original").expect("member bytes");
    fs::write(&other, b"original").expect("same bytes in another inode");
    let mut batch = DurableFileBatch::new();
    assert_eq!(
        batch
            .written(&other, &writer)
            .expect_err("wrong inode")
            .kind(),
        ErrorKind::InvalidData
    );
    assert_eq!(batch.pending.len(), 0);
    batch.written(&path, &writer).expect("correct writer path");
    drop(writer);
    batch.sync().expect("flush caller-closed writer");
    assert_eq!(fs::read(path).expect("read member"), b"original");
}

#[test]
fn durable_batch_refuses_pending_and_already_flushed_replacements() {
    for members in [1, DURABLE_BATCH_CONCURRENT_SYNCS + 1] {
        let root = tempfile::tempdir().expect("batch root");
        let mut batch = DurableFileBatch::new();
        for index in 0..members {
            write_member(&mut batch, root.path(), index);
        }
        let first = root.path().join("segment-0.tmp");
        let replacement = root.path().join("replacement.tmp");
        // Same bytes rule out content mismatch as the reason for refusing.
        fs::write(&replacement, b"sealed segment 0").expect("replacement");
        fs::rename(&replacement, &first).expect("replace recorded inode");
        for _ in 0..2 {
            assert_eq!(
                batch.sync().expect_err("replaced member").kind(),
                ErrorKind::InvalidData
            );
        }
        assert_eq!(
            fs::read(first).expect("replacement preserved"),
            b"sealed segment 0"
        );
    }
}

#[test]
fn durable_batch_refuses_a_missing_member_from_an_earlier_window() {
    let root = tempfile::tempdir().expect("batch root");
    let mut batch = DurableFileBatch::new();
    for index in 0..=DURABLE_BATCH_CONCURRENT_SYNCS {
        write_member(&mut batch, root.path(), index);
    }
    assert_eq!(batch.synced.len(), DURABLE_BATCH_CONCURRENT_SYNCS);
    fs::remove_file(root.path().join("segment-0.tmp")).expect("remove flushed member");
    assert_eq!(
        batch.sync().expect_err("missing flushed member").kind(),
        ErrorKind::NotFound
    );
}

#[test]
fn durable_batch_refuses_same_inode_mutation_after_early_flush() {
    let root = tempfile::tempdir().expect("batch root");
    let mut batch = DurableFileBatch::new();
    for index in 0..=DURABLE_BATCH_CONCURRENT_SYNCS {
        write_member(&mut batch, root.path(), index);
    }
    let path = root.path().join("segment-0.tmp");
    let before = fs::metadata(&path).expect("original metadata");
    let mut writer = File::options()
        .write(true)
        .open(&path)
        .expect("same inode");
    writer
        .write_all(b"changed segment!")
        .expect("same-length mutation");
    writer
        .set_modified(before.modified().expect("mtime") + Duration::from_secs(2))
        .expect("observable mutation on coarse timestamp filesystems");
    drop(writer);
    let after = fs::metadata(&path).expect("mutated metadata");
    assert_eq!(
        (before.dev(), before.ino(), before.len()),
        (after.dev(), after.ino(), after.len())
    );
    assert_ne!(
        before.modified().expect("original mtime"),
        after.modified().expect("changed mtime")
    );
    assert_eq!(
        batch.sync().expect_err("mutated early member").kind(),
        ErrorKind::InvalidData
    );
}

#[test]
fn durable_batch_failed_window_keeps_handles_and_does_not_admit_another_member() {
    let root = tempfile::tempdir().expect("batch root");
    let mut batch = DurableFileBatch::new();
    for index in 0..DURABLE_BATCH_CONCURRENT_SYNCS {
        write_member(&mut batch, root.path(), index);
    }
    fs::remove_file(root.path().join("segment-0.tmp")).expect("remove pending member");
    let next = root.path().join("next.tmp");
    let mut writer = File::create(&next).expect("next writer");
    writer.write_all(b"next segment").expect("next bytes");
    assert_eq!(
        batch
            .written(&next, &writer)
            .expect_err("failed window")
            .kind(),
        ErrorKind::NotFound
    );
    assert_eq!(batch.pending.len(), DURABLE_BATCH_CONCURRENT_SYNCS);
    assert_eq!(
        batch.synced.len(),
        0,
        "no partial window may claim durability"
    );
    assert_eq!(
        batch.sync().expect_err("retry remains refused").kind(),
        ErrorKind::NotFound
    );
}
