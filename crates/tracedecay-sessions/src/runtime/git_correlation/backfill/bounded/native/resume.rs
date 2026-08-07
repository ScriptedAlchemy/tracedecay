use std::collections::BTreeMap;
use std::io::{Read, Seek};
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use sha2::{Digest, Sha256};

use crate::runtime::git_correlation::backfill::history_progress::initial_reflog_content_chain;

use super::seal::{RepositorySeal, capture_repository_seal, verify_repository_identity};
use super::{
    BoundedBackfillInterruption, BoundedGitControl, Checkout, HeadSeal, HeadState, capture_head,
    classify_checkout_target, exact_ref_tip, parse_checkout, validate_checkout_to,
};

const MAX_REFLOG_CHUNK_BYTES: usize = 64 * 1024;
/// Maximum record payload, excluding the preceding/terminating delimiters.
const MAX_REFLOG_RECORD_BYTES: usize = 1024 * 1024;
const MAX_REFLOG_RECORD_FRAME_BYTES: usize = MAX_REFLOG_RECORD_BYTES + 2;
const MAX_REFLOG_CHUNK_ITEMS: usize = 256;
const MAX_GRAPH_CHUNK_BYTES: usize = 256 * 1024;
const MAX_GRAPH_CHUNK_ITEMS: usize = 128;
const COMPLETION_RESERVE: Duration = Duration::from_millis(750);

#[cfg(test)]
mod tests;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in super::super) enum ReflogHeadState {
    LocalBranch(String),
    Detached,
}

impl From<HeadState> for ReflogHeadState {
    fn from(value: HeadState) -> Self {
        match value {
            HeadState::LocalBranch(branch) => Self::LocalBranch(branch),
            HeadState::Detached => Self::Detached,
        }
    }
}

impl From<ReflogHeadState> for HeadState {
    fn from(value: ReflogHeadState) -> Self {
        match value {
            ReflogHeadState::LocalBranch(branch) => Self::LocalBranch(branch),
            ReflogHeadState::Detached => Self::Detached,
        }
    }
}

#[derive(Clone, Debug)]
pub(in super::super) struct ReflogCursor {
    pub worktree: PathBuf,
    pub worktree_identity: Vec<u8>,
    pub git_dir: PathBuf,
    pub git_dir_identity: Vec<u8>,
    pub common_dir: PathBuf,
    pub common_dir_identity: Vec<u8>,
    pub reflog_path: PathBuf,
    pub source_generation: String,
    pub source_head_referent: Option<Vec<u8>>,
    pub source_head_oid: String,
    pub byte_offset: u64,
    pub state: ReflogHeadState,
    pub state_oid: String,
    pub segment_end: i64,
    pub segment_tip_oid: String,
    pub next_segment_ordinal: i64,
    pub consulted_refs: BTreeMap<Vec<u8>, Option<String>>,
    pub content_chain: String,
}

impl ReflogCursor {
    pub(in super::super) fn repository_seal(&self) -> RepositorySeal {
        RepositorySeal {
            worktree: self.worktree.clone(),
            worktree_identity: self.worktree_identity.clone(),
            git_dir: self.git_dir.clone(),
            git_dir_identity: self.git_dir_identity.clone(),
            common_dir: self.common_dir.clone(),
            common_dir_identity: self.common_dir_identity.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in super::super) struct ReflogSegment {
    pub ordinal: i64,
    pub branch: Option<String>,
    pub start: i64,
    pub end: i64,
    pub tip_oid: String,
}

#[derive(Debug)]
pub(in super::super) struct ReflogChunk {
    pub cursor: ReflogCursor,
    pub segments: Vec<ReflogSegment>,
    pub complete: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in super::super) struct ReflogVerificationCursor {
    pub byte_offset: u64,
    pub content_chain: String,
}

#[derive(Debug)]
pub(in super::super) struct ReflogVerificationChunk {
    pub cursor: ReflogVerificationCursor,
    pub complete: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in super::super) struct GraphPending {
    pub oid: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in super::super) struct GraphCommit {
    pub oid: String,
    pub committed_at: i64,
}

#[derive(Debug)]
pub(in super::super) struct GraphChunk {
    pub pending: Vec<GraphPending>,
    pub newly_seen: Vec<String>,
    pub commits: Vec<GraphCommit>,
    pub examined_nodes: usize,
    pub examined_bytes: usize,
    pub budget_exhausted: bool,
}

pub(in super::super) fn initialize_reflog_cursor(
    project_path: &Path,
    window_end: i64,
    control: &BoundedGitControl,
) -> Result<ReflogCursor, BoundedBackfillInterruption> {
    control.check()?;
    let repository =
        gix::discover(project_path).map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
    let repository_seal = capture_repository_seal(&repository)?;
    let head = capture_head(&repository)?;
    let state = super::head_state(&head)?;
    let source_head_oid = head
        .target
        .ok_or(BoundedBackfillInterruption::SourceUnavailable)?;
    let platform = repository
        .head()
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?
        .log_iter();
    let relative = platform.store.namespace.as_ref().map_or_else(
        || platform.name.to_path().to_owned(),
        |namespace| namespace.to_path().join(platform.name.to_path()),
    );
    let unresolved_reflog_path = platform.store.git_dir().join("logs").join(relative);
    let (reflog_path, source_generation, source_length) =
        match std::fs::metadata(&unresolved_reflog_path) {
            Ok(metadata) => {
                let path = unresolved_reflog_path
                    .canonicalize()
                    .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
                verify_reflog_termination(&path, metadata.len())?;
                let generation = present_source_generation(&path, &metadata)?;
                (path, generation, metadata.len())
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let generation = absent_source_generation(&unresolved_reflog_path)?;
                (unresolved_reflog_path, generation, 0)
            }
            Err(_) => return Err(BoundedBackfillInterruption::SourceUnavailable),
        };
    control.check()?;
    Ok(ReflogCursor {
        worktree: repository_seal.worktree,
        worktree_identity: repository_seal.worktree_identity,
        git_dir: repository_seal.git_dir,
        git_dir_identity: repository_seal.git_dir_identity,
        common_dir: repository_seal.common_dir,
        common_dir_identity: repository_seal.common_dir_identity,
        reflog_path,
        source_generation,
        source_head_referent: head.referent,
        source_head_oid: source_head_oid.to_hex().to_string(),
        byte_offset: source_length,
        state: state.into(),
        state_oid: source_head_oid.to_hex().to_string(),
        segment_end: window_end,
        segment_tip_oid: source_head_oid.to_hex().to_string(),
        next_segment_ordinal: 0,
        consulted_refs: BTreeMap::new(),
        content_chain: initial_reflog_content_chain().to_owned(),
    })
}

pub(in super::super) fn scan_reflog_chunk(
    project_path: &Path,
    window_start: i64,
    window_end: i64,
    mut cursor: ReflogCursor,
    control: &BoundedGitControl,
) -> Result<ReflogChunk, BoundedBackfillInterruption> {
    control.check()?;
    let repository =
        gix::discover(project_path).map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
    verify_source(&repository, &cursor)?;
    let mut consulted_refs = decode_ref_seal(&cursor.consulted_refs)?;
    let mut state: HeadState = cursor.state.clone().into();
    let mut state_oid = parse_oid(&cursor.state_oid)?;
    let mut segment_tip = parse_oid(&cursor.segment_tip_oid)?;
    let (bytes, absolute_start) = if cursor.byte_offset == 0 {
        (Vec::new(), 0)
    } else {
        read_reverse_block(&cursor.reflog_path, cursor.byte_offset)?
    };
    let ranges = complete_line_ranges(&bytes, absolute_start)?;
    let mut segments = Vec::new();
    let mut earliest_processed = cursor.byte_offset;
    let mut items_read = 0_usize;
    let mut complete = false;

    for (absolute_line_start, line) in ranges.into_iter().rev() {
        if items_read >= MAX_REFLOG_CHUNK_ITEMS
            || (items_read > 0 && control.should_soft_stop(COMPLETION_RESERVE)?)
        {
            break;
        }
        control.check()?;
        let entry = gix::refs::file::log::LineRef::from_bytes(line)
            .map_err(|_| BoundedBackfillInterruption::UnsupportedSourceFraming)?
            .to_owned();
        if entry.new_oid != state_oid {
            return Err(BoundedBackfillInterruption::UnsupportedSourceFraming);
        }
        earliest_processed = absolute_line_start;
        items_read = items_read.saturating_add(1);
        cursor.content_chain =
            extend_content_chain(&cursor.content_chain, absolute_line_start, line)?;
        let checkout = parse_checkout(&entry)?;
        let timestamp = entry.signature.time.seconds;
        if timestamp > window_end {
            cross_entry(
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
            let segment_end = cursor.segment_end;
            push_segment(
                &mut cursor,
                &mut segments,
                &state,
                window_start,
                segment_end,
                segment_tip,
            )?;
            complete = true;
            break;
        }
        if let Some(checkout) = checkout {
            validate_checkout_to(&repository, &checkout.to, &state, &mut consulted_refs)?;
            let segment_end = cursor.segment_end;
            push_segment(
                &mut cursor,
                &mut segments,
                &state,
                timestamp,
                segment_end,
                segment_tip,
            )?;
            state = classify_checkout_target(&repository, &checkout.from, &mut consulted_refs)?;
            state_oid = entry.previous_oid;
            cursor.segment_end = timestamp;
            segment_tip = state_oid;
        } else {
            state_oid = entry.previous_oid;
        }
    }
    if !complete && earliest_processed == 0 {
        if !segment_tip.is_null() {
            let segment_end = cursor.segment_end;
            push_segment(
                &mut cursor,
                &mut segments,
                &state,
                window_start,
                segment_end,
                segment_tip,
            )?;
        }
        complete = true;
    }
    cursor.byte_offset = earliest_processed;
    cursor.state = state.into();
    cursor.state_oid = state_oid.to_hex().to_string();
    cursor.segment_tip_oid = segment_tip.to_hex().to_string();
    cursor.consulted_refs = encode_ref_seal(&consulted_refs);
    if capture_head(&repository)? != source_head(&cursor)? {
        return Err(BoundedBackfillInterruption::SourceChanged);
    }
    verify_source(&repository, &cursor)?;
    control.check()?;
    Ok(ReflogChunk {
        cursor,
        segments,
        complete,
    })
}

pub(in super::super) fn scan_reflog_verification_chunk(
    project_path: &Path,
    source: &ReflogCursor,
    target_byte_offset: u64,
    mut cursor: ReflogVerificationCursor,
    control: &BoundedGitControl,
) -> Result<ReflogVerificationChunk, BoundedBackfillInterruption> {
    control.check()?;
    let repository =
        gix::discover(project_path).map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
    verify_source(&repository, source)?;
    if cursor.byte_offset < target_byte_offset {
        return Err(BoundedBackfillInterruption::SourceUnavailable);
    }
    if cursor.byte_offset == target_byte_offset {
        if cursor.content_chain != source.content_chain {
            return Err(BoundedBackfillInterruption::SourceChanged);
        }
        return Ok(ReflogVerificationChunk {
            cursor,
            complete: true,
        });
    }
    let (bytes, absolute_start) = read_reverse_block(&source.reflog_path, cursor.byte_offset)?;
    let ranges = complete_line_ranges(&bytes, absolute_start)?;
    let mut earliest_processed = cursor.byte_offset;
    let mut items_read = 0_usize;
    for (absolute_line_start, line) in ranges.into_iter().rev() {
        if absolute_line_start < target_byte_offset {
            break;
        }
        if items_read >= MAX_REFLOG_CHUNK_ITEMS
            || (items_read > 0 && control.should_soft_stop(COMPLETION_RESERVE)?)
        {
            break;
        }
        control.check()?;
        gix::refs::file::log::LineRef::from_bytes(line)
            .map_err(|_| BoundedBackfillInterruption::UnsupportedSourceFraming)?;
        cursor.content_chain =
            extend_content_chain(&cursor.content_chain, absolute_line_start, line)?;
        cursor.byte_offset = absolute_line_start;
        earliest_processed = absolute_line_start;
        items_read = items_read.saturating_add(1);
    }
    cursor.byte_offset = earliest_processed;
    let complete = cursor.byte_offset == target_byte_offset;
    if cursor.byte_offset < target_byte_offset
        || (complete && cursor.content_chain != source.content_chain)
    {
        return Err(BoundedBackfillInterruption::SourceChanged);
    }
    verify_source(&repository, source)?;
    control.check()?;
    Ok(ReflogVerificationChunk { cursor, complete })
}

#[allow(clippy::too_many_arguments)]
pub(in super::super) fn scan_graph_chunk(
    project_path: &Path,
    window_start: i64,
    window_end: i64,
    repository_seal: &RepositorySeal,
    pending: Vec<GraphPending>,
    remaining_commit_cap: usize,
    remaining_examined_nodes: usize,
    remaining_examined_bytes: usize,
    control: &BoundedGitControl,
) -> Result<GraphChunk, BoundedBackfillInterruption> {
    control.check()?;
    if remaining_examined_nodes == 0 || remaining_examined_bytes == 0 {
        return Err(BoundedBackfillInterruption::HistoryTraversalBudgetReached);
    }
    let mut repository =
        gix::discover(project_path).map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
    repository.object_cache_size_if_unset(4 * 1024 * 1024);
    verify_repository_identity(&repository, repository_seal)?;
    let mut carry = pending
        .into_iter()
        .map(|pending| (pending.oid.clone(), pending))
        .collect::<BTreeMap<_, _>>();
    let mut newly_seen = Vec::new();
    let mut commits = Vec::new();
    let mut bytes_read = 0_usize;
    let mut items_read = 0_usize;
    let mut budget_exhausted = false;
    let input_oids = carry.keys().cloned().collect::<Vec<_>>();
    for current_oid in input_oids {
        if items_read >= MAX_GRAPH_CHUNK_ITEMS.min(remaining_examined_nodes)
            || bytes_read >= MAX_GRAPH_CHUNK_BYTES.min(remaining_examined_bytes)
            || (items_read > 0 && control.should_soft_stop(COMPLETION_RESERVE)?)
        {
            budget_exhausted =
                items_read >= remaining_examined_nodes || bytes_read >= remaining_examined_bytes;
            break;
        }
        control.check()?;
        let current = carry
            .get(&current_oid)
            .ok_or(BoundedBackfillInterruption::SourceUnavailable)?;
        let oid = parse_oid(&current.oid)?;
        let header = repository
            .find_header(oid)
            .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
        if header.kind() != gix::object::Kind::Commit
            || header.size() > MAX_GRAPH_CHUNK_BYTES as u64
        {
            return Err(BoundedBackfillInterruption::UnsupportedSourceFraming);
        }
        let commit = repository
            .find_commit(oid)
            .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
        let commit_bytes = commit.data.len();
        if commit_bytes > MAX_GRAPH_CHUNK_BYTES {
            return Err(BoundedBackfillInterruption::UnsupportedSourceFraming);
        }
        if bytes_read.saturating_add(commit_bytes) > remaining_examined_bytes {
            budget_exhausted = true;
            break;
        }
        if items_read > 0 && bytes_read.saturating_add(commit_bytes) > MAX_GRAPH_CHUNK_BYTES {
            break;
        }
        let committed_at = commit
            .time()
            .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?
            .seconds;
        let parents = commit.parent_ids().map(gix::Id::detach).collect::<Vec<_>>();
        bytes_read = bytes_read.saturating_add(commit_bytes);
        items_read = items_read.saturating_add(1);
        let current = carry
            .remove(&current_oid)
            .ok_or(BoundedBackfillInterruption::SourceUnavailable)?;
        newly_seen.push(current.oid.clone());
        if (window_start..=window_end).contains(&committed_at) {
            if commits.len() >= remaining_commit_cap {
                return Err(BoundedBackfillInterruption::HistoryLimitReached);
            }
            commits.push(GraphCommit {
                oid: current.oid,
                committed_at,
            });
        }
        for parent in parents {
            let oid = parent.to_hex().to_string();
            if !newly_seen.contains(&oid) {
                carry.entry(oid.clone()).or_insert(GraphPending { oid });
            }
        }
    }
    verify_repository_identity(&repository, repository_seal)?;
    control.check()?;
    Ok(GraphChunk {
        pending: carry.into_values().collect(),
        newly_seen,
        commits,
        examined_nodes: items_read,
        examined_bytes: bytes_read,
        budget_exhausted,
    })
}

fn cross_entry(
    repository: &gix::Repository,
    entry: &gix::refs::log::Line,
    checkout: Option<&Checkout>,
    consulted_refs: &mut BTreeMap<Vec<u8>, Option<gix::ObjectId>>,
    state: &mut HeadState,
    state_oid: &mut gix::ObjectId,
) -> Result<(), BoundedBackfillInterruption> {
    if let Some(checkout) = checkout {
        validate_checkout_to(repository, &checkout.to, state, consulted_refs)?;
        *state = classify_checkout_target(repository, &checkout.from, consulted_refs)?;
    }
    *state_oid = entry.previous_oid;
    Ok(())
}

fn push_segment(
    cursor: &mut ReflogCursor,
    segments: &mut Vec<ReflogSegment>,
    state: &HeadState,
    start: i64,
    end: i64,
    tip: gix::ObjectId,
) -> Result<(), BoundedBackfillInterruption> {
    if start > end {
        return Ok(());
    }
    let ordinal = cursor.next_segment_ordinal;
    cursor.next_segment_ordinal = ordinal
        .checked_add(1)
        .ok_or(BoundedBackfillInterruption::SourceUnavailable)?;
    segments.push(ReflogSegment {
        ordinal,
        branch: state.branch().map(str::to_owned),
        start,
        end,
        tip_oid: tip.to_hex().to_string(),
    });
    Ok(())
}

fn read_reverse_block(
    path: &Path,
    end: u64,
) -> Result<(Vec<u8>, u64), BoundedBackfillInterruption> {
    let mut file =
        std::fs::File::open(path).map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
    let mut requested = MAX_REFLOG_CHUNK_BYTES;
    loop {
        let start = end.saturating_sub(requested as u64);
        let length = usize::try_from(end.saturating_sub(start))
            .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
        let mut bytes = vec![0_u8; length];
        file.seek(std::io::SeekFrom::Start(start))
            .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
        file.read_exact(&mut bytes)
            .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
        let has_preceding_record_boundary = bytes
            .get(..bytes.len().saturating_sub(1))
            .is_some_and(|prefix| prefix.contains(&b'\n'));
        if start == 0 || has_preceding_record_boundary {
            return Ok((bytes, start));
        }
        if requested >= MAX_REFLOG_RECORD_FRAME_BYTES {
            return Err(BoundedBackfillInterruption::UnsupportedSourceFraming);
        }
        requested = requested
            .saturating_mul(2)
            .min(MAX_REFLOG_RECORD_FRAME_BYTES);
    }
}

fn complete_line_ranges(
    bytes: &[u8],
    absolute_start: u64,
) -> Result<Vec<(u64, &[u8])>, BoundedBackfillInterruption> {
    let usable_start = if absolute_start == 0 {
        0
    } else {
        bytes
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|position| position.saturating_add(1))
            .ok_or(BoundedBackfillInterruption::UnsupportedSourceFraming)?
    };
    let mut ranges = Vec::new();
    let mut cursor = usable_start;
    while cursor < bytes.len() {
        let relative_end = bytes[cursor..]
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(bytes.len(), |position| cursor + position);
        if relative_end == cursor {
            return Err(BoundedBackfillInterruption::UnsupportedSourceFraming);
        }
        ranges.push((
            absolute_start.saturating_add(cursor as u64),
            &bytes[cursor..relative_end],
        ));
        if relative_end == bytes.len() {
            break;
        }
        cursor = relative_end.saturating_add(1);
    }
    Ok(ranges)
}

fn source_head(cursor: &ReflogCursor) -> Result<HeadSeal, BoundedBackfillInterruption> {
    Ok(HeadSeal {
        referent: cursor.source_head_referent.clone(),
        target: Some(parse_oid(&cursor.source_head_oid)?),
    })
}

fn parse_oid(value: &str) -> Result<gix::ObjectId, BoundedBackfillInterruption> {
    gix::ObjectId::from_hex(value.as_bytes())
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)
}

fn present_source_generation(
    path: &Path,
    metadata: &std::fs::Metadata,
) -> Result<String, BoundedBackfillInterruption> {
    let mut hasher = Sha256::new();
    hasher.update(b"tracedecay-git-reflog-source-generation-v1\0");
    hash_path(&mut hasher, path);
    hasher.update(metadata.len().to_le_bytes());
    let modified = metadata
        .modified()
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?
        .duration_since(UNIX_EPOCH)
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
    hasher.update(modified.as_secs().to_le_bytes());
    hasher.update(modified.subsec_nanos().to_le_bytes());
    hash_file_identity(&mut hasher, path, metadata)?;
    Ok(format!("sha256:{}", hex::encode(hasher.finalize())))
}

fn verify_reflog_termination(
    path: &Path,
    source_length: u64,
) -> Result<(), BoundedBackfillInterruption> {
    if source_length == 0 {
        return Ok(());
    }
    let mut file =
        std::fs::File::open(path).map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
    file.seek(std::io::SeekFrom::Start(source_length.saturating_sub(1)))
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
    let mut last = [0_u8; 1];
    file.read_exact(&mut last)
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
    if last != *b"\n" {
        return Err(BoundedBackfillInterruption::UnsupportedSourceFraming);
    }
    Ok(())
}

#[cfg(unix)]
fn hash_path(hasher: &mut Sha256, path: &Path) {
    use std::os::unix::ffi::OsStrExt as _;

    let bytes = path.as_os_str().as_bytes();
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

#[cfg(windows)]
fn hash_path(hasher: &mut Sha256, path: &Path) {
    use std::os::windows::ffi::OsStrExt as _;

    let encoded = path.as_os_str().encode_wide().collect::<Vec<_>>();
    hasher.update((encoded.len() as u64).to_le_bytes());
    for unit in encoded {
        hasher.update(unit.to_le_bytes());
    }
}

#[cfg(not(any(unix, windows)))]
fn hash_path(_hasher: &mut Sha256, _path: &Path) {}

#[cfg(unix)]
fn hash_file_identity(
    hasher: &mut Sha256,
    _path: &Path,
    metadata: &std::fs::Metadata,
) -> Result<(), BoundedBackfillInterruption> {
    use std::os::unix::fs::MetadataExt as _;

    hasher.update(metadata.dev().to_le_bytes());
    hasher.update(metadata.ino().to_le_bytes());
    hasher.update(metadata.ctime().to_le_bytes());
    hasher.update(metadata.ctime_nsec().to_le_bytes());
    Ok(())
}

#[cfg(windows)]
fn hash_file_identity(
    hasher: &mut Sha256,
    path: &Path,
    _metadata: &std::fs::Metadata,
) -> Result<(), BoundedBackfillInterruption> {
    // Identity comes from the stable GetFileInformationByHandle authority in
    // runtime-core instead of the unstable `windows_by_handle` metadata
    // surface. The hashed byte layout (u32 volume + u64 index, little endian)
    // is unchanged.
    let file =
        std::fs::File::open(path).map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
    let information = tracedecay_runtime_core::windows_file::information(&file)
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
    hasher.update(information.volume_serial_number.to_le_bytes());
    hasher.update(information.file_index.to_le_bytes());
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn hash_file_identity(
    _hasher: &mut Sha256,
    _path: &Path,
    _metadata: &std::fs::Metadata,
) -> Result<(), BoundedBackfillInterruption> {
    Err(BoundedBackfillInterruption::SourceUnavailable)
}

#[cfg(any(unix, windows))]
fn absent_source_generation(path: &Path) -> Result<String, BoundedBackfillInterruption> {
    let mut hasher = Sha256::new();
    hasher.update(b"tracedecay-git-reflog-absent-source-v1\0");
    hash_path(&mut hasher, path);
    Ok(format!("absent:sha256:{}", hex::encode(hasher.finalize())))
}

#[cfg(not(any(unix, windows)))]
fn absent_source_generation(_path: &Path) -> Result<String, BoundedBackfillInterruption> {
    Err(BoundedBackfillInterruption::SourceUnavailable)
}

fn extend_content_chain(
    previous: &str,
    absolute_line_start: u64,
    line: &[u8],
) -> Result<String, BoundedBackfillInterruption> {
    let previous = previous
        .strip_prefix("sha256:")
        .ok_or(BoundedBackfillInterruption::SourceUnavailable)?;
    let previous =
        hex::decode(previous).map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
    if previous.len() != 32 {
        return Err(BoundedBackfillInterruption::SourceUnavailable);
    }
    let mut hasher = Sha256::new();
    hasher.update(b"tracedecay-git-reflog-reverse-content-chain-v1\0");
    hasher.update(previous);
    hasher.update(absolute_line_start.to_le_bytes());
    hasher.update((line.len() as u64).to_le_bytes());
    hasher.update(line);
    Ok(format!("sha256:{}", hex::encode(hasher.finalize())))
}

pub(in super::super) fn verify_source(
    repository: &gix::Repository,
    cursor: &ReflogCursor,
) -> Result<(), BoundedBackfillInterruption> {
    verify_repository_identity(repository, &cursor.repository_seal())?;
    if capture_head(repository)? != source_head(cursor)? {
        return Err(BoundedBackfillInterruption::SourceChanged);
    }
    match std::fs::metadata(&cursor.reflog_path) {
        Ok(metadata) => {
            if verify_reflog_termination(&cursor.reflog_path, metadata.len()).is_err() {
                return Err(BoundedBackfillInterruption::SourceChanged);
            }
            if present_source_generation(&cursor.reflog_path, &metadata)?
                != cursor.source_generation
            {
                return Err(BoundedBackfillInterruption::SourceChanged);
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if absent_source_generation(&cursor.reflog_path)? != cursor.source_generation {
                return Err(BoundedBackfillInterruption::SourceChanged);
            }
        }
        Err(_) => return Err(BoundedBackfillInterruption::SourceUnavailable),
    }
    for (reference, expected) in decode_ref_seal(&cursor.consulted_refs)? {
        if exact_ref_tip(repository, &reference)? != expected {
            return Err(BoundedBackfillInterruption::SourceChanged);
        }
    }
    Ok(())
}

pub(in super::super) fn verify_reflog_source(
    project_path: &Path,
    cursor: &ReflogCursor,
    control: &BoundedGitControl,
) -> Result<(), BoundedBackfillInterruption> {
    control.check()?;
    let repository =
        gix::discover(project_path).map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
    verify_source(&repository, cursor)?;
    control.check()
}

#[cfg(unix)]
pub(in super::super) fn encode_path(path: &Path) -> Result<Vec<u8>, BoundedBackfillInterruption> {
    use std::os::unix::ffi::OsStrExt as _;

    Ok(path.as_os_str().as_bytes().to_vec())
}

#[cfg(unix)]
pub(in super::super) fn decode_path(
    encoded: &[u8],
) -> Result<PathBuf, BoundedBackfillInterruption> {
    use std::os::unix::ffi::OsStrExt as _;

    Ok(PathBuf::from(std::ffi::OsStr::from_bytes(encoded)))
}

#[cfg(windows)]
pub(in super::super) fn encode_path(path: &Path) -> Result<Vec<u8>, BoundedBackfillInterruption> {
    use std::os::windows::ffi::OsStrExt as _;

    Ok(path
        .as_os_str()
        .encode_wide()
        .flat_map(u16::to_le_bytes)
        .collect())
}

#[cfg(windows)]
pub(in super::super) fn decode_path(
    encoded: &[u8],
) -> Result<PathBuf, BoundedBackfillInterruption> {
    use std::os::windows::ffi::OsStringExt as _;

    if !encoded.len().is_multiple_of(2) {
        return Err(BoundedBackfillInterruption::SourceUnavailable);
    }
    let wide = encoded
        .chunks_exact(2)
        .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
        .collect::<Vec<_>>();
    Ok(PathBuf::from(std::ffi::OsString::from_wide(&wide)))
}

#[cfg(not(any(unix, windows)))]
pub(in super::super) fn encode_path(_path: &Path) -> Result<Vec<u8>, BoundedBackfillInterruption> {
    Err(BoundedBackfillInterruption::SourceUnavailable)
}

#[cfg(not(any(unix, windows)))]
pub(in super::super) fn decode_path(
    _encoded: &[u8],
) -> Result<PathBuf, BoundedBackfillInterruption> {
    Err(BoundedBackfillInterruption::SourceUnavailable)
}

fn decode_ref_seal(
    encoded: &BTreeMap<Vec<u8>, Option<String>>,
) -> Result<BTreeMap<Vec<u8>, Option<gix::ObjectId>>, BoundedBackfillInterruption> {
    encoded
        .iter()
        .map(|(reference, oid)| {
            oid.as_deref()
                .map(parse_oid)
                .transpose()
                .map(|oid| (reference.clone(), oid))
        })
        .collect()
}

fn encode_ref_seal(
    seal: &BTreeMap<Vec<u8>, Option<gix::ObjectId>>,
) -> BTreeMap<Vec<u8>, Option<String>> {
    seal.iter()
        .map(|(reference, oid)| (reference.clone(), oid.map(|oid| oid.to_hex().to_string())))
        .collect()
}
