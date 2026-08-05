#![allow(clippy::unwrap_used)]

use super::*;
use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Clone, Copy)]
enum ReadFailure {
    Offset,
    Session,
}

struct ReadFailureStore(ReadFailure);

struct SinglePathSource;

#[derive(Default)]
struct CountingStore(AtomicUsize);

struct MixedPathSource;

impl TranscriptSource for SinglePathSource {
    fn provider(&self) -> &'static str {
        "test"
    }

    fn transcript_paths(&self, _project_root: &Path) -> Vec<PathBuf> {
        vec![PathBuf::from("failure.jsonl")]
    }

    fn parse_new(
        &self,
        _path: &Path,
        _prev: StoredCursor,
        _project_root: &Path,
        _max_new_bytes: Option<u64>,
    ) -> Option<ParsedTranscript> {
        None
    }
}

impl TranscriptSource for MixedPathSource {
    fn provider(&self) -> &'static str {
        "mixed"
    }

    fn transcript_paths(&self, _project_root: &Path) -> Vec<PathBuf> {
        ["good-first.jsonl", "bad-middle.jsonl", "good-last.jsonl"]
            .into_iter()
            .map(PathBuf::from)
            .collect()
    }

    fn parse_new(
        &self,
        _path: &Path,
        _prev: StoredCursor,
        _project_root: &Path,
        _max_new_bytes: Option<u64>,
    ) -> Option<ParsedTranscript> {
        unreachable!("typed test source uses try_parse_new")
    }

    fn try_parse_new(
        &self,
        path: &Path,
        _prev: StoredCursor,
        _project_root: &Path,
        _max_new_bytes: Option<u64>,
    ) -> TranscriptIngestResult<Option<ParsedTranscript>> {
        if path == Path::new("bad-middle.jsonl") {
            return Err(TranscriptIngestError::scan_io(
                "read",
                path,
                std::io::Error::other("injected source failure"),
            ));
        }
        Ok(Some(ParsedTranscript {
            draft: SessionDraft {
                session_id: path.to_string_lossy().into_owned(),
                project_key: "mixed-project".to_string(),
                project_path: "mixed-project".to_string(),
                title: None,
                metadata_json: None,
                parent_session_id: None,
                is_subagent: false,
                agent_id: None,
                parent_tool_use_id: None,
            },
            messages: Vec::new(),
            new_cursor: StoredCursor {
                position: 1,
                mtime: 1,
                file_id: 1,
            },
        }))
    }
}

fn injected_store_error(operation: &'static str) -> TranscriptStoreError {
    TranscriptStoreError::Storage {
        operation,
        source: Box::new(std::io::Error::other("injected transcript store failure")),
    }
}

impl tracedecay_store::TranscriptStore for ReadFailureStore {
    fn get_parse_offset(
        &self,
        _cursor_path: &Path,
    ) -> impl std::future::Future<Output = tracedecay_store::TranscriptStoreResult<ParseOffset>> + Send
    {
        std::future::ready(match self.0 {
            ReadFailure::Offset => Err(injected_store_error("get_parse_offset")),
            ReadFailure::Session => Ok(ParseOffset::default()),
        })
    }

    fn persist_transcript_batch(
        &self,
        _batch: TranscriptWriteBatch,
    ) -> impl std::future::Future<Output = tracedecay_store::TranscriptStoreResult<()>> + Send {
        std::future::ready(Ok(()))
    }
}

impl TranscriptIngestStore for ReadFailureStore {
    fn get_session(
        &self,
        _provider: &str,
        _session_id: &str,
    ) -> impl std::future::Future<
        Output = tracedecay_store::TranscriptStoreResult<Option<SessionRecord>>,
    > + Send {
        std::future::ready(match self.0 {
            ReadFailure::Offset => Ok(None),
            ReadFailure::Session => Err(injected_store_error("get_session")),
        })
    }

    fn persist_transcript_batch_with_git_evidence(
        &self,
        _batch: TranscriptWriteBatch,
        _commit_records: &[crate::runtime::git_correlation::CommitSessionRecord],
        _span_observations: &[crate::runtime::git_correlation::SpanObservation],
    ) -> impl std::future::Future<Output = tracedecay_store::TranscriptStoreResult<()>> + Send {
        std::future::ready(Ok(()))
    }
}

impl tracedecay_store::TranscriptStore for CountingStore {
    fn get_parse_offset(
        &self,
        _cursor_path: &Path,
    ) -> impl std::future::Future<Output = tracedecay_store::TranscriptStoreResult<ParseOffset>> + Send
    {
        std::future::ready(Ok(ParseOffset::default()))
    }

    fn persist_transcript_batch(
        &self,
        _batch: TranscriptWriteBatch,
    ) -> impl std::future::Future<Output = tracedecay_store::TranscriptStoreResult<()>> + Send {
        self.0.fetch_add(1, Ordering::Relaxed);
        std::future::ready(Ok(()))
    }
}

impl TranscriptIngestStore for CountingStore {
    fn get_session(
        &self,
        _provider: &str,
        _session_id: &str,
    ) -> impl std::future::Future<
        Output = tracedecay_store::TranscriptStoreResult<Option<SessionRecord>>,
    > + Send {
        std::future::ready(Ok(None))
    }

    fn persist_transcript_batch_with_git_evidence(
        &self,
        _batch: TranscriptWriteBatch,
        _commit_records: &[crate::runtime::git_correlation::CommitSessionRecord],
        _span_observations: &[crate::runtime::git_correlation::SpanObservation],
    ) -> impl std::future::Future<Output = tracedecay_store::TranscriptStoreResult<()>> + Send {
        self.0.fetch_add(1, Ordering::Relaxed);
        std::future::ready(Ok(()))
    }
}

#[tokio::test]
async fn fail_fast_ingest_stops_at_first_bad_path_after_persisting_earlier_paths() {
    let fail_fast_store = CountingStore::default();
    assert!(
        try_ingest_source_with_store(
            &fail_fast_store,
            &MixedPathSource,
            Path::new("mixed-project"),
            None,
        )
        .await
        .is_err()
    );
    assert_eq!(fail_fast_store.0.load(Ordering::Relaxed), 1);
}

#[test]
fn session_metadata_merge_is_additive_and_never_regresses_existing_values() {
    let merged = merge_session_metadata(
        Some(r#"{"stable":"original","existing_only":1}"#),
        Some(r#"{"stable":"replacement","new_only":2}"#.to_string()),
    )
    .unwrap();
    let merged: Value = serde_json::from_str(&merged).unwrap();

    assert_eq!(merged["stable"], "original");
    assert_eq!(merged["existing_only"], 1);
    assert_eq!(merged["new_only"], 2);
}

#[test]
fn session_metadata_merge_unions_incremental_rollups_stably() {
    let existing = serde_json::json!({
        "pr_links": [
            {"pr_url": "https://example.test/pull/2", "pr_number": 2},
            {"pr_url": "https://example.test/pull/1", "pr_number": 1}
        ],
        "edited_files": [
            {"path": "src/old.rs", "change_type": "edit", "hunks": 1}
        ]
    });
    let incoming = serde_json::json!({
        "pr_links": [
            {"pr_url": "https://example.test/pull/1", "pr_number": 1},
            {"pr_url": "https://example.test/pull/3", "pr_number": 3}
        ],
        "edited_files": [
            {"path": "src/old.rs", "change_type": "edit", "hunks": 9},
            {"path": "src/new.rs", "change_type": "create", "hunks": 2}
        ]
    });

    let merged =
        merge_session_metadata(Some(&existing.to_string()), Some(incoming.to_string())).unwrap();
    let merged: Value = serde_json::from_str(&merged).unwrap();

    assert_eq!(
        merged["pr_links"],
        serde_json::json!([
            {"pr_url": "https://example.test/pull/2", "pr_number": 2},
            {"pr_url": "https://example.test/pull/1", "pr_number": 1},
            {"pr_url": "https://example.test/pull/3", "pr_number": 3}
        ])
    );
    assert_eq!(
        merged["edited_files"],
        serde_json::json!([
            {"path": "src/old.rs", "change_type": "edit", "hunks": 1},
            {"path": "src/new.rs", "change_type": "create", "hunks": 2}
        ])
    );

    let merged_again =
        merge_session_metadata(Some(&merged.to_string()), Some(incoming.to_string())).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&merged_again).unwrap(),
        merged
    );
}

#[tokio::test]
async fn load_transcript_cursor_propagates_offset_read_failure() {
    let error = load_transcript_cursor(
        &ReadFailureStore(ReadFailure::Offset),
        TranscriptCursorKey::for_path(Path::new("failure.jsonl")),
    )
    .await
    .err()
    .expect("offset read failure must not look like zero work");
    assert!(matches!(
        error,
        TranscriptIngestError::Store(TranscriptStoreError::Storage {
            operation: "get_parse_offset",
            ..
        })
    ));
}

#[tokio::test]
async fn try_ingest_source_with_store_propagates_offset_read_failure() {
    let error = try_ingest_source_with_store(
        &ReadFailureStore(ReadFailure::Offset),
        &SinglePathSource,
        Path::new("failure-project"),
        None,
    )
    .await
    .unwrap_err();
    assert!(matches!(
        error,
        TranscriptIngestError::Store(TranscriptStoreError::Storage {
            operation: "get_parse_offset",
            ..
        })
    ));
}

#[tokio::test]
async fn persist_parsed_transcript_propagates_session_read_failure() {
    let store = ReadFailureStore(ReadFailure::Session);
    let path = Path::new("failure.jsonl");
    let loaded = load_transcript_cursor(&store, TranscriptCursorKey::for_path(path))
        .await
        .unwrap();
    let previous = loaded.checkpoint.clone();
    let parsed = ParsedTranscript {
        draft: SessionDraft {
            session_id: "failure-session".to_string(),
            project_key: "failure-project".to_string(),
            project_path: "failure-project".to_string(),
            title: None,
            metadata_json: None,
            parent_session_id: None,
            is_subagent: false,
            agent_id: None,
            parent_tool_use_id: None,
        },
        messages: vec![SessionMessageRecord {
            provider: "test".to_string(),
            message_id: "failure-message".to_string(),
            session_id: "failure-session".to_string(),
            role: "user".to_string(),
            timestamp: None,
            ordinal: 0,
            text: "failure".to_string(),
            kind: None,
            model: None,
            tool_names: None,
            source_path: None,
            source_offset: Some(0),
            metadata_json: None,
        }],
        new_cursor: StoredCursor {
            position: 8,
            mtime: 1,
            file_id: 1,
        },
    };

    let error = persist_parsed_transcript(
        &store,
        "test",
        path,
        Path::new("failure-project"),
        loaded,
        &previous,
        parsed,
    )
    .await
    .unwrap_err();
    assert!(matches!(
        error,
        TranscriptIngestError::Store(TranscriptStoreError::Storage {
            operation: "get_session",
            ..
        })
    ));
}

#[test]
fn stream_new_jsonl_reads_only_appended_lines() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.jsonl");
    std::fs::write(&path, "{\"a\":1}\n{\"a\":2}\n").unwrap();

    let first = stream_new_jsonl(&path, StoredCursor::default(), None).unwrap();
    assert_eq!(first.lines.len(), 2);

    // Re-reading from the advanced cursor yields nothing.
    let again = stream_new_jsonl(&path, first.new_cursor, None).unwrap();
    assert_eq!(again.lines.len(), 0);

    // Appending one line yields only that line on the next read.
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    f.write_all(b"{\"a\":3}\n").unwrap();
    drop(f);
    let third = stream_new_jsonl(&path, again.new_cursor, None).unwrap();
    assert_eq!(third.lines.len(), 1);
    assert_eq!(third.lines[0].value["a"], 3);
}

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
fn stream_new_jsonl_defers_partial_final_line_and_respects_cap() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.jsonl");
    std::fs::write(&path, "{\"a\":1}\n{\"a\":2}").unwrap(); // second line unterminated

    let read = stream_new_jsonl(&path, StoredCursor::default(), None).unwrap();
    assert_eq!(read.lines.len(), 1, "partial final line must be deferred");

    // A tiny nominal cap still finishes exactly one bounded complete record.
    let capped = stream_new_jsonl(&path, StoredCursor::default(), Some(1)).unwrap();
    assert_eq!(capped.lines.len(), 1);
    assert_eq!(capped.new_cursor.position, b"{\"a\":1}\n".len() as u64);
}

#[test]
fn stream_new_jsonl_cap_returns_and_resumes_complete_prefix() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.jsonl");
    let first = "{\"id\":1}\n";
    let second = "{\"id\":2}\n";
    std::fs::write(&path, format!("{first}{second}")).unwrap();

    let prefix =
        stream_new_jsonl(&path, StoredCursor::default(), Some(first.len() as u64)).unwrap();
    assert_eq!(prefix.lines.len(), 1);
    assert_eq!(prefix.lines[0].value["id"], 1);
    assert_eq!(prefix.new_cursor.position, first.len() as u64);

    let suffix = stream_new_jsonl(&path, prefix.new_cursor, Some(first.len() as u64)).unwrap();
    assert_eq!(suffix.lines.len(), 1);
    assert_eq!(suffix.lines[0].value["id"], 2);
    assert_eq!(
        suffix.new_cursor.position,
        (first.len() + second.len()) as u64
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
fn stream_new_jsonl_strict_defers_oversized_complete_and_partial_records() {
    const MAX_RECORD_BYTES: usize = 32;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.jsonl");
    let prefix = "{\"id\":\"prefix\"}\n";
    let oversized = format!("{{\"payload\":\"{}\"}}", "x".repeat(MAX_RECORD_BYTES));

    for terminator in ["\n", ""] {
        std::fs::write(&path, format!("{prefix}{oversized}{terminator}")).unwrap();

        let outcome =
            stream_new_jsonl_strict(&path, StoredCursor::default(), None, MAX_RECORD_BYTES)
                .unwrap();
        let StrictJsonlOutcome::Complete(parsed) = outcome else {
            panic!("oversized record must advance without payload");
        };
        assert_eq!(parsed.lines.len(), 1);
        assert_eq!(parsed.lines[0].value["id"], "prefix");
        assert_eq!(
            parsed.new_cursor.position,
            std::fs::metadata(&path).unwrap().len()
        );
    }
}

#[test]
fn stream_new_jsonl_strict_tracks_exact_record_end_offsets() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.jsonl");
    let first = "{\"id\":1}\n";
    let blank = "\n";
    let second = "{\"id\":2}\n";
    let partial = "{\"id\":3}";
    std::fs::write(&path, format!("{first}{blank}{second}{partial}")).unwrap();

    let outcome = stream_new_jsonl_strict(&path, StoredCursor::default(), None, 64).unwrap();
    let StrictJsonlOutcome::Deferred { parsed, reason } = outcome else {
        panic!("partial final record must defer");
    };
    assert_eq!(parsed.lines.len(), 2);
    assert_eq!(parsed.lines[0].offset, 0);
    assert_eq!(parsed.lines[1].offset, (first.len() + blank.len()) as i64);
    assert_eq!(
        reason,
        JsonlFrameDeferral::Partial {
            offset: (first.len() + blank.len() + second.len()) as u64
        }
    );
    assert_eq!(
        parsed.new_cursor.position,
        (first.len() + blank.len() + second.len()) as u64
    );
}

#[test]
fn stream_new_jsonl_strict_isolates_suffix_after_oversized_record() {
    const MAX_RECORD_BYTES: usize = 40;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.jsonl");
    let prefix = "{\"id\":\"prefix\"}\n";
    let suffix = "{\"id\":\"suffix\"}\n";
    let oversized = format!("{{\"payload\":\"{}\"}}\n", "x".repeat(MAX_RECORD_BYTES));
    std::fs::write(&path, format!("{prefix}{oversized}{suffix}")).unwrap();

    let first =
        stream_new_jsonl_strict(&path, StoredCursor::default(), None, MAX_RECORD_BYTES).unwrap();
    let StrictJsonlOutcome::Complete(parsed) = first else {
        panic!("terminated oversized record must isolate its suffix");
    };
    assert_eq!(parsed.lines.len(), 1);
    assert_eq!(parsed.lines[0].value["id"], "prefix");
    let suffix_offset = (prefix.len() + oversized.len()) as u64;
    assert_eq!(parsed.new_cursor.position, suffix_offset);

    let second = stream_new_jsonl_strict(&path, parsed.new_cursor, None, MAX_RECORD_BYTES).unwrap();
    let StrictJsonlOutcome::Complete(parsed) = second else {
        panic!("suffix must resume after the skipped oversized range");
    };
    assert_eq!(parsed.lines.len(), 1);
    assert_eq!(parsed.lines[0].value["id"], "suffix");
    assert_eq!(
        parsed.new_cursor.position,
        std::fs::metadata(&path).unwrap().len()
    );
}

#[test]
fn stream_new_jsonl_legacy_skips_oversized_complete_frame_and_reads_suffix() {
    const MAX_RECORD_BYTES: usize = 32;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.jsonl");
    let prefix = "{\"id\":\"prefix\"}\n";
    let suffix = "{\"id\":\"suffix\"}\n";
    let oversized = format!("{{\"payload\":\"{}\"}}\n", "x".repeat(MAX_RECORD_BYTES));
    std::fs::write(&path, format!("{prefix}{oversized}{suffix}")).unwrap();

    let (parsed, deferred, _) = stream_new_jsonl_with_policy(
        &path,
        StoredCursor::default(),
        None,
        MalformedJsonlPolicy::Skip,
        MAX_RECORD_BYTES,
    )
    .unwrap();
    assert_eq!(deferred, None);
    assert_eq!(parsed.lines.len(), 2);
    assert_eq!(parsed.lines[0].value["id"], "prefix");
    assert_eq!(parsed.lines[1].value["id"], "suffix");
    assert_eq!(
        parsed.new_cursor.position,
        std::fs::metadata(&path).unwrap().len()
    );
}

#[test]
fn shared_jsonl_framer_applies_invalid_encoding_policy_once() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.jsonl");
    let prefix = b"{\"id\":\"prefix\"}\n";
    let suffix = b"{\"id\":\"suffix\"}\n";
    let mut contents = prefix.to_vec();
    contents.extend_from_slice(b"{\"payload\":\"");
    contents.push(0xff);
    contents.extend_from_slice(b"\"}\n");
    contents.extend_from_slice(suffix);
    std::fs::write(&path, contents).unwrap();

    let legacy = stream_new_jsonl(&path, StoredCursor::default(), None).unwrap();
    assert_eq!(legacy.lines.len(), 2);
    assert_eq!(legacy.lines[1].value["id"], "suffix");

    let strict = stream_new_jsonl_strict(&path, StoredCursor::default(), None, 64).unwrap();
    let StrictJsonlOutcome::Deferred { parsed, reason } = strict else {
        panic!("strict policy must defer invalid encoding");
    };
    assert_eq!(
        reason,
        JsonlFrameDeferral::Malformed {
            offset: prefix.len() as u64
        }
    );
    assert_eq!(parsed.lines.len(), 1);
    assert_eq!(parsed.new_cursor.position, prefix.len() as u64);
}

#[test]
fn stream_new_jsonl_resets_offset_when_file_identity_changes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.jsonl");
    // Keep byte length stable across rewrite to simulate same-size rotation.
    std::fs::write(&path, "{\"a\":1}\n{\"a\":2}\n").unwrap();

    let first = stream_new_jsonl(&path, StoredCursor::default(), None).unwrap();
    assert_eq!(first.lines.len(), 2);

    std::fs::write(&path, "{\"a\":9}\n{\"a\":8}\n").unwrap();
    // Simulate a non-regressing mtime guard; identity must still force a reset.
    let stale = StoredCursor {
        mtime: 0,
        ..first.new_cursor
    };
    let rewritten = stream_new_jsonl(&path, stale, None).unwrap();
    assert_eq!(rewritten.lines.len(), 2);
    assert_eq!(rewritten.lines[0].value["a"], 9);
    assert_eq!(rewritten.lines[1].value["a"], 8);
}

#[test]
fn raw_strict_resume_checkpoint_detects_same_inode_rewrite_past_the_head() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("same-inode.jsonl");
    let original = b"{\"v\":0}\n".repeat(3_000);
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
    let changed = rewritten.len() - b"{\"v\":0}\n".len();
    rewritten[changed..].copy_from_slice(b"{\"v\":1}\n");
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
    assert_eq!(second.frames.len(), 3_000);
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
fn raw_strict_resume_checkpoint_preserves_append_only_progress() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("append-only.jsonl");
    std::fs::write(&path, b"{\"v\":0}\n").unwrap();
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
    let first_end = first.new_cursor.position;
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
    assert_eq!(second.start_offset, first_end);
    assert_eq!(second.new_cursor.file_id, checkpoint.generation);
    assert_eq!(second.frames.len(), 1);
}

#[cfg(any(unix, windows))]
#[test]
fn stream_new_jsonl_resets_when_replaced_file_keeps_same_head() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.jsonl");
    let replacement = dir.path().join("replacement.jsonl");
    std::fs::write(&path, "{\"same\":1}\n{\"old\":2}\n").unwrap();

    let first = stream_new_jsonl(&path, StoredCursor::default(), None).unwrap();
    assert_eq!(first.lines.len(), 2);

    // Create the replacement before removing the original so its native
    // identity cannot be recycled, while retaining the same head line.
    std::fs::write(&replacement, "{\"same\":1}\n{\"new\":2}\n").unwrap();
    std::fs::remove_file(&path).unwrap();
    std::fs::rename(&replacement, &path).unwrap();

    let stale = StoredCursor {
        mtime: 0,
        ..first.new_cursor
    };
    let rewritten = stream_new_jsonl(&path, stale, None).unwrap();
    assert_eq!(rewritten.lines.len(), 2);
    assert_eq!(rewritten.lines[0].value["same"], 1);
    assert_eq!(rewritten.lines[1].value["new"], 2);
}

#[test]
fn read_changed_file_detects_change_and_noops_when_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("chat.json");
    std::fs::write(&path, "[{\"role\":\"user\"}]").unwrap();

    let changed = read_changed_file(&path, StoredCursor::default(), 1024).unwrap();
    assert!(changed.contents.contains("user"));
    // Unchanged file → None.
    assert!(read_changed_file(&path, changed.new_cursor, 1024).is_none());
}

#[test]
fn collect_files_preserves_the_callers_root_spelling() {
    let dir = tempfile::tempdir().unwrap();
    let nested = dir.path().join("nested");
    let transcript = nested.join("session.jsonl");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(&transcript, "{}\n").unwrap();

    assert_eq!(collect_files_with_ext(dir.path(), "jsonl", 1), [transcript]);
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

#[test]
fn stream_new_jsonl_returns_none_for_missing_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("missing.jsonl");

    assert!(stream_new_jsonl(&path, StoredCursor::default(), None).is_none());
}

#[test]
fn stream_new_jsonl_skips_invalid_json_lines_without_panicking() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("invalid.jsonl");
    std::fs::write(&path, "not-json\n{\"a\":2}\n").unwrap();

    let read = stream_new_jsonl(&path, StoredCursor::default(), None).unwrap();
    assert_eq!(read.lines.len(), 1);
    assert_eq!(read.lines[0].value["a"], 2);
}

#[test]
fn read_changed_file_returns_none_for_missing_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("missing.json");

    assert!(read_changed_file(&path, StoredCursor::default(), 1024).is_none());
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
    let conn = crate::runtime::shared::SqliteReadConn::new(
        rusqlite::Connection::open_in_memory().unwrap(),
    );

    let rows = read_new_rows(
        &conn,
        "SELECT not_a_column FROM missing_table WHERE rowid > ? ORDER BY rowid",
        StoredCursor::default(),
        |_rowid: i64, row: &rusqlite::Row<'_>| row.get::<_, String>(0).ok(),
    )
    .await;

    assert!(rows.is_none());
}

#[test]
fn parsed_transcript_structural_ids_are_protected_before_store_writes() {
    let raw = ["AKIA", "PARSEDID", "00000000"].concat();
    let mut parsed = ParsedTranscript {
        draft: SessionDraft {
            session_id: raw.clone(),
            project_key: "project.fixture".to_owned(),
            project_path: "/fixture".to_owned(),
            title: None,
            metadata_json: None,
            parent_session_id: Some(raw.clone()),
            is_subagent: true,
            agent_id: Some(raw.clone()),
            parent_tool_use_id: Some(raw.clone()),
        },
        messages: vec![SessionMessageRecord {
            provider: "test".to_owned(),
            message_id: raw.clone(),
            session_id: raw.clone(),
            role: "assistant".to_owned(),
            timestamp: None,
            ordinal: 0,
            text: "safe".to_owned(),
            kind: None,
            model: None,
            tool_names: None,
            source_path: None,
            source_offset: None,
            metadata_json: None,
        }],
        new_cursor: StoredCursor::default(),
    };

    protect_parsed_transcript_structural_ids(&mut parsed).unwrap();
    let protected = parsed.draft.session_id.clone();
    assert!(protected.starts_with("privacy.structural-id.v1."));
    assert_eq!(
        parsed.draft.parent_session_id.as_deref(),
        Some(protected.as_str())
    );
    assert_eq!(parsed.draft.agent_id.as_deref(), Some(protected.as_str()));
    assert_eq!(
        parsed.draft.parent_tool_use_id.as_deref(),
        Some(protected.as_str())
    );
    assert_eq!(parsed.messages[0].message_id, protected);
    assert_eq!(parsed.messages[0].session_id, protected);
}
