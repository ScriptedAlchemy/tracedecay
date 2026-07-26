use std::path::Path;
use std::process::Command;

use tracedecay::tracedecay::{TraceDecay, TraceDecayOpenOptions};

fn git(project: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(project)
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed");
}

#[tokio::test]
async fn direct_lifecycle_entry_points_retain_production_authority() {
    let root = tempfile::tempdir().expect("temporary fixture root");
    let project = root.path().join("project");
    let profile = root.path().join("profile");
    std::fs::create_dir_all(project.join("src")).expect("project source directory");
    std::fs::write(
        project.join("src/lib.rs"),
        "pub fn lifecycle_fixture() {}\n",
    )
    .expect("project source");
    git(&project, &["init", "-b", "main"]);
    git(&project, &["add", "."]);
    git(
        &project,
        &[
            "-c",
            "user.name=TraceDecay Test",
            "-c",
            "user.email=test@tracedecay.local",
            "commit",
            "-m",
            "initial",
        ],
    );

    let options = TraceDecayOpenOptions {
        profile_root: Some(profile.clone()),
        global_db_path: Some(profile.join("global.db")),
    };

    let initialized = TraceDecay::init_with_options(&project, options.clone())
        .await
        .expect("direct production init");
    initialized
        .index_all()
        .await
        .expect("returned init runtime retains write authority");
    initialized.close();

    let opened = TraceDecay::open_with_options(&project, options.clone())
        .await
        .expect("direct production open");
    let concurrent = TraceDecay::open_with_options(&project, options.clone())
        .await
        .expect("direct production authority is shared within its owning process");
    opened.close();
    concurrent.close();

    let read_only = TraceDecay::open_read_only_with_options(&project, options.clone())
        .await
        .expect("direct production read-only open");
    assert!(read_only.is_read_only());
    read_only.close();

    TraceDecay::open_branch_with_options(&project, "main", options)
        .await
        .expect("direct production branch open")
        .close();
}
