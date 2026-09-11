use super::*;

/// The canonical inline replay must hydrate back into a manifest equal to the
/// one that produced it, on a corpus-shaped fixture with entities, relations,
/// and a dependency.
#[test]
fn inline_replay_hydrates_the_identical_manifest() {
    let identity = projection("decode", "round-trip");
    let mut entities = (0..1_500)
        .map(|index| entity(&format!("entity:{index:04}"), &"m".repeat(96)))
        .collect::<Vec<_>>();
    entities.push(entity("entity:anchor", "anchor"));
    let relation = GraphGenerationRelation::new(
        GraphRelationId::new("relation:anchor").unwrap(),
        GraphEntityRef::new(identity.clone(), GraphEntityId::new("entity:0000").unwrap()),
        GraphEntityRef::new(
            identity.clone(),
            GraphEntityId::new("entity:anchor").unwrap(),
        ),
        GraphRelationKind::new("anchors").unwrap(),
        BTreeMap::new(),
    )
    .unwrap();
    let dependency = GraphGenerationDependency::new(
        projection("decode", "dependency"),
        GraphGenerationId::new("dependency-g1").unwrap(),
        GraphIdempotencyKey::new("publish:dependency-g1").unwrap(),
    );
    let manifest = GraphGenerationManifest::new(
        identity,
        GraphGenerationId::new("round-trip-g1").unwrap(),
        SourceGeneration::new("round-trip-source").unwrap(),
        GraphWatermark::new("round-trip-watermark").unwrap(),
        vec![dependency],
        entities,
        vec![relation],
    )
    .unwrap();
    let replay = manifest
        .relational_replay(
            StoreShardIdV1::project(
                tracedecay_store::BrainId::new("brain.decode").unwrap(),
                tracedecay_store::UserProfileId::new("profile.decode").unwrap(),
                tracedecay_store::ProjectId::new("project.decode").unwrap(),
            ),
            GraphIdempotencyKey::new("publish:round-trip-g1").unwrap(),
            digest('a'),
            None,
            &|| Ok(()),
        )
        .unwrap();
    let hydrated = GraphGenerationManifest::from_inline_replay(&replay, &|| Ok(())).unwrap();
    assert_eq!(hydrated, manifest);
}

#[test]
fn canonical_replay_decode_preserves_mid_stream_cancellation() {
    let identity = projection("decode", "checked");
    let entities = (0..4_000)
        .map(|index| entity(&format!("entity:{index:04}"), &"x".repeat(64)))
        .collect::<Vec<_>>();
    let manifest = GraphGenerationManifest::new(
        identity,
        GraphGenerationId::new("decode-generation").unwrap(),
        SourceGeneration::new("decode-source").unwrap(),
        GraphWatermark::new("decode-watermark").unwrap(),
        vec![],
        entities,
        vec![],
    )
    .unwrap();
    let replay = manifest
        .relational_replay(
            StoreShardIdV1::project(
                tracedecay_store::BrainId::new("brain.decode").unwrap(),
                tracedecay_store::UserProfileId::new("profile.decode").unwrap(),
                tracedecay_store::ProjectId::new("project.decode").unwrap(),
            ),
            GraphIdempotencyKey::new("publish:decode").unwrap(),
            digest('d'),
            None,
            &|| Ok(()),
        )
        .unwrap();
    assert!(replay.canonical_replay_source.len() > 64 * 1024);
    let polls = AtomicUsize::new(0);
    let result = GraphGenerationManifest::from_inline_replay(&replay, &|| {
        if polls.fetch_add(1, Ordering::SeqCst) >= 2 {
            Err(GraphDbError::Cancelled)
        } else {
            Ok(())
        }
    });
    assert_eq!(result, Err(GraphDbError::Cancelled));
}
