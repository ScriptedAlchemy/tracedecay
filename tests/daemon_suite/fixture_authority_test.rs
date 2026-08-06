//! Contract tests for the shared fixture authority (`common::fixture`).
//!
//! Each case pins one identity invariant that test setup used to get wrong by
//! hand, so a regression in the authority fails here instead of surfacing as a
//! scattered "project not enrolled" or "prefix not found" across suites.

use crate::common::fixture::{GitFixture, TestProfile};
use tracedecay::storage;

#[tokio::test]
async fn one_profile_serves_two_projects_with_distinct_stores() {
    let profile = TestProfile::acquire().await;
    let first = GitFixture::primary(profile.path("first"));
    let second = GitFixture::primary(profile.path("second"));

    let first = profile.enroll(first.root()).await;
    let second = profile.enroll(second.root()).await;

    assert_ne!(
        first.project_id(),
        second.project_id(),
        "two checkouts are two projects"
    );
    assert_ne!(
        first.data_root(),
        second.data_root(),
        "two projects get two stores"
    );
    for project in [&first, &second] {
        assert!(
            project.data_root().starts_with(profile.root()),
            "both stores must live in the fixture's one profile: {} is not under {}",
            project.data_root().display(),
            profile.root().display()
        );
    }
}

#[tokio::test]
async fn enrolled_layout_comes_from_the_opened_graph() {
    let profile = TestProfile::acquire().await;
    let repo = GitFixture::primary(profile.path("project"));
    std::fs::write(repo.root().join("lib.rs"), "pub fn seeded() {}\n").unwrap();
    repo.commit_all("seed");

    let project = profile.enroll_indexed(repo.root()).await;

    assert!(
        project.graph_db_path().is_file(),
        "the layout must name the graph database the graph actually wrote: {}",
        project.graph_db_path().display()
    );
    assert!(
        project.graph_db_path().starts_with(project.data_root()),
        "the graph database must live in the reported data root"
    );
    let marker = storage::read_enrollment_marker(project.root())
        .expect("enrollment marker is readable")
        .expect("enrolling writes a marker");
    assert_eq!(
        marker.project_id,
        project.project_id(),
        "the marker, the registered identity, and the graph must name one project"
    );
    assert!(
        !project.search("seeded", 10).await.unwrap().is_empty(),
        "enroll_indexed must leave the seeded symbol searchable"
    );
}

#[tokio::test]
async fn linked_worktree_collapses_onto_the_primary_checkout() {
    let profile = TestProfile::acquire().await;
    let repo = GitFixture::primary(profile.path("project"));
    let worktree = profile.scratch().join("feature-worktree");

    let worktree = repo.linked_worktree(&worktree, "feature");

    assert_eq!(
        storage::default_profile_project_id(&worktree),
        storage::default_profile_project_id(repo.root()),
        "every linked worktree of a repository resolves to one project identity"
    );
}

#[tokio::test]
async fn committing_a_fixture_tree_never_stages_enrollment_state() {
    let profile = TestProfile::acquire().await;
    let repo = GitFixture::primary(profile.path("project"));
    let project = profile.enroll(repo.root()).await;
    assert!(
        storage::enrollment_marker_path(project.root()).is_file(),
        "the fixture must have an enrollment marker to exclude"
    );

    std::fs::write(repo.root().join("lib.rs"), "pub fn after_enrollment() {}\n").unwrap();
    repo.commit_all("work after enrollment");

    let tracked = repo.capture(&["ls-files"]);
    assert!(
        !tracked.lines().any(|path| path.starts_with(".tracedecay")),
        "committing a fixture tree must never track enrollment state: {tracked}"
    );
}

#[tokio::test]
async fn unenrolled_root_stays_unenrolled() {
    let profile = TestProfile::acquire().await;

    let unenrolled = profile.unenrolled("never-enrolled");

    assert!(
        storage::read_enrollment_marker(unenrolled.root())
            .expect("enrollment marker is readable")
            .is_none(),
        "the unenrolled negative case must not acquire a marker"
    );
}

/// A server built from a bare direct context has no registry database and no
/// retained project-graph resolver, so hook notifications fail closed before
/// reaching the code under test. The authority's server must arrive with both.
#[cfg(feature = "test-transport")]
#[tokio::test]
async fn project_server_arrives_with_its_session_authority() {
    let profile = TestProfile::acquire().await;
    let repo = GitFixture::primary(profile.path("project"));
    let project = profile.enroll_indexed(repo.root()).await;

    let server = project.mcp_server().await;

    assert!(
        server.has_project_session_retrieval_service_for_test(),
        "the fixture server must mount the project session retrieval service"
    );
}
