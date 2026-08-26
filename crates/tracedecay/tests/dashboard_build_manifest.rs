#[path = "../build-support/dashboard_manifest.rs"]
mod dashboard_manifest;

use dashboard_manifest::{DASHBOARD_ASSET_MANIFEST, dashboard_asset_paths};
#[cfg(unix)]
use std::os::unix::fs::symlink;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_FIXTURE_ID: AtomicU64 = AtomicU64::new(0);

struct DashboardDist {
    path: PathBuf,
}

impl DashboardDist {
    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for DashboardDist {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn dashboard_dist(all_files: &[&str], existing_files: &[&str]) -> DashboardDist {
    let fixture_id = NEXT_FIXTURE_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "tracedecay-dashboard-manifest-{}-{fixture_id}",
        std::process::id()
    ));
    fs::create_dir(&path).expect("create dashboard dist fixture");
    for relative in existing_files {
        let asset_path = path.join(relative);
        fs::create_dir_all(asset_path.parent().expect("fixture file parent"))
            .expect("create fixture file parent");
        fs::write(asset_path, relative).expect("write fixture asset");
    }
    let all_files = all_files
        .iter()
        .map(|relative| format!("{relative:?}"))
        .collect::<Vec<_>>()
        .join(",");
    fs::write(
        path.join(DASHBOARD_ASSET_MANIFEST),
        format!("{{\"allFiles\":[{all_files}]}}"),
    )
    .expect("write manifest fixture");
    DashboardDist { path }
}

#[test]
fn accepts_existing_relative_manifest_entries() {
    let dist = dashboard_dist(
        &["static/js/app.js", "index.html", "static/css/app.css"],
        &["index.html", "static/js/app.js", "static/css/app.css"],
    );

    let paths = dashboard_asset_paths(dist.path()).expect("valid dashboard manifest");

    assert_eq!(
        paths,
        ["index.html", "static/css/app.css", "static/js/app.js"]
    );
}

#[test]
fn rejects_manifest_without_index_html() {
    let dist = dashboard_dist(&["static/js/app.js"], &["static/js/app.js"]);

    let error = dashboard_asset_paths(dist.path())
        .expect_err("manifest without index.html must fail")
        .to_string();

    assert!(error.contains("index.html"), "{error}");
}

#[test]
fn rejects_duplicate_normalized_paths() {
    let dist = dashboard_dist(&["index.html", "./index.html"], &["index.html"]);

    let error = dashboard_asset_paths(dist.path())
        .expect_err("duplicate normalized paths must fail")
        .to_string();

    assert!(error.contains("duplicate"), "{error}");
}

#[test]
fn rejects_unsafe_manifest_path_shapes() {
    for invalid in [
        "/tmp/index.html",
        "../index.html",
        "static/../../index.html",
        "https://example.test/app.js",
        "static/app.js?version=1",
        "static/app.js#fragment",
        "static/%2e%2e/app.js",
        "static%2fapp.js",
        "static\\app.js",
    ] {
        let dist = dashboard_dist(&["index.html", invalid], &["index.html"]);

        let error = dashboard_asset_paths(dist.path())
            .expect_err("unsafe manifest path must fail")
            .to_string();

        assert!(
            error.contains("dashboard asset manifest path"),
            "{invalid}: {error}"
        );
    }
}

#[test]
fn rejects_listed_files_missing_from_dist() {
    let dist = dashboard_dist(&["index.html", "static/js/missing.js"], &["index.html"]);

    let error = dashboard_asset_paths(dist.path())
        .expect_err("listed missing asset must fail")
        .to_string();

    assert!(error.contains("static/js/missing.js"), "{error}");
}

#[cfg(unix)]
#[test]
fn rejects_symlinked_manifest() {
    let dist = dashboard_dist(&["index.html"], &["index.html"]);
    let outside_manifest = dist.path().with_extension("outside-manifest.json");
    fs::write(&outside_manifest, r#"{"allFiles":["index.html"]}"#).expect("write outside manifest");
    fs::remove_file(dist.path().join(DASHBOARD_ASSET_MANIFEST)).expect("remove fixture manifest");
    symlink(
        &outside_manifest,
        dist.path().join(DASHBOARD_ASSET_MANIFEST),
    )
    .expect("symlink outside manifest");

    let error = dashboard_asset_paths(dist.path())
        .expect_err("symlinked manifest must fail")
        .to_string();

    fs::remove_file(outside_manifest).expect("remove outside manifest");
    assert!(error.contains("symlink"), "{error}");
}

#[cfg(unix)]
#[test]
fn rejects_symlinked_app_dist_root() {
    let dist = dashboard_dist(&["index.html"], &["index.html"]);
    let symlink_root = dist.path().with_extension("root-link");
    symlink(dist.path(), &symlink_root).expect("symlink app-dist root");

    let result = dashboard_asset_paths(&symlink_root);

    fs::remove_file(symlink_root).expect("remove app-dist root symlink");
    let error = result
        .expect_err("symlinked app-dist root must fail")
        .to_string();
    assert!(error.contains("symlink"), "{error}");
}

#[cfg(unix)]
#[test]
fn rejects_symlinked_asset_outside_dist() {
    let dist = dashboard_dist(&["index.html", "static/js/app.js"], &["index.html"]);
    let outside_asset = dist.path().with_extension("outside.js");
    fs::write(&outside_asset, "outside").expect("write outside asset");
    let asset_path = dist.path().join("static/js/app.js");
    fs::create_dir_all(asset_path.parent().expect("asset parent")).expect("create asset parent");
    symlink(&outside_asset, &asset_path).expect("symlink outside asset");

    let error = dashboard_asset_paths(dist.path())
        .expect_err("symlinked asset must fail")
        .to_string();

    fs::remove_file(outside_asset).expect("remove outside asset");
    assert!(error.contains("symlink"), "{error}");
}

#[cfg(unix)]
#[test]
fn rejects_symlinked_intermediate_asset_directory() {
    let dist = dashboard_dist(&["index.html", "static/js/app.js"], &["index.html"]);
    let outside_assets = dist.path().with_extension("outside-assets");
    fs::create_dir_all(outside_assets.join("js")).expect("create outside asset directory");
    fs::write(outside_assets.join("js/app.js"), "outside").expect("write outside asset");
    let symlink_directory = dist.path().join("static");
    symlink(&outside_assets, &symlink_directory).expect("symlink outside asset directory");

    let result = dashboard_asset_paths(dist.path());

    fs::remove_file(symlink_directory).expect("remove asset directory symlink");
    fs::remove_dir_all(outside_assets).expect("remove outside asset directory");
    let error = result
        .expect_err("symlinked asset directory must fail")
        .to_string();
    assert!(error.contains("symlink"), "{error}");
}
