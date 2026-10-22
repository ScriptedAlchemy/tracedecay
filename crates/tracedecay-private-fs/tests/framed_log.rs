#[cfg(unix)]
use std::ffi::CString;
use std::fs;
use std::io::ErrorKind;
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;

#[cfg(target_os = "linux")]
use std::io::Write;

#[cfg(target_os = "linux")]
use std::time::{Duration, Instant};

use tracedecay_private_fs::framed_log::{DirectorySyncPolicy, atomic_write_prepared, read_bounded};
#[cfg(target_os = "linux")]
use tracedecay_private_fs::framed_log::{DurableFileBatch, sync_latency};

#[test]
fn bounded_read_distinguishes_missing_oversized_and_non_regular_objects() {
    let root = tempfile::tempdir().expect("read fixture root");
    let record = root.path().join("record");

    assert_eq!(read_bounded(&record, 16).expect("missing record"), None);

    fs::write(&record, b"0123456789abcdef!").expect("oversized record");
    assert_eq!(
        read_bounded(&record, 16).expect_err("oversized").kind(),
        ErrorKind::InvalidData
    );
    fs::write(&record, b"").expect("empty record");
    assert_eq!(
        read_bounded(&record, 16).expect_err("empty").kind(),
        ErrorKind::InvalidData
    );

    let directory = root.path().join("directory");
    fs::create_dir(&directory).expect("directory");
    assert_eq!(
        read_bounded(&directory, 16).expect_err("directory").kind(),
        ErrorKind::InvalidInput
    );
}

/// The symlink check binds to the object that is read: a link planted at the
/// final component is refused even though its target is a valid record, and
/// the target's bytes are never returned through the link.
#[cfg(unix)]
#[test]
fn bounded_read_refuses_a_symlink_at_the_final_component() {
    let root = tempfile::tempdir().expect("symlink fixture root");
    let target = root.path().join("target");
    fs::write(&target, b"secret").expect("target record");
    let link = root.path().join("link");
    std::os::unix::fs::symlink(&target, &link).expect("symlink");

    let error = read_bounded(&link, 16).expect_err("symlink must be refused");

    assert_eq!(error.kind(), ErrorKind::InvalidInput);
    assert_eq!(
        read_bounded(&target, 16).expect("direct read"),
        Some(b"secret".to_vec())
    );
}

/// Opening happens before the kind check, so a FIFO substituted for the record
/// must be refused rather than parking the reader until a writer shows up.
#[cfg(unix)]
#[test]
fn bounded_read_refuses_a_fifo_without_blocking() {
    let root = tempfile::tempdir().expect("fifo fixture root");
    let fifo = root.path().join("fifo");
    let c_path = CString::new(fifo.as_os_str().as_bytes()).expect("fifo path");
    // SAFETY: `c_path` is a valid NUL-terminated path and `mkfifo` has no
    // other preconditions.
    assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);

    let error = read_bounded(&fifo, 16).expect_err("fifo must be refused");

    assert_eq!(error.kind(), ErrorKind::InvalidInput);
}

#[test]
fn prepared_publish_is_private_and_readable_through_the_adapter() {
    let root = tempfile::tempdir().expect("publish fixture root");
    let destination = root.path().join("config.json");
    let mut prepared = 0_u32;

    atomic_write_prepared(
        &destination,
        "fixture",
        b"published",
        |temporary| {
            prepared += 1;
            assert!(temporary.exists(), "prepare observes the staging file");
            Ok(())
        },
        DirectorySyncPolicy::TolerateUnsupported,
    )
    .expect("prepared publish");

    assert_eq!(prepared, 1);
    assert_eq!(
        read_bounded(&destination, 64).expect("bounded read"),
        Some(b"published".to_vec())
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&destination)
                .expect("published metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

/// Observe the barriers themselves: a filesystem can coalesce unrelated
/// writeback with a member's fsync, so their relative elapsed times cannot
/// establish which files the batch requested to flush.
#[cfg(target_os = "linux")]
#[test]
fn durable_batch_sync_flushes_only_its_members() {
    let root = tempfile::tempdir().expect("batch fixture root");
    let unrelated_path = root.path().join("unrelated");
    fs::write(&unrelated_path, b"unrelated dirty data").expect("unrelated file");
    let member_path = root.path().join("member.tmp");
    let mut member = fs::File::create(&member_path).expect("member file");
    member.write_all(b"sealed segment").expect("member bytes");
    let mut batch = DurableFileBatch::new();
    batch.written(&member_path, &member).expect("record member");
    let member_barriers = sync_latency::inject(&member_path, Duration::ZERO);
    let unrelated_barriers = sync_latency::inject(&unrelated_path, Duration::ZERO);
    let all_barriers = sync_latency::inject(root.path(), Duration::ZERO);
    batch.sync().expect("batch sync");

    assert_eq!(member_barriers.syncs(), 1, "the member must be flushed");
    assert_eq!(
        unrelated_barriers.syncs(),
        0,
        "unrelated data is not a member"
    );
    assert_eq!(all_barriers.syncs(), 0, "no additional filesystem barriers");

    assert_eq!(
        fs::read(&member_path).expect("member readback"),
        b"sealed segment"
    );
    fs::remove_file(&member_path).expect("remove member before another sync");
    assert_eq!(
        batch
            .sync()
            .expect_err("missing member must refuse durability")
            .kind(),
        ErrorKind::NotFound
    );
}

/// On a disk whose every fsync is its own log commit (ZFS's intent log), a
/// batch that syncs its members one after another pays one commit per member.
/// The members' barriers must be in flight together, so a seal of many
/// segments waits about one barrier, not one per segment.
#[cfg(target_os = "linux")]
#[test]
fn durable_batch_sync_overlaps_member_barriers_on_a_slow_disk() {
    const MEMBERS: u32 = 16;
    const BARRIER: Duration = Duration::from_millis(500);
    let root = tempfile::tempdir().expect("batch fixture root");
    let mut batch = DurableFileBatch::new();
    for index in 0..MEMBERS {
        let path = root.path().join(format!("segment-{index}.tmp"));
        let mut member = fs::File::create(&path).expect("member file");
        member.write_all(b"sealed segment").expect("member bytes");
        batch.written(&path, &member).expect("record member");
    }
    let slow_disk = sync_latency::inject(root.path(), BARRIER);

    let started = Instant::now();
    batch.sync().expect("batch sync");
    let elapsed = started.elapsed();

    assert_eq!(slow_disk.syncs(), u64::from(MEMBERS));
    assert!(
        elapsed < BARRIER * (MEMBERS / 4),
        "{MEMBERS} member barriers of {BARRIER:?} each took {elapsed:?} together"
    );
}
