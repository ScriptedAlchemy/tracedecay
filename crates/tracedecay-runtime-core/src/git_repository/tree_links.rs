//! Committed symlink resolution inside one Git tree.

use super::{GitRepositoryError, operation};

/// Linux's `MAXSYMLINKS`: links one resolution may follow before it is a loop.
const MAX_LINK_HOPS: usize = 40;

/// The blob a checkout of `tree` reads through the symlink at `link_path`,
/// whose committed blob holds `target`. Relative targets resolve against the
/// link's directory, through nested links, the way the filesystem would.
/// `None` when no file in the tree answers: an absolute target, one that
/// leaves the tree, a dangling or looping link, or a non-file destination.
pub fn resolve_tree_link(
    tree: &gix::Tree<'_>,
    link_path: &[u8],
    target: &[u8],
) -> Result<Option<gix::ObjectId>, GitRepositoryError> {
    let mut resolved = components(link_path).collect::<Vec<_>>();
    resolved.pop();
    let mut pending = Vec::new();
    if !push_target(&mut pending, target) {
        return Ok(None);
    }
    let mut hops = 0;
    while let Some(component) = pending.pop() {
        match component.as_slice() {
            b"" | b"." => continue,
            b".." => {
                if resolved.pop().is_none() {
                    return Ok(None);
                }
                continue;
            }
            _ => resolved.push(component),
        }
        let Some(entry) = tree
            .lookup_entry(resolved.iter().map(Vec::as_slice))
            .map_err(|error| operation("tree link lookup", error))?
        else {
            return Ok(None);
        };
        let mode = entry.mode();
        if mode.is_tree() {
            continue;
        }
        if !mode.is_link() {
            return Ok((pending.is_empty() && mode.is_blob()).then(|| entry.object_id()));
        }
        hops += 1;
        resolved.pop();
        let link = entry
            .object()
            .map_err(|error| operation("tree link target", error))?;
        if hops > MAX_LINK_HOPS || !push_target(&mut pending, &link.data) {
            return Ok(None);
        }
    }
    Ok(None)
}

fn components(path: &[u8]) -> impl DoubleEndedIterator<Item = Vec<u8>> + '_ {
    path.split(|byte| *byte == b'/').map(<[u8]>::to_vec)
}

/// Queues `target`'s components to walk next; an absolute target names no
/// path inside the tree.
fn push_target(pending: &mut Vec<Vec<u8>>, target: &[u8]) -> bool {
    if target.first() == Some(&b'/') {
        return false;
    }
    pending.extend(components(target).rev());
    true
}
