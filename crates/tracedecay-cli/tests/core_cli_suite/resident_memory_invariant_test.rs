//! A settled daemon's anonymous memory is what the resident-memory authority
//! charges: its retained owners, its pooled canonical scratch, and the fixed
//! runtime allowance. Checked on the shipped daemon after a cold index and
//! after a refresh, under the 6 GiB admission limit the operator journeys use.

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;
use tempfile::TempDir;

use crate::common::{
    git_program, initialize_tracedecay_cli_project, spawn_tracedecay_daemon_with,
    tracedecay_command_with_home,
};

const ADMISSION_LIMIT_BYTES: u64 = 6 * 1024 * 1024 * 1024;
const CORPUS_FILES: usize = 300;
const REFRESHED_FILES: usize = 40;
/// Two resident-memory samples: the first asks idle threads to return their
/// heaps, and every thread has idled and collected before the second.
const SETTLE: Duration = Duration::from_secs(70);
const GRAPH_SERVED_TIMEOUT: Duration = Duration::from_secs(600);

fn git(project: &Path, args: &[&str]) {
    let status = Command::new(git_program())
        .args([
            "-c",
            "user.email=corpus@example.invalid",
            "-c",
            "user.name=corpus",
        ])
        .args(args)
        .current_dir(project)
        .stdout(Stdio::null())
        .status()
        .expect("git runs");
    assert!(status.success(), "git {args:?} failed");
}

fn module_source(index: usize, revision: usize) -> String {
    let next = (index + 1) % CORPUS_FILES;
    let mut source = format!(
        "use crate::module_{next}::Record{next};\n\n\
         pub struct Record{index} {{\n    pub id: u64,\n    pub name: String,\n}}\n\n"
    );
    for item in 0..12 {
        source.push_str(&format!(
            "pub fn step_{index}_{item}(record: &Record{index}, other: &Record{next}) -> u64 {{\n    \
             let base = record.id.wrapping_mul({item}) ^ other.id;\n    \
             if base % 3 == {revision} {{ base + record.name.len() as u64 }} else {{ base }}\n}}\n\n"
        ));
    }
    source
}

fn write_corpus(project: &Path) {
    let source_dir = project.join("src");
    fs::create_dir_all(&source_dir).expect("corpus source dir");
    let mut lib = String::new();
    for index in 0..CORPUS_FILES {
        fs::write(
            source_dir.join(format!("module_{index}.rs")),
            module_source(index, 0),
        )
        .expect("corpus module");
        lib.push_str(&format!("pub mod module_{index};\n"));
    }
    fs::write(source_dir.join("lib.rs"), lib).expect("corpus lib");
    git(project, &["init", "-q"]);
    git(project, &["add", "-A"]);
    git(project, &["commit", "-qm", "corpus"]);
}

fn refresh_corpus(project: &Path) {
    for index in 0..REFRESHED_FILES {
        fs::write(
            project.join("src").join(format!("module_{index}.rs")),
            module_source(index, 1),
        )
        .expect("refreshed module");
    }
    git(project, &["commit", "-qam", "refresh"]);
}

fn status(home: &Path, project: &Path) -> Value {
    let output = tracedecay_command_with_home(home)
        .args(["tool", "status", "--format", "json", "--json"])
        .current_dir(project)
        .stdin(Stdio::null())
        .output()
        .expect("tracedecay tool status runs");
    assert!(
        output.status.success(),
        "status failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let envelope: Value = serde_json::from_slice(&output.stdout).expect("status envelope");
    let text = envelope["content"][0]["text"]
        .as_str()
        .expect("status text content");
    serde_json::from_str(text).expect("status payload")
}

/// The generation whose graph engine and catalog are both resident and charged.
fn served_generation(status: &Value) -> Option<String> {
    let charged = |kind: &str| {
        status["memory"]["owners"].as_array().is_some_and(|owners| {
            owners
                .iter()
                .any(|owner| owner["kind"] == kind && owner["bytes"].as_u64().is_some())
        })
    };
    let fresh = matches!(
        status["code_index_freshness"]["status"].as_str(),
        Some("current" | "ready")
    );
    (fresh && charged("graph_engine") && charged("graph_catalog"))
        .then(|| status["graph_statistics"]["generation_id"].as_str())
        .flatten()
        .map(str::to_owned)
}

fn wait_for_served_generation(home: &Path, project: &Path, not: Option<&str>) -> String {
    let deadline = Instant::now() + GRAPH_SERVED_TIMEOUT;
    loop {
        let status = status(home, project);
        if let Some(generation) = served_generation(&status)
            && Some(generation.as_str()) != not
        {
            return generation;
        }
        assert!(
            Instant::now() < deadline,
            "no newly served graph generation: {}",
            status["code_index_freshness"]
        );
        std::thread::sleep(Duration::from_secs(2));
    }
}

fn anonymous_bytes(pid: u32) -> u64 {
    let status = fs::read_to_string(format!("/proc/{pid}/status")).expect("daemon status");
    ["RssAnon:", "VmSwap:"]
        .iter()
        .map(|field| {
            status
                .lines()
                .find_map(|line| line.strip_prefix(field))
                .and_then(|value| value.trim().strip_suffix("kB"))
                .and_then(|kib| kib.trim().parse::<u64>().ok())
                .map(|kib| kib * 1024)
                .unwrap_or_else(|| panic!("daemon status reports {field}"))
        })
        .sum()
}

fn assert_anonymous_memory_is_charged(pid: u32, home: &Path, project: &Path, phase: &str) {
    std::thread::sleep(SETTLE);
    let anonymous = anonymous_bytes(pid);
    let memory = &status(home, project)["memory"];
    let retained = memory["retained_bytes"].as_u64().expect("retained bytes");
    let scratch = memory["canonical_scratch_bytes"]
        .as_u64()
        .expect("canonical scratch bytes");
    let allowance = memory["runtime_allowance_bytes"]
        .as_u64()
        .expect("runtime allowance");
    assert!(
        anonymous <= retained + scratch + allowance,
        "{phase}: anonymous memory {anonymous} exceeds retained {retained} + canonical \
         scratch {scratch} + allowance {allowance} by {}",
        anonymous - (retained + scratch + allowance)
    );
}

#[test]
fn settled_daemon_anonymous_memory_stays_within_its_charges() {
    let home = TempDir::new().expect("home");
    let project = home.path().join("corpus");
    write_corpus(&project);
    let daemon = spawn_tracedecay_daemon_with(home.path(), |command| {
        command.env(
            "TRACEDECAY_RESIDENT_MEMORY_LIMIT_BYTES",
            ADMISSION_LIMIT_BYTES.to_string(),
        );
    });
    initialize_tracedecay_cli_project(home.path(), &project);

    let cold = wait_for_served_generation(home.path(), &project, None);
    assert_anonymous_memory_is_charged(daemon.id(), home.path(), &project, "cold index");

    refresh_corpus(&project);
    wait_for_served_generation(home.path(), &project, Some(&cold));
    assert_anonymous_memory_is_charged(daemon.id(), home.path(), &project, "refresh");
}
