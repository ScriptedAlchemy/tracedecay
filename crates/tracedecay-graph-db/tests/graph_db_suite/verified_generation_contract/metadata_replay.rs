use std::sync::Arc;

use super::*;

#[test]
fn omits_native_rows_and_recovers_only_persisted_native_rows() {
    let temp = TempDir::new().unwrap();
    let registered = RegisteredGraph::new_mounted(temp.path()).unwrap();
    let mut authority = RelationalAuthority::default();
    let identity = projection("metadata", "rows");
    let mut row_manifest = manifest(identity, "rows-g1", "secret-marker", vec![], vec![]);
    row_manifest.entities[0].properties.insert(
        GraphPropertyName::new("embedding").unwrap(),
        GraphProperty::F64(0.125),
    );
    let replay = row_manifest
        .relational_metadata_replay(
            registered.binding.shard_id.clone(),
            GraphIdempotencyKey::new("publish:rows-g1").unwrap(),
            digest('7'),
            None,
            &|| Ok(()),
        )
        .unwrap();
    assert!(
        !replay
            .canonical_replay_source
            .windows("secret-marker".len())
            .any(|window| window == b"secret-marker")
    );
    assert!(
        !replay
            .canonical_replay_source
            .windows("embedding".len())
            .any(|window| window == b"embedding")
    );
    let record = authority.stage(replay);
    let (control, probe) = control_and_probe();
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert!(matches!(
        registered.registry.publish_verified(
            registration(registered.binding.clone(), temp.path()),
            &mut authority,
            &context,
            &record.publication.key,
            None,
        ),
        Err(GraphDbError::Unavailable { .. })
    ));
    assert!(
        authority
            .verified_head(&record.publication.key.projection, &context)
            .unwrap()
            .is_none()
    );

    let mut mismatched_manifest = row_manifest.clone();
    mismatched_manifest.entities[0].properties.insert(
        GraphPropertyName::new("unexpected").unwrap(),
        GraphProperty::String("different-digest".to_owned()),
    );
    assert!(matches!(
        registered.registry.publish_verified(
            registration(registered.binding.clone(), temp.path()),
            &mut authority,
            &context,
            &record.publication.key,
            Some(Arc::new(mismatched_manifest)),
        ),
        Err(GraphDbError::Conflict { .. })
    ));

    let committed = registered
        .registry
        .publish_verified(
            registration(registered.binding.clone(), temp.path()),
            &mut authority,
            &context,
            &record.publication.key,
            Some(Arc::new(row_manifest.clone())),
        )
        .unwrap();
    assert_eq!(
        committed
            .snapshot
            .entity(
                &GraphEntityRef::new(
                    row_manifest.projection.clone(),
                    row_manifest.entities[0].identity.clone(),
                ),
                Arc::new(TestCancellation),
            )
            .unwrap()
            .unwrap()
            .properties
            .get(&GraphPropertyName::new("embedding").unwrap()),
        row_manifest.entities[0]
            .properties
            .get(&GraphPropertyName::new("embedding").unwrap())
    );
    let expected_head = committed.head.clone();
    drop(committed);

    let (replay_control, replay_probe) = control_and_probe();
    let replay_context =
        GraphPublicationOperationContextV1::new(&replay_control, &replay_probe).unwrap();
    let exact_replay = registered
        .registry
        .publish_verified(
            registration(registered.binding.clone(), temp.path()),
            &mut authority,
            &replay_context,
            &record.publication.key,
            Some(Arc::new(row_manifest)),
        )
        .unwrap();
    assert_eq!(exact_replay.head, expected_head);
    drop(exact_replay);
    assert!(registered.close().unwrap());
    drop(registered);

    let registered = RegisteredGraph::new_mounted(temp.path()).unwrap();
    let recovered = registered
        .registry
        .recover_verified_snapshot(
            registration(registered.binding.clone(), temp.path()),
            &mut authority,
            &replay_context,
            &record.publication.key.projection,
        )
        .unwrap();
    assert_eq!(recovered.verified_head(), &expected_head);
}

#[test]
fn incomplete_pending_generation_cannot_advance_verified_head() {
    let temp = TempDir::new().unwrap();
    let registered = RegisteredGraph::new_mounted(temp.path()).unwrap();
    let mut authority = RelationalAuthority::default();
    let identity = projection("metadata", "partial");
    let mut candidate = manifest(identity, "partial-g1", "node-1", vec![], vec![]);
    let mut second = candidate.entities[0].clone();
    second.identity = GraphEntityId::new("node-2").unwrap();
    candidate.entities.push(second);
    let replay = candidate
        .relational_metadata_replay(
            registered.binding.shard_id.clone(),
            GraphIdempotencyKey::new("publish:partial-g1").unwrap(),
            digest('8'),
            None,
            &|| Ok(()),
        )
        .unwrap();
    let record = authority.stage(replay);
    let mut incomplete = candidate;
    incomplete.entities.pop();
    let (control, probe) = control_and_probe();
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();

    assert!(matches!(
        registered.registry.publish_verified(
            registration(registered.binding.clone(), temp.path()),
            &mut authority,
            &context,
            &record.publication.key,
            Some(Arc::new(incomplete)),
        ),
        Err(GraphDbError::Conflict { .. })
    ));
    assert!(
        authority
            .verified_head(&record.publication.key.projection, &context)
            .unwrap()
            .is_none()
    );
}
