#[cfg(unix)]
use std::ffi::CString;
use std::fs;
use std::io::ErrorKind;
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;

#[cfg(target_os = "linux")]
use std::io::Write;

#[cfg(target_os = "linux")]
use tracedecay_private_fs::framed_log::DurableFileBatch;
use tracedecay_private_fs::framed_log::{DirectorySyncPolicy, atomic_write_prepared, read_bounded};

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

/// A batch makes its own members durable and nothing else: syncing one small
/// member must not wait for another writer's dirty data on the same
/// filesystem. Asserted as a latency class against the cost of flushing that
/// unrelated data afterwards, on disk under the target directory (a tmpfs
/// would make both free).
#[cfg(target_os = "linux")]
#[test]
fn durable_batch_sync_is_sized_to_its_members_not_the_filesystem() {
    let root = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).expect("batch fixture root");
    // Incompressible, so a compressing filesystem still has to write it.
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    let chunk = (0..1 << 20)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state.to_le_bytes()[0]
        })
        .collect::<Vec<_>>();
    let mut unrelated = fs::File::create(root.path().join("unrelated")).expect("unrelated file");
    for round in 0..128_u8 {
        let mut block = chunk.clone();
        block[0] = round;
        unrelated.write_all(&block).expect("unrelated dirty data");
    }

    let member_path = root.path().join("member.tmp");
    let mut member = fs::File::create(&member_path).expect("member file");
    member.write_all(b"sealed segment").expect("member bytes");
    let mut batch = DurableFileBatch::new();
    batch.written(&member_path, &member).expect("record member");
    let started = std::time::Instant::now();
    batch.sync().expect("batch sync");
    let batch_sync = started.elapsed();
    let started = std::time::Instant::now();
    unrelated.sync_all().expect("unrelated sync");
    let unrelated_sync = started.elapsed();

    assert_eq!(
        fs::read(&member_path).expect("member readback"),
        b"sealed segment"
    );
    assert!(
        batch_sync * 4 < unrelated_sync,
        "one-member batch sync took {batch_sync:?}; flushing the unrelated 128 MiB took {unrelated_sync:?}"
    );
}
