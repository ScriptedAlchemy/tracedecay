use crate::common::fixture::{GitFixture, TestProfile};
use tracedecay::tracedecay::TraceDecay;

mod common;

#[tokio::test]
async fn direct_lifecycle_entry_points_retain_production_authority() {
    let profile = TestProfile::acquire().await;
    let repository = GitFixture::primary(profile.path("project"));
    let project = repository.root();
    std::fs::create_dir_all(project.join("src")).expect("project source directory");
    std::fs::write(
        project.join("src/lib.rs"),
        "pub fn lifecycle_fixture() {}\n",
    )
    .expect("project source");
    repository.commit_all("initial");

    let options = profile.open_options();

    let initialized = TraceDecay::init_with_options(project, options.clone())
        .await
        .expect("direct production init");
    initialized
        .set_tokens_saved(41)
        .await
        .expect("returned init runtime retains write authority");
    assert_eq!(
        initialized
            .get_tokens_saved()
            .await
            .expect("read durable state through returned init runtime"),
        41
    );
    initialized.close();

    let opened = TraceDecay::open_with_options(project, options.clone())
        .await
        .expect("direct production open");
    assert_eq!(
        opened
            .get_tokens_saved()
            .await
            .expect("reopened production runtime reads durable state"),
        41
    );
    let concurrent = TraceDecay::open_with_options(project, options.clone())
        .await
        .expect("direct production authority is shared within its owning process");
    opened.close();
    concurrent.close();

    let read_only = TraceDecay::open_read_only_with_options(project, options.clone())
        .await
        .expect("direct production read-only open");
    assert!(read_only.is_read_only());
    read_only.close();
}
