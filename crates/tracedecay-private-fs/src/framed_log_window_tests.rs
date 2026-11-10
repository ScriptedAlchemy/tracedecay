//! Changes after one member's completed barrier must invalidate its whole window.
use std::fs::{self, File};
use std::io::{ErrorKind, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use super::{DURABLE_BATCH_CONCURRENT_SYNCS, DurableFileBatch, sync_latency};

fn write_members(batch: &mut DurableFileBatch, root: &Path, count: usize) -> Vec<PathBuf> {
    (0..count)
        .map(|index| {
            let path = root.join(format!("segment-{index}.tmp"));
            let mut writer = File::create(&path).expect("member writer");
            writer
                .write_all(format!("sealed segment {index}").as_bytes())
                .expect("nonempty member bytes");
            batch.written(&path, &writer).expect("admit original member");
            path
        })
        .collect()
}

#[test]
fn final_sync_refuses_replacement_after_a_pending_member_completed() {
    let root = tempfile::tempdir().expect("isolated batch root");
    let mut batch = DurableFileBatch::new();
    let paths = write_members(&mut batch, root.path(), 2);
    let first = &paths[0];
    let original = fs::metadata(first).expect("original member identity");

    let (barriers, result) = thread::scope(|scope| {
        // This handshake happens after the real fsync and both per-member
        // identity checks. No sleep or scheduler ordering establishes the race.
        // Keep the pause local so any assertion failure disconnects the worker
        // before thread::scope joins it.
        let (barriers, pause) = sync_latency::pause_after_batch_member(first);
        let worker = scope.spawn(|| batch.sync());
        assert!(pause.wait_for_sync(Duration::from_secs(10)));
        assert_eq!(
            barriers.syncs(),
            1,
            "a real member fsync must have completed"
        );

        let replacement = root.path().join("replacement.tmp");
        fs::write(&replacement, b"sealed segment 0").expect("identical replacement bytes");
        fs::rename(&replacement, first).expect("replace the already-checked inode");
        let current = fs::metadata(first).expect("replacement identity");
        assert_ne!(
            (original.dev(), original.ino()),
            (current.dev(), current.ino()),
            "same-byte replacement must actually change the inode"
        );
        pause.resume();
        (barriers, worker.join().expect("batch worker"))
    });

    let error = result.expect_err("changed pending window");
    assert_eq!(error.kind(), ErrorKind::InvalidData);
    assert_eq!(batch.pending.len(), 2, "failed sync retains both handles");
    assert!(batch.synced.is_empty());
    assert_eq!(
        fs::read(first).expect("replacement survives"),
        b"sealed segment 0"
    );
    let error = batch
        .sync()
        .expect_err("retry must still refuse the old identity");
    assert_eq!(error.kind(), ErrorKind::InvalidData);
    assert_eq!(
        barriers.syncs(),
        1,
        "retry must not flush the substituted member"
    );
    drop(barriers);
    drop(batch);

    // A fresh, explicitly authenticated batch can publish the surviving bytes.
    let mut recovered = DurableFileBatch::new();
    for path in &paths {
        let writer = File::options()
            .read(true)
            .write(true)
            .open(path)
            .expect("fresh writer");
        recovered
            .written(path, &writer)
            .expect("authenticate current inode");
    }
    let barriers = sync_latency::inject(root.path(), Duration::ZERO);
    recovered.sync().expect("flush freshly authenticated batch");
    assert_eq!(barriers.syncs(), 2);
    for (index, path) in paths.iter().enumerate() {
        assert_eq!(
            fs::read(path).expect("exact durable recovery bytes"),
            format!("sealed segment {index}").as_bytes()
        );
    }
}

#[test]
fn window_rollover_refuses_mutation_after_a_pending_member_completed() {
    let root = tempfile::tempdir().expect("isolated full-window root");
    let mut batch = DurableFileBatch::new();
    let paths = write_members(&mut batch, root.path(), DURABLE_BATCH_CONCURRENT_SYNCS);
    let first = &paths[0];
    let original = fs::metadata(first).expect("original member identity");
    let incoming = root.path().join("incoming.tmp");
    let mut writer = File::create(&incoming).expect("incoming writer");
    writer.write_all(b"incoming segment").expect("incoming bytes");

    let (barriers, result) = thread::scope(|scope| {
        let (barriers, pause) = sync_latency::pause_after_batch_member(first);
        let worker = scope.spawn(|| batch.written(&incoming, &writer));
        assert!(pause.wait_for_sync(Duration::from_secs(10)));
        assert_eq!(
            barriers.syncs(),
            1,
            "a real member fsync must have completed"
        );

        let mut changed = File::options()
            .write(true)
            .open(first)
            .expect("same inode writer");
        changed
            .write_all(b"changed segment!")
            .expect("same-length mutation");
        changed
            .set_modified(original.modified().expect("original mtime") + Duration::from_secs(2))
            .expect("observable mutation even on coarse timestamp filesystems");
        drop(changed);
        let current = fs::metadata(first).expect("mutated member identity");
        assert_eq!(
            (original.dev(), original.ino(), original.len()),
            (current.dev(), current.ino(), current.len()),
            "the mutation must preserve both inode and byte count"
        );
        pause.resume();
        (barriers, worker.join().expect("admission worker"))
    });

    let error = result.expect_err("changed full window");
    assert_eq!(error.kind(), ErrorKind::InvalidData);
    assert_eq!(batch.pending.len(), DURABLE_BATCH_CONCURRENT_SYNCS);
    assert!(batch.synced.is_empty(), "no unproven member may be promoted");
    assert!(
        batch
            .pending
            .iter()
            .all(|member| member.identity.path != incoming)
    );
    assert_eq!(
        fs::read(first).expect("mutated bytes survive"),
        b"changed segment!"
    );
    assert_eq!(
        fs::read(&incoming).expect("incoming bytes survive"),
        b"incoming segment"
    );
    let error = batch
        .written(&incoming, &writer)
        .expect_err("retry preserves failed admission");
    assert_eq!(error.kind(), ErrorKind::InvalidData);
    assert_eq!(
        barriers.syncs(),
        1,
        "changed identity must fail before another fsync"
    );
    assert_eq!(batch.pending.len(), DURABLE_BATCH_CONCURRENT_SYNCS);
    assert!(batch.synced.is_empty());
}
