use std::collections::BTreeSet;

use tracedecay_domain::RetrievalSourceRoleV1;
use tracedecay_query::retrieval::lexical::{LexicalLane, LexicalLaneRetriever};

use super::candidate_producers::{
    complete, real_lexical_source_fixture_from_sources, sealed_artifact,
};

#[test]
fn artifact_definition_priority_names_the_symbol_instead_of_its_signature_types() {
    let fixture = real_lexical_source_fixture_from_sources(vec![
        (
            "file.definition".to_owned(),
            "src/widget.rs".to_owned(),
            b"pub struct Widget;\n".to_vec(),
        ),
        (
            "file.reference".to_owned(),
            "src/wrap.rs".to_owned(),
            b"pub fn wrap(value: Widget) -> Widget { value }\n".to_vec(),
        ),
        (
            "file.test".to_owned(),
            "tests/widget.rs".to_owned(),
            b"pub fn widget_reference(value: Widget) -> Widget { value }\n".to_vec(),
        ),
    ]);
    let artifact = sealed_artifact(&fixture, fixture.metadata.clone());
    let lane = LexicalLane::new(artifact.reader.clone());
    for (query, terms, fuzzy_budget) in [
        ("Widget", vec!["Widget"], 0),
        ("Widger", vec!["Widger"], 8),
        ("Widget value", vec!["Widget", "value"], 0),
    ] {
        let request = artifact.request(query, &terms, &[], &[], fuzzy_budget, 64);
        let result = complete(
            lane.retrieve_lexical(&request)
                .expect("indexed lexical query"),
        );
        let mut files = BTreeSet::new();
        for candidate in &result.candidates {
            let file = candidate
                .file_occurrence_id
                .as_ref()
                .expect("candidate file")
                .as_str();
            files.insert(file);
            let evidence = &result.evidence_by_occurrence[&candidate.source_occurrence_id];
            match file {
                "file.definition" => assert_eq!(
                    evidence.source_role,
                    RetrievalSourceRoleV1::ProductionDefinition,
                    "{query}"
                ),
                "file.reference" => assert_eq!(
                    evidence.source_role,
                    RetrievalSourceRoleV1::ProductionOther,
                    "{query}: a parameter/return type is a reference"
                ),
                "file.test" => assert_eq!(
                    evidence.source_role,
                    RetrievalSourceRoleV1::TestReference,
                    "{query}"
                ),
                _ => panic!("unexpected file {file}"),
            }
        }
        assert_eq!(
            files,
            BTreeSet::from(["file.definition", "file.reference", "file.test"]),
            "{query}"
        );
        assert_eq!(
            result.candidates[0]
                .file_occurrence_id
                .as_ref()
                .unwrap()
                .as_str(),
            "file.definition",
            "{query}: the definition leads stronger references"
        );
        let capped = artifact.request(query, &terms, &[], &[], fuzzy_budget, 1);
        let capped = complete(
            lane.retrieve_lexical(&capped)
                .expect("capped indexed lexical query"),
        );
        assert_eq!(capped.candidates.len(), 1);
        assert_eq!(
            capped.candidates[0]
                .file_occurrence_id
                .as_ref()
                .unwrap()
                .as_str(),
            "file.definition",
            "{query}: admission and emission use the same definition evidence"
        );
    }
}
