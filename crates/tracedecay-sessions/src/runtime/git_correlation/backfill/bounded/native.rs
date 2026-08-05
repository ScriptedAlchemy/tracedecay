use std::collections::BTreeMap;
use std::io::{BufWriter, Seek, Write};
use std::path::{Path, PathBuf};

use gix::bstr::ByteSlice;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{BoundedBackfillInterruption, BoundedGitControl};

#[derive(Debug, Deserialize, Serialize)]
pub(super) enum PreparedGitEvent {
    Begin {
        worktree: PathBuf,
    },
    Segment {
        branch: Option<String>,
        start: i64,
        end: i64,
    },
    Commit {
        branch: String,
        sha: String,
        committed_at: i64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct HeadSeal {
    referent: Option<Vec<u8>>,
    target: Option<gix::ObjectId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ReflogPrefixSeal {
    entry_count: usize,
    digest: [u8; 32],
    exhausted: bool,
}

struct ReflogSealBuilder {
    entry_count: usize,
    hasher: Sha256,
}

impl ReflogSealBuilder {
    fn new() -> Self {
        let mut hasher = Sha256::new();
        hasher.update(b"tracedecay-head-reflog-prefix-v1\0");
        Self {
            entry_count: 0,
            hasher,
        }
    }

    fn push(&mut self, entry: &gix::refs::log::Line) -> Result<(), BoundedBackfillInterruption> {
        hash_frame(&mut self.hasher, entry.previous_oid.as_slice());
        hash_frame(&mut self.hasher, entry.new_oid.as_slice());
        hash_frame(
            &mut self.hasher,
            &entry.signature.time.seconds.to_le_bytes(),
        );
        hash_frame(&mut self.hasher, entry.message.as_ref());
        self.entry_count = self
            .entry_count
            .checked_add(1)
            .ok_or(BoundedBackfillInterruption::SourceUnavailable)?;
        Ok(())
    }

    fn finish(self, exhausted: bool) -> ReflogPrefixSeal {
        ReflogPrefixSeal {
            entry_count: self.entry_count,
            digest: self.hasher.finalize().into(),
            exhausted,
        }
    }
}

#[derive(Debug)]
struct RepositorySeal {
    head: HeadSeal,
    reflog_prefix: ReflogPrefixSeal,
    consulted_refs: BTreeMap<String, Option<gix::ObjectId>>,
}

impl RepositorySeal {
    fn verify(
        &self,
        repository: &gix::Repository,
        control: &BoundedGitControl,
    ) -> Result<(), BoundedBackfillInterruption> {
        control.check()?;
        if capture_head(repository)? != self.head
            || capture_reflog_prefix(repository, &self.reflog_prefix, control)?
                != self.reflog_prefix
        {
            return Err(BoundedBackfillInterruption::SourceUnavailable);
        }
        for (reference, expected) in &self.consulted_refs {
            control.check()?;
            if exact_ref_tip(repository, reference)? != *expected {
                return Err(BoundedBackfillInterruption::SourceUnavailable);
            }
        }
        control.check()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum HeadState {
    LocalBranch(String),
    Detached,
}

impl HeadState {
    fn branch(&self) -> Option<&str> {
        match self {
            Self::LocalBranch(branch) => Some(branch),
            Self::Detached => None,
        }
    }
}

#[derive(Debug)]
struct CheckoutTarget(String);

#[derive(Debug)]
struct Checkout {
    from: CheckoutTarget,
    to: CheckoutTarget,
}

pub(super) fn produce(
    project_path: &Path,
    window_start: i64,
    window_end: i64,
    max_commits: usize,
    control: &BoundedGitControl,
) -> Result<std::fs::File, BoundedBackfillInterruption> {
    control.check()?;
    let spool = tempfile::tempfile().map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
    let mut writer = BufWriter::new(spool);
    let mut repository =
        gix::discover(project_path).map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
    repository.object_cache_size_if_unset(4 * 1024 * 1024);
    let worktree = repository
        .workdir()
        .ok_or(BoundedBackfillInterruption::SourceUnavailable)?
        .canonicalize()
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
    write_event(&mut writer, PreparedGitEvent::Begin { worktree }, control)?;

    let head_before = capture_head(&repository)?;
    let current_state = head_state(&head_before)?;
    let current_oid = head_before
        .target
        .ok_or(BoundedBackfillInterruption::SourceUnavailable)?;
    let head = repository
        .head()
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
    let mut platform = head.log_iter();
    let mut entries = platform
        .rev()
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
    let mut reflog_seal = ReflogSealBuilder::new();
    let mut stopped_at_boundary = false;
    let mut state = current_state;
    let mut state_oid = current_oid;
    let mut segment_end = window_end;
    let mut segment_tip = current_oid;
    let mut emitted_commits = 0_usize;
    let mut consulted_refs = BTreeMap::new();

    if let Some(entries) = entries.as_mut() {
        for entry in entries {
            control.check()?;
            let entry = entry.map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
            if entry.new_oid != state_oid {
                return Err(BoundedBackfillInterruption::SourceUnavailable);
            }
            reflog_seal.push(&entry)?;
            let checkout = parse_checkout(&entry)?;
            let timestamp = entry.signature.time.seconds;
            if timestamp > window_end {
                cross_entry_backwards(
                    &repository,
                    &entry,
                    checkout.as_ref(),
                    &mut consulted_refs,
                    &mut state,
                    &mut state_oid,
                )?;
                segment_tip = state_oid;
                continue;
            }
            if timestamp <= window_start {
                stopped_at_boundary = true;
                break;
            }
            match checkout {
                Some(checkout) => {
                    let to =
                        classify_checkout_target(&repository, &checkout.to, &mut consulted_refs)?;
                    if to != state {
                        return Err(BoundedBackfillInterruption::SourceUnavailable);
                    }
                    emit_segment(
                        &repository,
                        &mut writer,
                        &mut consulted_refs,
                        state.branch(),
                        timestamp,
                        segment_end,
                        segment_tip,
                        max_commits,
                        &mut emitted_commits,
                        control,
                    )?;
                    state =
                        classify_checkout_target(&repository, &checkout.from, &mut consulted_refs)?;
                    state_oid = entry.previous_oid;
                    segment_end = timestamp;
                    segment_tip = state_oid;
                }
                None => {
                    state_oid = entry.previous_oid;
                }
            }
        }
    }
    emit_segment(
        &repository,
        &mut writer,
        &mut consulted_refs,
        state.branch(),
        window_start,
        segment_end,
        segment_tip,
        max_commits,
        &mut emitted_commits,
        control,
    )?;

    let head_after = capture_head(&repository)?;
    if head_after != head_before {
        return Err(BoundedBackfillInterruption::SourceUnavailable);
    }
    RepositorySeal {
        head: head_after,
        reflog_prefix: reflog_seal.finish(!stopped_at_boundary),
        consulted_refs,
    }
    .verify(&repository, control)?;
    control.check()?;
    writer
        .flush()
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
    let mut spool = writer
        .into_inner()
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
    spool
        .seek(std::io::SeekFrom::Start(0))
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
    control.check()?;
    Ok(spool)
}

fn emit_segment(
    repository: &gix::Repository,
    writer: &mut impl Write,
    consulted_refs: &mut BTreeMap<String, Option<gix::ObjectId>>,
    branch: Option<&str>,
    start: i64,
    end: i64,
    tip: gix::ObjectId,
    max_commits: usize,
    emitted_commits: &mut usize,
    control: &BoundedGitControl,
) -> Result<(), BoundedBackfillInterruption> {
    if start > end {
        return Ok(());
    }
    write_event(
        writer,
        PreparedGitEvent::Segment {
            branch: branch.map(str::to_owned),
            start,
            end,
        },
        control,
    )?;
    let Some(branch) = branch else {
        return Ok(());
    };
    let local_ref = format!("refs/heads/{branch}");
    consult_exact_ref(repository, consulted_refs, &local_ref)?;
    scan_segment(
        repository,
        writer,
        branch,
        tip,
        start,
        end,
        max_commits,
        emitted_commits,
        control,
    )
}

fn scan_segment(
    repository: &gix::Repository,
    writer: &mut impl Write,
    branch: &str,
    tip: gix::ObjectId,
    start: i64,
    end: i64,
    max_commits: usize,
    emitted_commits: &mut usize,
    control: &BoundedGitControl,
) -> Result<(), BoundedBackfillInterruption> {
    use gix::traverse::commit::simple::CommitTimeOrder;

    repository
        .find_commit(tip)
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
    let walk = repository
        .rev_walk([tip])
        .sorting(gix::revision::walk::Sorting::ByCommitTimeCutoff {
            order: CommitTimeOrder::NewestFirst,
            seconds: start,
        })
        .all()
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
    for info in walk {
        control.check()?;
        let info = info.map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
        let committed_at = info
            .commit_time
            .ok_or(BoundedBackfillInterruption::SourceUnavailable)?;
        if committed_at < start || committed_at > end {
            continue;
        }
        if *emitted_commits >= max_commits {
            return Err(BoundedBackfillInterruption::HistoryLimitReached);
        }
        write_event(
            writer,
            PreparedGitEvent::Commit {
                branch: branch.to_owned(),
                sha: info.id.to_hex().to_string(),
                committed_at,
            },
            control,
        )?;
        *emitted_commits = (*emitted_commits).saturating_add(1);
    }
    Ok(())
}

fn write_event(
    writer: &mut impl Write,
    event: PreparedGitEvent,
    control: &BoundedGitControl,
) -> Result<(), BoundedBackfillInterruption> {
    control.check()?;
    serde_json::to_writer(&mut *writer, &event)
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
    writer
        .write_all(b"\n")
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
    control.check()
}

fn cross_entry_backwards(
    repository: &gix::Repository,
    entry: &gix::refs::log::Line,
    checkout: Option<&Checkout>,
    consulted_refs: &mut BTreeMap<String, Option<gix::ObjectId>>,
    state: &mut HeadState,
    state_oid: &mut gix::ObjectId,
) -> Result<(), BoundedBackfillInterruption> {
    if let Some(checkout) = checkout {
        let to = classify_checkout_target(repository, &checkout.to, consulted_refs)?;
        if to != *state {
            return Err(BoundedBackfillInterruption::SourceUnavailable);
        }
        *state = classify_checkout_target(repository, &checkout.from, consulted_refs)?;
    }
    *state_oid = entry.previous_oid;
    Ok(())
}

fn capture_head(repository: &gix::Repository) -> Result<HeadSeal, BoundedBackfillInterruption> {
    let head = repository
        .head()
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
    Ok(HeadSeal {
        referent: head.referent_name().map(|name| name.as_bstr().to_vec()),
        target: head.id().map(gix::Id::detach),
    })
}

fn head_state(seal: &HeadSeal) -> Result<HeadState, BoundedBackfillInterruption> {
    match seal.referent.as_deref() {
        Some(name) => {
            let short = name
                .strip_prefix(b"refs/heads/")
                .ok_or(BoundedBackfillInterruption::SourceUnavailable)?;
            std::str::from_utf8(short)
                .map(str::to_owned)
                .map(HeadState::LocalBranch)
                .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)
        }
        None => Ok(HeadState::Detached),
    }
}

fn capture_reflog_prefix(
    repository: &gix::Repository,
    expected: &ReflogPrefixSeal,
    control: &BoundedGitControl,
) -> Result<ReflogPrefixSeal, BoundedBackfillInterruption> {
    control.check()?;
    let head = repository
        .head()
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
    let mut platform = head.log_iter();
    let mut entries = platform
        .rev()
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
    let mut seal = ReflogSealBuilder::new();
    for _ in 0..expected.entry_count {
        control.check()?;
        let entry = entries
            .as_mut()
            .and_then(Iterator::next)
            .transpose()
            .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?
            .ok_or(BoundedBackfillInterruption::SourceUnavailable)?;
        seal.push(&entry)?;
    }
    if expected.exhausted {
        control.check()?;
        if entries
            .as_mut()
            .and_then(Iterator::next)
            .transpose()
            .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?
            .is_some()
        {
            return Err(BoundedBackfillInterruption::SourceUnavailable);
        }
    }
    control.check()?;
    Ok(seal.finish(expected.exhausted))
}

fn hash_frame(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u128).to_le_bytes());
    hasher.update(bytes);
}

fn parse_checkout(
    entry: &gix::refs::log::Line,
) -> Result<Option<Checkout>, BoundedBackfillInterruption> {
    let Some(moving) = entry.message.strip_prefix(b"checkout: moving from ") else {
        return Ok(None);
    };
    let split = moving
        .rfind(b" to ")
        .ok_or(BoundedBackfillInterruption::SourceUnavailable)?;
    let from = moving
        .get(..split)
        .ok_or(BoundedBackfillInterruption::SourceUnavailable)?;
    let to = moving
        .get(split + b" to ".len()..)
        .ok_or(BoundedBackfillInterruption::SourceUnavailable)?;
    if from.is_empty() || to.is_empty() {
        return Err(BoundedBackfillInterruption::SourceUnavailable);
    }
    Ok(Some(Checkout {
        from: parse_checkout_target(from)?,
        to: parse_checkout_target(to)?,
    }))
}

fn parse_checkout_target(target: &[u8]) -> Result<CheckoutTarget, BoundedBackfillInterruption> {
    std::str::from_utf8(target)
        .map(|target| CheckoutTarget(target.to_owned()))
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)
}

fn classify_checkout_target(
    repository: &gix::Repository,
    target: &CheckoutTarget,
    consulted_refs: &mut BTreeMap<String, Option<gix::ObjectId>>,
) -> Result<HeadState, BoundedBackfillInterruption> {
    let label = &target.0;
    if label.starts_with("refs/") {
        return Ok(HeadState::Detached);
    }
    let local_ref = format!("refs/heads/{label}");
    if gix::refs::FullName::try_from(local_ref.as_str()).is_err() {
        return Ok(HeadState::Detached);
    }
    if consult_exact_ref(repository, consulted_refs, &local_ref)?.is_some() {
        return Ok(HeadState::LocalBranch(label.clone()));
    }
    for alternate in [
        format!("refs/tags/{label}"),
        format!("refs/remotes/{label}"),
    ] {
        if gix::refs::FullName::try_from(alternate.as_str()).is_ok()
            && consult_exact_ref(repository, consulted_refs, &alternate)?.is_some()
        {
            return Ok(HeadState::Detached);
        }
    }
    if (7..=64).contains(&label.len()) && label.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Ok(HeadState::Detached);
    }
    if gix::refs::FullName::try_from(label.as_str()).is_ok()
        && consult_exact_ref(repository, consulted_refs, label)?.is_some()
    {
        return Ok(HeadState::Detached);
    }
    Ok(HeadState::LocalBranch(label.clone()))
}

fn consult_exact_ref(
    repository: &gix::Repository,
    consulted_refs: &mut BTreeMap<String, Option<gix::ObjectId>>,
    reference: &str,
) -> Result<Option<gix::ObjectId>, BoundedBackfillInterruption> {
    let tip = exact_ref_tip(repository, reference)?;
    if let Some(previous) = consulted_refs.insert(reference.to_owned(), tip)
        && previous != tip
    {
        return Err(BoundedBackfillInterruption::SourceUnavailable);
    }
    Ok(tip)
}

fn exact_ref_tip(
    repository: &gix::Repository,
    reference: &str,
) -> Result<Option<gix::ObjectId>, BoundedBackfillInterruption> {
    let reference = repository
        .try_find_reference(reference)
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
    reference
        .map(|reference| {
            reference
                .try_id()
                .map(gix::Id::detach)
                .ok_or(BoundedBackfillInterruption::SourceUnavailable)
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    use std::io::BufRead;
    use std::process::Command;
    use std::time::Duration;

    use super::*;
    use crate::observation::ObservationCancellation;

    fn git(path: &Path, args: &[&str]) {
        let output = Command::new(tracedecay_runtime_core::git::git_program())
            .current_dir(path)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn repository_fixture() -> tempfile::TempDir {
        let fixture = tempfile::tempdir().unwrap();
        git(fixture.path(), &["init", "-b", "main"]);
        git(
            fixture.path(),
            &["config", "user.email", "history@example.com"],
        );
        git(fixture.path(), &["config", "user.name", "History Fixture"]);
        std::fs::write(fixture.path().join("fixture.txt"), "one").unwrap();
        git(fixture.path(), &["add", "fixture.txt"]);
        git(fixture.path(), &["commit", "-m", "one"]);
        fixture
    }

    fn read_events(spool: std::fs::File) -> Vec<PreparedGitEvent> {
        std::io::BufReader::new(spool)
            .lines()
            .map(|line| serde_json::from_str(&line.unwrap()).unwrap())
            .collect()
    }

    fn complete_reflog_seal(repository: &gix::Repository) -> ReflogPrefixSeal {
        let head = repository.head().unwrap();
        let mut platform = head.log_iter();
        let mut entries = platform.rev().unwrap();
        let mut seal = ReflogSealBuilder::new();
        if let Some(entries) = entries.as_mut() {
            for entry in entries {
                seal.push(&entry.unwrap()).unwrap();
            }
        }
        seal.finish(true)
    }

    #[test]
    fn exact_branch_lookup_does_not_accept_revision_syntax() {
        let fixture = repository_fixture();
        let repository = gix::discover(fixture.path()).unwrap();

        assert!(
            exact_ref_tip(&repository, "refs/heads/main")
                .unwrap()
                .is_some()
        );
        assert!(gix::refs::FullName::try_from("refs/heads/main~1").is_err());
    }

    #[test]
    fn missing_worktree_is_retryable_source_unavailability() {
        let missing = std::env::temp_dir().join(format!(
            "tracedecay-missing-native-git-history-{}",
            std::process::id()
        ));
        assert!(!missing.exists());
        let control =
            BoundedGitControl::new(ObservationCancellation::default(), Duration::from_secs(1));

        assert_eq!(
            produce(&missing, 0, 1, usize::MAX, &control).unwrap_err(),
            BoundedBackfillInterruption::SourceUnavailable
        );
    }

    #[test]
    fn repository_seal_rejects_ref_and_reflog_drift() {
        let fixture = repository_fixture();
        let repository = gix::discover(fixture.path()).unwrap();
        let control =
            BoundedGitControl::new(ObservationCancellation::default(), Duration::from_secs(10));
        let seal = RepositorySeal {
            head: capture_head(&repository).unwrap(),
            reflog_prefix: complete_reflog_seal(&repository),
            consulted_refs: BTreeMap::from([(
                "refs/heads/main".to_owned(),
                exact_ref_tip(&repository, "refs/heads/main").unwrap(),
            )]),
        };

        std::fs::write(fixture.path().join("fixture.txt"), "two").unwrap();
        git(fixture.path(), &["add", "fixture.txt"]);
        git(fixture.path(), &["commit", "-m", "two"]);

        assert_eq!(
            seal.verify(&repository, &control).unwrap_err(),
            BoundedBackfillInterruption::SourceUnavailable
        );
    }

    #[test]
    fn repository_seal_rejects_consumed_older_reflog_rewrite() {
        let fixture = repository_fixture();
        std::fs::write(fixture.path().join("fixture.txt"), "two").unwrap();
        git(fixture.path(), &["add", "fixture.txt"]);
        git(fixture.path(), &["commit", "-m", "two"]);
        let repository = gix::discover(fixture.path()).unwrap();
        let control =
            BoundedGitControl::new(ObservationCancellation::default(), Duration::from_secs(10));
        let head_before = capture_head(&repository).unwrap();
        let branch_tip_before = exact_ref_tip(&repository, "refs/heads/main").unwrap();
        let seal = RepositorySeal {
            head: head_before.clone(),
            reflog_prefix: complete_reflog_seal(&repository),
            consulted_refs: BTreeMap::from([("refs/heads/main".to_owned(), branch_tip_before)]),
        };

        let reflog_path = fixture.path().join(".git/logs/HEAD");
        let reflog = std::fs::read(&reflog_path).unwrap();
        let mut lines = reflog
            .split_inclusive(|byte| *byte == b'\n')
            .map(<[u8]>::to_vec)
            .collect::<Vec<_>>();
        assert!(lines.len() >= 2);
        let newest_before = lines.last().unwrap().clone();
        let message_start = lines[0].iter().position(|byte| *byte == b'\t').unwrap() + 1;
        lines[0][message_start] ^= 1;
        std::fs::write(&reflog_path, lines.concat()).unwrap();

        let rewritten = std::fs::read(&reflog_path).unwrap();
        assert_eq!(
            rewritten
                .split_inclusive(|byte| *byte == b'\n')
                .next_back()
                .unwrap(),
            newest_before
        );
        assert_eq!(capture_head(&repository).unwrap(), head_before);
        assert_eq!(
            exact_ref_tip(&repository, "refs/heads/main").unwrap(),
            branch_tip_before
        );
        assert_eq!(
            seal.verify(&repository, &control).unwrap_err(),
            BoundedBackfillInterruption::SourceUnavailable
        );
    }

    #[test]
    fn cancellation_stops_production_before_a_spool_is_sealed() {
        let fixture = repository_fixture();
        let cancellation = ObservationCancellation::default();
        cancellation.cancel();
        let control = BoundedGitControl::new(cancellation, Duration::from_secs(10));

        assert_eq!(
            produce(fixture.path(), 0, i64::MAX, 10, &control).unwrap_err(),
            BoundedBackfillInterruption::Cancelled
        );
    }

    #[test]
    fn expired_deadline_stops_production_before_a_spool_is_sealed() {
        let fixture = repository_fixture();
        let control = BoundedGitControl::new(ObservationCancellation::default(), Duration::ZERO);

        assert_eq!(
            produce(fixture.path(), 0, i64::MAX, 10, &control).unwrap_err(),
            BoundedBackfillInterruption::CommandTimedOut
        );
    }

    #[test]
    fn finite_session_cap_never_returns_a_sealed_spool_for_large_history() {
        let fixture = repository_fixture();
        for revision in 2..=12 {
            std::fs::write(fixture.path().join("fixture.txt"), revision.to_string()).unwrap();
            git(fixture.path(), &["add", "fixture.txt"]);
            git(fixture.path(), &["commit", "-m", &revision.to_string()]);
        }
        let control =
            BoundedGitControl::new(ObservationCancellation::default(), Duration::from_secs(10));

        assert_eq!(
            produce(fixture.path(), 0, i64::MAX, 3, &control).unwrap_err(),
            BoundedBackfillInterruption::HistoryLimitReached
        );
    }

    fn assert_checkout_label_is_detached(fixture: &tempfile::TempDir, label: &str) {
        let control =
            BoundedGitControl::new(ObservationCancellation::default(), Duration::from_secs(10));
        let events =
            read_events(produce(fixture.path(), 0, i64::MAX, usize::MAX, &control).unwrap());

        assert!(
            events
                .iter()
                .any(|event| matches!(event, PreparedGitEvent::Segment { branch: None, .. }))
        );
        assert!(!events.iter().any(|event| matches!(
            event,
            PreparedGitEvent::Segment {
                branch: Some(branch),
                ..
            } if branch == label
        )));
        assert!(!events.iter().any(|event| matches!(
            event,
            PreparedGitEvent::Commit { branch, .. } if branch == label
        )));
    }

    #[test]
    fn tag_checkout_is_detached_without_a_fake_local_branch() {
        let fixture = repository_fixture();
        git(fixture.path(), &["tag", "release"]);
        git(fixture.path(), &["checkout", "release"]);

        assert_checkout_label_is_detached(&fixture, "release");
    }

    #[test]
    fn remote_checkout_is_detached_without_a_fake_local_branch() {
        let fixture = repository_fixture();
        git(
            fixture.path(),
            &["update-ref", "refs/remotes/origin/main", "HEAD"],
        );
        git(fixture.path(), &["checkout", "origin/main"]);

        assert_checkout_label_is_detached(&fixture, "origin/main");
    }

    #[test]
    fn revision_checkout_is_detached_instead_of_source_unavailable() {
        let fixture = repository_fixture();
        std::fs::write(fixture.path().join("fixture.txt"), "two").unwrap();
        git(fixture.path(), &["add", "fixture.txt"]);
        git(fixture.path(), &["commit", "-m", "two"]);
        git(fixture.path(), &["checkout", "HEAD~1"]);

        assert_checkout_label_is_detached(&fixture, "HEAD~1");
    }

    #[test]
    fn reflog_oid_discontinuity_is_source_unavailable() {
        let fixture = repository_fixture();
        std::fs::write(fixture.path().join("fixture.txt"), "two").unwrap();
        git(fixture.path(), &["add", "fixture.txt"]);
        git(fixture.path(), &["commit", "-m", "two"]);
        let reflog_path = fixture.path().join(".git/logs/HEAD");
        let reflog = std::fs::read(&reflog_path).unwrap();
        let mut lines = reflog
            .split_inclusive(|byte| *byte == b'\n')
            .map(<[u8]>::to_vec)
            .collect::<Vec<_>>();
        assert!(lines.len() >= 2);
        lines[0][41..81].fill(b'0');
        std::fs::write(reflog_path, lines.concat()).unwrap();
        let control =
            BoundedGitControl::new(ObservationCancellation::default(), Duration::from_secs(10));

        assert_eq!(
            produce(fixture.path(), 0, i64::MAX, usize::MAX, &control).unwrap_err(),
            BoundedBackfillInterruption::SourceUnavailable
        );
    }

    #[test]
    fn linked_worktree_stream_uses_its_private_head() {
        let fixture = repository_fixture();
        let linked_parent = tempfile::tempdir().unwrap();
        let linked = linked_parent.path().join("linked");
        git(
            fixture.path(),
            &["worktree", "add", "-b", "linked", linked.to_str().unwrap()],
        );
        let control =
            BoundedGitControl::new(ObservationCancellation::default(), Duration::from_secs(10));
        let events = read_events(produce(&linked, 0, i64::MAX, usize::MAX, &control).unwrap());

        assert!(events.into_iter().any(|event| matches!(
            event,
            PreparedGitEvent::Segment {
                branch: Some(branch),
                ..
            } if branch == "linked"
        )));
    }

    #[test]
    fn deleted_historical_branch_walks_reflog_tip_oid() {
        let fixture = repository_fixture();
        git(fixture.path(), &["checkout", "-b", "historical"]);
        std::fs::write(fixture.path().join("historical.txt"), "history").unwrap();
        git(fixture.path(), &["add", "historical.txt"]);
        git(fixture.path(), &["commit", "-m", "historical"]);
        git(fixture.path(), &["checkout", "main"]);
        git(fixture.path(), &["branch", "-D", "historical"]);
        let repository = gix::discover(fixture.path()).unwrap();
        assert!(
            exact_ref_tip(&repository, "refs/heads/historical")
                .unwrap()
                .is_none()
        );

        let control =
            BoundedGitControl::new(ObservationCancellation::default(), Duration::from_secs(10));
        let events =
            read_events(produce(fixture.path(), 0, i64::MAX, usize::MAX, &control).unwrap());

        assert!(events.into_iter().any(|event| matches!(
            event,
            PreparedGitEvent::Commit { branch, .. } if branch == "historical"
        )));
    }
}
