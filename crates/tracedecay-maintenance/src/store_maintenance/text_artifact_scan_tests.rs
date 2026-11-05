use std::collections::{BTreeMap, BTreeSet};

use tracedecay_code_index_retention::code_index_generations::code_text_artifacts_root;
use tracedecay_domain::canonical_text::sha256_hex;
use tracedecay_runtime_core::cancellation::CancellationToken;

use super::{execute_collection, plan_collection};
use crate::telemetry::StoreTelemetrySamplingRegistry;

#[asupersync::test]
async fn maintenance_owner_advances_noop_pages_and_applies_a_later_orphan() {
    let store = tempfile::tempdir().unwrap();
    let artifacts = code_text_artifacts_root(store.path());
    std::fs::create_dir_all(&artifacts).unwrap();
    let mut contents = BTreeMap::new();
    for index in 0..150 {
        let bytes = format!("orphan payload {index}");
        let path = artifacts.join(format!(
            "text-artifact-{}.bin",
            sha256_hex(bytes.as_bytes())
        ));
        std::fs::write(&path, b"untrusted content").unwrap();
        contents.insert(path, bytes);
    }
    let last = std::fs::read_dir(&artifacts)
        .unwrap()
        .last()
        .unwrap()
        .unwrap()
        .path();
    std::fs::write(&last, &contents[&last]).unwrap();
    let observations = StoreTelemetrySamplingRegistry::default();
    let cancellation = CancellationToken::new();
    let replay = store.path().join("graph-replay");
    let mut idle_pages = 0;
    let mut finished = false;
    for _ in 0..6 {
        // Fetch through the registry again, just as a new maintenance tick
        // does. Constructing a fresh scan per call would starve this fixture.
        let plan = plan_collection(
            store.path(),
            &BTreeSet::new(),
            &replay,
            store.path(),
            store.path(),
            &observations.clone(),
            &cancellation,
        )
        .await
        .unwrap();
        let more = plan.has_pending_text_artifact_inventory();
        if plan.has_collectable_work() {
            let report = execute_collection(
                store.path(),
                plan,
                &replay,
                store.path(),
                store.path(),
                &observations,
                &cancellation,
            )
            .await
            .unwrap();
            assert_eq!(report.deleted_text_artifacts.len(), 1);
        } else {
            idle_pages += usize::from(more);
        }
        if !more {
            finished = true;
            break;
        }
    }
    assert!(finished);
    assert!(idle_pages > 0);
    assert!(!last.exists());
    for path in contents.keys().filter(|path| **path != last) {
        assert_eq!(std::fs::read(path).unwrap(), b"untrusted content");
    }
}
