//! The public Git surfaces carry Rust-owned schema authority: the shared
//! `public_wire` types back every catalog capability, so SDK generation and
//! daemon transport parsing cannot drift apart.

use schemars::schema_for;
use serde_json::Value;
use tracedecay_application::git::{
    GitApplySurfaceRequest, GitBlameSurfaceRequest, GitDiffSurfaceRequest, GitHistorySurfaceRequest,
    GitHunksSurfaceRequest, GitPreviewSurfaceRequest, GitReadResultV1, GitStatusSurfaceRequest,
};
use tracedecay_application::git_surface_catalog_contribution;
use tracedecay_tool_catalog::CapabilityId;

fn request_schema_body(capability: &str) -> Value {
    let contribution = git_surface_catalog_contribution().expect("Git contribution");
    let capability_id = CapabilityId::new(capability).expect("capability ID");
    let authority = contribution
        .executable_schema(&capability_id)
        .expect("schema-backed Git capability");
    authority.request_schema().body().clone()
}

#[test]
fn every_public_git_capability_is_schema_backed() {
    let contribution = git_surface_catalog_contribution().expect("Git contribution");
    for capability in [
        "capability.application.git.status",
        "capability.application.git.diff",
        "capability.application.git.history",
        "capability.application.git.blame",
        "capability.application.git.hunks",
        "capability.application.git.preview",
        "capability.application.git.apply",
    ] {
        let capability_id = CapabilityId::new(capability).expect("capability ID");
        let authority = contribution
            .executable_schema(&capability_id)
            .unwrap_or_else(|| panic!("{capability} must carry executable schema authority"));
        assert!(
            authority.request_schema().body().is_object(),
            "{capability} request schema body"
        );
        assert!(
            authority.result_schema().body().is_object(),
            "{capability} result schema body"
        );
    }
}

#[test]
fn git_schema_bodies_are_generated_from_the_shared_wire_types() {
    for (capability, expected) in [
        (
            "capability.application.git.status",
            schema_for!(GitStatusSurfaceRequest),
        ),
        (
            "capability.application.git.diff",
            schema_for!(GitDiffSurfaceRequest),
        ),
        (
            "capability.application.git.history",
            schema_for!(GitHistorySurfaceRequest),
        ),
        (
            "capability.application.git.blame",
            schema_for!(GitBlameSurfaceRequest),
        ),
        (
            "capability.application.git.hunks",
            schema_for!(GitHunksSurfaceRequest),
        ),
        (
            "capability.application.git.preview",
            schema_for!(GitPreviewSurfaceRequest),
        ),
        (
            "capability.application.git.apply",
            schema_for!(GitApplySurfaceRequest),
        ),
    ] {
        let expected = serde_json::to_value(expected).expect("schema JSON");
        let actual = request_schema_body(capability);
        // The catalog canonicalizes JSON ordering; compare semantic content.
        assert_eq!(
            actual["properties"], expected["properties"],
            "{capability} request properties"
        );
    }
}

#[test]
fn read_result_schema_covers_every_typed_query_payload() {
    let schema =
        serde_json::to_value(schema_for!(GitReadResultV1)).expect("read result schema JSON");
    let rendered = schema.to_string();
    for query in ["status", "diff", "history", "blame", "hunks"] {
        assert!(
            rendered.contains(&format!("\"{query}\"")),
            "read result schema must cover the {query} query"
        );
    }
}
