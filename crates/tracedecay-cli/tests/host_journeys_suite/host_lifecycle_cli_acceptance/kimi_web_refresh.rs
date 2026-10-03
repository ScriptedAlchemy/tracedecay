//! Kimi Code plugin refresh through `kimi web`'s local REST API, against a
//! fake `kimi` in the isolated home: TraceDecay never runs the operator's
//! real Kimi Code here.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use serde_json::{Value, json};

use super::IsolatedCli;
use super::sweep_outcomes::complete_kimi_plugins_install;
use crate::isolated_profile::hermetic_path;
use tracedecay_runtime_core::test_executable::write_executable_script;

const PENDING_OPERATOR_ACTION_EXIT: i32 = 75;
const TOKEN: &str = "fake-kimi-server-token";
const PREVIOUS_VERSION: &str = "0.0.0-previous";

/// `kimi web --no-open --port <p>` serving Kimi's plugin routes over the
/// isolated `installed.json`, the way Kimi Code 2.1 does. `mode` selects a
/// failure: `unauthorized` answers 401, `hang` never serves.
const FAKE_KIMI: &str = r#"
import json, os, shutil, sys, time
from http.server import BaseHTTPRequestHandler, HTTPServer

args = sys.argv[1:]
if len(args) != 4 or args[:3] != ["web", "--no-open", "--port"]:
    sys.stderr.write("error: unknown command\n")
    sys.exit(2)
home = os.environ["KIMI_CODE_HOME"]
state = os.path.join(home, "fake-kimi")
os.makedirs(state, exist_ok=True)

def state_file(name, default):
    try:
        with open(os.path.join(state, name)) as f:
            return f.read().strip()
    except FileNotFoundError:
        return default

with open(os.path.join(state, "pid"), "w") as f:
    f.write(str(os.getpid()))
with open(os.path.join(state, "env.json"), "w") as f:
    json.dump({"KIMI_CODE_NO_AUTO_UPDATE": os.environ.get("KIMI_CODE_NO_AUTO_UPDATE")}, f)
mode = state_file("mode", "ok")
if mode == "hang":
    while True:
        time.sleep(1)
with open(os.path.join(home, "server.token")) as f:
    token = f.read().strip()
installed_path = os.path.join(home, "plugins", "installed.json")

def summary(entry, original_source):
    with open(os.path.join(entry["root"], ".kimi-plugin", "plugin.json")) as f:
        manifest = json.load(f)
    return {"id": entry["id"], "displayName": "TraceDecay", "version": manifest["version"],
            "enabled": entry["enabled"], "state": "ok", "hasErrors": False,
            "source": entry["source"], "originalSource": original_source, "root": entry["root"]}

class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    # HTTP/1.0 closes after every response without saying so; closing late
    # makes a client that reuses the connection send into a closing socket.
    def finish(self):
        time.sleep(0.2)
        super().finish()

    def reply(self, status, payload):
        body = json.dumps(payload).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def handle_plugins(self, method):
        length = int(self.headers.get("Content-Length") or 0)
        body = json.loads(self.rfile.read(length)) if length else None
        with open(os.path.join(state, "requests.jsonl"), "a") as f:
            f.write(json.dumps({"method": method, "path": self.path,
                                "authorization": self.headers.get("Authorization"),
                                "body": body}) + "\n")
        if mode == "unauthorized" or self.headers.get("Authorization") != "Bearer " + token:
            return self.reply(401, {"code": 401, "msg": "unauthorized", "data": None})
        if self.path != "/api/v1/plugins":
            return self.reply(404, {"code": 404, "msg": "not found", "data": None})
        with open(installed_path) as f:
            installed = json.load(f)
        if method == "GET":
            listed = state_file("listed-source", None)
            plugins = [summary(e, listed or e["originalSource"]) for e in installed["plugins"]]
            return self.reply(200, {"code": 0, "msg": "success", "data": {"plugins": plugins}})
        entry = next(e for e in installed["plugins"] if e["id"] == "tracedecay")
        shutil.rmtree(entry["root"])
        shutil.copytree(body["source"], entry["root"])
        entry["originalSource"] = body["source"]
        entry["updatedAt"] = "2026-09-28T02:17:16.331Z"
        with open(installed_path, "w") as f:
            json.dump(installed, f)
        self.reply(200, {"code": 0, "msg": "success", "data": summary(entry, body["source"])})

    def do_GET(self):
        self.handle_plugins("GET")

    def do_POST(self):
        self.handle_plugins("POST")

port = int(args[3])
server = HTTPServer(("127.0.0.1", port), Handler)
print("  Kimi server ready  2.1.1")
print("  Local:    http://127.0.0.1:%d/#token=%s" % (port, token), flush=True)
server.serve_forever()
"#;

struct FakeKimi {
    code_home: PathBuf,
    staged: PathBuf,
}

impl FakeKimi {
    /// Put the fake `kimi` first on the isolated `PATH`, with its interpreter
    /// named absolutely so the fake needs nothing else from `PATH`.
    fn install(cli: &IsolatedCli) -> Self {
        let python = std::env::split_paths(&hermetic_path::<&Path>(&[]))
            .map(|dir| dir.join("python3"))
            .find(|candidate| candidate.is_file())
            .expect("python3 in a system dir for the fake Kimi Code server");
        let kimi = cli.bin_dir.join("kimi");
        write_executable_script(&kimi, format!("#!{}\n{FAKE_KIMI}", python.display())).unwrap();
        let code_home = cli.home.path().join(".kimi-code");
        fs::create_dir_all(code_home.join("fake-kimi")).unwrap();
        fs::write(code_home.join("server.token"), format!("{TOKEN}\n")).unwrap();
        Self {
            code_home,
            staged: cli
                .home
                .path()
                .join(".tracedecay/host-bundle-stage/kimi/tracedecay"),
        }
    }

    fn set(&self, name: &str, value: &str) {
        fs::write(self.code_home.join("fake-kimi").join(name), value).unwrap();
    }

    /// `(method, authorization, body)` of every plugin API request received.
    fn requests(&self) -> Vec<(String, String, Value)> {
        fs::read_to_string(self.code_home.join("fake-kimi/requests.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|line| {
                let request: Value = serde_json::from_str(line).unwrap();
                (
                    request["method"].as_str().unwrap().to_string(),
                    request["authorization"].as_str().unwrap_or("").to_string(),
                    request["body"].clone(),
                )
            })
            .collect()
    }

    /// Whether the fake was started, and then whether its process is still
    /// alive.
    fn process(&self) -> Option<bool> {
        let pid = fs::read_to_string(self.code_home.join("fake-kimi/pid")).ok()?;
        let alive = std::process::Command::new("kill")
            .args(["-0", pid.trim()])
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success();
        Some(alive)
    }

    fn managed_version(&self) -> String {
        let manifest: Value = serde_json::from_slice(
            &fs::read(
                self.code_home
                    .join("plugins/managed/tracedecay/.kimi-plugin/plugin.json"),
            )
            .unwrap(),
        )
        .unwrap();
        manifest["version"].as_str().unwrap().to_string()
    }

    fn staged_version(&self) -> String {
        let manifest: Value = serde_json::from_slice(
            &fs::read(self.staged.join(".kimi-plugin/plugin.json")).unwrap(),
        )
        .unwrap();
        manifest["version"].as_str().unwrap().to_string()
    }

    /// The operator's earlier interactive `/plugins install`, whose managed
    /// copy a later TraceDecay release has since restaged past.
    fn install_previous_release(&self, cli: &IsolatedCli) {
        complete_kimi_plugins_install(cli, &self.staged);
        let manifest_path = self
            .code_home
            .join("plugins/managed/tracedecay/.kimi-plugin/plugin.json");
        let mut manifest: Value =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        manifest["version"] = json!(PREVIOUS_VERSION);
        fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
    }
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn pending_install_line(staged: &Path) -> String {
    format!(
        "kimi: pending operator action: `/plugins install {}`",
        staged.display()
    )
}

#[test]
fn update_plugin_refreshes_the_installed_kimi_plugin_through_kimi_web() {
    let cli = IsolatedCli::new();
    let kimi = FakeKimi::install(&cli);

    let install = cli.run(&["install", "--agent", "kimi"]);
    let install_stderr = stderr(&install);
    assert_eq!(
        install.status.code(),
        Some(PENDING_OPERATOR_ACTION_EXIT),
        "{install_stderr}"
    );
    assert!(
        install_stderr.contains(&pending_install_line(&kimi.staged)),
        "{install_stderr}"
    );
    assert_eq!(
        (kimi.process(), kimi.requests()),
        (None, Vec::new()),
        "the first install is Kimi's interactive step; TraceDecay never asks Kimi's server"
    );

    kimi.install_previous_release(&cli);
    let update = cli.run(&["update-plugin"]);
    let update_stderr = stderr(&update);

    assert_eq!(update.status.code(), Some(0), "{update_stderr}");
    let version = kimi.staged_version();
    assert!(
        update_stderr.contains(&format!(
            "Kimi Code refreshed its TraceDecay plugin to {version} through `kimi web`"
        )),
        "{update_stderr}"
    );
    assert!(
        update_stderr.contains("  kimi: refreshed\n"),
        "{update_stderr}"
    );
    let bearer = format!("Bearer {TOKEN}");
    assert_eq!(
        kimi.requests(),
        vec![
            ("GET".to_string(), bearer.clone(), Value::Null),
            (
                "POST".to_string(),
                bearer,
                json!({"source": kimi.staged.display().to_string()})
            ),
        ]
    );
    assert_eq!(kimi.managed_version(), version);
    assert_eq!(
        kimi.process(),
        Some(false),
        "the spawned `kimi web` must be gone"
    );
    let child_env: Value =
        serde_json::from_slice(&fs::read(kimi.code_home.join("fake-kimi/env.json")).unwrap())
            .unwrap();
    assert_eq!(child_env, json!({"KIMI_CODE_NO_AUTO_UPDATE": "1"}));

    let doctor = stderr(&cli.run(&["doctor"]));
    assert!(
        doctor.contains("Kimi Code CLI managed plugin matches its staged source"),
        "{doctor}"
    );
    // No daemon listens here, so `daemon_unavailable` is the one step left.
    assert!(!doctor.contains("open Kimi Code and run"), "{doctor}");
    assert!(
        doctor.contains("daemon_unavailable: no TraceDecay daemon is listening")
            && doctor.contains("1 pending operator action(s), "),
        "{doctor}"
    );
}

#[test]
fn kimi_web_refresh_never_installs_a_plugin_kimi_lists_from_another_source() {
    let cli = IsolatedCli::new();
    let kimi = FakeKimi::install(&cli);
    let _ = cli.run(&["install", "--agent", "kimi"]);
    kimi.install_previous_release(&cli);
    kimi.set(
        "listed-source",
        &cli.home.path().join("foreign-plugin").display().to_string(),
    );

    let update = cli.run(&["update-plugin"]);
    let update_stderr = stderr(&update);

    assert_eq!(
        update.status.code(),
        Some(PENDING_OPERATOR_ACTION_EXIT),
        "{update_stderr}"
    );
    assert!(
        update_stderr.contains(&pending_install_line(&kimi.staged)),
        "{update_stderr}"
    );
    assert_eq!(
        kimi.requests()
            .into_iter()
            .map(|(method, _, _)| method)
            .collect::<Vec<_>>(),
        ["GET"],
        "Kimi's listing withholds consent, so nothing is installed"
    );
    assert_eq!(kimi.managed_version(), PREVIOUS_VERSION);
    assert_eq!(kimi.process(), Some(false));
}

#[test]
fn kimi_web_refresh_falls_back_to_the_operator_step_when_kimi_refuses_the_token() {
    let cli = IsolatedCli::new();
    let kimi = FakeKimi::install(&cli);
    let _ = cli.run(&["install", "--agent", "kimi"]);
    kimi.install_previous_release(&cli);
    kimi.set("mode", "unauthorized");

    let update = cli.run(&["update-plugin"]);
    let update_stderr = stderr(&update);

    assert_eq!(
        update.status.code(),
        Some(PENDING_OPERATOR_ACTION_EXIT),
        "{update_stderr}"
    );
    assert!(
        update_stderr.contains(&pending_install_line(&kimi.staged)),
        "{update_stderr}"
    );
    assert!(
        update_stderr.contains(
            "The automatic refresh through `kimi web` did not apply: `kimi web` rejected the \
             server token (HTTP 401)"
        ),
        "{update_stderr}"
    );
    assert_eq!(
        kimi.requests()
            .into_iter()
            .map(|(method, _, _)| method)
            .collect::<Vec<_>>(),
        ["GET"]
    );
    assert_eq!(kimi.managed_version(), PREVIOUS_VERSION);
    assert_eq!(kimi.process(), Some(false));
}

#[test]
fn kimi_web_refresh_falls_back_to_the_operator_step_when_kimi_never_serves() {
    let cli = IsolatedCli::new();
    let kimi = FakeKimi::install(&cli);
    let _ = cli.run(&["install", "--agent", "kimi"]);
    kimi.install_previous_release(&cli);
    kimi.set("mode", "hang");

    let update = cli.run(&["update-plugin"]);
    let update_stderr = stderr(&update);

    assert_eq!(
        update.status.code(),
        Some(PENDING_OPERATOR_ACTION_EXIT),
        "{update_stderr}"
    );
    assert!(
        update_stderr.contains(
            "The automatic refresh through `kimi web` did not apply: `kimi web` did not serve \
             its plugin API within 20 seconds"
        ),
        "{update_stderr}"
    );
    assert_eq!(kimi.requests(), Vec::new());
    assert_eq!(kimi.managed_version(), PREVIOUS_VERSION);
    assert_eq!(
        kimi.process(),
        Some(false),
        "the hung `kimi web` must be stopped"
    );
}

/// Without its `kimi` CLI, Kimi Code is not installed: the sweep skips it and
/// exits 0 rather than waiting on an operator step nothing can reach.
#[test]
fn kimi_web_refresh_skips_kimi_code_without_a_kimi_cli() {
    let cli = IsolatedCli::new();
    let kimi = FakeKimi::install(&cli);
    let _ = cli.run(&["install", "--agent", "kimi"]);
    kimi.install_previous_release(&cli);
    fs::remove_file(cli.bin_dir.join("kimi")).unwrap();

    let update = cli.run(&["update-plugin"]);
    let update_stderr = stderr(&update);

    assert_eq!(update.status.code(), Some(0), "{update_stderr}");
    assert!(
        update_stderr.contains(
            "  kimi: skipped, not installed (host CLI `kimi` is unavailable for Kimi Code \
             plugin lifecycle; install it or add it to PATH and retry)\n"
        ),
        "{update_stderr}"
    );
    assert!(
        !update_stderr.contains("pending operator action"),
        "{update_stderr}"
    );
    assert_eq!(kimi.managed_version(), PREVIOUS_VERSION);
}
