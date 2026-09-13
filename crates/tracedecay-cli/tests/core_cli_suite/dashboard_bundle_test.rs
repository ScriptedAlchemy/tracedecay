//! The preparation-to-embedding boundary `build.rs` crosses, exercised as the
//! exact code the build script mounts — not a copy that can drift.
//!
//! Covers the property the bundle store exists for: once a producer's output
//! is promoted, the bytes the compiler embeds no longer depend on the producer
//! directory, which `rsbuild dev` or another target directory may clean at any
//! moment. Also covers the fingerprint that decides whether the frontend is
//! rebuilt at all, and the fail-closed cases (tampered store, mismatched
//! producer, malformed record).

use dashboard_bundle::{
    BUILD_RECORD_FILE, BUNDLE_DIGEST_PREFIX, BuildRecord, STAGING_DIR, bundle_digest,
    inputs_fingerprint, open, prepare_staging, promote, read_build_record, stage_copy,
    write_build_record,
};
use sha2::{Digest, Sha256};

#[path = "../../build-support/dashboard_bundle.rs"]
mod dashboard_bundle;
#[path = "../../build-support/dashboard_manifest.rs"]
mod dashboard_manifest;

const INDEX_HTML: &[u8] = b"<!doctype html><script src=\"/static/js/index.js\"></script>";
const INDEX_JS: &[u8] = b"console.log('dashboard');";
const INDEX_CSS: &[u8] = b"body{margin:0}";

fn write(root: &std::path::Path, relative: &str, bytes: &[u8]) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().expect("asset has a parent")).expect("create parent");
    std::fs::write(path, bytes).expect("write asset");
}

/// A producer's complete bundle: manifest plus every listed file.
fn producer_bundle(root: &std::path::Path, css: &[u8]) {
    write(
        root,
        "asset-manifest.json",
        br#"{"allFiles":["index.html","static/js/index.js","static/css/index.css"]}"#,
    );
    write(root, "index.html", INDEX_HTML);
    write(root, "static/js/index.js", INDEX_JS);
    write(root, "static/css/index.css", css);
}

/// The documented cross-tool digest, spelled out independently of the code
/// under test.
fn expected_digest(entries: &[(&str, &[u8])]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(BUNDLE_DIGEST_PREFIX);
    for (relative, bytes) in entries {
        hasher.update(relative.as_bytes());
        hasher.update([0u8]);
        hasher.update((bytes.len() as u64).to_le_bytes());
        hasher.update(bytes);
    }
    hex::encode(hasher.finalize())
}

#[test]
fn promoted_bundle_survives_producer_cleanup_and_matches_the_documented_digest() {
    let temp = tempfile::tempdir().expect("tempdir");
    let producer = temp.path().join("app-dist");
    let store = temp.path().join("store");
    producer_bundle(&producer, INDEX_CSS);

    let staged = stage_copy(&store, &producer).expect("stage the producer's bundle");
    let expected = expected_digest(&[
        ("index.html", INDEX_HTML),
        ("static/css/index.css", INDEX_CSS),
        ("static/js/index.js", INDEX_JS),
    ]);
    assert_eq!(staged.digest_hex, expected);
    assert_eq!(staged.root, store.join(&expected));
    assert_eq!(
        staged.asset_paths,
        ["index.html", "static/css/index.css", "static/js/index.js"]
    );
    assert!(
        !store.join(STAGING_DIR).exists(),
        "promotion must rename staging away, not copy it"
    );

    // The dev-server race: the producer directory is emptied after promotion.
    std::fs::remove_dir_all(&producer).expect("simulate rsbuild cleaning app-dist");
    let reopened = open(&store, &expected)
        .expect("reopen the promoted bundle")
        .expect("promoted bundle is present");
    assert_eq!(reopened, staged);
    assert_eq!(
        std::fs::read(reopened.root.join("static/css/index.css")).expect("read staged css"),
        INDEX_CSS,
        "the compiler's include paths resolve to the staged bytes, not the producer's"
    );
}

#[test]
fn promoting_a_new_bundle_prunes_the_superseded_one() {
    let temp = tempfile::tempdir().expect("tempdir");
    let producer = temp.path().join("app-dist");
    let store = temp.path().join("store");
    producer_bundle(&producer, INDEX_CSS);
    let first = stage_copy(&store, &producer).expect("stage first bundle");

    producer_bundle(&producer, b"body{margin:1px}");
    let second = stage_copy(&store, &producer).expect("stage second bundle");
    assert_ne!(first.digest_hex, second.digest_hex);
    assert!(!first.root.exists(), "superseded bundle must be pruned");
    assert!(second.root.is_dir());
    assert_eq!(
        open(&store, &first.digest_hex).expect("lookup of pruned bundle"),
        None
    );
}

#[test]
fn a_producer_that_writes_directly_into_staging_is_promoted_from_there() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = temp.path().join("store");
    std::fs::create_dir_all(&store).expect("create store");
    let staging = prepare_staging(&store).expect("prepare staging");
    write(&staging, "leftover.txt", b"from an interrupted run");
    let staging = prepare_staging(&store).expect("prepare staging again");
    assert!(
        !staging.join("leftover.txt").exists(),
        "staging must start empty for the producer"
    );

    producer_bundle(&staging, INDEX_CSS);
    let promoted = promote(&store).expect("promote the staged output");
    assert_eq!(promoted.root, store.join(&promoted.digest_hex));
    assert_eq!(
        bundle_digest(&promoted.root, &promoted.asset_paths).expect("digest promoted bytes"),
        promoted.digest_hex
    );
}

#[test]
fn a_bundle_missing_a_listed_file_is_refused_before_promotion() {
    let temp = tempfile::tempdir().expect("tempdir");
    let producer = temp.path().join("app-dist");
    let store = temp.path().join("store");
    producer_bundle(&producer, INDEX_CSS);
    std::fs::remove_file(producer.join("static/css/index.css")).expect("drop a listed asset");

    let error = stage_copy(&store, &producer)
        .expect_err("an incomplete producer bundle must not be staged")
        .to_string();
    assert!(error.contains("static/css/index.css"), "{error}");
    assert!(
        !store.exists()
            || std::fs::read_dir(&store)
                .expect("list store")
                .next()
                .is_none(),
        "nothing may be promoted from a refused bundle"
    );
}

#[test]
fn a_tampered_store_entry_fails_closed_instead_of_self_healing() {
    let temp = tempfile::tempdir().expect("tempdir");
    let producer = temp.path().join("app-dist");
    let store = temp.path().join("store");
    producer_bundle(&producer, INDEX_CSS);
    let staged = stage_copy(&store, &producer).expect("stage bundle");

    std::fs::write(staged.root.join("static/js/index.js"), b"tampered").expect("tamper");
    let error = open(&store, &staged.digest_hex)
        .expect_err("a digest directory whose bytes drifted must be an error")
        .to_string();
    assert!(
        error.contains("not the digest it is named after"),
        "{error}"
    );
    assert_eq!(open(&store, &"0".repeat(64)).expect("unknown digest"), None);
}

#[test]
fn build_record_round_trips_and_rejects_malformed_content() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = temp.path();
    assert_eq!(read_build_record(store), None);

    let record = BuildRecord {
        inputs_fingerprint: "1".repeat(64),
        bundle_digest: "2".repeat(64),
    };
    write_build_record(store, &record).expect("write record");
    assert_eq!(read_build_record(store), Some(record));

    std::fs::write(store.join(BUILD_RECORD_FILE), "short\n").expect("corrupt record");
    assert_eq!(read_build_record(store), None);
}

#[test]
fn inputs_fingerprint_tracks_content_presence_and_nesting_only() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();
    write(root, "dashboard/src/app/main.tsx", b"export {}");
    write(root, "dashboard/package.json", b"{}");
    let inputs = [
        "dashboard/src",
        "dashboard/package.json",
        "dashboard/absent.json",
    ];

    let baseline = inputs_fingerprint(root, &inputs).expect("fingerprint");
    assert_eq!(
        inputs_fingerprint(root, &inputs).expect("fingerprint again"),
        baseline,
        "unchanged inputs must fingerprint identically across runs"
    );

    write(
        root,
        "dashboard/src/app/main.tsx",
        b"export const changed = 1;",
    );
    let edited = inputs_fingerprint(root, &inputs).expect("fingerprint after edit");
    assert_ne!(
        edited, baseline,
        "editing a nested file must change the fingerprint"
    );

    write(root, "dashboard/src/app/new.ts", b"");
    let added = inputs_fingerprint(root, &inputs).expect("fingerprint after add");
    assert_ne!(
        added, edited,
        "adding an empty file must change the fingerprint"
    );

    std::fs::remove_file(root.join("dashboard/src/app/new.ts")).expect("remove added file");
    assert_eq!(
        inputs_fingerprint(root, &inputs).expect("fingerprint after removal"),
        edited,
        "removing the file must restore the previous fingerprint"
    );

    write(root, "dashboard/app-dist/index.html", b"built output");
    assert_eq!(
        inputs_fingerprint(root, &inputs).expect("fingerprint with build output present"),
        edited,
        "build output is not an input"
    );
}
