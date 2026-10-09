#![allow(clippy::unwrap_used)]

use super::*;
use crate::runtime::shared::read_new_rows;
use std::io::Write;

#[test]
fn raw_strict_scan_reports_typed_open_failure() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("missing.jsonl");

    let error = try_stream_new_jsonl_raw_strict(
        &path,
        StoredCursor::default(),
        None,
        MAX_JSONL_RECORD_BYTES,
    )
    .err()
    .expect("missing transcript must be a typed scan failure");

    assert!(matches!(
        error,
        TranscriptIngestError::ScanIo {
            operation: "open",
            path: error_path,
            ..
        } if error_path == path
    ));
}

#[test]
fn raw_strict_scan_finishes_one_valid_large_record_past_batch_budget() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("explicit-budget.jsonl");
    let contents = format!(
        "{{\"payload\":\"{}\"}}\n",
        "x".repeat((STRICT_JSONL_BATCH_BYTES as usize) + 1024)
    );
    assert!(contents.len() as u64 > STRICT_JSONL_BATCH_BYTES);
    assert!(contents.len() < MAX_JSONL_RECORD_BYTES);
    std::fs::write(&path, &contents).unwrap();

    let raw = try_stream_new_jsonl_raw_strict(
        &path,
        StoredCursor::default(),
        Some(STRICT_JSONL_BATCH_BYTES),
        MAX_JSONL_RECORD_BYTES,
    )
    .unwrap();

    assert_eq!(raw.frames.len(), 1);
    assert_eq!(raw.frames[0].offset, 0);
    assert_eq!(raw.frames[0].end_offset, contents.len() as u64);
    assert!(raw.skipped.is_empty());
    assert_eq!(raw.new_cursor.position, contents.len() as u64);
    assert_eq!(raw.deferred, None);
}

#[test]
fn raw_strict_scan_reports_partial_bytes_without_advancing_cursor() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("partial.jsonl");
    let contents = b"{\"payload\":\"unterminated";
    std::fs::write(&path, contents).unwrap();

    let raw = try_stream_new_jsonl_raw_strict(
        &path,
        StoredCursor::default(),
        Some(1),
        MAX_JSONL_RECORD_BYTES,
    )
    .unwrap();

    assert!(raw.frames.is_empty());
    assert!(raw.skipped.is_empty());
    assert_eq!(raw.start_offset, 0);
    assert_eq!(raw.read_through, contents.len() as u64);
    assert_eq!(raw.new_cursor.position, 0);
    assert_eq!(
        raw.deferred,
        Some(JsonlFrameDeferral::Partial { offset: 0 })
    );
}

#[test]
fn raw_strict_scan_starts_at_zero_after_fingerprint_prefetch() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("prefetch-boundary.jsonl");
    let record = b"{\"id\":1}\n";
    let contents = record.repeat(1_024);
    assert!(contents.len() > 8 * 1024);
    std::fs::write(&path, &contents).unwrap();

    let raw = try_stream_new_jsonl_raw_strict(
        &path,
        StoredCursor::default(),
        None,
        MAX_JSONL_RECORD_BYTES,
    )
    .unwrap();

    assert_eq!(raw.start_offset, 0);
    assert_eq!(raw.frames.first().unwrap().offset, 0);
    assert_eq!(raw.frames.len(), 1_024);
    assert!(raw.skipped.is_empty());
    assert_eq!(raw.new_cursor.position, contents.len() as u64);
    assert!(raw.deferred.is_none(), "{:?}", raw.deferred);
}

#[test]
fn raw_strict_scan_caps_tiny_frames_and_resumes_to_eof() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("many-tiny-frames.jsonl");
    let contents = b"{}\n".repeat((2 * 1024 * 1024) / 3 + 1);
    std::fs::write(&path, &contents).unwrap();

    let mut cursor = StoredCursor::default();
    let mut frame_count = 0_usize;
    let mut batches = 0_usize;
    while cursor.position < contents.len() as u64 {
        let raw =
            try_stream_new_jsonl_raw_strict(&path, cursor, None, MAX_JSONL_RECORD_BYTES).unwrap();
        assert!(!raw.frames.is_empty());
        assert!(raw.frames.len() <= MAX_JSONL_FRAMES_PER_BATCH);
        assert!(
            raw.new_cursor.position > cursor.position,
            "cursor stalled at {} with deferral {:?}",
            cursor.position,
            raw.deferred
        );
        frame_count += raw.frames.len();
        batches += 1;
        cursor = raw.new_cursor;
    }

    assert!(batches > 1);
    assert_eq!(frame_count, contents.len() / 3);
    assert_eq!(cursor.position, contents.len() as u64);
}

#[test]
fn raw_strict_scan_bounds_sparse_oversized_record_without_newline() {
    const RECORD_LIMIT: usize = 64 * 1024;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sparse-no-newline.jsonl");
    let file = std::fs::File::create(&path).unwrap();
    file.set_len(5 * 1024 * 1024).unwrap();

    let file_size = std::fs::metadata(&path).unwrap().len();
    let mut cursor = StoredCursor::default();
    let mut batches = 0_usize;
    while cursor.position < file_size {
        let raw = try_stream_new_jsonl_raw_strict(&path, cursor, None, RECORD_LIMIT).unwrap();
        assert!(raw.frames.is_empty());
        assert!(raw.new_cursor.position > cursor.position);
        assert!(
            raw.new_cursor.position - cursor.position <= STRICT_JSONL_BATCH_BYTES,
            "oversized quarantine exceeded the bounded recovery budget"
        );
        assert_eq!(
            raw.skipped.first().map(|range| range.offset),
            Some(cursor.position)
        );
        cursor = raw.new_cursor;
        batches += 1;
    }

    assert!(batches > 1);
    assert_eq!(cursor.position, file_size);
}

#[test]
fn raw_strict_recovery_advances_in_bounded_batches_through_large_backlog() {
    const PAYLOAD_BYTES: usize = 700 * 1024;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.jsonl");
    let record = format!("{{\"payload\":\"{}\"}}\n", "x".repeat(PAYLOAD_BYTES));
    let contents = record.repeat(4);
    assert!(contents.len() as u64 > STRICT_JSONL_BATCH_BYTES);
    std::fs::write(&path, contents).unwrap();

    let mut cursor = StoredCursor::default();
    let mut batches = 0;
    loop {
        let raw = stream_new_jsonl_raw_strict(&path, cursor, None, 1024 * 1024).unwrap();
        let retained_bytes = raw
            .frames
            .iter()
            .map(|frame| frame.bytes.len() as u64)
            .sum::<u64>();
        assert!(retained_bytes <= STRICT_JSONL_BATCH_BYTES);
        assert!(raw.new_cursor.position > cursor.position);
        assert_eq!(retained_bytes, raw.new_cursor.position - cursor.position);
        batches += 1;
        cursor = raw.new_cursor;

        match raw.deferred {
            Some(JsonlFrameDeferral::Backlog { offset, .. }) => {
                assert_eq!(offset, cursor.position);
            }
            None => break,
            Some(reason) => panic!("unexpected bounded-scan deferral: {reason:?}"),
        }
    }

    assert!(batches > 1);
    assert_eq!(cursor.position, std::fs::metadata(&path).unwrap().len());
}

#[test]
fn raw_strict_scan_covers_oversized_complete_and_partial_records_without_payload() {
    const MAX_RECORD_BYTES: usize = 32;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.jsonl");
    let prefix = "{\"id\":\"prefix\"}\n";
    let oversized = format!("{{\"payload\":\"{}\"}}", "x".repeat(MAX_RECORD_BYTES));

    for terminator in ["\n", ""] {
        std::fs::write(&path, format!("{prefix}{oversized}{terminator}")).unwrap();
        let file_len = std::fs::metadata(&path).unwrap().len();

        let raw =
            try_stream_new_jsonl_raw_strict(&path, StoredCursor::default(), None, MAX_RECORD_BYTES)
                .unwrap();
        assert_eq!(raw.frames.len(), 1);
        assert_eq!(raw.frames[0].bytes, prefix.as_bytes());
        assert_eq!(raw.skipped.len(), 1);
        assert_eq!(raw.skipped[0].reason, RawJsonlSkippedReason::Oversized);
        assert_eq!(raw.skipped[0].offset, prefix.len() as u64);
        assert_eq!(raw.skipped[0].end_offset, file_len);
        assert_eq!(raw.new_cursor.position, file_len);
    }
}

#[test]
fn raw_strict_scan_reports_exact_record_ranges_before_a_partial_tail() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.jsonl");
    let first = "{\"id\":1}\n";
    let blank = "\n";
    let second = "{\"id\":2}\n";
    let partial = "{\"id\":3}";
    std::fs::write(&path, format!("{first}{blank}{second}{partial}")).unwrap();

    let raw = try_stream_new_jsonl_raw_strict(&path, StoredCursor::default(), None, 64).unwrap();
    let second_offset = (first.len() + blank.len()) as u64;
    let partial_offset = second_offset + second.len() as u64;
    assert_eq!(
        raw.frames
            .iter()
            .map(|frame| (frame.offset, frame.end_offset))
            .collect::<Vec<_>>(),
        vec![(0, first.len() as u64), (second_offset, partial_offset)]
    );
    assert_eq!(
        raw.deferred,
        Some(JsonlFrameDeferral::Partial {
            offset: partial_offset
        })
    );
    assert_eq!(raw.new_cursor.position, partial_offset);
}

#[test]
fn raw_strict_scan_resumes_the_suffix_after_an_oversized_record() {
    const MAX_RECORD_BYTES: usize = 40;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.jsonl");
    let prefix = "{\"id\":\"prefix\"}\n";
    let suffix = "{\"id\":\"suffix\"}\n";
    let oversized = format!("{{\"payload\":\"{}\"}}\n", "x".repeat(MAX_RECORD_BYTES));
    std::fs::write(&path, format!("{prefix}{oversized}{suffix}")).unwrap();

    let first =
        try_stream_new_jsonl_raw_strict(&path, StoredCursor::default(), None, MAX_RECORD_BYTES)
            .unwrap();
    assert_eq!(first.frames.len(), 1);
    assert_eq!(first.frames[0].bytes, prefix.as_bytes());
    let suffix_offset = (prefix.len() + oversized.len()) as u64;
    assert_eq!(first.new_cursor.position, suffix_offset);

    let second =
        try_stream_new_jsonl_raw_strict(&path, first.new_cursor, None, MAX_RECORD_BYTES).unwrap();
    assert_eq!(second.frames.len(), 1);
    assert_eq!(second.frames[0].bytes, suffix.as_bytes());
    assert_eq!(
        second.new_cursor.position,
        std::fs::metadata(&path).unwrap().len()
    );
}

#[test]
fn raw_strict_scan_keeps_one_replacement_generation_across_batches() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rewritten.jsonl");
    std::fs::write(&path, "{\"a\":1}\n").unwrap();

    let first = try_stream_new_jsonl_raw_strict(
        &path,
        StoredCursor::default(),
        None,
        MAX_JSONL_RECORD_BYTES,
    )
    .unwrap();
    assert!(!first.replacement_generation);

    // Replace the file with more records than one batch can frame, so the
    // rewrite is only visible at the head of the first batch.
    let records = MAX_JSONL_FRAMES_PER_BATCH + 500;
    let rewritten = (0..records).fold(String::new(), |mut text, index| {
        text.push_str(&format!("{{\"b\":{index}}}\n"));
        text
    });
    std::fs::write(&path, &rewritten).unwrap();

    let head =
        try_stream_new_jsonl_raw_strict(&path, first.new_cursor, None, MAX_JSONL_RECORD_BYTES)
            .unwrap();
    assert_eq!(head.start_offset, 0);
    assert!(head.replacement_generation);
    assert_eq!(head.frames.len(), MAX_JSONL_FRAMES_PER_BATCH);
    assert_ne!(head.new_cursor.file_id, first.new_cursor.file_id);

    let tail =
        try_stream_new_jsonl_raw_strict(&path, head.new_cursor, None, MAX_JSONL_RECORD_BYTES)
            .unwrap();
    assert!(tail.start_offset > 0);
    // The rewrite is a property of the stored generation, so the tail keeps
    // the head's generation instead of reverting to the file identity.
    assert!(tail.replacement_generation);
    assert_eq!(tail.new_cursor.file_id, head.new_cursor.file_id);
    assert_eq!(tail.frames.len(), records - MAX_JSONL_FRAMES_PER_BATCH);
}

#[test]
fn raw_strict_scan_mints_a_distinct_generation_for_each_rewrite() {
    let scan = |path: &Path, cursor| {
        try_stream_new_jsonl_raw_strict(path, cursor, None, MAX_JSONL_RECORD_BYTES).unwrap()
    };
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("repeated-rewrite.jsonl");
    // A stable head line keeps the file identity constant across rewrites so
    // only the recorded generation can separate them.
    std::fs::write(&path, "{\"same\":1}\n{\"a\":1}\n").unwrap();
    let first = scan(&path, StoredCursor::default());
    assert!(!first.replacement_generation);

    std::fs::write(&path, "{\"same\":1}\n").unwrap();
    let second = scan(&path, first.new_cursor);
    assert_eq!(second.start_offset, 0);
    assert!(second.replacement_generation);

    std::fs::write(&path, "{\"same\":1}\n{\"b\":2}\n").unwrap();
    let appended = scan(&path, second.new_cursor);
    assert!(appended.start_offset > 0);
    assert!(appended.replacement_generation);
    assert_eq!(appended.new_cursor.file_id, second.new_cursor.file_id);

    std::fs::write(&path, "{\"same\":1}\n").unwrap();
    let third = scan(&path, appended.new_cursor);
    assert_eq!(third.start_offset, 0);
    assert!(third.replacement_generation);
    assert_ne!(third.new_cursor.file_id, second.new_cursor.file_id);
}

#[test]
fn raw_strict_resume_checkpoint_detects_same_inode_middle_rewrite() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("same-inode-middle.jsonl");
    let original = b"{\"v\":0}\n".repeat(12_000);
    std::fs::write(&path, &original).unwrap();

    let first = try_stream_new_jsonl_raw_strict_with_resume(
        &path,
        StoredCursor::default(),
        None,
        MAX_JSONL_RECORD_BYTES,
        None,
    )
    .unwrap();
    let checkpoint = JsonlResumeState {
        generation: first.new_cursor.file_id,
        file_identity: first.file_identity,
        fingerprint: first.frames.last().unwrap().resume_fingerprint,
    };

    let mut rewritten = original;
    let changed = b"{\"v\":0}\n".len() * 2_111;
    rewritten[changed..changed + b"{\"v\":0}\n".len()].copy_from_slice(b"{\"v\":1}\n");
    std::fs::write(&path, rewritten).unwrap();

    let second = try_stream_new_jsonl_raw_strict_with_resume(
        &path,
        first.new_cursor,
        None,
        MAX_JSONL_RECORD_BYTES,
        Some(checkpoint),
    )
    .unwrap();
    assert_eq!(second.start_offset, 0);
    assert_ne!(second.new_cursor.file_id, checkpoint.generation);
    assert_eq!(second.frames.len(), 4_096);
}

#[test]
fn raw_strict_resume_survives_rename_replacement_with_unchanged_prefix() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("renamed.jsonl");
    let staged = dir.path().join("renamed.jsonl.tmp");
    let replace = |contents: &[u8]| {
        std::fs::write(&staged, contents).unwrap();
        std::fs::rename(&staged, &path).unwrap();
    };
    let checkpoint_of = |scan: &jsonl::RawNewJsonl| JsonlResumeState {
        generation: scan.new_cursor.file_id,
        file_identity: scan.file_identity,
        fingerprint: scan.frames.last().unwrap().resume_fingerprint,
    };
    let original = b"{\"v\":0}\n{\"v\":1}\n";
    std::fs::write(&path, original).unwrap();
    let first = try_stream_new_jsonl_raw_strict_with_resume(
        &path,
        StoredCursor::default(),
        None,
        MAX_JSONL_RECORD_BYTES,
        None,
    )
    .unwrap();
    assert_eq!(first.frames.len(), 2);

    replace(original);
    let identical = try_stream_new_jsonl_raw_strict_with_resume(
        &path,
        first.new_cursor,
        None,
        MAX_JSONL_RECORD_BYTES,
        Some(checkpoint_of(&first)),
    )
    .unwrap();
    assert_eq!(
        (
            identical.start_offset,
            identical.frames.len(),
            identical.new_cursor.file_id,
            identical.replacement_generation,
        ),
        (16, 0, first.new_cursor.file_id, false)
    );

    replace(b"{\"v\":0}\n{\"v\":1}\n{\"v\":2}\n");
    let appended = try_stream_new_jsonl_raw_strict_with_resume(
        &path,
        first.new_cursor,
        None,
        MAX_JSONL_RECORD_BYTES,
        Some(checkpoint_of(&first)),
    )
    .unwrap();
    assert_eq!(
        (
            appended.start_offset,
            appended.frames.len(),
            appended.new_cursor.position,
            appended.new_cursor.file_id,
            appended.file_identity,
            appended.replacement_generation,
        ),
        (
            16,
            1,
            24,
            first.new_cursor.file_id,
            first.file_identity,
            false
        )
    );

    replace(b"{\"v\":9}\n{\"v\":1}\n{\"v\":2}\n");
    let edited = try_stream_new_jsonl_raw_strict_with_resume(
        &path,
        appended.new_cursor,
        None,
        MAX_JSONL_RECORD_BYTES,
        Some(checkpoint_of(&appended)),
    )
    .unwrap();
    assert_eq!(edited.start_offset, 0);
    assert_eq!(edited.frames.len(), 3);
    assert!(edited.replacement_generation);
    assert_ne!(edited.new_cursor.file_id, first.new_cursor.file_id);
}

/// One resumed scan must walk the validated prefix exactly once.
///
/// Checkpoint validation and the scanner's resume digest both need the digest
/// of `[0, cursor)`, and they used to derive it independently, so every resumed
/// scan read that prefix twice. The charge is byte-exact, so this asserts the
/// count rather than elapsed time: before the fix it was `2 * prefix`.
#[test]
fn raw_strict_resume_validates_the_prefix_once_per_scan() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("prefix-once.jsonl");
    let record = b"{\"v\":0}\n";
    let prefix_records = 512;
    std::fs::write(&path, record.repeat(prefix_records)).unwrap();

    let first = try_stream_new_jsonl_raw_strict_with_resume(
        &path,
        StoredCursor::default(),
        None,
        MAX_JSONL_RECORD_BYTES,
        None,
    )
    .unwrap();
    let checkpoint = JsonlResumeState {
        generation: first.new_cursor.file_id,
        file_identity: first.file_identity,
        fingerprint: first.frames.last().unwrap().resume_fingerprint,
    };
    let prefix_bytes = first.new_cursor.position;
    assert_eq!(prefix_bytes, (record.len() * prefix_records) as u64);

    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"{\"v\":1}\n")
        .unwrap();

    let second = try_stream_new_jsonl_raw_strict_with_resume(
        &path,
        first.new_cursor,
        None,
        MAX_JSONL_RECORD_BYTES,
        Some(checkpoint),
    )
    .unwrap();

    // The append is still read, so the scan did real work and the byte charge
    // below is not vacuously zero.
    assert_eq!(second.frames.len(), 1, "the appended frame must be scanned");
    assert_eq!(second.start_offset, prefix_bytes);
    // A platform without a rewrite witness cannot trust the token, so the
    // commit step re-proves the consumed prefix at `read_through` bytes.
    let commit_proof = if tracedecay_private_fs::RewriteWitness::NATIVE.proves_unchanged_bytes() {
        0
    } else {
        second.read_through
    };
    assert_eq!(
        second.io.prefix_validation_bytes,
        prefix_bytes + commit_proof,
        "the resumed prefix must be hashed once, not once per consumer"
    );
}

/// A first-sight scan must not hash the whole file to police itself.
///
/// The snapshot fingerprint exists to catch a rewrite that lands *during* a
/// scan, and catching that needs two independent full passes, one before the
/// read and one after. On a cold catch-up every file takes both, which is why
/// ingesting 109 MB of transcript cost 43.5 GB of hashing. Identity, size and
/// mtime already fail closed on every observable change, so the pair is spent
/// only where a rewrite has actually been observed, never on first sight.
#[test]
fn cold_full_file_scan_does_not_hash_the_whole_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cold.jsonl");
    let record = b"{\"v\":0}\n";
    let records = 512;
    std::fs::write(&path, record.repeat(records)).unwrap();
    // A change time still inside the kernel's coarse quantum is not proof the
    // bytes are stable, so the scan seals a snapshot. This assertion is about
    // a settled file, whose token already rules that rewrite out.
    super::jsonl::spin_until_jsonl_change_settled(&path);

    let scan = try_stream_new_jsonl_raw_strict_with_resume(
        &path,
        StoredCursor::default(),
        None,
        MAX_JSONL_RECORD_BYTES,
        None,
    )
    .unwrap();

    assert_eq!(scan.frames.len(), records);
    assert_eq!(scan.io.content_bytes, (record.len() * records) as u64);
    assert_eq!(scan.io.snapshot_hash_bytes, 0);
    assert_eq!(scan.io.change, JsonlChangeKind::Cold);
}

/// A settled re-poll of an unchanged transcript performs zero content reads
/// after a fully verified scan has populated the generation cache.
///
/// The canonical handle is still opened so native identity and high-resolution
/// metadata can be checked. The production read meter proves that neither
/// identity hashing nor prefix validation touched its contents.
#[cfg(unix)]
#[test]
fn unchanged_settled_repoll_reads_zero_file_bytes() {
    // The warm entry this test proves must survive between its two polls, and
    // the isolation reset is process-global.
    let dir = tempfile::tempdir().unwrap();
    let _hold = super::jsonl::HoldUnchangedGenerationCache::enter(dir.path());
    let path = dir.path().join("warm.jsonl");
    let record = b"{\"v\":0}\n";
    std::fs::write(&path, record.repeat(8)).unwrap();
    // The proving scan has to observe a settled change time. A cache entry
    // recorded inside the coarse quantum would authorize a later repoll to
    // skip bytes a same-length rewrite could still have replaced.
    super::jsonl::spin_until_jsonl_change_settled(&path);

    let first = try_stream_new_jsonl_raw_strict_with_resume(
        &path,
        StoredCursor::default(),
        None,
        MAX_JSONL_RECORD_BYTES,
        None,
    )
    .unwrap();
    assert_eq!(first.frames.len(), 8, "the cold pass must read the file");
    let checkpoint = JsonlResumeState {
        generation: first.new_cursor.file_id,
        file_identity: first.file_identity,
        fingerprint: first.frames.last().unwrap().resume_fingerprint,
    };

    let second = try_stream_new_jsonl_raw_strict_with_resume(
        &path,
        first.new_cursor,
        None,
        MAX_JSONL_RECORD_BYTES,
        Some(checkpoint),
    )
    .unwrap();

    assert!(second.frames.is_empty(), "nothing was appended");
    assert_eq!(
        second.new_cursor, first.new_cursor,
        "the cursor must not move"
    );
    assert_eq!(second.file_identity, first.file_identity);
    assert_eq!(
        second.io.identity_window_bytes, 0,
        "identity must come from the checkpoint, not from re-hashing a head window"
    );
    assert_eq!(
        second.io.prefix_validation_bytes, 0,
        "an unchanged file must not re-walk its prefix"
    );
    assert_eq!(second.io.content_bytes, 0);
    assert_eq!(second.io.scan_payload_read_bytes, 0);
    assert_eq!(second.io.change, JsonlChangeKind::Unchanged);
}

#[test]
fn collect_files_bounds_recursive_discovery_by_depth() {
    let dir = tempfile::tempdir().unwrap();
    let root_transcript = dir.path().join("root.jsonl");
    let nested = dir.path().join("nested");
    let nested_transcript = nested.join("nested.jsonl");
    let too_deep = nested.join("deeper").join("ignored.jsonl");
    std::fs::create_dir_all(too_deep.parent().unwrap()).unwrap();
    std::fs::write(&root_transcript, "{}\n").unwrap();
    std::fs::write(&nested_transcript, "{}\n").unwrap();
    std::fs::write(&too_deep, "{}\n").unwrap();

    let mut discovered = collect_files_with_ext(dir.path(), "jsonl", 1);
    discovered.sort();
    let mut expected = vec![root_transcript, nested_transcript];
    expected.sort();

    assert_eq!(discovered, expected);
}

#[test]
fn collect_files_enforces_file_count_before_materializing_all_entries() {
    let dir = tempfile::tempdir().unwrap();
    // Generate candidates incrementally so the walk must stop mid-directory.
    for index in 0..32 {
        std::fs::write(dir.path().join(format!("session-{index:02}.jsonl")), "{}\n").unwrap();
    }
    let bounds = TranscriptDiscoveryBounds {
        max_files: 3,
        ..TranscriptDiscoveryBounds::default_walk()
    };
    let report = collect_files_with_ext_bounded(dir.path(), "jsonl", 0, bounds);
    assert_eq!(report.paths.len(), 3);
    assert_eq!(report.truncated, Some(FileDiscoveryLimit::FileCount));
    assert!(
        report.bytes_charged > 0,
        "discovery must charge path/metadata bytes before retention"
    );
}

#[test]
#[cfg(unix)]
fn collect_files_skips_directory_symlink_trees() {
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let link = dir.path().join("link");
    std::fs::write(outside.path().join("hidden.jsonl"), "{}\n").unwrap();
    std::fs::write(dir.path().join("visible.jsonl"), "{}\n").unwrap();
    std::os::unix::fs::symlink(outside.path(), &link).unwrap();
    let report = collect_files_with_ext_bounded(
        dir.path(),
        "jsonl",
        2,
        TranscriptDiscoveryBounds::default_walk(),
    );
    assert_eq!(report.paths, vec![dir.path().join("visible.jsonl")]);
    assert!(
        !report
            .paths
            .iter()
            .any(|path| path.ends_with("hidden.jsonl")),
        "directory symlink escape must not be followed"
    );
    assert!(report.truncated.is_none());
}

#[test]
fn collect_files_rejects_oversized_path_components_without_retaining_them() {
    let dir = tempfile::tempdir().unwrap();
    let ok = dir.path().join("ok.jsonl");
    std::fs::write(&ok, "{}\n").unwrap();
    let ok_bytes = path_byte_len(&ok);
    let bounds = TranscriptDiscoveryBounds {
        max_path_bytes: ok_bytes,
        ..TranscriptDiscoveryBounds::default_walk()
    };
    // Stay under common NAME_MAX (255) while exceeding the full-path discovery cap.
    let oversized_name = format!("{}.jsonl", "x".repeat(80));
    std::fs::write(dir.path().join(&oversized_name), "{}\n").unwrap();
    let report = collect_files_with_ext_bounded(dir.path(), "jsonl", 0, bounds);
    assert_eq!(report.paths, vec![ok]);
    assert!(report.skipped_oversized_entries >= 1);
    let leaked = format!("{:?}", report.paths);
    assert!(
        !leaked.contains(&oversized_name),
        "oversized path payload must not be retained"
    );
}

#[test]
fn bound_path_list_stops_on_cumulative_discovery_bytes() {
    let meta = std::mem::size_of::<std::fs::Metadata>() as u64;
    let bounds = TranscriptDiscoveryBounds {
        max_files: 100,
        max_path_bytes: 64,
        max_metadata_bytes: meta.max(64),
        max_discovery_bytes: meta.saturating_mul(2).saturating_add(40),
    };
    let paths = (0..20)
        .map(|index| PathBuf::from(format!("session-{index:02}.jsonl")))
        .collect::<Vec<_>>();
    let report = bound_path_list(paths, bounds);
    assert!(report.paths.len() < 20);
    assert_eq!(report.truncated, Some(FileDiscoveryLimit::DiscoveryBytes));
    assert!(report.bytes_charged <= bounds.max_discovery_bytes);
}

#[tokio::test]
async fn read_new_rows_tracks_last_rowid() {
    // A synthetic SQLite-backed source exercises the RowCursor kind. Seed via
    // a shared in-memory database so the reader handle observes later writes.
    let seed =
        rusqlite::Connection::open("file:read_new_rows_tracks?mode=memory&cache=shared").unwrap();
    seed.execute_batch(
        "CREATE TABLE turns (role TEXT, text TEXT);\n\
         INSERT INTO turns (role, text) VALUES ('user', 'hello'), ('assistant', 'hi');",
    )
    .unwrap();
    let conn = crate::runtime::shared::SqliteReadConn::new(
        rusqlite::Connection::open("file:read_new_rows_tracks?mode=memory&cache=shared").unwrap(),
    );

    let sql = "SELECT rowid, role, text FROM turns WHERE rowid > ? ORDER BY rowid";
    let map = |_rowid: i64, row: &rusqlite::Row<'_>| row.get::<_, String>(2).ok();
    let first = read_new_rows(&conn, sql, StoredCursor::default(), map)
        .await
        .unwrap();
    assert_eq!(first.items, vec!["hello".to_string(), "hi".to_string()]);
    assert_eq!(first.new_cursor.position, 2);

    // No new rows past the advanced cursor.
    let again = read_new_rows(&conn, sql, first.new_cursor, map)
        .await
        .unwrap();
    assert_eq!(again.items.len(), 0);

    seed.execute(
        "INSERT INTO turns (role, text) VALUES ('user', 'again')",
        (),
    )
    .unwrap();
    let third = read_new_rows(&conn, sql, again.new_cursor, map)
        .await
        .unwrap();
    assert_eq!(third.items, vec!["again".to_string()]);
    assert_eq!(third.new_cursor.position, 3);
}

#[tokio::test]
async fn read_new_rows_returns_none_for_invalid_query() {
    let connection = rusqlite::Connection::open_in_memory().unwrap();
    connection
        .execute_batch("CREATE TABLE turns (text TEXT); INSERT INTO turns (text) VALUES ('hello');")
        .unwrap();
    let conn = crate::runtime::shared::SqliteReadConn::new(connection);
    let map = |_rowid: i64, row: &rusqlite::Row<'_>| row.get::<_, String>(1).ok();

    let rows = read_new_rows(
        &conn,
        "SELECT rowid, not_a_column FROM missing_table WHERE rowid > ? ORDER BY rowid",
        StoredCursor::default(),
        map,
    )
    .await;
    assert!(rows.is_none());

    let rows = read_new_rows(
        &conn,
        "SELECT rowid, text FROM turns WHERE rowid > ? ORDER BY rowid",
        StoredCursor::default(),
        map,
    )
    .await
    .unwrap();
    assert_eq!(rows.items, vec!["hello".to_string()]);
}

#[test]
fn jsonl_file_identity_is_stable_for_an_unchanged_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    std::fs::write(&path, "{\"role\":\"user\"}\n").unwrap();
    let first = jsonl_file_identity(&path).expect("file identity");
    let second = jsonl_file_identity(&path).expect("file identity");
    assert_eq!(first, second);
    assert_ne!(first, 0);
}

#[test]
fn content_hash_stays_inside_the_persisted_cursor_domain() {
    let probes = ["", "a", "cline ui_messages", "{\"ts\":1}", "kiro chat"];
    for content in probes {
        assert!(
            content_hash64(content) <= i64::MAX as u64,
            "hash for {content:?} must fit the typed non-negative cursor column"
        );
    }
    // Anti-vacuity: at least one probe's raw digest sets the top bit, so this
    // test fails if the 63-bit mask is removed.
    assert!(
        probes.iter().any(|content| {
            let digest = Sha256::digest(content.as_bytes());
            digest[0] & 0x80 != 0
        }),
        "probe set must include a digest with the top bit set"
    );
}

fn busy_transcript_section() -> (u64, std::thread::ThreadId) {
    let work_thread = std::thread::current().id();
    let start = std::time::Instant::now();
    let mut acc = 0u64;
    while start.elapsed() < std::time::Duration::from_millis(200) {
        acc = acc.wrapping_add(1);
        std::hint::black_box(acc);
    }
    (acc, work_thread)
}

async fn ping_spawned_request_runtime(
    started: tokio::sync::oneshot::Receiver<std::thread::ThreadId>,
) -> (
    std::time::Duration,
    Vec<std::time::Duration>,
    std::thread::ThreadId,
) {
    let worker = started.await.expect("busy section must start");
    let first_poll_start = std::time::Instant::now();
    let first_poll = tokio::spawn(async move { first_poll_start.elapsed() })
        .await
        .expect("join first request poll");
    let mut latencies = Vec::with_capacity(40);
    for _ in 0..40 {
        let ping = std::time::Instant::now();
        tokio::task::yield_now().await;
        latencies.push(ping.elapsed());
    }
    (first_poll, latencies, worker)
}

fn print_section_scorecard(
    label: &str,
    first_poll: std::time::Duration,
    mut latencies: Vec<std::time::Duration>,
    ingest_elapsed: std::time::Duration,
) {
    latencies.sort();
    let p50 = latencies[latencies.len() / 2];
    let p95 = latencies[(latencies.len() * 95) / 100];
    eprintln!(
        "blocking-section scorecard ({label}): first-poll={first_poll:?} request p50={p50:?} p95={p95:?} ingest={ingest_elapsed:?}"
    );
}

/// Same-host scorecard: request-runtime ping latency while a CPU-heavy
/// historical ingest section runs with the old `block_in_place` placement
/// and with `spawn_blocking`. Pings are spawned tasks so they compete for
/// the only worker the way daemon requests do.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn blocking_section_request_latency_scorecard() {
    let (before_started_tx, before_started_rx) = tokio::sync::oneshot::channel();
    let before_ingest = tokio::spawn(async move {
        let worker = std::thread::current().id();
        let wall = std::time::Instant::now();
        tokio::task::block_in_place(move || {
            let _ = before_started_tx.send(worker);
            let (work, work_thread) = busy_transcript_section();
            (wall.elapsed(), work, work_thread)
        })
    });
    let before_pings = tokio::spawn(ping_spawned_request_runtime(before_started_rx));
    let (before_first_poll, before_latencies, before_worker) =
        before_pings.await.expect("join before pings");
    let (before_ingest, before_work, before_thread) =
        before_ingest.await.expect("join legacy section");
    assert!(before_work > 0, "legacy section must perform work");
    assert_eq!(
        before_thread, before_worker,
        "block_in_place must keep CPU on the Tokio worker"
    );
    print_section_scorecard(
        "before block_in_place",
        before_first_poll,
        before_latencies,
        before_ingest,
    );

    let (after_started_tx, after_started_rx) = tokio::sync::oneshot::channel();
    let after_ingest = tokio::spawn(async move {
        let worker = std::thread::current().id();
        let wall = std::time::Instant::now();
        run_blocking_transcript_section(move || {
            let _ = after_started_tx.send(worker);
            let (work, work_thread) = busy_transcript_section();
            (wall.elapsed(), work, work_thread)
        })
        .await
    });
    let after_pings = tokio::spawn(ping_spawned_request_runtime(after_started_rx));
    let (after_first_poll, after_latencies, after_worker) =
        after_pings.await.expect("join after pings");
    let (after_ingest, after_work, after_thread) =
        after_ingest.await.expect("join ingest section");
    assert!(after_work > 0, "ingest section must perform work");
    assert_ne!(
        after_thread, after_worker,
        "historical ingest CPU must leave the Tokio worker thread"
    );
    print_section_scorecard(
        "after spawn_blocking",
        after_first_poll,
        after_latencies,
        after_ingest,
    );
}

/// The offload helper must run the section on the blocking pool: with a
/// single-worker multi-thread runtime, a task spawned *from inside* the
/// section can only run if the worker is free. Running the section inline
/// would deadlock this test until the receive timeout fails it.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn blocking_transcript_section_yields_the_worker_queue() {
    let handle = tokio::runtime::Handle::current();
    let worker = std::thread::current().id();
    let (value, work_thread) = tokio::spawn(async move {
        let (sender, receiver) = std::sync::mpsc::channel();
        run_blocking_transcript_section(move || {
            let work_thread = std::thread::current().id();
            handle.spawn(async move {
                let _ = sender.send(());
            });
            receiver
                .recv_timeout(std::time::Duration::from_secs(5))
                .map(|()| (7, work_thread))
                .expect("a task spawned during the blocking section must run")
        })
        .await
    })
    .await
    .expect("join blocking section");
    assert_eq!(value, 7);
    assert_ne!(
        work_thread, worker,
        "transcript sections must not execute on a Tokio worker"
    );
}

/// Current-thread runtimes have no `block_in_place`, so the helper must
/// still return its value through the blocking pool.
#[tokio::test]
async fn blocking_transcript_section_runs_on_current_thread_runtime() {
    assert_eq!(run_blocking_transcript_section(|| 11).await, 11);
}
