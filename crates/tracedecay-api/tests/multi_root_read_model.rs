use schemars::{JsonSchema, schema_for};
use serde_json::Value;
use tracedecay_api::read_model::multi_root::MultiRootQueryReadModelV1;
use tracedecay_application::{MultiRootContinuationV1, MultiRootQueryPageV1};
use tracedecay_domain::{
    CollectionRevision, ManifestDigest, RootGenerationV1, RootScopeOutcomeV1, ScopeOutcome,
    ScopePartialReasonV1, ScopeSetId, ScopeSetRevision, ScopeUnavailableReasonV1, StackRevision,
};

fn digest(byte: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
}

#[test]
fn dashboard_read_model_preserves_per_root_partial_truth() {
    let generation = RootGenerationV1::new(
        digest('a'),
        CollectionRevision::new(digest('b')).unwrap(),
        StackRevision::new(digest('c')).unwrap(),
    )
    .unwrap();
    let generations = vec![
        RootScopeOutcomeV1::new(digest('a'), ScopeOutcome::Exact(generation)).unwrap(),
        RootScopeOutcomeV1::new(
            digest('d'),
            ScopeOutcome::Unavailable {
                reason: ScopeUnavailableReasonV1::StoreUnavailable,
            },
        )
        .unwrap(),
    ];
    let continuation =
        MultiRootContinuationV1::new(digest('e'), generations, digest('f'), digest('1'), 1)
            .unwrap();
    let page = MultiRootQueryPageV1 {
        scope_set_id: ScopeSetId::new("scope-set.dashboard").unwrap(),
        scope_set_revision: ScopeSetRevision::new(1).unwrap(),
        scope_set_digest: digest('e'),
        roots: vec![
            RootScopeOutcomeV1::new(digest('a'), ScopeOutcome::Exact(vec!["result".to_owned()]))
                .unwrap(),
            RootScopeOutcomeV1::new(
                digest('d'),
                ScopeOutcome::Unavailable {
                    reason: ScopeUnavailableReasonV1::StoreUnavailable,
                },
            )
            .unwrap(),
        ],
        aggregate: ScopeOutcome::Partial {
            value: vec!["result".to_owned()],
            reason: ScopePartialReasonV1::RootUnavailable,
        },
        continuation,
    };

    let wire = serde_json::to_value(MultiRootQueryReadModelV1::from(page)).unwrap();
    assert_eq!(wire["aggregate"]["outcome"], "partial");
    assert_eq!(wire["roots"][0]["outcome"]["outcome"], "exact");
    assert_eq!(wire["roots"][1]["outcome"]["outcome"], "unavailable");
    assert_eq!(wire["continuation"]["next_page"], 1);
}

#[derive(JsonSchema)]
struct DashboardPayloadMarkerV1;

#[test]
fn dashboard_multi_root_schema_has_stable_resolved_concrete_names() {
    let schema = serde_json::to_value(schema_for!(
        MultiRootQueryReadModelV1<DashboardPayloadMarkerV1>
    ))
    .unwrap();
    let definitions = schema["$defs"].as_object().unwrap();

    assert!(
        definitions
            .keys()
            .any(|name| name.starts_with("ScopeOutcome_for_"))
    );
    assert!(
        definitions
            .keys()
            .any(|name| name.starts_with("RootScopeOutcomeV1_for_"))
    );
    assert!(!definitions.contains_key("ScopeOutcome2"));
    assert!(!definitions.contains_key("RootScopeOutcomeV12"));
    assert_all_local_refs_resolve(&schema, definitions);
    assert!(
        !contains_ref_to(&schema, "DashboardPayloadMarkerV1"),
        "catalog placeholder payloads must be inlined instead of creating order-dependent refs"
    );
}

fn assert_all_local_refs_resolve(value: &Value, definitions: &serde_json::Map<String, Value>) {
    match value {
        Value::Object(object) => {
            if let Some(reference) = object.get("$ref").and_then(Value::as_str)
                && let Some(name) = reference.strip_prefix("#/$defs/")
            {
                assert!(
                    definitions.contains_key(name),
                    "schema reference {reference} has no definition"
                );
            }
            for child in object.values() {
                assert_all_local_refs_resolve(child, definitions);
            }
        }
        Value::Array(values) => {
            for child in values {
                assert_all_local_refs_resolve(child, definitions);
            }
        }
        _ => {}
    }
}

fn contains_ref_to(value: &Value, definition: &str) -> bool {
    match value {
        Value::Object(object) => {
            object.get("$ref").and_then(Value::as_str) == Some(&format!("#/$defs/{definition}"))
                || object
                    .values()
                    .any(|child| contains_ref_to(child, definition))
        }
        Value::Array(values) => values
            .iter()
            .any(|child| contains_ref_to(child, definition)),
        _ => false,
    }
}
