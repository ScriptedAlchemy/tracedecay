use std::fs;
use std::path::Path;
use std::sync::PoisonError;

use super::{
    SemanticModelLifecycleOwnerV1, SemanticModelLifecycleStateV1, catalog_package_digest,
    open_local_semantic_evaluation_lifecycle, verify_member_file,
};
use crate::SemanticResourceCeilings;

const HF_HUB_CACHE_DIRECTORY_V1: &str = "hf-hub-cache";

fn seed_product_cache(root: &Path, fixture: &Path, owner: &SemanticModelLifecycleOwnerV1) {
    let model = owner
        .catalog()
        .get(crate::DEFAULT_FASTEMBED_MODEL_ID)
        .expect("packaged production catalog must contain the default Jina model");
    let repository = format!("models--{}", model.model_code.replace('/', "--"));
    let repository_root = root.join(HF_HUB_CACHE_DIRECTORY_V1).join(repository);
    let snapshot = repository_root
        .join("snapshots")
        .join(&model.source.revision);

    for member in model.members.values() {
        let source = fixture.join(&member.path);
        let destination = snapshot.join(&member.upstream_path);
        fs::create_dir_all(
            destination
                .parent()
                .expect("catalog member destination must have a parent"),
        )
        .expect("create packaged acquisition cache member directory");
        fs::copy(&source, &destination).unwrap_or_else(|error| {
            panic!(
                "copy verified distribution fixture {} to product cache {}: {error}",
                source.display(),
                destination.display()
            )
        });
    }

    let reference = repository_root.join("refs").join(&model.source.revision);
    fs::create_dir_all(
        reference
            .parent()
            .expect("immutable revision reference must have a parent"),
    )
    .expect("create packaged acquisition cache reference directory");
    fs::write(&reference, &model.source.revision)
        .expect("write immutable packaged acquisition cache reference");
}

#[test]
#[ignore = "requires the verified Jina fixture and isolated profile provided by \
            scripts/check-distribution-acceptance.sh, which runs this with --run-ignored"]
fn distribution_background_acquisition_installs_verified_jina_model() {
    let profile_parent = std::env::var_os("TRACEDECAY_DISTRIBUTION_FASTEMBED_PROFILE_PARENT")
        .map(std::path::PathBuf::from)
        .expect("distribution gate must provide its isolated profile parent");
    fs::create_dir_all(&profile_parent).expect("create isolated profile parent");
    let root = tempfile::tempdir_in(profile_parent).expect("create clean semantic model profile");
    let owner =
        SemanticModelLifecycleOwnerV1::open_default(root.path()).expect("open clean profile");
    if std::env::var_os("TRACEDECAY_DISTRIBUTION_FASTEMBED_NETWORK").is_none() {
        let fixture = std::env::var_os("TRACEDECAY_DISTRIBUTION_FASTEMBED_FIXTURE")
            .map(std::path::PathBuf::from)
            .expect("offline distribution gate must provide its verified Jina fixture");
        seed_product_cache(root.path(), &fixture, &owner);
    }

    let initial = owner
        .select_model(Some(crate::DEFAULT_FASTEMBED_MODEL_ID), true)
        .expect("select default Jina model for background acquisition");
    assert!(matches!(
        initial.state,
        Some(SemanticModelLifecycleStateV1::SelectedNotDownloaded { .. })
    ));
    assert!(initial.semantics_omitted);
    assert!(
        owner.enqueue_startup_acquisition_if_needed(),
        "fresh profile must queue acquisition without blocking startup"
    );

    let worker = owner
        .worker
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .handle
        .take()
        .expect("background acquisition worker must be retained");
    worker
        .join()
        .expect("background acquisition worker must not panic")
        .expect("background acquisition must complete successfully");

    let status = owner.status();
    let Some(SemanticModelLifecycleStateV1::Installed {
        model_id,
        revision,
        artifact_digest,
        install_path,
    }) = status.state
    else {
        panic!("background acquisition did not install the packaged Jina model: {status:?}");
    };
    let model = owner
        .catalog()
        .get(&model_id)
        .expect("installed model must remain cataloged");
    assert_eq!(revision, model.source.revision);
    assert_eq!(artifact_digest, catalog_package_digest(model));
    for member in model.members.values() {
        assert!(
            verify_member_file(
                &install_path.join(&member.path),
                member.length,
                &member.sha256,
            ),
            "installed member {} must match its catalog length and digest",
            member.path
        );
    }
    assert!(
        status.semantics_omitted,
        "an installed model must remain omitted until vector indexing publishes readiness"
    );
}

#[test]
#[ignore = "requires the verified Jina fixture and isolated profile provided by \
            scripts/check-distribution-acceptance.sh, which runs this with --run-ignored"]
fn distribution_local_evaluation_import_admits_verified_jina_without_network_or_profile() {
    let profile_parent = std::env::var_os("TRACEDECAY_DISTRIBUTION_FASTEMBED_PROFILE_PARENT")
        .map(std::path::PathBuf::from)
        .expect("distribution gate must provide its isolated runtime parent");
    let fixture = std::env::var_os("TRACEDECAY_DISTRIBUTION_FASTEMBED_FIXTURE")
        .map(std::path::PathBuf::from)
        .expect("local evaluation gate must provide its verified Jina fixture");
    fs::create_dir_all(&profile_parent).expect("create isolated runtime parent");
    let root = tempfile::tempdir_in(profile_parent).expect("create isolated lifecycle root");

    // The evaluator manifest pins the full cataloged truncation length, so the
    // evaluation ceiling must admit that capability rather than the tighter
    // default serving budget.
    let catalog = super::FastEmbedModelCatalogV1::production();
    let model = catalog
        .get(crate::DEFAULT_FASTEMBED_MODEL_ID)
        .expect("production catalog must contain the default Jina model");
    let resources = SemanticResourceCeilings {
        max_sequence_length: model.max_length,
        ..SemanticResourceCeilings::default()
    };
    let owner = open_local_semantic_evaluation_lifecycle(root.path(), &fixture, resources, 1)
        .expect("import exact catalog members through the production verifier");

    assert!(matches!(
        owner.status().state,
        Some(SemanticModelLifecycleStateV1::Installed { .. })
    ));
}
