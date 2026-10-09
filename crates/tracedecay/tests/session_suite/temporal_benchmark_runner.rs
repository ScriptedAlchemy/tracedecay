use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tracedecay_runtime_core::test_executable::write_executable_script;

use tempfile::TempDir;

const RUNNER_PATH: &str = "scripts/run-session-temporal-benchmark.sh";
const FAKE_BAZEL_STATUS: i32 = 47;

struct RunnerInvocation {
    output: Output,
    bazel_receipt: Option<String>,
}

fn write_executable(path: &Path, body: &str) {
    write_executable_script(path, body).expect("write fake executable");
}

fn invoke_runner(uname: &str, mode: &str) -> RunnerInvocation {
    let temp = TempDir::new().expect("runner tempdir");
    let fake_bin = temp.path().join("bin");
    fs::create_dir_all(&fake_bin).expect("create fake bin");
    let bazel_receipt_path = temp.path().join("bazel-receipt.txt");

    write_executable(
        &fake_bin.join("uname"),
        "#!/bin/sh\nprintf '%s\\n' \"$FAKE_UNAME\"\n",
    );
    write_executable(
        &fake_bin.join("bazel"),
        "#!/bin/sh\n{\n  printf 'argv='\n  printf '<%s>' \"$@\"\n  printf '\\nHOME=<%s>\\n' \"$HOME\"\n  printf 'TRACEDECAY_DATA_DIR=<%s>\\n' \"$TRACEDECAY_DATA_DIR\"\n} >\"$FAKE_BAZEL_RECEIPT\"\nexit 47\n",
    );

    let parent_home = temp.path().join("parent-home");
    let parent_data = temp.path().join("parent-data");
    fs::create_dir_all(&parent_home).expect("create parent home");
    fs::create_dir_all(&parent_data).expect("create parent data dir");
    let path = std::env::var_os("PATH").expect("PATH is set");
    let mut fake_path = OsString::from(fake_bin.as_os_str());
    fake_path.push(":");
    fake_path.push(path);

    let output = Command::new("bash")
        .arg(repository_root().join(RUNNER_PATH))
        .arg(mode)
        .current_dir(repository_root())
        .env("CARGO_HOME", temp.path().join("cargo-home"))
        .env("FAKE_BAZEL_RECEIPT", &bazel_receipt_path)
        .env("FAKE_UNAME", uname)
        .env("HOME", &parent_home)
        .env("PATH", fake_path)
        .env("RUSTUP_HOME", temp.path().join("rustup-home"))
        .env("TMPDIR", temp.path())
        .env("TRACEDECAY_DATA_DIR", &parent_data)
        .output()
        .expect("run session-temporal runner");

    let bazel_receipt = match fs::read_to_string(&bazel_receipt_path) {
        Ok(receipt) => Some(receipt),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => panic!("read Bazel receipt: {error}"),
    };
    RunnerInvocation {
        output,
        bazel_receipt,
    }
}

fn repository_root() -> PathBuf {
    crate::common::repository_root()
        .canonicalize()
        .expect("canonical fixture repository root")
}

#[test]
fn diagnostic_runner_reaches_bazel_on_linux_and_macos() {
    for (uname, platform) in [("Linux", "linux"), ("Darwin", "macos")] {
        let invocation = invoke_runner(uname, "--run");

        assert_eq!(
            invocation.output.status.code(),
            Some(FAKE_BAZEL_STATUS),
            "{platform} runner output: {}",
            String::from_utf8_lossy(&invocation.output.stderr)
        );
        let receipt = invocation
            .bazel_receipt
            .expect("diagnostic runner must execute Bazel");
        assert_eq!(
            receipt.lines().next().expect("Bazel argument receipt"),
            format!(
                "argv=<test><--config=release><--config=ci><//crates/tracedecay:session_temporal><--test_strategy=standalone><--nocache_test_results><--test_output=all><--test_env=TRACEDECAY_BENCHMARK_REPO_ROOT={}><--test_arg=--bench><--test_arg=--run>",
                repository_root().display()
            ),
            "{platform} Bazel receipt: {receipt}"
        );
    }
}
