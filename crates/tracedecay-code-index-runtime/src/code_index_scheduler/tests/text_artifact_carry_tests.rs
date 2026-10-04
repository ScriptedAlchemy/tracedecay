use std::{
    cmp::Ordering,
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::Arc,
};

use tempfile::TempDir;

use super::{GitFixture, UNTOUCHED_FILLERS, active_text_artifact_path, published, scheduler};
use crate::code_index_scheduler::{
    CodeIndexWorktreeSchedulerV1, LatestCompleteCodeIndexV1, SharedCodeIndexBytePoolV1,
};

const SEGMENTS: &str = "code-generation-segments-v1";

/// A module that imports its neighbour, owns a body every module repeats
/// (so clone payloads are shared across files), and one body of its own.
fn module(index: usize, extra: &str) -> String {
    let next = (index + 1) % 9;
    format!(
        "use crate::m{next:02}::helper_{next:02};\n\n\
         /// Folds the module offset into `value`.\n\
         pub fn helper_{index:02}(value: u64) -> u64 {{\n    let mut total = value;\n    for step in 0..{index} {{\n        total = total.wrapping_mul(31).wrapping_add(step);\n    }}\n    total\n}}\n\n\
         pub fn shared_body(value: u64) -> u64 {{\n    let mut total = value;\n    for step in 0..8 {{\n        total = total.wrapping_mul(17).wrapping_add(step);\n    }}\n    total\n}}\n\n\
         pub fn call_{index:02}() -> u64 {{\n    helper_{next:02}({index})\n}}\n{extra}"
    )
}

/// [`module`] with `functions` more one-line functions, one chunk each, so
/// enough of them spill the file past a 128-chunk text page.
fn long_module(index: usize, functions: usize) -> String {
    let extra = (0..functions)
        .map(|function| format!("pub fn f{index:02}_{function}() -> u64 {{ {function} }}\n"))
        .collect::<Vec<_>>()
        .concat();
    module(index, &extra)
}

fn corpus() -> Vec<(String, String)> {
    let mut files = vec![(
        "src/lib.rs".to_owned(),
        (0..9)
            .map(|index| format!("pub mod m{index:02};\n"))
            .collect::<Vec<_>>()
            .concat(),
    )];
    files.extend((0..9).map(|index| (format!("src/m{index:02}.rs"), module(index, ""))));
    files.extend(
        UNTOUCHED_FILLERS
            .iter()
            .map(|(path, source)| ((*path).to_owned(), (*source).to_owned())),
    );
    files
}

fn corpus_fixture() -> GitFixture {
    let files = corpus();
    let file_refs: Vec<(&str, &str)> = files
        .iter()
        .map(|(path, source)| (path.as_str(), source.as_str()))
        .collect();
    GitFixture::new(&file_refs)
}

fn drain_text(scheduler: &CodeIndexWorktreeSchedulerV1) {
    drain_latest_text(&scheduler.latest_complete().expect("latest generation"));
}

fn drain_latest_text(latest: &LatestCompleteCodeIndexV1) {
    let mut passes = 0_usize;
    while !latest
        .advance_text_serving(64)
        .expect("advance the text artifact build")
    {
        passes += 1;
        assert!(passes < 10_000, "the text artifact build never completed");
    }
}

/// The bytes of the active generation's published text artifact.
fn active_artifact(store: &Path) -> Vec<u8> {
    std::fs::read(active_text_artifact_path(store)).expect("read artifact")
}

/// How many source pages a published text artifact holds.
fn page_count(artifact: &[u8]) -> i64 {
    let copy = TempDir::new().expect("artifact copy");
    let path = copy.path().join("artifact.sqlite");
    std::fs::write(&path, artifact).expect("copy the artifact");
    rusqlite::Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("open the artifact copy")
        .query_row("SELECT COUNT(*) FROM source_pages", [], |row| row.get(0))
        .expect("count source pages")
}

fn segment_files(store: &Path) -> BTreeSet<PathBuf> {
    std::fs::read_dir(store.join(SEGMENTS))
        .expect("read segments")
        .map(|entry| entry.expect("segment entry").path())
        .collect()
}

/// A cold build of the fixture's current tree in a fresh store.
fn cold_artifact(fixture: &GitFixture) -> Vec<u8> {
    let store = TempDir::new().expect("cold store");
    let mut scheduler = scheduler(
        fixture,
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );
    published(scheduler.reconcile_now().expect("cold publish"));
    drain_text(&scheduler);
    active_artifact(store.path())
}

/// An edit's text artifact is built from the parent's artifact and only the
/// edited files' segments: while the successor builds, every segment the
/// parent generation wrote is out of the store, so a build that decoded an
/// unchanged file could not finish. What it seals is byte for byte what a
/// cold build of the same tree seals, through edits that keep the page count
/// (documents and clones move, pages stay), an edit that spills the edited
/// file onto a second page (every later page moves forward), one that pulls
/// it back beside a later edited file staged at the shifted position, and
/// one that spills two files so a later edited file both moves and grows.
#[test]
fn an_edit_carries_the_parent_text_artifact_byte_identical_to_a_cold_build() {
    let fixture = corpus_fixture();
    let store = TempDir::new().expect("store");
    let mut scheduler = scheduler(
        &fixture,
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );
    published(scheduler.reconcile_now().expect("publish the parent"));
    drain_text(&scheduler);

    let grown = module(
        4,
        "\npub fn added_04(value: u64) -> u64 {\n    value.rotate_left(4) ^ 0x55\n}\n\n\
         pub fn shared_twin(value: u64) -> u64 {\n    let mut total = value;\n    for step in 0..8 {\n        total = total.wrapping_mul(17).wrapping_add(step);\n    }\n    total\n}\n",
    );
    let shrunk = "use crate::m05::helper_05;\n\npub fn call_04() -> u64 {\n    helper_05(4)\n}\n";
    let body_edit = module(7, "").replace("wrapping_mul(31)", "wrapping_mul(37)");
    let second_body_edit = module(7, "").replace("wrapping_mul(31)", "wrapping_mul(41)");
    // Not module(4, "") itself: segments are content-addressed, so restoring
    // the first generation's text would reuse a segment hidden as the parent's.
    let one_page_04 = module(4, "").replace("wrapping_mul(31)", "wrapping_mul(43)");
    let two_pages_04 = long_module(4, 150);
    let two_pages_02 = long_module(2, 140);
    let three_pages_07 = long_module(7, 300);
    let rounds: [(Vec<(&str, &str)>, Ordering); 5] = [
        (vec![("src/m04.rs", grown.as_str())], Ordering::Equal),
        (
            vec![("src/m04.rs", shrunk), ("src/m07.rs", body_edit.as_str())],
            Ordering::Equal,
        ),
        (
            vec![("src/m04.rs", two_pages_04.as_str())],
            Ordering::Greater,
        ),
        (
            vec![
                ("src/m04.rs", one_page_04.as_str()),
                ("src/m07.rs", second_body_edit.as_str()),
            ],
            Ordering::Less,
        ),
        (
            vec![
                ("src/m02.rs", two_pages_02.as_str()),
                ("src/m07.rs", three_pages_07.as_str()),
            ],
            Ordering::Greater,
        ),
    ];
    let hidden = TempDir::new().expect("hidden segments");
    let mut pages = page_count(&active_artifact(store.path()));
    for (round, (edit, page_change)) in rounds.iter().enumerate() {
        let parent_segments = segment_files(store.path());
        for (path, source) in edit {
            fixture.edit(path, source);
        }
        fixture.commit_all(&format!("edit {round}"));
        published(scheduler.reconcile_now().expect("publish the successor"));
        let successor = scheduler.latest_complete().expect("successor generation");
        for segment in &parent_segments {
            std::fs::rename(
                segment,
                hidden.path().join(segment.file_name().expect("name")),
            )
            .expect("hide a parent segment");
        }
        drain_latest_text(&successor);
        drop(successor);
        for segment in &parent_segments {
            std::fs::rename(
                hidden.path().join(segment.file_name().expect("name")),
                segment,
            )
            .expect("restore a parent segment");
        }
        let carried = active_artifact(store.path());
        let carried_pages = page_count(&carried);
        assert_eq!(
            carried_pages.cmp(&pages),
            *page_change,
            "round {round}: the edit moved the page count from {pages} to {carried_pages}"
        );
        pages = carried_pages;
        let cold = cold_artifact(&fixture);
        assert_eq!(
            carried.len(),
            cold.len(),
            "round {round}: the carried artifact's size differs from a cold build's"
        );
        assert!(
            carried == cold,
            "round {round}: the carried artifact's bytes differ from a cold build's"
        );
    }
}

/// One commit's written files and removed paths.
type RosterChange<'a> = (Vec<(&'a str, &'a str)>, Vec<&'a str>);

/// Files added and removed shift every later file's ordinal, yet the edit
/// still carries: with every parent segment out of the store, the successor
/// seals byte for byte what a cold build seals through an added file, a
/// removed one, a file replaced by its neighbour beside a body edit, and a
/// rename across the roster.
#[test]
fn an_added_or_removed_file_carries_the_parent_text_artifact_byte_identical_to_a_cold_build() {
    let fixture = corpus_fixture();
    let store = TempDir::new().expect("store");
    let mut scheduler = scheduler(
        &fixture,
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );
    published(scheduler.reconcile_now().expect("publish the parent"));
    drain_text(&scheduler);

    let added = long_module(9, 150);
    let body_edit = module(6, "").replace("wrapping_mul(31)", "wrapping_mul(37)");
    let replacement = module(10, "");
    let renamed = module(1, "").replace("wrapping_mul(31)", "wrapping_mul(43)");
    let rounds: [RosterChange<'_>; 4] = [
        (vec![("src/m03a.rs", added.as_str())], vec![]),
        (vec![], vec!["src/m05.rs"]),
        (
            vec![
                ("src/m03b.rs", replacement.as_str()),
                ("src/m06.rs", body_edit.as_str()),
            ],
            vec!["src/m03a.rs"],
        ),
        (vec![("src/n01.rs", renamed.as_str())], vec!["src/m01.rs"]),
    ];
    let hidden = TempDir::new().expect("hidden segments");
    for (round, (edits, removals)) in rounds.iter().enumerate() {
        let parent_segments = segment_files(store.path());
        for (path, source) in edits {
            fixture.edit(path, source);
        }
        for path in removals {
            fixture.remove(path);
        }
        fixture.commit_all(&format!("roster change {round}"));
        published(scheduler.reconcile_now().expect("publish the successor"));
        let successor = scheduler.latest_complete().expect("successor generation");
        for segment in &parent_segments {
            std::fs::rename(
                segment,
                hidden.path().join(segment.file_name().expect("name")),
            )
            .expect("hide a parent segment");
        }
        drain_latest_text(&successor);
        drop(successor);
        for segment in &parent_segments {
            std::fs::rename(
                hidden.path().join(segment.file_name().expect("name")),
                segment,
            )
            .expect("restore a parent segment");
        }
        let carried = active_artifact(store.path());
        let cold = cold_artifact(&fixture);
        assert_eq!(
            carried.len(),
            cold.len(),
            "round {round}: the carried artifact's size differs from a cold build's"
        );
        assert!(
            carried == cold,
            "round {round}: the carried artifact's bytes differ from a cold build's"
        );
    }
}

/// A parent artifact the carry cannot trust is not carried: the edit's build
/// falls back to a cold build of its own source instead of failing or sealing
/// the parent's damage into the successor.
fn assert_edit_over_damaged_parent_builds_cold(damage: impl FnOnce(&Path)) {
    let fixture = corpus_fixture();
    let store = TempDir::new().expect("store");
    let mut scheduler = scheduler(
        &fixture,
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );
    published(scheduler.reconcile_now().expect("publish the parent"));
    drain_text(&scheduler);
    damage(&active_text_artifact_path(store.path()));

    fixture.edit(
        "src/m04.rs",
        &module(4, "").replace("wrapping_mul(31)", "wrapping_mul(37)"),
    );
    fixture.commit_all("edit over a damaged parent");
    published(scheduler.reconcile_now().expect("publish the successor"));
    drain_text(&scheduler);
    let built = active_artifact(store.path());
    assert!(
        built == cold_artifact(&fixture),
        "the successor's artifact differs from a cold build's"
    );
}

#[test]
fn an_edit_over_a_corrupted_parent_text_artifact_builds_cold() {
    assert_edit_over_damaged_parent_builds_cold(|artifact| {
        let mut bytes = std::fs::read(artifact).expect("read the parent artifact");
        let needle = b"UNTOUCHED_3";
        let at = bytes
            .windows(needle.len())
            .position(|window| window == needle)
            .expect("the parent artifact stores an unchanged file's text");
        bytes[at] ^= 0x20;
        std::fs::write(artifact, bytes).expect("corrupt one parent artifact byte");
    });
}

#[test]
fn an_edit_over_a_missing_parent_text_artifact_builds_cold() {
    assert_edit_over_damaged_parent_builds_cold(|artifact| {
        std::fs::remove_file(artifact).expect("delete the parent artifact");
    });
}
