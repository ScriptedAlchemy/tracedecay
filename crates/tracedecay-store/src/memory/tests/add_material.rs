use serde_json::json;
use tracedecay_domain::{ActorId, Confidence, DomainError, FactCategoryV1, FactOwnerV1};

use super::{FactStoreError, ProjectMemoryFactAddMaterialV1, id, receipt_for};

#[test]
fn one_authority_binds_regular_and_automatic_input_digests() {
    let owner = FactOwnerV1::Profile;
    let payload = json!({
        "content": "one canonical add material",
        "category": "project",
        "tags": ["canonical"],
        "entities": ["TraceDecay"],
        "metadata": {"fixture": "add-material"},
    });
    let build = |automation_run_id: Option<String>, trust: f64, actor: Option<&str>| {
        ProjectMemoryFactAddMaterialV1::new(
            owner.clone(),
            "one canonical add material".to_owned(),
            FactCategoryV1::Project,
            None,
            vec!["canonical".to_owned()],
            vec!["TraceDecay".to_owned()],
            json!({
                "fixture": "add-material",
                "automation_run_id": "must-not-enter-payload-metadata",
            }),
            receipt_for(&payload),
            automation_run_id,
            Confidence::new(trust).unwrap(),
            actor.map(id::<ActorId>),
        )
        .unwrap()
    };
    let regular = build(None, 0.5, None);
    assert_eq!(regular.metadata(), &payload["metadata"]);
    let regular_digest = regular.input_digest().to_owned();
    let expected_material = json!({
        "owner": &owner,
        "content": "one canonical add material",
        "category": FactCategoryV1::Project,
        "tags": ["canonical"],
        "entities": ["TraceDecay"],
        "metadata": {"fixture": "add-material"},
        "sanitization_receipt": receipt_for(&payload),
        "automation_run_id": Option::<String>::None,
        "default_trust": 0.5,
        "actor": Option::<String>::None,
    });
    let expected = tracedecay_domain::canonical_sha256(&(
        "tracedecay.project-memory.fact-add-input.v1",
        expected_material,
    ))
    .unwrap();
    assert_eq!(
        regular_digest,
        expected.as_str().trim_start_matches("sha256:")
    );
    let command_a = regular
        .clone()
        .into_command(id("operation.add-material.a"))
        .unwrap();
    let command_b = regular
        .clone()
        .into_command(id("operation.add-material.b"))
        .unwrap();
    assert_eq!(command_a.input_digest(), regular_digest);
    assert_eq!(command_b.input_digest(), regular_digest);

    let automatic_direct = build(Some("run.add-material".to_owned()), 0.5, None);
    let automatic_built = regular
        .clone()
        .with_automation_run_id("run.add-material".to_owned())
        .unwrap();
    assert_eq!(
        automatic_direct.input_digest(),
        automatic_built.input_digest()
    );
    assert_ne!(regular.input_digest(), automatic_direct.input_digest());
    assert_ne!(
        regular.input_digest(),
        build(None, 0.75, None).input_digest()
    );
    assert_ne!(
        regular.input_digest(),
        build(None, 0.5, Some("actor.add-material")).input_digest()
    );
}

#[test]
fn labels_are_canonicalized_and_invalid_payloads_fail_before_digesting() {
    let owner = FactOwnerV1::Profile;
    let payload = json!({
        "content": "canonical label ordering",
        "category": "project",
        "tags": ["alpha", "beta"],
        "entities": ["TraceDecay", "Workspace"],
        "metadata": {},
    });
    let build = |tags: Vec<String>, entities: Vec<String>| {
        ProjectMemoryFactAddMaterialV1::new(
            owner.clone(),
            "canonical label ordering".to_owned(),
            FactCategoryV1::Project,
            None,
            tags,
            entities,
            json!({}),
            receipt_for(&payload),
            None,
            Confidence::new(0.5).unwrap(),
            None,
        )
    };
    let canonical = build(
        vec!["alpha".to_owned(), "beta".to_owned()],
        vec!["TraceDecay".to_owned(), "Workspace".to_owned()],
    )
    .unwrap();
    let permuted = build(
        vec!["beta".to_owned(), "alpha".to_owned()],
        vec!["Workspace".to_owned(), "TraceDecay".to_owned()],
    )
    .unwrap();
    assert_eq!(canonical.tags(), permuted.tags());
    assert_eq!(canonical.entities(), permuted.entities());
    assert_eq!(canonical.input_digest(), permuted.input_digest());

    let duplicate_payload = json!({
        "content": "canonical label ordering",
        "category": "project",
        "tags": ["alpha", "alpha"],
        "entities": ["TraceDecay"],
        "metadata": {},
    });
    assert!(matches!(
        ProjectMemoryFactAddMaterialV1::new(
            owner.clone(),
            "canonical label ordering".to_owned(),
            FactCategoryV1::Project,
            None,
            vec!["alpha".to_owned(), "alpha".to_owned()],
            vec!["TraceDecay".to_owned()],
            json!({}),
            receipt_for(&duplicate_payload),
            None,
            Confidence::new(0.5).unwrap(),
            None,
        ),
        Err(FactStoreError::Contract(DomainError::DuplicateId { .. }))
    ));

    let oversized_content = "x".repeat(64 * 1024 + 1);
    let oversized_payload = json!({
        "content": &oversized_content,
        "category": "project",
        "tags": [],
        "entities": [],
        "metadata": {},
    });
    assert!(matches!(
        ProjectMemoryFactAddMaterialV1::new(
            owner,
            oversized_content,
            FactCategoryV1::Project,
            None,
            vec![],
            vec![],
            json!({}),
            receipt_for(&oversized_payload),
            None,
            Confidence::new(0.5).unwrap(),
            None,
        ),
        Err(FactStoreError::Contract(DomainError::NonCanonical { .. }))
    ));
}
