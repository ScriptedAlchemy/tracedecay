use super::*;

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
