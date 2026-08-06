use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use tempfile::TempDir;
use tracedecay_runtime_core::git_repository::{GitHistoryOptions, GitRepositoryAuthority};

struct RepositoryFixture {
    _directory: TempDir,
    authority: GitRepositoryAuthority,
}

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args([
            "-c",
            "user.name=Benchmark",
            "-c",
            "user.email=benchmark@example.com",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(root)
        .output()
        .expect("git executable");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn init() -> TempDir {
    let directory = tempfile::tempdir().expect("temporary repository");
    git(directory.path(), &["init", "--quiet", "-b", "main"]);
    directory
}

fn history_fixture(commits: usize) -> RepositoryFixture {
    let directory = init();
    let mut child = Command::new("git")
        .arg("fast-import")
        .arg("--quiet")
        .current_dir(directory.path())
        .stdin(Stdio::piped())
        .spawn()
        .expect("git fast-import");
    let input = child.stdin.as_mut().expect("fast-import stdin");
    for index in 0..commits {
        let message = format!("commit {index}");
        writeln!(input, "commit refs/heads/main").expect("fast-import commit");
        writeln!(input, "mark :{}", index + 1).expect("fast-import mark");
        writeln!(
            input,
            "author Benchmark <benchmark@example.com> {} +0000",
            1_000_000_000 + index
        )
        .expect("fast-import author");
        writeln!(
            input,
            "committer Benchmark <benchmark@example.com> {} +0000",
            1_000_000_000 + index
        )
        .expect("fast-import committer");
        writeln!(input, "data {}", message.len()).expect("fast-import message size");
        writeln!(input, "{message}").expect("fast-import message");
        if index > 0 {
            writeln!(input, "from :{index}").expect("fast-import parent");
        }
        let content = format!("{index}\n");
        writeln!(input, "M 100644 inline counter.txt").expect("fast-import modify");
        writeln!(input, "data {}", content.len()).expect("fast-import content size");
        write!(input, "{content}").expect("fast-import content");
    }
    writeln!(input, "done").expect("fast-import done");
    drop(child.stdin.take());
    assert!(child.wait().expect("fast-import exit").success());
    let authority = GitRepositoryAuthority::discover(directory.path()).expect("history authority");
    RepositoryFixture {
        _directory: directory,
        authority,
    }
}

fn dirty_fixture(files: usize) -> RepositoryFixture {
    let directory = init();
    let files_root = directory.path().join("files");
    std::fs::create_dir(&files_root).expect("files directory");
    for index in 0..files {
        std::fs::write(files_root.join(format!("{index:05}.txt")), b"before\n")
            .expect("fixture file");
    }
    git(directory.path(), &["add", "-A"]);
    git(directory.path(), &["commit", "--quiet", "-m", "baseline"]);
    for index in 0..files {
        std::fs::write(files_root.join(format!("{index:05}.txt")), b"after\n")
            .expect("dirty fixture file");
    }
    let authority = GitRepositoryAuthority::discover(directory.path()).expect("status authority");
    RepositoryFixture {
        _directory: directory,
        authority,
    }
}

fn benchmark_authority(c: &mut Criterion) {
    let history = history_fixture(1_000);
    let history_options = GitHistoryOptions {
        max_count: 1_000,
        first_parent: false,
        path: None,
        follow_renames: false,
    };
    let history_result = history
        .authority
        .history(&history_options)
        .expect("1k history");
    assert_eq!(history_result.commits.len(), 1_000);
    let mut history_group = c.benchmark_group("git_repository_authority/history");
    history_group.throughput(Throughput::Elements(1_000));
    history_group.bench_function("commits_1000", |bencher| {
        bencher.iter(|| {
            history
                .authority
                .history(&history_options)
                .expect("bounded history")
        });
    });
    history_group.finish();

    let dirty = dirty_fixture(10_000);
    let status_result = dirty.authority.status().expect("10k status");
    assert_eq!(status_result.entries.len(), 10_000);
    let mut status_group = c.benchmark_group("git_repository_authority/status");
    status_group.throughput(Throughput::Elements(10_000));
    status_group.bench_function("dirty_files_10000", |bencher| {
        bencher.iter(|| dirty.authority.status().expect("live status"));
    });
    status_group.finish();
}

fn criterion_config() -> Criterion {
    Criterion::default()
        .sample_size(10)
        .warm_up_time(std::time::Duration::from_secs(1))
        .measurement_time(std::time::Duration::from_secs(3))
}

criterion_group! {
    name = benches;
    config = criterion_config();
    targets = benchmark_authority
}
criterion_main!(benches);
