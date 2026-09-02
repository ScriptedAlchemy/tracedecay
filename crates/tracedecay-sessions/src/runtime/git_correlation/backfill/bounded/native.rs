use std::collections::BTreeMap;

use gix::bstr::ByteSlice;

use super::{BoundedBackfillInterruption, BoundedGitControl};

mod resume;
mod seal;
pub(super) use resume::{
    GraphChunk, GraphPending, ReflogCursor, ReflogHeadState, ReflogVerificationCursor, decode_path,
    encode_path, initialize_reflog_cursor, scan_graph_chunk, scan_reflog_chunk,
    scan_reflog_verification_chunk, verify_reflog_source,
};
pub(super) use seal::{RepositorySeal, verify_repository_source};

#[derive(Clone, Debug, PartialEq, Eq)]
struct HeadSeal {
    referent: Option<Vec<u8>>,
    target: Option<gix::ObjectId>,
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
struct CheckoutTarget(Vec<u8>);

#[derive(Debug)]
struct Checkout {
    from: CheckoutTarget,
    to: CheckoutTarget,
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
            Ok(std::str::from_utf8(short)
                .map(str::to_owned)
                .map_or(HeadState::Detached, HeadState::LocalBranch))
        }
        None => Ok(HeadState::Detached),
    }
}

fn parse_checkout(
    entry: &gix::refs::log::Line,
) -> Result<Option<Checkout>, BoundedBackfillInterruption> {
    let Some(moving) = entry.message.strip_prefix(b"checkout: moving from ") else {
        return Ok(None);
    };
    let split = moving
        .rfind(b" to ")
        .ok_or(BoundedBackfillInterruption::UnsupportedSourceFraming)?;
    let from = moving
        .get(..split)
        .ok_or(BoundedBackfillInterruption::UnsupportedSourceFraming)?;
    let to = moving
        .get(split + b" to ".len()..)
        .ok_or(BoundedBackfillInterruption::UnsupportedSourceFraming)?;
    if from.is_empty() || to.is_empty() {
        return Err(BoundedBackfillInterruption::UnsupportedSourceFraming);
    }
    Ok(Some(Checkout {
        from: parse_checkout_target(from)?,
        to: parse_checkout_target(to)?,
    }))
}

fn parse_checkout_target(target: &[u8]) -> Result<CheckoutTarget, BoundedBackfillInterruption> {
    Ok(CheckoutTarget(target.to_vec()))
}

fn classify_checkout_target(
    repository: &gix::Repository,
    target: &CheckoutTarget,
    consulted_refs: &mut BTreeMap<Vec<u8>, Option<gix::ObjectId>>,
) -> Result<HeadState, BoundedBackfillInterruption> {
    let label = &target.0;
    if label.starts_with(b"refs/") {
        return Ok(HeadState::Detached);
    }
    let local_ref = [b"refs/heads/".as_slice(), label.as_slice()].concat();
    if gix::refs::FullName::try_from(local_ref.as_bstr()).is_err() {
        return Ok(HeadState::Detached);
    }
    if consult_exact_ref(repository, consulted_refs, &local_ref)?.is_some() {
        return Ok(std::str::from_utf8(label)
            .map(str::to_owned)
            .map_or(HeadState::Detached, HeadState::LocalBranch));
    }
    for alternate in [
        [b"refs/tags/".as_slice(), label.as_slice()].concat(),
        [b"refs/remotes/".as_slice(), label.as_slice()].concat(),
    ] {
        if gix::refs::FullName::try_from(alternate.as_bstr()).is_ok()
            && consult_exact_ref(repository, consulted_refs, &alternate)?.is_some()
        {
            return Ok(HeadState::Detached);
        }
    }
    if (7..=64).contains(&label.len()) && label.iter().all(u8::is_ascii_hexdigit) {
        return Ok(HeadState::Detached);
    }
    if gix::refs::FullName::try_from(label.as_bstr()).is_ok()
        && consult_exact_ref(repository, consulted_refs, label)?.is_some()
    {
        return Ok(HeadState::Detached);
    }
    // An unresolved historical label is ambiguous after ref deletion: it
    // could have named either a local branch or a detached tag/revision.
    // Detached is the only attribution that does not invent branch evidence.
    Ok(HeadState::Detached)
}

fn validate_checkout_to(
    repository: &gix::Repository,
    target: &CheckoutTarget,
    established: &HeadState,
    consulted_refs: &mut BTreeMap<Vec<u8>, Option<gix::ObjectId>>,
) -> Result<(), BoundedBackfillInterruption> {
    seal_checkout_target_refs(repository, target, consulted_refs)?;
    match (established, target) {
        (HeadState::LocalBranch(expected), CheckoutTarget(actual))
            if expected.as_bytes() == actual =>
        {
            Ok(())
        }
        (HeadState::Detached, _) => Ok(()),
        _ => Err(BoundedBackfillInterruption::UnsupportedSourceFraming),
    }
}

fn seal_checkout_target_refs(
    repository: &gix::Repository,
    target: &CheckoutTarget,
    consulted_refs: &mut BTreeMap<Vec<u8>, Option<gix::ObjectId>>,
) -> Result<(), BoundedBackfillInterruption> {
    let label = &target.0;
    if label.starts_with(b"refs/") {
        if gix::refs::FullName::try_from(label.as_bstr()).is_ok() {
            consult_exact_ref(repository, consulted_refs, label)?;
        }
        return Ok(());
    }
    for candidate in [
        [b"refs/heads/".as_slice(), label.as_slice()].concat(),
        [b"refs/tags/".as_slice(), label.as_slice()].concat(),
        [b"refs/remotes/".as_slice(), label.as_slice()].concat(),
    ] {
        if gix::refs::FullName::try_from(candidate.as_bstr()).is_ok() {
            consult_exact_ref(repository, consulted_refs, &candidate)?;
        }
    }
    if gix::refs::FullName::try_from(label.as_bstr()).is_ok() {
        consult_exact_ref(repository, consulted_refs, label)?;
    }
    Ok(())
}

fn consult_exact_ref(
    repository: &gix::Repository,
    consulted_refs: &mut BTreeMap<Vec<u8>, Option<gix::ObjectId>>,
    reference: &[u8],
) -> Result<Option<gix::ObjectId>, BoundedBackfillInterruption> {
    let tip = exact_ref_tip(repository, reference)?;
    if let Some(previous) = consulted_refs.insert(reference.to_vec(), tip)
        && previous != tip
    {
        return Err(BoundedBackfillInterruption::SourceChanged);
    }
    Ok(tip)
}

fn exact_ref_tip(
    repository: &gix::Repository,
    reference: &[u8],
) -> Result<Option<gix::ObjectId>, BoundedBackfillInterruption> {
    let full_name = gix::refs::FullName::try_from(reference.as_bstr())
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
    let reference = repository
        .try_find_reference(&full_name)
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
    reference
        .map(|reference| {
            if reference.name() != full_name.as_ref() {
                return Err(BoundedBackfillInterruption::SourceUnavailable);
            }
            reference
                .try_id()
                .map(gix::Id::detach)
                .ok_or(BoundedBackfillInterruption::SourceUnavailable)
        })
        .transpose()
}
