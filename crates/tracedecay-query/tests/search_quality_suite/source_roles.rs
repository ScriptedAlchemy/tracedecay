use std::borrow::Cow;
use std::collections::BTreeSet;

use tracedecay_domain::RetrievalSourceRoleV1;
use tracedecay_query::retrieval::lexical::{LexicalFieldFilterV1, LexicalFieldV1, LexicalLane};

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

#[test]
fn artifact_definition_priority_covers_short_aliases_of_the_own_symbol() {
    let fixture = real_lexical_source_fixture_from_sources(vec![
        (
            "file.definition".to_owned(),
            "src/coverage.rs".to_owned(),
            b"/// Signed cache-grant state.\npub struct VerifiedCacheGrantSnapshotV1 {\n    pub grant_digest: u64,\n}\n".to_vec(),
        ),
        (
            "file.field".to_owned(),
            "src/remote.rs".to_owned(),
            b"pub struct Remote {\n    pub grant_digest: u64,\n}\n".to_vec(),
        ),
    ]);
    let artifact = sealed_artifact(&fixture, fixture.metadata.clone());
    let lane = LexicalLane::new(artifact.reader.clone());
    let request = artifact.request("cache grant", &["cache", "grant"], &[], &[], 0, 64);
    let result = complete(
        lane.retrieve_lexical(&request)
            .expect("indexed lexical query"),
    );
    let definition = result
        .candidates
        .iter()
        .find(|candidate| {
            candidate
                .file_occurrence_id
                .as_ref()
                .is_some_and(|file| file.as_str() == "file.definition")
        })
        .expect("own-symbol alias hit");
    assert_eq!(
        result.evidence_by_occurrence[&definition.source_occurrence_id].source_role,
        RetrievalSourceRoleV1::ProductionDefinition,
        "every alias term is a subtoken of VerifiedCacheGrantSnapshotV1"
    );
    if let Some(field) = result.candidates.iter().find(|candidate| {
        candidate
            .file_occurrence_id
            .as_ref()
            .is_some_and(|file| file.as_str() == "file.field")
    }) {
        assert_eq!(
            result.evidence_by_occurrence[&field.source_occurrence_id].source_role,
            RetrievalSourceRoleV1::ProductionOther,
            "grant_digest is not named by cache+grant"
        );
    }
    assert_eq!(
        result.candidates[0]
            .file_occurrence_id
            .as_ref()
            .unwrap()
            .as_str(),
        "file.definition"
    );
}

#[test]
fn name_filtered_routes_keep_undocumented_symbol_hits() {
    let fixture = real_lexical_source_fixture_from_sources(vec![(
        "file.definition".to_owned(),
        "src/lookup.rs".to_owned(),
        b"pub fn getUserById(id: u64) -> u64 { id }\n".to_vec(),
    )]);
    let artifact = sealed_artifact(&fixture, fixture.metadata.clone());
    let lane = LexicalLane::new(artifact.reader.clone());
    // A preferred-symbol route admits only SymbolName and can carry several
    // whole terms; a phrase-only read can whitelist the same field. Both must
    // keep their name score when the row carries no rustdoc.
    let mut terms_request = artifact.request(
        "getUserById grant",
        &["getUserById", "grant"],
        &[],
        &[],
        0,
        64,
    );
    terms_request.field_filters = Cow::Owned(vec![LexicalFieldFilterV1 {
        field: LexicalFieldV1::SymbolName,
        include: true,
    }]);
    let mut phrase_request = artifact.request("getUserById", &[], &[], &["getUserById"], 0, 64);
    phrase_request.field_filters = Cow::Owned(vec![LexicalFieldFilterV1 {
        field: LexicalFieldV1::SymbolName,
        include: true,
    }]);
    for (label, request) in [
        ("multi-term preferred-symbol", terms_request),
        ("phrase-only", phrase_request),
    ] {
        let result = complete(
            lane.retrieve_lexical(&request)
                .expect("indexed lexical query"),
        );
        let files: BTreeSet<&str> = result
            .candidates
            .iter()
            .map(|candidate| {
                candidate
                    .file_occurrence_id
                    .as_ref()
                    .expect("candidate file")
                    .as_str()
            })
            .collect();
        assert_eq!(
            files,
            BTreeSet::from(["file.definition"]),
            "{label}: the name-whitelisted route keeps its undocumented name hit"
        );
    }
}

#[test]
fn json_object_keys_remain_searchable_but_do_not_outrank_code_definitions() {
    // This fixture travels through the real JSON/Rust extractors, sealed
    // lexical artifact, bounded winner heap and emitted source-role evidence.
    // A JSON pair is extracted as a Const, but it is configuration data, not
    // an executable definition of the same-named Rust function.
    let fixture = real_lexical_source_fixture_from_sources(vec![
        (
            "file.json".to_owned(),
            "config/render.json".to_owned(),
            br#"{"render": true, "other": false}"#.to_vec(),
        ),
        (
            "file.code".to_owned(),
            "src/render.rs".to_owned(),
            b"pub fn render() -> bool { true }\n".to_vec(),
        ),
        (
            "file.test".to_owned(),
            "tests/render.rs".to_owned(),
            b"#[test] fn render() { assert!(true); }\n".to_vec(),
        ),
    ]);
    let artifact = sealed_artifact(&fixture, fixture.metadata.clone());
    let lane = LexicalLane::new(artifact.reader.clone());
    let request = artifact.request("render", &["render"], &[], &[], 0, 32);
    let results = complete(
        lane.retrieve_lexical(&request)
            .expect("sealed lexical search"),
    );

    let mut json_seen = false;
    let mut code_definition_seen = false;
    for candidate in &results.candidates {
        let file = candidate
            .file_occurrence_id
            .as_ref()
            .expect("file occurrence")
            .as_str();
        let role = results.evidence_by_occurrence[&candidate.source_occurrence_id].source_role;
        match file {
            "file.json" => {
                json_seen = true;
                assert_ne!(
                    role,
                    RetrievalSourceRoleV1::ProductionDefinition,
                    "JSON object keys must not take the code-definition tier"
                );
            }
            "file.code" => {
                code_definition_seen |= role == RetrievalSourceRoleV1::ProductionDefinition;
            }
            "file.test" => assert_eq!(role, RetrievalSourceRoleV1::TestReference),
            other => panic!("unexpected fixture file {other}"),
        }
    }
    assert!(json_seen, "the JSON key must remain retrievable");
    assert!(code_definition_seen, "the Rust function must remain a definition");

    let first = results.candidates.first().expect("named code candidate");
    assert_eq!(
        first.file_occurrence_id.as_ref().unwrap().as_str(),
        "file.code",
        "a production declaration outranks a same-named JSON object key"
    );
    let capped = artifact.request("render", &["render"], &[], &[], 0, 1);
    let capped_results = complete(
        lane.retrieve_lexical(&capped)
            .expect("bounded lexical search"),
    );
    assert_eq!(capped_results.candidates.len(), 1);
    assert_eq!(
        capped_results.candidates[0]
            .file_occurrence_id
            .as_ref()
            .unwrap()
            .as_str(),
        "file.code",
        "capped admission must apply the same priority as emitted evidence"
    );

    // A user explicitly searching configuration still finds its actual keys.
    let mut scoped = artifact.request("render", &["render"], &[], &[], 0, 32);
    scoped.path_prefix = Some("config");
    let scoped_results = complete(
        lane.retrieve_lexical(&scoped)
            .expect("configuration-scoped search"),
    );
    assert!(
        !scoped_results.candidates.is_empty(),
        "lower priority is not a filter or an opt-out"
    );
    assert!(scoped_results.candidates.iter().all(|candidate| {
        candidate
            .file_occurrence_id
            .as_ref()
            .is_some_and(|file| file.as_str() == "file.json")
    }));
}
