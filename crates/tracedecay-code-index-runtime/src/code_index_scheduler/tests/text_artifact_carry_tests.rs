use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::Arc,
};

use tempfile::TempDir;

use super::{GitFixture, UNTOUCHED_FILLERS, published, scheduler};
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
    let pointer: serde_json::Value = serde_json::from_slice(
        &std::fs::read(store.join("active-code-generation-v1.json")).expect("read pointer"),
    )
    .expect("parse pointer");
    let active = pointer["generation_id"]
        .as_str()
        .expect("active generation");
    let file = pointer["generation_index"]
        .as_array()
        .expect("generation index")
        .iter()
        .find(|entry| entry["generation_id"] == active)
        .and_then(|entry| entry["text_artifact"]["artifact_file"].as_str())
        .expect("active generation names its text artifact")
        .to_owned();
    std::fs::read(store.join("code-text-artifacts-v1").join(file)).expect("read artifact")
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
/// cold build of the same tree seals, through an edit that grows the edited
/// file (every later document, page, and clone moves forward) and one that
/// shrinks it beside a second edited file (they move back).
#[test]
fn an_edit_carries_the_parent_text_artifact_byte_identical_to_a_cold_build() {
    let files = corpus();
    let file_refs: Vec<(&str, &str)> = files
        .iter()
        .map(|(path, source)| (path.as_str(), source.as_str()))
        .collect();
    let fixture = GitFixture::new(&file_refs);
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
    let edits: [Vec<(&str, &str)>; 2] = [
        vec![("src/m04.rs", grown.as_str())],
        vec![("src/m04.rs", shrunk), ("src/m07.rs", body_edit.as_str())],
    ];
    let hidden = TempDir::new().expect("hidden segments");
    for (round, edit) in edits.iter().enumerate() {
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
