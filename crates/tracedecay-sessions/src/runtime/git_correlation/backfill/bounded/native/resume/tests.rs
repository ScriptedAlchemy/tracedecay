use std::path::Path;
use std::process::Command;
use std::time::Duration;

use super::*;
use crate::observation::ObservationCancellation;

fn control() -> BoundedGitControl {
    BoundedGitControl::new(ObservationCancellation::default(), Duration::from_secs(10))
}

fn git(path: &Path, args: &[&str]) {
    let output = Command::new(tracedecay_runtime_core::git::git_program())
        .current_dir(path)
        .args(args)
        .env("GIT_AUTHOR_NAME", "TraceDecay")
        .env("GIT_AUTHOR_EMAIL", "test@tracedecay.invalid")
        .env("GIT_COMMITTER_NAME", "TraceDecay")
        .env("GIT_COMMITTER_EMAIL", "test@tracedecay.invalid")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_with_dates(path: &Path, args: &[&str], timestamp: i64) {
    let date = format!("@{timestamp} +0000");
    let output = Command::new(tracedecay_runtime_core::git::git_program())
        .current_dir(path)
        .args(args)
        .env("GIT_AUTHOR_NAME", "TraceDecay")
        .env("GIT_AUTHOR_EMAIL", "test@tracedecay.invalid")
        .env("GIT_COMMITTER_NAME", "TraceDecay")
        .env("GIT_COMMITTER_EMAIL", "test@tracedecay.invalid")
        .env("GIT_AUTHOR_DATE", &date)
        .env("GIT_COMMITTER_DATE", &date)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn fixture() -> tempfile::TempDir {
    let fixture = tempfile::tempdir().unwrap();
    git(fixture.path(), &["init", "-b", "main"]);
    std::fs::write(fixture.path().join("tracked"), "one").unwrap();
    git(fixture.path(), &["add", "tracked"]);
    git(fixture.path(), &["commit", "-m", "initial"]);
    fixture
}

fn collect_segments(path: &Path) -> Vec<ReflogSegment> {
    let control = control();
    let mut cursor = initialize_reflog_cursor(path, i64::MAX, &control).unwrap();
    let mut segments = Vec::new();
    loop {
        let chunk = scan_reflog_chunk(path, 0, i64::MAX, cursor, &control).unwrap();
        segments.extend(chunk.segments);
        cursor = chunk.cursor;
        if chunk.complete {
            return segments;
        }
    }
}

#[test]
fn absent_head_reflog_is_a_sealed_single_segment() {
    let fixture = fixture();
    let repository = gix::discover(fixture.path()).unwrap();
    let head = repository.head().unwrap().log_iter();
    let relative = head.store.namespace.as_ref().map_or_else(
        || head.name.to_path().to_owned(),
        |namespace| namespace.to_path().join(head.name.to_path()),
    );
    let path = head.store.git_dir().join("logs").join(relative);
    std::fs::remove_file(path).unwrap();

    let segments = collect_segments(fixture.path());

    assert_eq!(segments.len(), 1);
    assert_eq!(segments[0].branch.as_deref(), Some("main"));
}

#[test]
fn reverse_reader_accepts_large_record_but_rejects_unbounded_record() {
    let file = tempfile::NamedTempFile::new().unwrap();
    for payload_len in [MAX_REFLOG_RECORD_BYTES - 1, MAX_REFLOG_RECORD_BYTES] {
        let mut within = b"first\n".to_vec();
        within.extend(std::iter::repeat_n(b'x', payload_len));
        within.push(b'\n');
        std::fs::write(file.path(), &within).unwrap();
        let (bytes, start) = read_reverse_block(file.path(), within.len() as u64).unwrap();
        assert_eq!(complete_line_ranges(&bytes, start).unwrap().len(), 1);
    }

    let mut oversized = b"first\n".to_vec();
    oversized.extend(std::iter::repeat_n(b'x', MAX_REFLOG_RECORD_BYTES + 1));
    oversized.push(b'\n');
    std::fs::write(file.path(), &oversized).unwrap();
    assert_eq!(
        read_reverse_block(file.path(), oversized.len() as u64).unwrap_err(),
        BoundedBackfillInterruption::UnsupportedSourceFraming
    );
}

#[test]
fn blank_reflog_record_is_unsupported_framing() {
    assert_eq!(
        complete_line_ranges(b"valid\n\n", 0).unwrap_err(),
        BoundedBackfillInterruption::UnsupportedSourceFraming
    );
}

#[test]
fn truncated_reflog_is_permanent_unsupported_framing() {
    let fixture = fixture();
    let repository = gix::discover(fixture.path()).unwrap();
    let head = repository.head().unwrap().log_iter();
    let relative = head.store.namespace.as_ref().map_or_else(
        || head.name.to_path().to_owned(),
        |namespace| namespace.to_path().join(head.name.to_path()),
    );
    let path = head.store.git_dir().join("logs").join(relative);
    let mut bytes = std::fs::read(&path).unwrap();
    assert_eq!(bytes.pop(), Some(b'\n'));
    std::fs::write(path, bytes).unwrap();

    assert_eq!(
        initialize_reflog_cursor(fixture.path(), i64::MAX, &control()).unwrap_err(),
        BoundedBackfillInterruption::UnsupportedSourceFraming
    );
}

#[test]
fn window_before_repository_creation_emits_no_zero_oid_segment() {
    let fixture = tempfile::tempdir().unwrap();
    git(fixture.path(), &["init", "-b", "main"]);
    std::fs::write(fixture.path().join("tracked"), "content").unwrap();
    git(fixture.path(), &["add", "tracked"]);
    git(fixture.path(), &["commit", "-m", "initial"]);
    let cursor = initialize_reflog_cursor(fixture.path(), 1, &control()).unwrap();

    let chunk = scan_reflog_chunk(fixture.path(), 0, 1, cursor, &control()).unwrap();

    assert!(chunk.complete);
    assert!(chunk.segments.is_empty());
}

#[test]
fn sealed_graph_ignores_later_head_drift_and_keeps_nonmonotonic_parent_time() {
    let fixture = tempfile::tempdir().unwrap();
    git(fixture.path(), &["init", "-b", "main"]);
    std::fs::write(fixture.path().join("tracked"), "parent").unwrap();
    git(fixture.path(), &["add", "tracked"]);
    git_with_dates(fixture.path(), &["commit", "-m", "parent"], 200);
    std::fs::write(fixture.path().join("tracked"), "child").unwrap();
    git(fixture.path(), &["add", "tracked"]);
    git_with_dates(fixture.path(), &["commit", "-m", "child"], 100);
    let repository = gix::discover(fixture.path()).unwrap();
    let tip = repository.head_id().unwrap().detach().to_hex().to_string();
    let source = initialize_reflog_cursor(fixture.path(), 250, &control()).unwrap();
    std::fs::write(fixture.path().join("tracked"), "later head").unwrap();
    git(fixture.path(), &["commit", "-am", "later head"]);

    let first = scan_graph_chunk(
        fixture.path(),
        150,
        250,
        &source.repository_seal(),
        vec![GraphPending { oid: tip }],
        10,
        128,
        256 * 1024,
        &control(),
    )
    .unwrap();
    assert!(first.commits.is_empty());
    assert_eq!(first.pending.len(), 1);
    let second = scan_graph_chunk(
        fixture.path(),
        150,
        250,
        &source.repository_seal(),
        first.pending,
        10,
        128,
        256 * 1024,
        &control(),
    )
    .unwrap();
    assert_eq!(second.commits.len(), 1);
    assert_eq!(second.commits[0].committed_at, 200);
}

#[test]
fn existing_tag_transition_is_detached() {
    let fixture = fixture();
    git(fixture.path(), &["tag", "release"]);
    git(fixture.path(), &["checkout", "release"]);
    git(fixture.path(), &["checkout", "main"]);
    assert!(
        collect_segments(fixture.path())
            .iter()
            .any(|segment| segment.branch.is_none())
    );
}

#[test]
fn deleted_tag_transition_remains_detached() {
    let fixture = fixture();
    git(fixture.path(), &["tag", "release"]);
    git(fixture.path(), &["checkout", "release"]);
    git(fixture.path(), &["tag", "-d", "release"]);
    git(fixture.path(), &["checkout", "main"]);
    assert!(
        collect_segments(fixture.path())
            .iter()
            .any(|segment| segment.branch.is_none())
    );
}

#[test]
fn deleted_local_label_is_conservatively_detached() {
    let fixture = fixture();
    git(fixture.path(), &["branch", "historical"]);
    git(fixture.path(), &["checkout", "historical"]);
    git(fixture.path(), &["checkout", "main"]);
    git(fixture.path(), &["branch", "-D", "historical"]);
    assert!(
        !collect_segments(fixture.path())
            .iter()
            .any(|segment| segment.branch.as_deref() == Some("historical"))
    );
}

#[test]
fn detached_remote_revision_and_short_oid_are_unattributed() {
    for target in ["refs/remotes/origin/topic", "HEAD~0", "SHORT_OID"] {
        let fixture = fixture();
        if target == "refs/remotes/origin/topic" {
            git(
                fixture.path(),
                &["update-ref", "refs/remotes/origin/topic", "HEAD"],
            );
        }
        let repository = gix::discover(fixture.path()).unwrap();
        let short = repository.head_id().unwrap().detach().to_hex().to_string()[..8].to_owned();
        let target = if target == "SHORT_OID" {
            short.as_str()
        } else {
            target
        };
        git(fixture.path(), &["checkout", target]);
        git(fixture.path(), &["checkout", "main"]);
        assert!(
            collect_segments(fixture.path())
                .iter()
                .any(|segment| segment.branch.is_none()),
            "{target}"
        );
    }
}

#[cfg(unix)]
#[test]
fn non_utf8_local_ref_is_sealed_without_fabricated_branch_text() {
    use std::os::unix::ffi::OsStrExt as _;

    let fixture = fixture();
    let branch = std::ffi::OsStr::from_bytes(b"topic-\xff");
    let output = Command::new(tracedecay_runtime_core::git::git_program())
        .current_dir(fixture.path())
        .arg("checkout")
        .arg("-b")
        .arg(branch)
        .output()
        .unwrap();
    assert!(output.status.success());
    git(fixture.path(), &["checkout", "main"]);
    assert!(
        collect_segments(fixture.path())
            .iter()
            .any(|segment| segment.branch.is_none())
    );
}

#[test]
fn linked_worktree_uses_its_private_head_reflog() {
    let fixture = fixture();
    let linked = fixture.path().join("linked");
    git(
        fixture.path(),
        &["worktree", "add", "-b", "linked", linked.to_str().unwrap()],
    );
    let cursor = initialize_reflog_cursor(&linked, i64::MAX, &control()).unwrap();
    assert!(cursor.reflog_path.to_string_lossy().contains("worktrees"));
    assert!(
        collect_segments(&linked)
            .iter()
            .any(|segment| segment.branch.as_deref() == Some("linked"))
    );
}

#[test]
fn reflog_oid_discontinuity_is_unsupported_framing() {
    let fixture = fixture();
    let cursor = initialize_reflog_cursor(fixture.path(), i64::MAX, &control()).unwrap();
    let mut bytes = std::fs::read(&cursor.reflog_path).unwrap();
    let latest_start = bytes[..bytes.len() - 1]
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |position| position + 1);
    let oid_width = cursor.source_head_oid.len();
    let new_oid_start = latest_start + oid_width + 1;
    bytes[new_oid_start..new_oid_start + oid_width].fill(b'0');
    std::fs::write(&cursor.reflog_path, bytes).unwrap();
    let changed = initialize_reflog_cursor(fixture.path(), i64::MAX, &control()).unwrap();

    assert_eq!(
        scan_reflog_chunk(fixture.path(), 0, i64::MAX, changed, &control()).unwrap_err(),
        BoundedBackfillInterruption::UnsupportedSourceFraming
    );
}
