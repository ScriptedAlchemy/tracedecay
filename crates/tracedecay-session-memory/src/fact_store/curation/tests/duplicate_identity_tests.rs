use super::*;

#[tokio::test]
async fn duplicate_normalization_identity_is_rejected_before_any_write() {
    let fixture = Fixture::new("duplicate-normalization-identity").await;
    let target = fixture.seed("duplicate-normalization-target", 10).await;
    let evidence = fixture.seed("duplicate-normalization-evidence", 20).await;
    let operation_id = provenance_id("fixture.duplicate-normalization-identity");
    let target_ref = reviewed_ref(&fixture.db, &fixture.owner, &target).await;
    let evidence_ref = reviewed_ref(&fixture.db, &fixture.owner, &evidence).await;
    let normalize = |tags: Vec<String>| {
        ProjectMemoryFactCurationOperationV1::NormalizeTags(
            ProjectMemoryFactNormalizeTagsV1::new(
                target_ref.clone(),
                tags,
                vec![evidence_ref.clone()],
                Confidence::new(0.9).unwrap(),
            )
            .unwrap(),
        )
    };
    let before = lineage_events_for_fact(&fixture.db, &fixture.owner, &target).await;

    assert!(matches!(
        ProjectMemoryFactCurationBatchV1::new(
            fixture.owner.clone(),
            operation_id.clone(),
            None,
            Confidence::new(0.5).unwrap(),
            vec![
                normalize(vec!["canonical".to_owned()]),
                normalize(vec!["conflicting".to_owned()]),
            ],
        ),
        Err(FactStoreError::Contract(DomainError::DuplicateId {
            field: "curation operation identity",
        }))
    ));
    assert_eq!(
        lineage_events_for_fact(&fixture.db, &fixture.owner, &target).await,
        before
    );
    assert!(
        operation_receipts(&fixture.db, &fixture.owner, &operation_id)
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn duplicate_link_identity_is_rejected_before_any_write() {
    let fixture = Fixture::new("duplicate-link-identity").await;
    let source = fixture.seed("duplicate-link-source", 10).await;
    let target = fixture.seed("duplicate-link-target", 20).await;
    let evidence = fixture.seed("duplicate-link-evidence", 30).await;
    let operation_id = provenance_id("fixture.duplicate-link-identity");
    let relation = relation(
        &fixture.owner,
        &source,
        &target,
        vec![evidence],
        FactRelationKindV1::Supports,
    );
    let source_ref = reviewed_ref(&fixture.db, &fixture.owner, &source).await;
    let target_ref = reviewed_ref(&fixture.db, &fixture.owner, &target).await;
    let evidence_ref = reviewed_ref(
        &fixture.db,
        &fixture.owner,
        relation.evidence_fact_ids().first().unwrap(),
    )
    .await;

    assert!(matches!(
        ProjectMemoryFactCurationBatchV1::new(
            fixture.owner.clone(),
            operation_id.clone(),
            None,
            Confidence::new(0.5).unwrap(),
            vec![
                ProjectMemoryFactCurationOperationV1::LinkFacts(
                    ProjectMemoryFactLinkV1::new(
                        relation.clone(),
                        source_ref.clone(),
                        target_ref.clone(),
                        vec![evidence_ref.clone()]
                    )
                    .unwrap(),
                ),
                ProjectMemoryFactCurationOperationV1::LinkFacts(
                    ProjectMemoryFactLinkV1::new(
                        relation,
                        source_ref,
                        target_ref,
                        vec![evidence_ref]
                    )
                    .unwrap(),
                ),
            ],
        ),
        Err(FactStoreError::Contract(DomainError::DuplicateId {
            field: "curation operation identity",
        }))
    ));
    assert!(linked_events(&fixture.db, &fixture.owner).await.is_empty());
    assert!(
        operation_receipts(&fixture.db, &fixture.owner, &operation_id)
            .await
            .is_empty()
    );
}
