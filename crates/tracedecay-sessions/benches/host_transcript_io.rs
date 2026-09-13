//! Adapter filesystem-walk cost for host transcript discovery.
//!
//! Worker-yield (one-worker heartbeat) is covered by the snapshot admission
//! tests; this bench times the sync walk each adapter still performs inside
//! `run_blocking_transcript_section`.

use std::hint::black_box;
use std::path::Path;
use std::time::{Duration, SystemTime};

use criterion::{Criterion, criterion_group, criterion_main};
use tempfile::TempDir;
use tracedecay_sessions::runtime::source::{TranscriptDiscoveryBounds, TranscriptSource};
use tracedecay_sessions::runtime::vibe::VibeSource;

fn write_vibe_session(root: &Path, name: &str, mtime_secs: u64) {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("messages.jsonl");
    std::fs::write(&path, "").unwrap();
    let mtime = SystemTime::UNIX_EPOCH + Duration::from_secs(mtime_secs);
    filetime::set_file_mtime(&path, filetime::FileTime::from_system_time(mtime)).unwrap();
}

fn vibe_tree(sessions: usize) -> (TempDir, VibeSource) {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join(".vibe/logs/session");
    for index in 0..sessions {
        write_vibe_session(
            &root,
            &format!("session-{index:03}"),
            1_700_000_000 + index as u64,
        );
    }
    let source = VibeSource::with_home(tmp.path());
    (tmp, source)
}

fn bench_vibe_discover(c: &mut Criterion) {
    let (tmp, source) = vibe_tree(64);
    let bounds = TranscriptDiscoveryBounds::from_discovered_units(64);
    c.bench_function("vibe_discover_64_sessions", |b| {
        b.iter(|| {
            black_box(source.discover_transcript_paths(black_box(tmp.path()), bounds));
        });
    });
}

fn bench_one_worker_heartbeat(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .expect("one-worker runtime");
    c.bench_function("one_worker_yield_during_40ms_section", |b| {
        b.to_async(&rt).iter(|| async {
            let handle = tokio::runtime::Handle::current();
            let (sender, receiver) = std::sync::mpsc::channel();
            let started = std::time::Instant::now();
            tokio::spawn(async move {
                tokio::task::block_in_place(|| {
                    handle.spawn(async move {
                        let _ = sender.send(std::time::Instant::now());
                    });
                    std::thread::sleep(Duration::from_millis(40));
                });
            })
            .await
            .expect("join blocking section");
            let heartbeat = receiver.recv().expect("heartbeat");
            black_box(heartbeat.saturating_duration_since(started))
        });
    });
}

criterion_group!(benches, bench_vibe_discover, bench_one_worker_heartbeat);
criterion_main!(benches);
