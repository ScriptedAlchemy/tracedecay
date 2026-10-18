fn compiler_version(
    program: impl AsRef<std::ffi::OsStr>,
    home: &std::path::Path,
    rustup_home: Option<&std::path::Path>,
) -> String {
    let mut command = std::process::Command::new(program.as_ref());
    command.arg("--version").current_dir(home);
    if let Some(rustup_home) = rustup_home {
        command.env("RUSTUP_HOME", rustup_home);
    }
    let output = command
        .output()
        .expect("run fixture compiler outside the workspace");
    assert!(
        output.status.success(),
        "{:?}: {output:?}",
        program.as_ref()
    );
    String::from_utf8(output.stdout).expect("compiler version is UTF-8")
}

fn assert_pinned_compiler(channel: &str, version: &str, label: &str) {
    if channel.starts_with("nightly-") {
        assert!(
            version.contains("nightly"),
            "{label} {version} is not the pinned nightly {channel}"
        );
    } else {
        assert!(
            version.contains(channel),
            "{label} {version} does not contain {channel}"
        );
    }
}

fn main() {
    let manifest_dir =
        std::env::var("CARGO_MANIFEST_DIR").expect("rules_rust must provide CARGO_MANIFEST_DIR");
    let manifest_dir = std::path::PathBuf::from(manifest_dir);
    assert!(manifest_dir.is_absolute());
    assert_eq!(manifest_dir, std::env::current_dir().unwrap());
    assert_eq!(
        std::fs::read_to_string(manifest_dir.join("bazel/rust_test_environment_probe.txt"))
            .expect("fixture beneath the runtime manifest directory"),
        "runfile probe\n",
    );

    let test_tmpdir = std::env::var("TEST_TMPDIR").expect("Bazel must provide TEST_TMPDIR");
    assert_eq!(
        std::path::PathBuf::from(
            std::env::var("TRACEDECAY_DATA_DIR").expect("isolated profile data dir")
        ),
        std::path::Path::new(&test_tmpdir).join(".tracedecay"),
    );
    assert_eq!(
        std::env::var("TRACEDECAY_DISABLE_GLOBAL_DB").as_deref(),
        Ok("1"),
    );

    let target_tmpdir = std::path::PathBuf::from(
        std::env::var("CARGO_TARGET_TMPDIR").expect("the launcher must set CARGO_TARGET_TMPDIR"),
    );
    assert_eq!(
        target_tmpdir,
        std::path::Path::new(&test_tmpdir).join("cargo-target-tmp"),
    );
    assert!(target_tmpdir.is_dir());

    let home = std::path::PathBuf::from(std::env::var_os("HOME").expect("isolated home"));
    assert_eq!(home, std::path::Path::new(&test_tmpdir).join("home"));
    assert!(home.is_dir());
    assert_eq!(
        std::path::PathBuf::from(std::env::var_os("CARGO_HOME").expect("isolated Cargo home")),
        std::path::Path::new(&test_tmpdir).join("cargo-home"),
    );
    let toolchain = std::env::var("RUSTUP_TOOLCHAIN").expect("pinned test toolchain");
    // A dated nightly's `rustc --version` is `1.x.0-nightly (hash date)`.
    // The channel date is not in that string, so PATH identity is the
    // declared binary's version text.
    let channel = toolchain.replace('/', "-");
    let mut declared = std::collections::BTreeMap::new();
    for variable in ["CARGO", "RUSTC", "RUSTDOC"] {
        let tool = std::path::PathBuf::from(std::env::var_os(variable).expect("fixture compiler"));
        assert!(tool.is_absolute(), "{variable}={}", tool.display());
        let version = compiler_version(&tool, &home, None);
        assert_pinned_compiler(&channel, &version, variable);
        declared.insert(variable, version);
    }

    // Exercise the production runner's literal executable lookup under a
    // fresh home: no rustup proxy may download a second toolchain here.
    for tool in ["cargo", "rustc", "rustdoc"] {
        let version = compiler_version(tool, &home, Some(home.join("empty-rustup").as_path()));
        let variable = tool.to_ascii_uppercase();
        assert_eq!(
            version,
            declared[&variable.as_str()],
            "PATH {tool} is not the declared toolchain"
        );
    }
    for (variable, executable) in [("NODE", "node"), ("RUST_ANALYZER", "rust-analyzer")] {
        let tool =
            std::path::PathBuf::from(std::env::var_os(variable).expect("declared test tool"));
        assert!(tool.is_absolute(), "{variable}={}", tool.display());
        assert!(tool.starts_with(std::env::var_os("TEST_SRCDIR").unwrap()));
        assert_eq!(
            compiler_version(&tool, &home, Some(home.join("empty-rustup").as_path())),
            compiler_version(executable, &home, Some(home.join("empty-rustup").as_path())),
            "PATH {executable} is not the declared binary",
        );
    }
    let rust_sources = std::path::PathBuf::from(std::env::var_os("RUST_SRC_PATH").unwrap());
    assert!(rust_sources.join("core/src/lib.rs").is_file());
    let fixture = home.join("fixture");
    // Windows test scratch lives beneath the execroot workspace. Exercise
    // ancestor workspace discovery on every host.
    std::fs::write(home.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
    std::fs::create_dir_all(fixture.join("src")).unwrap();
    std::fs::write(
        fixture.join("Cargo.toml"),
        "[package]\nname = \"toolchain-probe\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\n[workspace]\n",
    )
    .unwrap();
    std::fs::write(
        fixture.join("src/lib.rs"),
        "/// ```\n/// assert_eq!(toolchain_probe::answer(), 42);\n/// ```\npub fn answer() -> u32 { 42 }\n",
    ).unwrap();
    let output = std::process::Command::new("cargo")
        .args(["test", "--offline"])
        .env("RUSTUP_HOME", home.join("empty-rustup"))
        .env("CARGO_HOME", home.join("empty-cargo"))
        .current_dir(&fixture)
        .output()
        .expect("compile and doctest an isolated fixture");
    assert!(output.status.success(), "fixture compilation: {output:?}");
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("1 passed")
    );
    assert!(!home.join("empty-rustup").exists());
    let analyzer = std::env::var_os("RUST_ANALYZER").unwrap();
    let lsp = std::process::Command::new("node")
        .args(["-e", ANALYZER_PROTOCOL_PROBE])
        .arg(analyzer)
        .arg(&fixture)
        .env("RUSTUP_HOME", home.join("empty-rustup"))
        .env("CARGO_HOME", home.join("empty-cargo"))
        .current_dir(&fixture)
        .output()
        .expect("run declared Node and rust-analyzer against an isolated fixture");
    assert!(lsp.status.success(), "analyzer protocol: {lsp:?}");
    assert!(
        String::from_utf8(lsp.stdout)
            .unwrap()
            .contains("analyzed answer")
    );
    assert!(!home.join("empty-rustup").exists());

    for variable in ["TRACEDECAY_RUNFILE_PROBE", "CARGO_BIN_EXE_runfile-probe"] {
        let runfile = std::env::var(variable)
            .unwrap_or_else(|_| panic!("the launcher must resolve runfiles_env {}", variable));
        assert!(
            std::path::Path::new(&runfile).is_absolute(),
            "{}={}",
            variable,
            runfile,
        );
        assert_eq!(
            std::fs::read_to_string(&runfile)
                .unwrap_or_else(|error| panic!("{}={}: {}", variable, runfile, error)),
            "runfile probe\n",
        );
    }
}

const ANALYZER_PROTOCOL_PROBE: &str = r#"
const {spawn} = require('node:child_process');
const fs = require('node:fs');
const path = require('node:path');
const {pathToFileURL} = require('node:url');
if (fs.realpathSync(process.execPath) !== fs.realpathSync(process.env.NODE)) {
    throw new Error('PATH node is not the declared runtime');
}
const root = process.argv[2];
const server = spawn(process.argv[1], [], {cwd: root, stdio: ['pipe', 'pipe', 'pipe']});
let buffered = Buffer.alloc(0), shutdown = false, errors = '';
const timeout = setTimeout(() => fail('analyzer protocol timed out'), 30000);
function fail(message) {
    console.error(message, errors);
    process.exitCode = 1;
    server.kill();
}
function send(message) {
    const body = Buffer.from(JSON.stringify({jsonrpc: '2.0', ...message}));
    server.stdin.write(`Content-Length: ${body.length}\r\n\r\n`);
    server.stdin.write(body);
}
server.on('error', error => fail(error.message));
server.stderr.on('data', data => { errors = (errors + data).slice(-8000); });
server.stdout.on('data', data => {
    buffered = Buffer.concat([buffered, data]);
    for (;;) {
        const boundary = buffered.indexOf('\r\n\r\n');
        if (boundary < 0) return;
        const length = Number(/Content-Length: (\d+)/i.exec(buffered.subarray(0, boundary).toString())[1]);
        if (buffered.length < boundary + 4 + length) return;
        const message = JSON.parse(buffered.subarray(boundary + 4, boundary + 4 + length));
        buffered = buffered.subarray(boundary + 4 + length);
        if (message.method && message.id !== undefined) {
            send({id: message.id, result: null});
            continue;
        }
        if (message.error) { fail(JSON.stringify(message.error)); return; }
        if (message.id === 1) {
            const uri = pathToFileURL(path.join(root, 'src/lib.rs')).href;
            send({method: 'initialized', params: {}});
            send({method: 'textDocument/didOpen', params: {textDocument: {
                uri, languageId: 'rust', version: 1,
                text: fs.readFileSync(path.join(root, 'src/lib.rs'), 'utf8'),
            }}});
            send({id: 2, method: 'textDocument/documentSymbol', params: {textDocument: {uri}}});
        } else if (message.id === 2) {
            if (!JSON.stringify(message.result).includes('"answer"')) {
                fail(`missing fixture symbol: ${JSON.stringify(message)}`); return;
            }
            send({id: 3, method: 'shutdown', params: null});
        } else if (message.id === 3) {
            shutdown = true;
            send({method: 'exit'});
        }
    }
});
server.on('exit', code => {
    clearTimeout(timeout);
    if (!shutdown || code !== 0) { fail(`analyzer exit ${code}`); return; }
    console.log('analyzed answer');
});
send({id: 1, method: 'initialize', params: {
    processId: process.pid, rootUri: pathToFileURL(root).href, capabilities: {},
}});
"#;
