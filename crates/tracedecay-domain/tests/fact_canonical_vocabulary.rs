use serde_json::json;
use tracedecay_domain::{FactCanonicalVocabularyV1, FactVocabularyProjectionV1};

fn vocabulary_json() -> serde_json::Value {
    json!({
        "revision": "component.memory-vocabulary.fixture.v1",
        "provenance": {
            "kind": "maintained",
            "provenance_id": "provenance.memory-vocabulary.fixture"
        },
        "concepts": [
            {
                "canonical": "missing_credential",
                "aliases": ["Missing secret", "credential absent"]
            },
            {
                "canonical": "deployment_failure",
                "aliases": ["Rollout broke", "release failed"]
            }
        ]
    })
}

#[test]
fn vocabulary_normalizes_aliases_and_projects_paraphrases() {
    let vocabulary: FactCanonicalVocabularyV1 =
        serde_json::from_value(vocabulary_json()).expect("valid vocabulary");

    let projection = vocabulary
        .project("The production rollout broke due to a missing secret.")
        .expect("project canonical concepts");

    assert_eq!(
        projection.concepts(),
        &[
            "deployment_failure".to_owned(),
            "missing_credential".to_owned()
        ]
    );
}

#[test]
fn vocabulary_refuses_ambiguous_or_noncanonical_concepts() {
    let ambiguous = json!({
        "revision": "component.memory-vocabulary.fixture.v1",
        "provenance": {
            "kind": "maintained",
            "provenance_id": "provenance.memory-vocabulary.fixture"
        },
        "concepts": [
            {"canonical": "deployment_failure", "aliases": ["release failed"]},
            {"canonical": "release_failure", "aliases": ["Release failed"]}
        ]
    });
    assert!(
        serde_json::from_value::<FactCanonicalVocabularyV1>(ambiguous)
            .expect_err("ambiguous alias must be refused")
            .to_string()
            .contains("fact canonical vocabulary aliases")
    );

    let mut invalid_concept = vocabulary_json();
    invalid_concept["concepts"][0]["canonical"] = json!("Missing Credential");
    assert!(
        serde_json::from_value::<FactCanonicalVocabularyV1>(invalid_concept)
            .expect_err("noncanonical concept must be refused")
            .to_string()
            .contains("fact canonical vocabulary concept")
    );
}

#[test]
fn vocabulary_and_projection_have_canonical_wire_identity() {
    let vocabulary: FactCanonicalVocabularyV1 =
        serde_json::from_value(vocabulary_json()).expect("valid vocabulary");
    assert_eq!(
        serde_json::to_value(&vocabulary).expect("serialize vocabulary"),
        json!({
            "revision": "component.memory-vocabulary.fixture.v1",
            "provenance": {
                "kind": "maintained",
                "provenance_id": "provenance.memory-vocabulary.fixture"
            },
            "concepts": [
                {
                    "canonical": "deployment_failure",
                    "aliases": ["release failed", "rollout broke"]
                },
                {
                    "canonical": "missing_credential",
                    "aliases": ["credential absent", "missing secret"]
                }
            ]
        })
    );

    let projection = vocabulary
        .project("The release failed because the credential was absent.")
        .expect("project canonical concepts");
    let wire = serde_json::to_value(&projection).expect("serialize projection");
    assert_eq!(
        wire,
        json!({
            "vocabulary_revision": "component.memory-vocabulary.fixture.v1",
            "provenance": {
                "kind": "maintained",
                "provenance_id": "provenance.memory-vocabulary.fixture"
            },
            "concepts": ["deployment_failure", "missing_credential"]
        })
    );
    assert_eq!(
        serde_json::from_value::<FactVocabularyProjectionV1>(wire)
            .expect("deserialize canonical projection"),
        projection
    );
}
