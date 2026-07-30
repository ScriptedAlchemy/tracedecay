use tracedecay_domain::{
    CollectionRevision, ManifestDigest, RootGenerationV1, ScopeOutcome, ScopePartialReasonV1,
    ScopeSetId, ScopeSetRevision, ScopeUnavailableReasonV1, StackRevision,
};

fn digest(byte: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
}

#[test]
fn scope_set_and_root_revision_identities_are_typed_and_nonzero() {
    assert!(ScopeSetId::new("scope-set.fixture").is_ok());
    assert!(ScopeSetId::new(" scope-set.fixture").is_err());
    assert!(ScopeSetRevision::new(0).is_err());
    assert_eq!(ScopeSetRevision::new(2).unwrap().get(), 2);

    let generation = RootGenerationV1::new(
        digest('a'),
        CollectionRevision::new(digest('b')).unwrap(),
        StackRevision::new(digest('c')).unwrap(),
    )
    .unwrap();
    generation.validate().unwrap();

    let changed_stack = RootGenerationV1::new(
        digest('a'),
        CollectionRevision::new(digest('b')).unwrap(),
        StackRevision::new(digest('d')).unwrap(),
    )
    .unwrap();
    assert_ne!(
        generation.generation_digest,
        changed_stack.generation_digest
    );

    let mut tampered = serde_json::to_value(generation).unwrap();
    tampered["generation_digest"] = serde_json::json!(digest('f'));
    assert!(serde_json::from_value::<RootGenerationV1>(tampered).is_err());
}

#[test]
fn per_root_outcomes_preserve_partial_denied_and_unavailable_truth() {
    let outcomes = [
        ScopeOutcome::Exact(vec!["zero"]),
        ScopeOutcome::Partial {
            value: vec!["one"],
            reason: ScopePartialReasonV1::Incomplete,
        },
        ScopeOutcome::<Vec<&str>>::Denied,
        ScopeOutcome::Unavailable {
            reason: ScopeUnavailableReasonV1::StoreUnavailable,
        },
    ];

    let encoded = serde_json::to_value(&outcomes).unwrap();
    assert_eq!(encoded[0]["outcome"], "exact");
    assert_eq!(encoded[1]["outcome"], "partial");
    assert_eq!(encoded[2]["outcome"], "denied");
    assert_eq!(encoded[3]["outcome"], "unavailable");

    let decoded: [ScopeOutcome<Vec<String>>; 4] = serde_json::from_value(encoded).unwrap();
    assert!(matches!(decoded[0], ScopeOutcome::Exact(_)));
    assert!(matches!(decoded[1], ScopeOutcome::Partial { .. }));
    assert!(matches!(decoded[2], ScopeOutcome::Denied));
    assert!(matches!(
        decoded[3],
        ScopeOutcome::Unavailable {
            reason: ScopeUnavailableReasonV1::StoreUnavailable
        }
    ));
}
