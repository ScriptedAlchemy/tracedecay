use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

const RUNNER_PATH: &str = "scripts/run-session-temporal-benchmark.sh";
const FAKE_CARGO_STATUS: i32 = 47;

struct RunnerInvocation {
    output: Output,
    cargo_receipt: Option<String>,
    parent_data: PathBuf,
    parent_home: PathBuf,
}

fn write_executable(path: &Path, body: &str) {
    fs::write(path, body).expect("write fake executable");
    let mut permissions = fs::metadata(path)
        .expect("inspect fake executable")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("make fake executable runnable");
}

fn invoke_runner(uname: &str, mode: &str) -> RunnerInvocation {
    let temp = TempDir::new().expect("runner tempdir");
    let fake_bin = temp.path().join("bin");
    fs::create_dir_all(&fake_bin).expect("create fake bin");
    let cargo_receipt_path = temp.path().join("cargo-receipt.txt");

    write_executable(
        &fake_bin.join("uname"),
        "#!/bin/sh\nprintf '%s\\n' \"$FAKE_UNAME\"\n",
    );
    write_executable(
        &fake_bin.join("cargo"),
        "#!/bin/sh\n{\n  printf 'argv='\n  printf '<%s>' \"$@\"\n  printf '\\nHOME=<%s>\\n' \"$HOME\"\n  printf 'TRACEDECAY_DATA_DIR=<%s>\\n' \"$TRACEDECAY_DATA_DIR\"\n} >\"$FAKE_CARGO_RECEIPT\"\nexit 47\n",
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
        .env("FAKE_CARGO_RECEIPT", &cargo_receipt_path)
        .env("FAKE_UNAME", uname)
        .env("HOME", &parent_home)
        .env("PATH", fake_path)
        .env("RUSTUP_HOME", temp.path().join("rustup-home"))
        .env("TMPDIR", temp.path())
        .env("TRACEDECAY_DATA_DIR", &parent_data)
        .output()
        .expect("run session-temporal runner");

    let cargo_receipt = match fs::read_to_string(&cargo_receipt_path) {
        Ok(receipt) => Some(receipt),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => panic!("read cargo receipt: {error}"),
    };
    RunnerInvocation {
        output,
        cargo_receipt,
        parent_data,
        parent_home,
    }
}

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn diagnostic_runner_reaches_cargo_on_linux_and_macos() {
    for (uname, platform) in [("Linux", "linux"), ("Darwin", "macos")] {
        let invocation = invoke_runner(uname, "--run");

        assert_eq!(
            invocation.output.status.code(),
            Some(FAKE_CARGO_STATUS),
            "{platform} runner output: {}",
            String::from_utf8_lossy(&invocation.output.stderr)
        );
        let receipt = invocation
            .cargo_receipt
            .expect("diagnostic runner must execute cargo");
        assert!(
            receipt.starts_with(
                "argv=<bench><--bench><session_temporal><--all-features><--><--run>\n"
            ),
            "{platform} cargo receipt: {receipt}"
        );
        assert!(
            receipt.contains("HOME=<"),
            "{platform} cargo receipt: {receipt}"
        );
        assert!(
            !receipt.contains(&format!("HOME=<{}>", invocation.parent_home.display())),
            "{platform} runner must isolate HOME: {receipt}"
        );
        assert!(
            receipt.contains("TRACEDECAY_DATA_DIR=<"),
            "{platform} cargo receipt: {receipt}"
        );
        assert!(
            !receipt.contains(&format!(
                "TRACEDECAY_DATA_DIR=<{}>",
                invocation.parent_data.display()
            )),
            "{platform} runner must isolate TRACEDECAY_DATA_DIR: {receipt}"
        );
    }
}

#[test]
fn contract_refresh_runner_refuses_macos_before_starting_cargo() {
    let invocation = invoke_runner("Darwin", "--refresh-contract");

    assert_eq!(invocation.output.status.code(), Some(64));
    assert!(invocation.cargo_receipt.is_none());
    assert!(String::from_utf8_lossy(&invocation.output.stderr).contains("Linux-hosted"));
}
