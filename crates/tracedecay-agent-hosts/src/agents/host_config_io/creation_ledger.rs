//! Which host config structure the running lifecycle operation created.
//!
//! An install records each config file it creates and each JSON container it
//! adds inside a file; the host lifecycle receipt keeps those facts, never
//! the operator's bytes. Uninstall replays them: once TraceDecay's own
//! entries are gone, a recorded container that is empty is removed and a
//! recorded file that is empty is deleted. A file or container that existed
//! before install is never recorded, so it survives uninstall even when it
//! ends empty. Outside a lifecycle operation nothing is recorded and nothing
//! is pruned.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use tracedecay_host_integration::HostConfigCreationV1;

thread_local! {
    static LEDGER: RefCell<Option<Ledger>> = const { RefCell::new(None) };
}

struct Ledger {
    root: PathBuf,
    removing: bool,
    files: BTreeMap<String, FileCreation>,
}

#[derive(Default)]
struct FileCreation {
    file: bool,
    containers: BTreeSet<String>,
}

/// `path` below `root` as `/`-joined normal UTF-8 components, or `None` when
/// it is not strictly below `root` in that form.
pub(crate) fn root_relative_path(root: &Path, path: &Path) -> Option<String> {
    path.strip_prefix(root)
        .ok()?
        .components()
        .map(|component| match component {
            Component::Normal(name) => name.to_str(),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()
        .map(|components| components.join("/"))
        .filter(|relative| !relative.is_empty())
}

/// Run one lifecycle operation's host config effects against the creation
/// facts its receipts recorded, returning the facts its new receipt keeps.
/// `removing` operations prune recorded structure and keep no facts.
pub(crate) fn with_host_config_creations<T>(
    root: &Path,
    recorded: &[HostConfigCreationV1],
    removing: bool,
    effect: impl FnOnce() -> T,
) -> (T, Vec<HostConfigCreationV1>) {
    let mut files = BTreeMap::<String, FileCreation>::new();
    for creation in recorded {
        let facts = files.entry(creation.relative_path.clone()).or_default();
        facts.file |= creation.created_file;
        facts
            .containers
            .extend(creation.created_containers.iter().cloned());
    }
    let previous = LEDGER.with(|ledger| {
        ledger.replace(Some(Ledger {
            root: root.to_path_buf(),
            removing,
            files,
        }))
    });
    let output = effect();
    let ledger = LEDGER.with(|ledger| ledger.replace(previous));
    let kept = match ledger {
        Some(ledger) if !ledger.removing => ledger
            .files
            .into_iter()
            .filter(|(_, facts)| facts.file || !facts.containers.is_empty())
            .map(|(relative_path, facts)| HostConfigCreationV1 {
                relative_path,
                created_file: facts.file,
                created_containers: facts.containers.into_iter().collect(),
            })
            .collect(),
        _ => Vec::new(),
    };
    (output, kept)
}

/// Run `effect` as one lifecycle operation over `facts`, replacing them with
/// the facts its receipt would keep, as the component-set transaction does.
#[cfg(test)]
pub(crate) fn recorded_lifecycle<T>(
    root: &Path,
    facts: &mut Vec<HostConfigCreationV1>,
    removing: bool,
    effect: impl FnOnce() -> T,
) -> T {
    let (output, kept) = with_host_config_creations(root, facts, removing, effect);
    *facts = kept;
    output
}

/// Install, then uninstall, as two recorded lifecycle operations over `root`.
#[cfg(test)]
#[allow(clippy::unwrap_used)]
pub(crate) fn recorded_install_then_uninstall<A, B>(
    root: &Path,
    install: impl FnOnce() -> tracedecay_domain::errors::Result<A>,
    uninstall: impl FnOnce() -> tracedecay_domain::errors::Result<B>,
) {
    let mut facts = Vec::new();
    recorded_lifecycle(root, &mut facts, false, install).unwrap();
    recorded_lifecycle(root, &mut facts, true, uninstall).unwrap();
}

fn with_facts<T>(path: &Path, read: impl FnOnce(bool, &mut FileCreation) -> T) -> Option<T> {
    LEDGER.with(|ledger| {
        let mut ledger = ledger.borrow_mut();
        let ledger = ledger.as_mut()?;
        let relative = root_relative_path(&ledger.root, path)?;
        let removing = ledger.removing;
        Some(read(removing, ledger.files.entry(relative).or_default()))
    })
}

/// Record that the running install published `path` where no file was.
pub(crate) fn note_created_file(path: &Path) {
    with_facts(path, |removing, facts| {
        if !removing {
            facts.file = true;
        }
    });
}

/// True when the running lifecycle, or the install its receipt records,
/// created `path`.
pub(crate) fn lifecycle_created_file(path: &Path) -> bool {
    with_facts(path, |_, facts| facts.file).unwrap_or(false)
}

/// Record that the running install added the container at `pointer`.
pub(crate) fn note_created_container(path: &Path, pointer: &str) {
    with_facts(path, |removing, facts| {
        if !removing {
            facts.containers.insert(pointer.to_string());
        }
    });
}

/// True when the running lifecycle, or the install its receipt records,
/// added the container at `pointer` to `path`.
pub(crate) fn lifecycle_created_container(path: &Path, pointer: &str) -> bool {
    with_facts(path, |_, facts| facts.containers.contains(pointer)).unwrap_or(false)
}

/// Bring the recorded containers of one JSON config in line with the value
/// about to be published. An install records every object member holding an
/// object or array that `before` lacked, and forgets recorded ones that no
/// longer exist; an uninstall removes every recorded container that is empty,
/// innermost first.
pub(super) fn reconcile_json_containers(
    path: &Path,
    before: &serde_json::Value,
    after: &mut serde_json::Value,
) {
    with_facts(path, |removing, facts| {
        if removing {
            // Longer pointers are deeper or later siblings; removing them
            // first empties a parent before the parent is checked.
            let mut recorded = facts.containers.iter().collect::<Vec<_>>();
            recorded.sort_by_key(|pointer| std::cmp::Reverse(pointer.matches('/').count()));
            for pointer in recorded {
                prune_empty_container(after, pointer);
            }
        } else {
            facts
                .containers
                .retain(|pointer| after.pointer(pointer).is_some());
            let mut added = Vec::new();
            collect_added_containers(before, after, &mut String::new(), &mut added);
            facts.containers.extend(added);
        }
    });
}

fn collect_added_containers(
    before: &serde_json::Value,
    after: &serde_json::Value,
    pointer: &mut String,
    added: &mut Vec<String>,
) {
    let Some(members) = after.as_object() else {
        return;
    };
    for (key, value) in members {
        if !(value.is_object() || value.is_array()) {
            continue;
        }
        let length = pointer.len();
        pointer.push('/');
        pointer.push_str(&key.replace('~', "~0").replace('/', "~1"));
        if before.pointer(pointer).is_none() {
            added.push(pointer.clone());
        }
        collect_added_containers(before, value, pointer, added);
        pointer.truncate(length);
    }
}

fn prune_empty_container(value: &mut serde_json::Value, pointer: &str) {
    let Some((parent, key)) = pointer.rsplit_once('/') else {
        return;
    };
    let key = key.replace("~1", "/").replace("~0", "~");
    let Some(members) = value
        .pointer_mut(parent)
        .and_then(serde_json::Value::as_object_mut)
    else {
        return;
    };
    let empty = members.get(&key).is_some_and(|member| {
        member.as_object().is_some_and(serde_json::Map::is_empty)
            || member.as_array().is_some_and(Vec::is_empty)
    });
    if empty {
        members.remove(&key);
    }
}
