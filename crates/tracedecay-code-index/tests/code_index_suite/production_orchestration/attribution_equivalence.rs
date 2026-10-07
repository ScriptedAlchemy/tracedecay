use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt::Write;

use tracedecay_code_index::intake::ValidatedCodeSnapshotV1;
use tracedecay_code_index::production::CodeIndexProductionOwnerV1;
use tracedecay_code_index::provider::GenerationTestAttributionJoinReadPort;
use tracedecay_code_index::test_attribution::{GenerationTestJoinV1, TestAttributionOccurrenceV1};
use tracedecay_domain::RelationEdgeKindV1;

use super::{
    ActiveControl, ApplyingProjectionSink, SharedPublicationStore, config, request_with_source,
};

#[test]
fn indexed_test_traversal_preserves_complete_authority_and_digest() {
    // Many tests share a cyclic component with overlapping paths. A separate
    // self-cycle and isolated test exercise closures with no shared neighbors.
    let mut source = String::new();
    for node in 0..32 {
        writeln!(
            source,
            "fn node_{node}() {{ node_{}(); node_{}(); node_{}(); }}",
            (node + 1) % 32,
            (node + 2) % 32,
            (node + 3) % 32,
        )
        .unwrap();
    }
    for test in 0..64 {
        writeln!(source, "#[test] fn shared_{test}() {{ node_0(); }}").unwrap();
    }
    source.push_str("#[test] fn self_cycle() { self_cycle(); }\n#[test] fn isolated() {}\n");
    let store = SharedPublicationStore::default();
    let mut owner =
        CodeIndexProductionOwnerV1::new(config(), store.clone(), ApplyingProjectionSink).unwrap();
    let published = owner
        .build_and_publish(
            request_with_source(
                "file.attribution.equivalence",
                1_100_000,
                "commit.attribution.equivalence",
                "tree.attribution.equivalence",
                &source,
            ),
            &ActiveControl,
        )
        .unwrap();
    let generation = store.generation(&published);
    let authority = generation.test_attribution_authority().unwrap();
    let actual = authority.read_test_attribution(&generation.manifest().generation_id);
    let actual_join = actual.evidence.as_ref().unwrap();
    assert_eq!(actual_join.records.len(), 66);
    assert!(
        generation
            .edges()
            .iter()
            .any(|edge| edge.kind == RelationEdgeKindV1::Calls)
    );
    assert!(
        generation
            .edges()
            .iter()
            .any(|edge| edge.kind == RelationEdgeKindV1::Annotates)
    );

    let files = generation
        .snapshot()
        .files
        .iter()
        .map(|file| (&file.file_occurrence_id, &file.content_digest))
        .collect::<BTreeMap<_, _>>();
    let mut occurrences = BTreeMap::new();
    for chunk in generation.chunks().chunks() {
        if let Some(occurrence) = &chunk.anchor.symbol_occurrence_id {
            occurrences.insert(
                occurrence.clone(),
                TestAttributionOccurrenceV1 {
                    occurrence_id: occurrence.clone(),
                    file_occurrence_id: chunk.anchor.file_occurrence_id.clone(),
                    content_digest: (*files[&chunk.anchor.file_occurrence_id]).clone(),
                },
            );
        }
    }
    let mut outgoing = BTreeMap::<_, Vec<_>>::new();
    for edge in generation.edges() {
        if occurrences.contains_key(&edge.from_occurrence)
            && occurrences.contains_key(&edge.to_occurrence)
        {
            outgoing
                .entry(edge.from_occurrence.clone())
                .or_default()
                .push(edge.to_occurrence.clone());
        }
    }
    for destinations in outgoing.values_mut() {
        destinations.sort();
        destinations.dedup();
    }
    let mut reference = Vec::new();
    for record in &actual_join.records {
        // The prior production traversal, deliberately using identity-keyed
        // sets and queues as an independent reference for the indexed walk.
        let mut attribution = record.attribution.clone();
        let mut covered = BTreeSet::from([attribution.test_occurrence.clone()]);
        let mut pending = VecDeque::from([attribution.test_occurrence.clone()]);
        while let Some(occurrence) = pending.pop_front() {
            for destination in outgoing.get(&occurrence).into_iter().flatten() {
                if covered.insert(destination.clone()) {
                    pending.push_back(destination.clone());
                }
            }
        }
        attribution.covered_occurrences = covered.into_iter().collect();
        reference.push(attribution);
    }
    let occurrences = occurrences.into_values().collect::<Vec<_>>();
    let mut watermark = actual_join.test_watermark.clone();
    watermark.evidence_digest = watermark
        .recompute_evidence_digest(&reference, &occurrences)
        .unwrap();
    let reference_join = GenerationTestJoinV1::join(
        generation.manifest(),
        &ValidatedCodeSnapshotV1 {
            snapshot: generation.snapshot().clone(),
            intake_digest: generation.manifest().snapshot_digest.clone(),
            validated_at: generation.manifest().seal.sealed_at,
        },
        &reference,
        &occurrences,
        &watermark,
    )
    .unwrap();
    let mut expected = (*actual).clone();
    expected.evidence = Some(reference_join);
    assert_eq!(
        serde_json::to_vec(&*actual).unwrap(),
        serde_json::to_vec(&expected).unwrap()
    );
    assert_eq!(
        actual_join.test_watermark.evidence_digest,
        watermark.evidence_digest
    );
}
