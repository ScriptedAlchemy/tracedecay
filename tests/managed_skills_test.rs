use tracedecay::automation::managed_skills::{
    ManagedSkillDraft, ManagedSkillProvenance, ManagedSkillSource, ManagedSkillState,
    ManagedSupportFile,
};

fn draft() -> ManagedSkillDraft {
    ManagedSkillDraft {
        id: "repo-hygiene".to_string(),
        title: "Repository hygiene".to_string(),
        summary: "Keep repository maintenance guidance current.".to_string(),
        category: "maintenance".to_string(),
        body_markdown: "Use focused checks before changing generated files.".to_string(),
        support_files: vec![ManagedSupportFile::new(
            "references/checklist.md",
            b"- check dirty tree\n- run focused tests\n".to_vec(),
        )
        .unwrap()],
        provenance: ManagedSkillProvenance {
            source: ManagedSkillSource::AutomationRun,
            actor: "tracedecay".to_string(),
            run_id: Some("run_123".to_string()),
        },
    }
}

#[test]
fn rejects_unsafe_skill_ids_and_support_paths() {
    for id in [
        "",
        "../escape",
        "bad/name",
        ".hidden",
        "Bad Name",
        "repo..x",
    ] {
        let mut draft = draft();
        draft.id = id.to_string();
        assert!(draft.materialize().is_err(), "accepted unsafe id: {id}");
    }

    for path in [
        "",
        "/tmp/escape.md",
        "../escape.md",
        "a/../../b.md",
        "a\\b.md",
    ] {
        assert!(
            ManagedSupportFile::new(path, b"body".to_vec()).is_err(),
            "accepted unsafe support path: {path}",
        );
    }
}

#[test]
fn validates_minimum_metadata_and_renders_frontmatter() {
    for (field, value) in [
        ("title", ""),
        ("summary", ""),
        ("category", ""),
        ("body_markdown", ""),
    ] {
        let mut draft = draft();
        match field {
            "title" => draft.title = value.to_string(),
            "summary" => draft.summary = value.to_string(),
            "category" => draft.category = value.to_string(),
            "body_markdown" => draft.body_markdown = value.to_string(),
            _ => unreachable!(),
        }
        assert!(draft.materialize().is_err(), "accepted empty {field}");
    }

    let skill = draft().materialize().unwrap();
    let markdown = skill.render_skill_markdown();
    for key in [
        "id: repo-hygiene",
        "title: Repository hygiene",
        "summary: Keep repository maintenance guidance current.",
        "category: maintenance",
        "state: pending_approval",
        "pinned: false",
        "checksum: sha256:",
        "provenance_source: automation_run",
        "provenance_actor: tracedecay",
        "provenance_run_id: run_123",
    ] {
        assert!(markdown.contains(key), "missing frontmatter key {key}");
    }
}

#[test]
fn checksum_is_deterministic_and_tracks_content_not_state_or_pin() {
    let mut first = draft().materialize().unwrap();
    let mut second = draft().materialize().unwrap();
    assert_eq!(first.metadata.checksum, second.metadata.checksum);

    first.set_state(ManagedSkillState::Active);
    first.set_pinned(true);
    assert_eq!(first.metadata.checksum, second.metadata.checksum);

    second.body_markdown.push_str("\nAdd one more rule.");
    second.refresh_checksum();
    assert_ne!(first.metadata.checksum, second.metadata.checksum);
}

#[test]
fn state_and_pin_lifecycle_is_explicit() {
    let mut skill = draft().materialize().unwrap();
    assert_eq!(skill.metadata.state, ManagedSkillState::PendingApproval);
    assert!(!skill.metadata.pinned);

    skill.set_state(ManagedSkillState::Active);
    skill.set_pinned(true);
    assert_eq!(skill.metadata.state, ManagedSkillState::Active);
    assert!(skill.metadata.pinned);

    skill.set_state(ManagedSkillState::Disabled);
    assert_eq!(skill.metadata.state, ManagedSkillState::Disabled);
    assert!(skill.metadata.pinned);
}
