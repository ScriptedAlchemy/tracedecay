use super::{format_index_age_phrase, staleness_banner, McpServer, StalenessBannerInputs};
use crate::tracedecay::TraceDecay;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tempfile::TempDir;

fn git(root: &std::path::Path, args: &[&str]) {
    let ok = std::process::Command::new(crate::git::git_program())
        .current_dir(root)
        .args(args)
        .output()
        .expect("git runs")
        .status
        .success();
    assert!(ok, "git {args:?} failed");
}

async fn init_indexed_repo() -> (TraceDecay, TempDir) {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    git(root, &["init", "-q", "-b", "main"]);
    git(root, &["config", "user.email", "t@t.com"]);
    git(root, &["config", "user.name", "T"]);
    std::fs::write(root.join(".gitignore"), ".tracedecay/\n").unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/a.rs"), "pub fn a() {}\n").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "initial"]);
    let cg = TraceDecay::init(root).await.expect("init");
    cg.index_all().await.expect("index");
    (cg, dir)
}

// ---- D7 pure-logic banner tests (test c) --------------------------

#[test]
fn format_index_age_phrase_preserves_shape() {
    // 2h 5m
    assert_eq!(format_index_age_phrase(2 * 3600 + 5 * 60), "2h 5m");
    // 1d 3h
    assert_eq!(format_index_age_phrase(27 * 3600), "1d 3h");
}

#[test]
fn banner_says_refresh_in_progress_when_auto_sync_on() {
    let banner = staleness_banner(StalenessBannerInputs {
        age_secs: 2 * 3600,
        auto_sync_on: true,
        fallback_store: false,
        refresh_running: true,
        refreshed_recently: false,
    })
    .expect("banner expected");
    assert!(banner.contains("refresh in progress"), "{banner}");
    assert!(!banner.contains("tracedecay sync"), "{banner}");
    assert!(!banner.starts_with("WARNING"), "{banner}");
}

#[test]
fn banner_says_scheduled_when_auto_sync_on_and_idle() {
    let banner = staleness_banner(StalenessBannerInputs {
        age_secs: 2 * 3600,
        auto_sync_on: true,
        fallback_store: false,
        refresh_running: false,
        refreshed_recently: false,
    })
    .expect("banner expected");
    assert!(banner.contains("refresh scheduled"), "{banner}");
    assert!(!banner.contains("tracedecay sync"), "{banner}");
}

#[test]
fn banner_suppressed_shortly_after_refresh() {
    let banner = staleness_banner(StalenessBannerInputs {
        age_secs: 2 * 3600,
        auto_sync_on: true,
        fallback_store: false,
        refresh_running: false,
        refreshed_recently: true,
    });
    assert!(banner.is_none(), "expected no banner, got {banner:?}");
}

#[test]
fn banner_instructs_manual_sync_only_on_fallback_store() {
    let banner = staleness_banner(StalenessBannerInputs {
        age_secs: 2 * 3600,
        auto_sync_on: true,
        fallback_store: true,
        refresh_running: true,
        refreshed_recently: false,
    })
    .expect("banner expected");
    assert!(banner.starts_with("WARNING"), "{banner}");
    assert!(banner.contains("Run `tracedecay sync`"), "{banner}");
}

#[test]
fn banner_instructs_manual_sync_when_auto_sync_disabled() {
    let banner = staleness_banner(StalenessBannerInputs {
        age_secs: 2 * 3600,
        auto_sync_on: false,
        fallback_store: false,
        refresh_running: false,
        refreshed_recently: false,
    })
    .expect("banner expected");
    assert!(banner.contains("Run `tracedecay sync`"), "{banner}");
}

// ---- D1: startup catch-up runs exactly once (test b) --------------

#[tokio::test]
async fn startup_catch_up_spawned_once_per_server() {
    let (cg, _dir) = init_indexed_repo().await;
    let server = McpServer::new(cg, None).await;
    // The D1 spawn should have claimed the one-shot flag.
    assert!(
        server.startup_catch_up_started.load(Ordering::Acquire),
        "startup catch-up should have been dispatched by new_with_dbs"
    );
    assert!(
        server
            .wait_for_startup_catch_up(Duration::from_secs(30))
            .await,
        "startup catch-up should settle"
    );
    assert!(server.startup_catch_up_done());

    // The one-shot flag is already claimed; a hypothetical second
    // new_with_dbs-style dispatch would be refused by the same
    // compare_exchange. Assert the flag can't be re-claimed.
    assert!(
        server
            .startup_catch_up_started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err(),
        "startup catch-up flag must stay claimed (runs at most once)"
    );
}

// ---- D4: sync-on-read never blocks + single-flight (tests a, d) ---

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_refresh_is_non_blocking_and_single_flighted() {
    let (cg, dir) = init_indexed_repo().await;
    let root = dir.path().to_path_buf();
    let mut config = crate::config::load_config(&root).expect("load config");
    config.sync.session_start_sync = false;
    crate::config::save_config(&root, &config).expect("save config");
    let server = McpServer::new(cg, None).await;
    // Reset the read cooldown so the next spawn is eligible regardless of
    // any startup timing.
    server
        .last_background_refresh_at
        .store(0, Ordering::Release);
    server
        .background_refresh_running
        .store(false, Ordering::Release);

    // Make the tree stale: a new committed source file, as the
    // diff-scoped refresh contract tracks git history.
    std::fs::write(root.join("src/b.rs"), "pub fn b() {}\n").unwrap();
    git(&root, &["add", "."]);
    git(&root, &["commit", "-q", "-m", "add b"]);

    let cg_snapshot = server.cg_snapshot().await;

    // First read-refresh: returns immediately (we never await the sync).
    // Assert it does not block by bounding the call duration well under a
    // real sync (~hundreds of ms); the spawn does the work off-thread.
    let start = std::time::Instant::now();
    server.maybe_spawn_read_refresh(&cg_snapshot);
    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_millis(100),
        "maybe_spawn_read_refresh must not block on the sync (took {elapsed:?})"
    );
    // The refresh should have been claimed (running flag set) — proving a
    // task was spawned rather than run inline.
    // (It may already have finished on a very fast machine; in that case
    // the cooldown stamp still advanced, which we assert below.)
    assert_ne!(
        server.last_background_refresh_at.load(Ordering::Acquire),
        0,
        "cooldown stamp must advance when a refresh is kicked"
    );

    // Second immediate read-refresh: single-flighted. Because the cooldown
    // stamp just advanced (and/or a refresh is running), no second task is
    // spawned. We verify by confirming the stamp does not change to a new
    // value on a back-to-back call within the cooldown window.
    let stamp_after_first = server.last_background_refresh_at.load(Ordering::Acquire);
    server.maybe_spawn_read_refresh(&cg_snapshot);
    let stamp_after_second = server.last_background_refresh_at.load(Ordering::Acquire);
    assert_eq!(
        stamp_after_first, stamp_after_second,
        "second read within cooldown must not re-kick (single-flight)"
    );
}
