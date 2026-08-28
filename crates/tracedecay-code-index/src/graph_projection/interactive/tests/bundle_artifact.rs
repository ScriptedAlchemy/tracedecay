//! Seal-time catalog artifact: the manifest-derived bundle artifact must
//! install as a ready catalog identical to the one the projection warm scan
//! builds, and a foreign or corrupt artifact must be a typed refusal.

use tracedecay_graph_db::NeverCancelled;

use super::*;
use crate::graph_projection::interactive::artifact::decode_interactive_catalog_artifact;
use crate::graph_projection::{
    INTERACTIVE_CATALOG_ARTIFACT_NAME, code_graph_generation_id, interactive_catalog_scan_builds,
    write_interactive_catalog_artifact,
};

fn encoded_fixture_artifact() -> Vec<u8> {
    let manifest = production_manifest();
    let mut bytes = Vec::new();
    write_interactive_catalog_artifact(&manifest, &mut bytes, &NeverCancelled)
        .expect("encode catalog artifact");
    bytes
}

#[test]
fn artifact_name_is_the_bundle_contract() {
    assert_eq!(INTERACTIVE_CATALOG_ARTIFACT_NAME, "interactive-catalog");
}

#[test]
fn installed_artifact_serves_identically_to_the_warm_scan_without_scanning() {
    let bytes = encoded_fixture_artifact();

    let warmed = store_for(production_manifest());
    warmed
        .warm_interactive_catalog_with_cancellation(request())
        .expect("warm catalog");

    let installed = store_for(production_manifest());
    let scans_before_install = interactive_catalog_scan_builds();
    installed
        .install_interactive_catalog_artifact(&bytes, request())
        .expect("install catalog artifact");
    assert!(
        installed
            .interactive_catalog_is_warm()
            .expect("catalog state readable"),
        "an installed artifact must be a ready catalog"
    );
    assert_eq!(
        interactive_catalog_scan_builds(),
        scans_before_install,
        "installing a bundle artifact must not run the projection warm scan"
    );

    let warmed_reader = reader(&warmed);
    let installed_reader = reader(&installed);
    for (label, from_warm, from_install) in [
        (
            "qualified name",
            warmed_reader
                .resolve_qualified_name("beta::run", None, 8, request())
                .expect("warm resolve"),
            installed_reader
                .resolve_qualified_name("beta::run", None, 8, request())
                .expect("installed resolve"),
        ),
        (
            "simple name",
            warmed_reader
                .resolve_simple_name("Runner", None, 8, request())
                .expect("warm resolve"),
            installed_reader
                .resolve_simple_name("Runner", None, 8, request())
                .expect("installed resolve"),
        ),
        (
            "logical file listing",
            warmed_reader
                .symbols_in_logical_file("src/beta.rs", 8, request())
                .expect("warm listing"),
            installed_reader
                .symbols_in_logical_file("src/beta.rs", 8, request())
                .expect("installed listing"),
        ),
    ] {
        assert_eq!(from_warm, from_install, "{label} diverged");
        assert!(!from_warm.is_empty(), "{label} fixture must resolve");
    }
    let warm_page = warmed_reader
        .symbols_page(None, 64, request())
        .expect("warm page");
    let installed_page = installed_reader
        .symbols_page(None, 64, request())
        .expect("installed page");
    assert_eq!(warm_page, installed_page);
    assert_eq!(
        warmed_reader.files(64, request()).expect("warm files"),
        installed_reader
            .files(64, request())
            .expect("installed files"),
    );

    // Idempotent over an already-ready catalog.
    installed
        .install_interactive_catalog_artifact(&bytes, request())
        .expect("reinstall is idempotent");
}

#[test]
fn artifact_for_a_foreign_generation_is_a_typed_mismatch() {
    let bytes = encoded_fixture_artifact();
    let foreign = code_graph_generation_id(
        &id::<CodeGenerationId>("generation.interactive.other"),
        &GraphProjectorRevision::try_from(CODE_GRAPH_PROJECTOR_REVISION.to_owned())
            .expect("projector revision"),
    )
    .expect("foreign generation id");
    let error = match decode_interactive_catalog_artifact(&bytes, foreign.as_str(), &NeverCancelled)
    {
        Ok(_) => panic!("foreign generation must be refused"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        CodeGraphProjectionError::GenerationMismatch
    ));
}

#[test]
fn corrupt_artifact_bytes_are_a_typed_corruption() {
    let store = store_for(production_manifest());
    let error = store
        .install_interactive_catalog_artifact(b"{\"not\": \"a catalog\"}", request())
        .expect_err("corrupt artifact must be refused");
    assert!(matches!(error, CodeGraphProjectionError::Corrupt(_)));
    assert!(
        !store
            .interactive_catalog_is_warm()
            .expect("catalog state readable"),
        "a refused artifact must not publish a catalog"
    );
}
