use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use tracedecay_domain::IndexPathPolicyV1;
use tracedecay_domain::errors::TraceDecayError;

use super::{SourceWalkDiagnostics, source_walk_with_diagnostics};
use crate::unmounted_files::audit_project;

fn write(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().expect("fixture parent")).unwrap();
    std::fs::write(path, contents).unwrap();
}

#[test]
fn repository_diagnostic_sample_is_bounded_and_order_independent() {
    let expected = (0..8)
        .map(|index| format!("repo-{index:02}"))
        .collect::<BTreeSet<_>>();
    let orders = [
        (0..24).collect::<Vec<_>>(),
        (0..24).rev().collect::<Vec<_>>(),
    ];
    for order in orders {
        let diagnostics = SourceWalkDiagnostics::default();
        assert!(diagnostics.skipped_nested_repositories_note().is_none());
        for index in order {
            diagnostics.record_skipped_nested_repository(format!("repo-{index:02}"));
            let summary = diagnostics.skipped_nested_repositories.lock().unwrap();
            assert!(summary.names.len() <= 8, "collection must be bounded too");
        }
        let summary = diagnostics.skipped_nested_repositories.lock().unwrap();
        assert_eq!(summary.total, 24);
        assert_eq!(summary.names, expected);
        drop(summary);
        let note = diagnostics.skipped_nested_repositories_note().unwrap();
        assert_eq!(
            note,
            "Skipped nested repositories outside parent index: \
             \"repo-00\", \"repo-01\", \"repo-02\", \"repo-03\", \
             \"repo-04\", \"repo-05\", \"repo-06\", \"repo-07\" (and 16 more)"
        );
    }
}

#[test]
fn repository_diagnostics_follow_policy_without_crossing_boundaries() {
    let root = tempfile::tempdir().unwrap();
    write(root.path(), "keep.rs", "pub fn parent() {}\n");
    for directory in ["app", "excluded", "node_modules"] {
        write(
            root.path(),
            &format!("{directory}/.git"),
            "gitdir: foreign\n",
        );
        write(
            root.path(),
            &format!("{directory}/child.rs"),
            "child source\n",
        );
    }
    for (include, expected_note) in [
        (
            vec![],
            "Skipped nested repositories outside parent index: \"app\"",
        ),
        (
            vec!["excluded/child.rs".to_owned()],
            "Skipped nested repositories outside parent index: \"app\", \"excluded\"",
        ),
    ] {
        let policy = IndexPathPolicyV1::new(
            vec!["excluded/**".to_owned(), "node_modules/**".to_owned()],
            include,
        )
        .unwrap();
        let (walk, diagnostics) =
            source_walk_with_diagnostics(root.path(), None, &policy).expect("source walk");
        let files = walk
            .map(|entry| entry.expect("source entry"))
            .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
            .map(|entry| {
                entry
                    .path()
                    .strip_prefix(root.path())
                    .unwrap()
                    .to_path_buf()
            })
            .collect::<Vec<_>>();
        assert_eq!(files, vec![PathBuf::from("keep.rs")]);
        assert_eq!(
            diagnostics.skipped_nested_repositories_note().as_deref(),
            Some(expected_note)
        );
    }
}

#[test]
fn audit_bounds_repository_summary_and_preserves_unsupported_caveat() {
    let root = tempfile::tempdir().unwrap();
    write(root.path(), "main.go", "package main\n");
    for index in (0..12).rev() {
        write(
            root.path(),
            &format!("repo-{index:02}/.git/HEAD"),
            "ref: refs/heads/main\n",
        );
        write(
            root.path(),
            &format!("repo-{index:02}/child.go"),
            "package child\n",
        );
    }
    let policy = IndexPathPolicyV1::new(vec![], vec![]).unwrap();
    let audit = audit_project(root.path(), &policy).expect("audit");
    let go = audit
        .ecosystems
        .iter()
        .find(|ecosystem| ecosystem.ecosystem == "go")
        .expect("parent Go source was counted");
    assert_eq!(go.scanned_file_count, 1);
    assert_eq!(go.status.as_str(), "unsupported");
    assert_eq!(
        go.note.as_deref(),
        Some(
            "1 go source file(s) are present and were not audited, \
             this report cannot say whether any of them is unreachable \
             Skipped nested repositories outside parent index: \
             \"repo-00\", \"repo-01\", \"repo-02\", \"repo-03\", \
             \"repo-04\", \"repo-05\", \"repo-06\", \"repo-07\" (and 4 more)"
        )
    );
}

#[test]
fn repository_diagnostic_paths_are_clipped_on_character_boundaries() {
    let diagnostics = SourceWalkDiagnostics::default();
    diagnostics.record_skipped_nested_repository("é".repeat(161));
    assert_eq!(
        diagnostics.skipped_nested_repositories_note().unwrap(),
        format!(
            "Skipped nested repositories outside parent index: \"{}\"…",
            "é".repeat(160)
        )
    );
}

#[cfg(unix)]
#[test]
fn audit_quotes_control_characters_in_repository_names() {
    let root = tempfile::tempdir().unwrap();
    write(root.path(), "keep.py", "print('parent')\n");
    write(root.path(), "line\nbreak/.git", "gitdir: foreign\n");
    write(root.path(), "line\nbreak/child.py", "print('child')\n");
    let policy = IndexPathPolicyV1::new(vec![], vec![]).unwrap();
    let audit = audit_project(root.path(), &policy).expect("audit");
    let python = audit
        .ecosystems
        .iter()
        .find(|ecosystem| ecosystem.ecosystem == "python")
        .expect("parent Python source was counted");
    assert_eq!(python.scanned_file_count, 1);
    let note = python.note.as_deref().expect("scope diagnostic");
    assert!(note.contains("\"line\\nbreak\""), "{note}");
    assert!(!note.contains('\n'), "a path must not inject report lines");
    assert!(root.path().join("line\nbreak/child.py").is_file());
}

#[test]
fn missing_source_root_is_an_error_not_a_clean_audit() {
    let root = tempfile::tempdir().unwrap();
    let missing = root.path().join("missing");
    let policy = IndexPathPolicyV1::new(vec![], vec![]).unwrap();
    match audit_project(&missing, &policy) {
        Err(TraceDecayError::File { path, message }) => {
            assert_eq!(path, missing.display().to_string());
            assert!(message.contains("source walk did not complete"));
        }
        Err(error) => panic!("unexpected error: {error}"),
        Ok(_) => panic!("a failed walk must not report an empty successful audit"),
    }
}

#[test]
fn incomplete_git_root_is_refused_before_walking_sources() {
    let root = tempfile::tempdir().unwrap();
    write(root.path(), "src/lib.rs", "pub fn source() {}");
    let policy = IndexPathPolicyV1::new(vec![], vec![]).unwrap();
    assert!(audit_project(root.path(), &policy).is_ok());

    std::fs::create_dir(root.path().join(".git")).unwrap();
    assert!(matches!(
        audit_project(root.path(), &policy),
        Err(TraceDecayError::Config { message }) if message.contains("parent Git boundaries")
    ));
}
