use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use tempfile::TempDir;
use tokensave::agents::*;
use tokensave::branch_meta;
use tokensave::config::get_tokensave_dir;
use tokensave::tokensave::TokenSave;

// ---------------------------------------------------------------------------
// 1. Registry tests
// ---------------------------------------------------------------------------

#[test]
fn test_get_all_integrations() {
    let all = all_integrations();
    assert_eq!(all.len(), 15);
}

#[test]
fn test_available_integrations() {
    let ids = available_integrations();
    assert!(ids.contains(&"claude"));
    assert!(ids.contains(&"copilot"));
    assert!(ids.contains(&"codex"));
    assert!(ids.contains(&"gemini"));
    assert!(ids.contains(&"opencode"));
    assert!(ids.contains(&"cursor"));
    assert!(ids.contains(&"hermes"));
    assert!(ids.contains(&"zed"));
    assert!(ids.contains(&"cline"));
    assert!(ids.contains(&"roo-code"));
    assert!(ids.contains(&"antigravity"));
    assert!(ids.contains(&"kilo"));
    assert!(ids.contains(&"kiro"));
    assert!(ids.contains(&"kimi"));
    assert!(ids.contains(&"vibe"));
    assert_eq!(ids.len(), 15);
}

#[test]
fn test_hermes_registry_entry() {
    let ids = available_integrations();
    assert!(ids.contains(&"hermes"));

    let agent = get_integration("hermes").unwrap();
    assert_eq!(agent.id(), "hermes");
    assert_eq!(agent.name(), "Hermes");
}

#[test]
fn test_get_integration_valid() {
    for id in &[
        "claude",
        "opencode",
        "codex",
        "gemini",
        "copilot",
        "cursor",
        "hermes",
        "zed",
        "cline",
        "roo-code",
        "antigravity",
        "kilo",
        "kiro",
        "kimi",
        "vibe",
    ] {
        let agent = get_integration(id).unwrap();
        assert_eq!(agent.id(), *id);
    }
}

#[test]
fn test_get_integration_invalid() {
    assert!(get_integration("nonexistent").is_err());
    assert!(get_integration("").is_err());
    assert!(get_integration("CLAUDE").is_err()); // case-sensitive
}

// ---------------------------------------------------------------------------
// 2. Agent trait tests (name/id)
// ---------------------------------------------------------------------------

#[test]
fn test_agent_names_and_ids() {
    for agent in all_integrations() {
        assert!(!agent.name().is_empty(), "agent name should not be empty");
        assert!(!agent.id().is_empty(), "agent id should not be empty");
    }
}

#[test]
fn test_agent_names_are_human_readable() {
    // Names should have at least one space or capital letter (human-readable, not slug)
    let expected_names: Vec<(&str, &str)> = vec![
        ("claude", "Claude Code"),
        ("copilot", "GitHub Copilot"),
        ("codex", "Codex CLI"),
        ("gemini", "Gemini CLI"),
        ("hermes", "Hermes"),
        ("opencode", "OpenCode"),
        ("cursor", "Cursor"),
        ("zed", "Zed"),
        ("cline", "Cline"),
        ("roo-code", "Roo Code"),
        ("antigravity", "Antigravity"),
        ("kilo", "Kilo CLI"),
        ("kiro", "Kiro"),
        ("kimi", "Kimi CLI"),
        ("vibe", "Mistral Vibe"),
    ];
    for (id, expected_name) in expected_names {
        let agent = get_integration(id).unwrap();
        assert_eq!(agent.name(), expected_name, "name mismatch for agent {id}");
    }
}

// ---------------------------------------------------------------------------
// 3. Install / config creation tests (with tempdir)
// ---------------------------------------------------------------------------

fn make_install_ctx(home: &Path) -> InstallContext {
    InstallContext {
        home: home.to_path_buf(),
        tokensave_bin: "/usr/local/bin/tokensave".to_string(),
        tool_permissions: expected_tool_perms(),
        profile: None,
        project_root: None,
        dashboard: true,
    }
}

fn tokensave_command(project: &Path, home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_tokensave"));
    command
        .current_dir(project)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("KIRO_HOME", home.join(".kiro"))
        .env("VIBE_HOME", home.join(".vibe"));
    command
}

fn run_local_install(agent: &str, project: &Path, home: &Path) -> std::process::Output {
    tokensave_command(project, home)
        .arg("install")
        .arg("--local")
        .arg("--agent")
        .arg(agent)
        .output()
        .unwrap_or_else(|e| panic!("failed to run local install for {agent}: {e}"))
}

fn assert_local_install_success(agent: &str, project: &Path, home: &Path) -> std::process::Output {
    let output = run_local_install(agent, project, home);
    assert!(
        output.status.success(),
        "local install for {agent} should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn read_json(path: &Path) -> serde_json::Value {
    serde_json::from_str(
        &std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("failed to read JSON {}: {e}", path.display())),
    )
    .unwrap_or_else(|e| panic!("failed to parse JSON {}: {e}", path.display()))
}

fn expected_tokensave_bin() -> String {
    env!("CARGO_BIN_EXE_tokensave").replace('\\', "/")
}

/// Minimal PyYAML stand-in covering only the YAML subset the generated
/// Hermes configs use: nested block mappings, block lists of scalars, and
/// plain/quoted scalars. Hermes itself always ships PyYAML; CI's system
/// python3 on macOS/Windows has no third-party packages, so checks that
/// exercise the plugin's config.yaml paths get this shim via PYTHONPATH.
const PYYAML_SHIM: &str = r##""""Minimal PyYAML stand-in for tokensave agent tests.

Implements safe_load/dump for the simple block-style YAML the generated
Hermes config files use. Only used when the system python3 lacks PyYAML.
"""

import json
import re

_PLAIN_SCALAR = re.compile(r"^[A-Za-z0-9_./~+-]+$")


def safe_load(stream):
    text = stream if isinstance(stream, str) else stream.read()
    items = []
    for raw in text.splitlines():
        stripped = raw.strip()
        if not stripped or stripped.startswith("#"):
            continue
        items.append((len(raw) - len(raw.lstrip(" ")), stripped))
    if not items:
        return None
    value, index = _parse_block(items, 0, items[0][0])
    if index != len(items):
        raise ValueError(f"unsupported yaml structure near: {items[index][1]!r}")
    return value


def _parse_scalar(token):
    if token in ("", "null", "~"):
        return None
    if token == "true":
        return True
    if token == "false":
        return False
    if len(token) >= 2 and token[0] == token[-1] and token[0] in "'\"":
        return token[1:-1]
    for parse in (int, float):
        try:
            return parse(token)
        except ValueError:
            pass
    return token


def _parse_block(items, index, indent):
    if items[index][1].startswith("- "):
        result = []
        while index < len(items) and items[index][0] == indent and items[index][1].startswith("- "):
            result.append(_parse_scalar(items[index][1][2:].strip()))
            index += 1
        return result, index
    mapping = {}
    while index < len(items) and items[index][0] == indent and not items[index][1].startswith("- "):
        line = items[index][1]
        if ":" not in line:
            raise ValueError(f"unsupported yaml line: {line!r}")
        key, _, rest = line.partition(":")
        index += 1
        rest = rest.strip()
        if rest:
            mapping[_parse_scalar(key.strip())] = _parse_scalar(rest)
            continue
        child = None
        if index < len(items) and items[index][0] > indent:
            child, index = _parse_block(items, index, items[index][0])
        elif index < len(items) and items[index][0] == indent and items[index][1].startswith("- "):
            child, index = _parse_block(items, index, indent)
        mapping[_parse_scalar(key.strip())] = child
    return mapping, index


def _dump_scalar(value):
    if value is None:
        return "null"
    if value is True:
        return "true"
    if value is False:
        return "false"
    if isinstance(value, (int, float)):
        return str(value)
    text = str(value)
    return text if _PLAIN_SCALAR.match(text) else json.dumps(text)


def _dump_lines(value, indent, lines):
    pad = " " * indent
    if isinstance(value, dict):
        for key, child in value.items():
            if isinstance(child, (dict, list)) and child:
                lines.append(f"{pad}{_dump_scalar(key)}:")
                _dump_lines(child, indent + 2, lines)
            else:
                child_repr = "{}" if child == {} else "[]" if child == [] else _dump_scalar(child)
                lines.append(f"{pad}{_dump_scalar(key)}: {child_repr}")
    elif isinstance(value, list):
        for item in value:
            lines.append(f"{pad}- {_dump_scalar(item)}")
    else:
        lines.append(f"{pad}{_dump_scalar(value)}")


def dump(data, stream=None, default_flow_style=False, **kwargs):
    lines = []
    _dump_lines(data, 0, lines)
    text = "\n".join(lines) + "\n"
    if stream is None:
        return text
    stream.write(text)
    return None
"##;

fn python3_has_real_yaml() -> bool {
    static HAS_YAML: OnceLock<bool> = OnceLock::new();
    *HAS_YAML.get_or_init(|| {
        Command::new("python3")
            .args(["-c", "import yaml"])
            .output()
            .map(|out| out.status.success())
            .unwrap_or(false)
    })
}

/// Returns a PYTHONPATH entry providing the `yaml` shim when the system
/// python3 has no real PyYAML, so config.yaml-dependent checks run on every
/// OS instead of failing on bare CI runners.
fn pyyaml_shim_pythonpath(scratch: &Path) -> Option<PathBuf> {
    if python3_has_real_yaml() {
        return None;
    }
    let shim_dir = scratch.join("pyyaml-shim");
    std::fs::create_dir_all(&shim_dir).unwrap();
    std::fs::write(shim_dir.join("yaml.py"), PYYAML_SHIM).unwrap();
    Some(shim_dir)
}

fn assert_python_compiles(paths: &[&Path]) {
    let output = Command::new("python3")
        .arg("-m")
        .arg("py_compile")
        .args(paths)
        .output()
        .expect("python3 should be available for Hermes generated Python syntax checks");
    assert!(
        output.status.success(),
        "generated Python should compile\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn cursor_plugin_install_dir(home: &Path) -> std::path::PathBuf {
    home.join(".cursor/plugins/local/tokensave")
}

fn assert_cursor_plugin_bundle(plugin_dir: &Path, expected_command: &str) {
    let manifest = read_json(&plugin_dir.join(".cursor-plugin/plugin.json"));
    assert_eq!(manifest["name"], "tokensave");
    assert_eq!(manifest["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(manifest["license"], "MIT");
    assert_eq!(manifest["mcpServers"], "mcp.json");
    assert_eq!(manifest["hooks"], "hooks/hooks.json");
    // Documented manifest metadata (displayName is not a documented field and
    // must not reappear; author/homepage/keywords are the documented ones).
    assert!(
        manifest.get("displayName").is_none(),
        "displayName is not a documented plugin.json field"
    );
    assert!(
        manifest["author"]["name"].is_string(),
        "plugin manifest should carry a documented author object"
    );
    assert!(manifest["homepage"].is_string());
    assert!(
        manifest["keywords"]
            .as_array()
            .is_some_and(|keywords| !keywords.is_empty()),
        "plugin manifest should carry keywords"
    );
    assert!(
        manifest.get("commands").is_none(),
        "the deprecated commands surface must not be referenced by the manifest"
    );
    assert!(
        manifest["rules"]
            .as_array()
            .is_some_and(|rules| rules.iter().any(|rule| rule == "rules/tokensave.mdc")),
        "plugin manifest should reference the tokensave Cursor rule"
    );

    let mcp = read_json(&plugin_dir.join("mcp.json"));
    let server = &mcp["mcpServers"]["tokensave"];
    assert_eq!(server["type"], "stdio");
    assert_eq!(server["command"], expected_command);
    assert_eq!(
        server["args"],
        serde_json::json!(["serve", "--path", "${workspaceFolder}"])
    );

    let hooks = read_json(&plugin_dir.join("hooks/hooks.json"));
    // The hint hook lives on postToolUse (the only generic tool event whose
    // documented output supports `additional_context`) and runs unmatched so
    // it also sees Read and Cursor's semantic search, whose matcher names are
    // not documented. afterFileEdit runs unmatched so every Agent edit tool
    // (not just Write) triggers the targeted sync.
    let expected_hooks = [
        ("sessionStart", "hook-cursor-session-start"),
        ("sessionEnd", "hook-cursor-session-end"),
        ("subagentStart", "hook-cursor-subagent-start"),
        ("postToolUse", "hook-cursor-post-tool-use"),
        ("beforeSubmitPrompt", "hook-cursor-before-submit-prompt"),
        ("afterFileEdit", "hook-cursor-after-file-edit"),
        ("afterShellExecution", "hook-cursor-after-shell"),
        ("workspaceOpen", "hook-cursor-workspace-open"),
        ("stop", "hook-cursor-stop"),
    ];
    for (event, subcommand) in expected_hooks {
        let entries = hooks["hooks"][event]
            .as_array()
            .unwrap_or_else(|| panic!("plugin hook {event} should be an array"));
        let hook = entries
            .iter()
            .find(|entry| {
                entry["command"]
                    .as_str()
                    .is_some_and(|command| command.contains(subcommand))
            })
            .unwrap_or_else(|| panic!("plugin hook {event} should call {subcommand}"));
        assert!(
            hook["command"]
                .as_str()
                .is_some_and(|command| command.starts_with(&format!("{expected_command} "))),
            "plugin hook commands should use the installed tokensave binary"
        );
        assert!(
            hook.get("matcher").is_none(),
            "plugin hook {event} should run unmatched (matchers either miss \
             undocumented tool names or restrict edits to Write only)"
        );
    }
    assert!(
        hooks["hooks"].get("preToolUse").is_none(),
        "the hint hook must not register on preToolUse: its documented output \
         schema has no context-injection field"
    );

    let rule = std::fs::read_to_string(plugin_dir.join("rules/tokensave.mdc")).unwrap();
    assert!(rule.contains("alwaysApply: true"));
    assert!(rule.contains("tokensave MCP tools"));
    assert!(rule.to_lowercase().contains("fall back"));
    assert!(plugin_dir.join("README.md").exists());
}

#[test]
fn test_cursor_plugin_bundle_files_are_valid() {
    assert_cursor_plugin_bundle(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("cursor-plugin")
            .as_path(),
        "tokensave",
    );
}

fn assert_hermes_config_enables_tokensave_memory(config_path: &Path) -> String {
    let config = std::fs::read_to_string(config_path).unwrap_or_else(|e| {
        panic!(
            "failed to read Hermes config {}: {e}",
            config_path.display()
        )
    });
    assert!(
        config.contains("memory:"),
        "missing memory block:\n{config}"
    );
    assert!(
        config.contains("  provider: tokensave"),
        "missing tokensave memory provider:\n{config}"
    );
    assert!(
        config.contains("plugins:"),
        "missing plugins block:\n{config}"
    );
    assert!(
        config.contains("enabled:"),
        "missing enabled block:\n{config}"
    );
    assert!(
        config.contains("- tokensave"),
        "missing tokensave plugin enablement:\n{config}"
    );
    assert!(
        config.contains("context:"),
        "missing context block (context.engine selects the plugin engine):\n{config}"
    );
    assert!(
        config.contains("  engine: tokensave"),
        "missing tokensave context engine activation:\n{config}"
    );
    config
}

#[test]
fn test_cursor_install_installs_local_plugin_without_global_mcp() {
    let home = TempDir::new().unwrap();
    let ctx = make_install_ctx(home.path());

    CursorIntegration.install(&ctx).unwrap();

    let plugin_dir = cursor_plugin_install_dir(home.path());
    assert_cursor_plugin_bundle(&plugin_dir, &ctx.tokensave_bin);
    assert!(
        !std::fs::symlink_metadata(&plugin_dir)
            .unwrap()
            .file_type()
            .is_symlink(),
        "Cursor install should write a real plugin directory, not a symlink"
    );
    assert!(
        !home.path().join(".cursor/mcp.json").exists(),
        "Cursor plugin install should not write legacy ~/.cursor/mcp.json"
    );
}

#[test]
fn test_local_install_cursor_installs_plugin_without_project_config() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();

    assert_local_install_success("cursor", project.path(), home.path());

    assert_cursor_plugin_bundle(
        &cursor_plugin_install_dir(home.path()),
        &expected_tokensave_bin(),
    );

    let mcp_path = project.path().join(".cursor/mcp.json");
    assert!(
        !mcp_path.exists(),
        "Cursor local install should not write legacy project MCP config"
    );
    assert!(
        !project.path().join(".cursor/hooks.json").exists(),
        "Cursor local install should not write legacy project hooks"
    );
    assert!(
        !project.path().join(".cursor/rules/tokensave.mdc").exists(),
        "Cursor local install should not write legacy project rule"
    );
    assert!(
        !project.path().join(".cursor/permissions.json").exists(),
        "Cursor local install should leave permissions to Cursor approval/run-mode behavior"
    );
    assert!(
        !home.path().join(".cursor/mcp.json").exists(),
        "local install must not write the legacy global Cursor MCP config"
    );
    assert!(
        !home.path().join(".tokensave/config.toml").exists(),
        "local install must not create or mutate user-level install tracking"
    );
}

#[tokio::test]
async fn test_local_install_cursor_tracks_current_branch_when_initialized() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let git_init = Command::new("git")
        .arg("init")
        .arg("-b")
        .arg("main")
        .current_dir(project.path())
        .output()
        .expect("git init should run");
    assert!(
        git_init.status.success(),
        "git init should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&git_init.stdout),
        String::from_utf8_lossy(&git_init.stderr)
    );
    std::fs::create_dir_all(project.path().join("src")).unwrap();
    std::fs::write(project.path().join("src/lib.rs"), "pub fn hello() {}\n").unwrap();
    TokenSave::init(project.path()).await.unwrap();
    let checkout = Command::new("git")
        .arg("checkout")
        .arg("-b")
        .arg("feature/install")
        .current_dir(project.path())
        .output()
        .expect("git checkout should run");
    assert!(
        checkout.status.success(),
        "git checkout should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&checkout.stdout),
        String::from_utf8_lossy(&checkout.stderr)
    );

    assert_local_install_success("cursor", project.path(), home.path());

    let meta = branch_meta::load_branch_meta(&get_tokensave_dir(project.path()))
        .expect("Cursor install should bootstrap branch tracking metadata");
    assert!(meta.is_tracked("main"));
    assert!(meta.is_tracked("feature/install"));
}

#[test]
fn test_local_install_cursor_preserves_existing_permissions_file() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let cursor_dir = project.path().join(".cursor");
    std::fs::create_dir_all(&cursor_dir).unwrap();
    std::fs::write(
        cursor_dir.join("permissions.json"),
        r#"{
  "mcpAllowlist": [
    "other:custom_tool",
    "tokensave:tokensave_not_a_real_tool",
    "tokensave:tokensave_str_replace"
  ]
}
"#,
    )
    .unwrap();

    assert_local_install_success("cursor", project.path(), home.path());

    let permissions = std::fs::read_to_string(cursor_dir.join("permissions.json")).unwrap();
    assert!(permissions.contains("other:custom_tool"));
    assert!(permissions.contains("tokensave:tokensave_not_a_real_tool"));
    assert!(permissions.contains("tokensave:tokensave_str_replace"));
}

#[test]
fn test_cursor_healthcheck_ignores_foreign_project_cursor_files() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    CursorIntegration
        .install(&make_install_ctx(home.path()))
        .unwrap();

    let cursor_dir = project.path().join(".cursor");
    std::fs::create_dir_all(cursor_dir.join("rules")).unwrap();
    std::fs::write(
        cursor_dir.join("mcp.json"),
        r#"{"mcpServers":{"other":{"command":"other-bin"}}}"#,
    )
    .unwrap();
    std::fs::write(
        cursor_dir.join("hooks.json"),
        r#"{"version":1,"hooks":{"afterFileEdit":[{"command":"other-hook","timeout":30}]}}"#,
    )
    .unwrap();
    std::fs::write(
        cursor_dir.join("rules/tokensave.mdc"),
        "---\nalwaysApply: false\n---\nforeign rule\n",
    )
    .unwrap();

    let mut dc = DoctorCounters::new();
    CursorIntegration.healthcheck(
        &mut dc,
        &HealthcheckContext {
            home: home.path().to_path_buf(),
            project_path: project.path().to_path_buf(),
        },
    );

    assert_eq!(dc.warnings, 0, "foreign Cursor files should not warn");
    assert_eq!(
        dc.issues, 0,
        "foreign Cursor files should not fail healthcheck"
    );
}

#[test]
fn test_hermes_local_install_writes_profile_plugin() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();

    let output = assert_local_install_success("hermes", project.path(), home.path());

    let plugin_dir = project.path().join(".hermes/plugins/tokensave");
    let manifest = std::fs::read_to_string(plugin_dir.join("plugin.yaml")).unwrap();
    assert!(manifest.contains("name: tokensave"));
    assert!(manifest.contains("kind: standalone"));
    // `hermes plugins list` shows the manifest version; it must track the
    // generating binary so stale plugins are detectable after upgrades.
    assert!(
        manifest.contains(&format!("version: {}\n", env!("CARGO_PKG_VERSION"))),
        "manifest version must match the generating binary:\n{manifest}"
    );
    assert!(manifest.contains("author: "));
    assert!(manifest.contains("provides_tools:"));
    assert!(manifest.contains("tokensave_context"));
    assert!(manifest.contains("tokensave_lcm_status"));
    assert!(manifest.contains("tokensave_lcm_compress"));
    assert!(manifest.contains("provides_hooks:"));
    assert!(manifest.contains("pre_llm_call"));
    assert!(manifest.contains("provides_commands:"));
    assert!(manifest.contains("/tokensave_status"));

    let init_py = std::fs::read_to_string(plugin_dir.join("__init__.py")).unwrap();
    assert!(init_py.contains("def register(ctx):"));
    assert!(init_py.contains("class TokensaveMemoryProvider"));
    assert!(init_py.contains("ctx.register_memory_provider("));
    assert!(init_py.contains("register_tool = getattr(ctx, \"register_tool\", None)"));
    assert!(init_py.contains("ctx.register_hook(\"pre_llm_call\""));
    assert!(init_py.contains("getattr(ctx, \"register_command\", None)"));
    assert!(init_py.contains("getattr(ctx, \"register_skill\", None)"));
    assert!(init_py.contains("register_skill(\"tokensave\""));
    assert!(init_py.contains("class TokenSaveContextEngine"));
    assert!(init_py.contains("storage_scope"));
    assert!(init_py.contains("tokensave_lcm_compress"));

    let schemas_py = std::fs::read_to_string(plugin_dir.join("schemas.py")).unwrap();
    assert!(schemas_py.contains("TOOL_SCHEMAS"));
    assert!(schemas_py.contains("json.load"));
    let schemas_json = read_json(&plugin_dir.join("schemas.json"));
    assert!(schemas_json.as_array().is_some_and(|schemas| schemas
        .iter()
        .any(|schema| schema["name"] == "tokensave_context")));

    let tools_py = std::fs::read_to_string(plugin_dir.join("tools.py")).unwrap();
    assert!(tools_py.contains(&expected_tokensave_bin()));
    assert!(tools_py.contains("subprocess.run"));
    assert!(tools_py.contains("tokensave tool"));
    assert!(tools_py.contains("TOKENSAVE_TIMEOUT_SECONDS = 600"));
    assert!(tools_py.contains("truncate_output"));
    assert!(tools_py.contains("\"stderr\""));
    assert!(tools_py.contains("\"stdout\""));
    assert!(tools_py.contains("kwargs.get(\"project_root\")"));
    assert!(tools_py.contains("or tool_args.get(\"project_root\")"));
    assert!(
        tools_py.contains("or PINNED_PROJECT_ROOT"),
        "tool dispatch must fall back to the install-time project pin"
    );
    assert!(tools_py.contains("argv.extend([\"--project\", str(project_root)])"));
    assert!(tools_py.contains("argv.extend([name, \"--json\", \"--args\", payload])"));
    assert!(!tools_py.contains("shell=True"));
    assert_python_compiles(&[
        &plugin_dir.join("tools.py"),
        &plugin_dir.join("schemas.py"),
        &plugin_dir.join("__init__.py"),
    ]);

    let skill = std::fs::read_to_string(plugin_dir.join("skills/tokensave/SKILL.md")).unwrap();
    assert!(skill.contains("Use tokensave"));

    assert_hermes_config_enables_tokensave_memory(&project.path().join(".hermes/config.yaml"));
    assert!(
        !home.path().join(".hermes/config.yaml").exists(),
        "plain local install must not mutate the user profile config"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    let expected_hermes_homes = [
        project.path().join(".hermes"),
        std::fs::canonicalize(project.path())
            .unwrap_or_else(|_| project.path().to_path_buf())
            .join(".hermes"),
    ];
    assert!(
        expected_hermes_homes
            .iter()
            .any(|path| stderr.contains(&format!("HERMES_HOME={}", path.display()))),
        "plain local install guidance must tell users to launch Hermes with the project-local HERMES_HOME\nstderr:\n{stderr}"
    );
}

#[test]
fn test_hermes_generated_python_registers_lcm_context_engine() {
    let home = TempDir::new().unwrap();
    HermesIntegration
        .install(&make_install_ctx(home.path()))
        .unwrap();

    let init_py =
        std::fs::read_to_string(home.path().join(".hermes/plugins/tokensave/__init__.py")).unwrap();

    assert!(init_py.contains("class TokenSaveContextEngine"));
    assert!(init_py.contains("ctx.register_context_engine"));
    assert!(init_py.contains("storage_scope"));
    assert!(init_py.contains("def call_tokensave_json"));
    assert!(init_py.contains("tokensave_lcm_status"));
    assert!(init_py.contains("call_tokensave_json(\"tokensave_lcm_preflight\""));
    assert!(init_py.contains("call_tokensave_json(\"tokensave_lcm_compress\""));
    assert!(init_py.contains("tokensave_lcm_session_boundary"));
    // Both registered provider identities are "tokensave"; "lcm" is reserved
    // for the tool surface (lcm_* / tokensave_lcm_*), not the engine name.
    assert!(init_py.contains("return \"tokensave\""));
    assert!(
        !init_py.contains("return \"lcm\""),
        "context engine identity must be \"tokensave\", not \"lcm\""
    );

    // The context engine exposes the full native LCM tool surface: every
    // native lcm_* tool must be aliased to its tokensave_lcm_* MCP tool and
    // ship a native schema entry.
    let native_lcm_tools = [
        "lcm_grep",
        "lcm_load_session",
        "lcm_describe",
        "lcm_expand",
        "lcm_expand_query",
        "lcm_status",
        "lcm_doctor",
    ];
    for native in native_lcm_tools {
        assert!(
            init_py.contains(&format!("\"{native}\": \"tokensave_{native}\"")),
            "__init__.py LCM_TOOL_ALIASES must map {native} -> tokensave_{native}"
        );
        assert!(
            init_py.contains(&format!("\"name\": \"{native}\"")),
            "__init__.py LCM_NATIVE_SCHEMAS must define a schema for {native}"
        );
    }

    // Every tokensave_lcm_* MCP tool must be declared in the generated plugin
    // manifest and tool schemas so the embedded constants cannot drift from
    // the current LCM tool surface.
    let manifest =
        std::fs::read_to_string(home.path().join(".hermes/plugins/tokensave/plugin.yaml")).unwrap();
    let schemas_json =
        std::fs::read_to_string(home.path().join(".hermes/plugins/tokensave/schemas.json"))
            .unwrap();
    let lcm_tool_names: Vec<String> = tool_names()
        .into_iter()
        .filter(|name| name.starts_with("tokensave_lcm_"))
        .collect();
    assert_eq!(
        lcm_tool_names.len(),
        10,
        "expected the 10 LCM MCP tools, got {lcm_tool_names:?}"
    );
    for name in &lcm_tool_names {
        assert!(
            manifest.contains(&format!("  - {name}")),
            "plugin.yaml provides_tools must list {name}"
        );
        assert!(
            schemas_json.contains(&format!("\"name\": \"{name}\"")),
            "schemas.json must contain a schema for {name}"
        );
    }
}

#[test]
fn test_hermes_generated_python_handles_quoted_unicode_tokensave_path() {
    let home = TempDir::new().unwrap();
    let tokensave_bin = home.path().join("bin with spaces").join("token\"save-π");
    let ctx = InstallContext {
        home: home.path().to_path_buf(),
        tokensave_bin: tokensave_bin.to_string_lossy().to_string(),
        tool_permissions: expected_tool_perms(),
        profile: None,
        project_root: None,
        dashboard: true,
    };

    HermesIntegration.install(&ctx).unwrap();

    let plugin_dir = home.path().join(".hermes/plugins/tokensave");
    assert_python_compiles(&[
        &plugin_dir.join("tools.py"),
        &plugin_dir.join("schemas.py"),
        &plugin_dir.join("__init__.py"),
    ]);

    let script = plugin_dir.join("check_tools.py");
    std::fs::write(
        &script,
        r#"
import importlib.util
import json
import os
import pathlib
import sys

tools_path = pathlib.Path(sys.argv[1])
expected_bin = sys.argv[2]
# Hermetic profile home so plugins.tokensave from the developer's real
# ~/.hermes config can never leak a --project argument into the argv checks.
os.environ["HERMES_HOME"] = str(tools_path.parent.parent.parent)
spec = importlib.util.spec_from_file_location("tokensave_hermes_tools", tools_path)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)

assert module.TOKENSAVE_BIN == expected_bin

class Result:
    returncode = 7
    stdout = "stdout-" * 1000
    stderr = "stderr-" * 1000

def fake_run(argv, **kwargs):
    assert argv[0] == expected_bin
    assert argv[1:] == ["tool", "tokensave_context", "--json", "--args", "{\"query\": \"x\"}"]
    assert "cwd" not in kwargs
    assert kwargs["timeout"] == 600
    assert kwargs["shell"] is False
    return Result()

module.subprocess.run = fake_run
payload = json.loads(module.call_tokensave_tool("tokensave_context", {"query": "x"}))
assert payload["error"] == "tokensave tool exited with status 7"
assert payload["stdout"].startswith("stdout-")
assert payload["stderr"].startswith("stderr-")
assert payload["stdout"].endswith("...<truncated>")
assert payload["stderr"].endswith("...<truncated>")
"#,
    )
    .unwrap();

    let output = Command::new("python3")
        .arg(&script)
        .arg(plugin_dir.join("tools.py"))
        .arg(tokensave_bin)
        .output()
        .expect("python3 should run generated Hermes tools import check");
    assert!(
        output.status.success(),
        "generated tools.py should import and expose diagnosable errors\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_hermes_generated_python_registers_memory_provider() {
    let home = TempDir::new().unwrap();
    HermesIntegration
        .install(&make_install_ctx_with_real_bin(home.path()))
        .unwrap();

    let plugin_dir = home.path().join(".hermes/plugins/tokensave");
    assert_python_compiles(&[
        &plugin_dir.join("tools.py"),
        &plugin_dir.join("schemas.py"),
        &plugin_dir.join("__init__.py"),
    ]);

    let script = plugin_dir.join("check_memory_provider.py");
    std::fs::write(
        &script,
        r#"
import importlib
import importlib.machinery
import importlib.util
import abc
import json
import os
import pathlib
import sys
import types

plugin_dir = pathlib.Path(sys.argv[1])
os.environ["HERMES_HOME"] = str(plugin_dir.parent.parent)

class MemoryProvider(abc.ABC):
    @property
    @abc.abstractmethod
    def name(self):
        pass

    @abc.abstractmethod
    def is_available(self):
        pass

    @abc.abstractmethod
    def initialize(self, session_id, **kwargs):
        pass

    @abc.abstractmethod
    def get_tool_schemas(self):
        pass

agent_module = types.ModuleType("agent")
memory_provider_module = types.ModuleType("agent.memory_provider")
memory_provider_module.MemoryProvider = MemoryProvider
sys.modules["agent"] = agent_module
sys.modules["agent.memory_provider"] = memory_provider_module

parent_name = "_hermes_user_memory"
parent_spec = importlib.machinery.ModuleSpec(parent_name, None, is_package=True)
parent_spec.submodule_search_locations = []
parent_module = importlib.util.module_from_spec(parent_spec)
sys.modules[parent_name] = parent_module

module_name = f"{parent_name}.tokensave"
spec = importlib.util.spec_from_file_location(
    module_name,
    plugin_dir / "__init__.py",
    submodule_search_locations=[str(plugin_dir)],
)
plugin = importlib.util.module_from_spec(spec)
sys.modules[module_name] = plugin
spec.loader.exec_module(plugin)

class FullCtx:
    context_engine_tool_handlers_receive_messages = True

    def __init__(self):
        self.tools = []
        self.hooks = []
        self.commands = []
        self.skills = []
        self.memory_providers = []
        self.config_defaults = []

    def register_tool(self, **kwargs):
        self.tools.append(kwargs)

    def register_hook(self, name, handler):
        self.hooks.append((name, handler))

    def register_command(self, name, handler, **kwargs):
        self.commands.append((name, handler, kwargs))

    def register_skill(self, name, path):
        self.skills.append((name, path))

    def register_memory_provider(self, provider):
        self.memory_providers.append(provider)

    def register_config_defaults(self, defaults):
        self.config_defaults.append(defaults)

ctx = FullCtx()
plugin.register(ctx)
assert any(tool["name"] == "tokensave_context" for tool in ctx.tools)
assert ctx.hooks and ctx.hooks[0][0] == "pre_llm_call"
assert ctx.commands and ctx.commands[0][0] == "/tokensave_status"
assert ctx.skills and ctx.skills[0][0] == "tokensave"
assert len(ctx.memory_providers) == 1

# Conventional config defaults registered under the plugins.tokensave block.
assert len(ctx.config_defaults) == 1
defaults = ctx.config_defaults[0]
assert set(defaults) == {"plugins"}
assert "project_root" in defaults["plugins"]["tokensave"]

provider = ctx.memory_providers[0]
assert isinstance(provider, MemoryProvider)
assert provider.name == "tokensave"
assert provider.provider_id == "tokensave"
assert provider.is_available() is True
original_bin = plugin.tools.TOKENSAVE_BIN
plugin.tools.TOKENSAVE_BIN = "/definitely/missing/tokensave"
assert provider.is_available() is False
plugin.tools.TOKENSAVE_BIN = original_bin
provider.initialize("session-123", hermes_home="/tmp/hermes-profile")
assert provider.hermes_home == "/tmp/hermes-profile"
assert provider.session_id == "session-123"
provider.initialize("session-only")
assert provider.hermes_home is None
assert provider.session_id == "session-only"

schemas = provider.get_tool_schemas()
schema_names = [schema.get("name") for schema in schemas]
assert schema_names == [
    "fact_store",
    "fact_add",
    "fact_search",
    "fact_probe",
    "fact_related",
    "fact_reason",
    "fact_contradict",
    "fact_update",
    "fact_remove",
    "fact_list",
    "fact_feedback",
    "memory_status",
]
assert all("function" not in schema for schema in schemas)
schema_by_name = {schema["name"]: schema for schema in schemas}
fact_store_schema = schema_by_name["fact_store"]
fact_feedback_schema = schema_by_name["fact_feedback"]
assert fact_store_schema["parameters"]["required"] == ["action"]
assert "action" not in schema_by_name["fact_search"]["parameters"].get("required", [])
assert fact_feedback_schema["parameters"]["required"] == ["fact_id"]

calls = []

def fake_call(name, args, **kwargs):
    calls.append((name, args, kwargs))
    return json.dumps({"name": name, "args": args})

plugin.tools.call_tokensave_tool = fake_call
store_result = provider.handle_tool_call("fact_store", {"action": "list"}, request_id="r1")
feedback_result = provider.handle_tool_call("fact_feedback", {"fact_id": 7, "helpful": True})
search_result = provider.handle_tool_call("fact_search", {"query": "Project Phoenix"})
status_result = provider.handle_tool_call("memory_status", None)
assert isinstance(store_result, str)
assert isinstance(feedback_result, str)
assert isinstance(search_result, str)
assert isinstance(status_result, str)
assert json.loads(store_result)["name"] == "tokensave_fact_store"
assert json.loads(feedback_result)["name"] == "tokensave_fact_feedback"
assert json.loads(search_result)["name"] == "tokensave_fact_store"
assert json.loads(status_result)["name"] == "tokensave_memory_status"
assert calls[0][0] == "tokensave_fact_store"
assert calls[0][1] == {"action": "list"}
assert calls[0][2]["request_id"] == "r1"
assert calls[1][0] == "tokensave_fact_feedback"
assert calls[2][0] == "tokensave_fact_store"
assert calls[2][1] == {"query": "Project Phoenix", "action": "search"}
assert calls[3][0] == "tokensave_memory_status"
assert calls[3][1] == {}

class LegacyCtx:
    context_engine_tool_handlers_receive_messages = True

    def __init__(self):
        self.tools = []
        self.hooks = []

    def register_tool(self, **kwargs):
        self.tools.append(kwargs)

    def register_hook(self, name, handler):
        self.hooks.append((name, handler))

    def register_skill(self, name, path):
        pass

legacy = LegacyCtx()
plugin.register(legacy)
assert any(tool["name"] == "tokensave_context" for tool in legacy.tools)
assert legacy.hooks and legacy.hooks[0][0] == "pre_llm_call"

class ProviderCollector:
    def __init__(self):
        self.provider = None

    def register_memory_provider(self, provider):
        self.provider = provider

    def register_tool(self, *args, **kwargs):
        pass

    def register_hook(self, *args, **kwargs):
        pass

    def register_cli_command(self, *args, **kwargs):
        pass

collector = ProviderCollector()
plugin.register(collector)
assert collector.provider is not None
assert collector.provider.name == "tokensave"
"#,
    )
    .unwrap();

    let output = Command::new("python3")
        .arg(&script)
        .arg(plugin_dir)
        .output()
        .expect("python3 should run generated Hermes memory provider check");
    assert!(
        output.status.success(),
        "generated plugin should register a Hermes memory provider\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_hermes_generated_memory_provider_is_discovered_from_active_home() {
    let home = TempDir::new().unwrap();
    HermesIntegration
        .install(&make_install_ctx_with_real_bin(home.path()))
        .unwrap();

    let hermes_home = home.path().join(".hermes");
    let plugin_dir = hermes_home.join("plugins/tokensave");
    let script = plugin_dir.join("check_hermes_discovery.py");
    std::fs::write(
        &script,
        r#"
import abc
import importlib.machinery
import importlib.util
import os
import pathlib
import sys
import types

hermes_home = pathlib.Path(sys.argv[1])
os.environ["HERMES_HOME"] = str(hermes_home)

class MemoryProvider(abc.ABC):
    @property
    @abc.abstractmethod
    def name(self):
        pass

    @abc.abstractmethod
    def is_available(self):
        pass

    @abc.abstractmethod
    def initialize(self, session_id, **kwargs):
        pass

    @abc.abstractmethod
    def get_tool_schemas(self):
        pass

    def get_config_schema(self):
        return []

    def save_config(self, values, hermes_home):
        pass

agent_module = types.ModuleType("agent")
memory_provider_module = types.ModuleType("agent.memory_provider")
memory_provider_module.MemoryProvider = MemoryProvider
sys.modules["agent"] = agent_module
sys.modules["agent.memory_provider"] = memory_provider_module

def is_memory_provider_dir(path):
    init_file = path / "__init__.py"
    if not init_file.exists():
        return False
    source = init_file.read_text(errors="replace")[:8192]
    return "register_memory_provider" in source or "MemoryProvider" in source

def iter_user_provider_dirs():
    plugins_dir = hermes_home / "plugins"
    for child in sorted(plugins_dir.iterdir()):
        if child.is_dir() and not child.name.startswith(("_", ".")) and is_memory_provider_dir(child):
            yield child.name, child

def load_provider(provider_dir):
    parent_name = "_hermes_user_memory"
    if parent_name not in sys.modules:
        parent_spec = importlib.machinery.ModuleSpec(parent_name, None, is_package=True)
        parent_spec.submodule_search_locations = []
        sys.modules[parent_name] = importlib.util.module_from_spec(parent_spec)

    module_name = f"{parent_name}.{provider_dir.name}"
    spec = importlib.util.spec_from_file_location(
        module_name,
        provider_dir / "__init__.py",
        submodule_search_locations=[str(provider_dir)],
    )
    module = importlib.util.module_from_spec(spec)
    sys.modules[module_name] = module
    spec.loader.exec_module(module)

    class ProviderCollector:
        def __init__(self):
            self.provider = None
        def register_memory_provider(self, provider):
            self.provider = provider
        def register_tool(self, *args, **kwargs):
            pass
        def register_hook(self, *args, **kwargs):
            pass
        def register_cli_command(self, *args, **kwargs):
            pass

    collector = ProviderCollector()
    module.register(collector)
    return collector.provider

config = (hermes_home / "config.yaml").read_text()
assert "memory:" in config
assert "provider: tokensave" in config
assert "context:" in config
assert "engine: tokensave" in config

providers = dict(iter_user_provider_dirs())
assert "tokensave" in providers
provider = load_provider(providers["tokensave"])
assert provider is not None
assert isinstance(provider, MemoryProvider)
assert provider.name == "tokensave"
assert provider.is_available() is True

# `hermes memory setup` walks get_config_schema(); the pin is the only field.
schema = provider.get_config_schema()
assert [field["key"] for field in schema] == ["project_root"]
assert "description" in schema[0]
assert not any(field.get("secret") for field in schema)

# Hermes layers get_config_defaults() under DEFAULT_CONFIG.
defaults = provider.get_config_defaults()
assert "project_root" in defaults["plugins"]["tokensave"]

# Dashboard hints use full config dot-paths.
field_meta = provider.get_config_field_meta()
assert "plugins.tokensave.project_root" in field_meta
assert "description" in field_meta["plugins.tokensave.project_root"]

# `hermes memory setup` persists non-secret values via save_config(); the
# conventional home is the plugins.tokensave block.
import yaml
provider.save_config({"project_root": "/pinned/by/setup"}, str(hermes_home))
saved = yaml.safe_load((hermes_home / "config.yaml").read_text())
assert saved["plugins"]["tokensave"]["project_root"] == "/pinned/by/setup"
assert saved["memory"]["provider"] == "tokensave"

provider.initialize("doctor-session", hermes_home=str(hermes_home), platform="cli")
assert provider.hermes_home == str(hermes_home)
assert "fact_search" in [schema["name"] for schema in provider.get_tool_schemas()]
assert "memory_status" in [schema["name"] for schema in provider.get_tool_schemas()]
"#,
    )
    .unwrap();

    let mut check = Command::new("python3");
    check.arg(&script).arg(hermes_home);
    if let Some(shim_dir) = pyyaml_shim_pythonpath(home.path()) {
        check.env("PYTHONPATH", shim_dir);
    }
    let output = check
        .output()
        .expect("python3 should run Hermes memory provider discovery check");
    assert!(
        output.status.success(),
        "Hermes-style memory provider discovery should find the generated tokensave provider\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Stock-ABC variant harness: upstream (NousResearch/hermes-agent) Hermes
/// differs from forks in two load-bearing ways — the ABCs enforce a fixed
/// abstract-method set (instantiation raises `TypeError` on any miss), and
/// the general `PluginContext` has **no** `register_memory_provider` or
/// `register_config_defaults` (memory providers load through the
/// `plugins/memory` `_ProviderCollector` instead). The generated plugin must
/// keep memory + context + tools functional on that surface and skip
/// fork-only registrations without raising.
#[test]
fn test_hermes_generated_python_degrades_gracefully_on_stock_hermes_api() {
    let home = TempDir::new().unwrap();
    HermesIntegration
        .install(&make_install_ctx_with_real_bin(home.path()))
        .unwrap();

    let plugin_dir = home.path().join(".hermes/plugins/tokensave");
    let script = plugin_dir.join("check_stock_abi.py");
    std::fs::write(
        &script,
        r#"
import abc
import importlib.machinery
import importlib.util
import json
import os
import pathlib
import sys
import types

plugin_dir = pathlib.Path(sys.argv[1])
os.environ["HERMES_HOME"] = str(plugin_dir.parent.parent)

# Stock agent/memory_provider.py abstract surface (upstream pins these four).
class MemoryProvider(abc.ABC):
    @property
    @abc.abstractmethod
    def name(self):
        pass

    @abc.abstractmethod
    def is_available(self):
        pass

    @abc.abstractmethod
    def initialize(self, session_id, **kwargs):
        pass

    @abc.abstractmethod
    def get_tool_schemas(self):
        pass

# Stock agent/context_engine.py abstract surface. Instantiating the generated
# engine under this ABC proves every stock-abstract method is implemented
# (update_from_response is the one newer stock releases added).
class ContextEngine(abc.ABC):
    @property
    @abc.abstractmethod
    def name(self):
        pass

    @abc.abstractmethod
    def update_from_response(self, usage):
        pass

    @abc.abstractmethod
    def should_compress(self, prompt_tokens=None):
        pass

    @abc.abstractmethod
    def compress(self, messages, current_tokens=None, focus_topic=None, **kwargs):
        pass

agent_module = types.ModuleType("agent")
memory_provider_module = types.ModuleType("agent.memory_provider")
memory_provider_module.MemoryProvider = MemoryProvider
context_engine_module = types.ModuleType("agent.context_engine")
context_engine_module.ContextEngine = ContextEngine
sys.modules["agent"] = agent_module
sys.modules["agent.memory_provider"] = memory_provider_module
sys.modules["agent.context_engine"] = context_engine_module

parent_name = "_hermes_stock_plugins"
parent_spec = importlib.machinery.ModuleSpec(parent_name, None, is_package=True)
parent_spec.submodule_search_locations = []
sys.modules[parent_name] = importlib.util.module_from_spec(parent_spec)

module_name = f"{parent_name}.tokensave"
spec = importlib.util.spec_from_file_location(
    module_name,
    plugin_dir / "__init__.py",
    submodule_search_locations=[str(plugin_dir)],
)
plugin = importlib.util.module_from_spec(spec)
sys.modules[module_name] = plugin
spec.loader.exec_module(plugin)

# Stock general PluginContext: hooks/commands/skills/context engine/tools
# exist; register_memory_provider, register_config_defaults, and the
# context_engine_tool_handlers_receive_messages capability do not.
class StockPluginContext:
    def __init__(self):
        self.tools = []
        self.hooks = []
        self.commands = []
        self.cli_commands = []
        self.middleware = []
        self.skills = []
        self.context_engine = None

    def register_tool(self, **kwargs):
        self.tools.append(kwargs)

    def register_cli_command(self, *args, **kwargs):
        self.cli_commands.append((args, kwargs))

    def register_command(self, name, handler, description="", args_hint=""):
        self.commands.append((name, handler, description))

    def register_context_engine(self, engine):
        assert self.context_engine is None, "only one context engine is allowed"
        self.context_engine = engine

    def register_hook(self, hook_name, callback):
        self.hooks.append((hook_name, callback))

    def register_middleware(self, kind, callback):
        self.middleware.append((kind, callback))

    def register_skill(self, name, path, description=""):
        self.skills.append((name, path))

ctx = StockPluginContext()
plugin.register(ctx)

# Core registrations stay functional on the stock surface.
assert ctx.hooks and ctx.hooks[0][0] == "pre_llm_call"
assert ctx.commands and ctx.commands[0][0] == "/tokensave_status"
assert ctx.skills and ctx.skills[0][0] == "tokensave"
assert ctx.context_engine is not None
assert isinstance(ctx.context_engine, ContextEngine)
assert ctx.context_engine.name == "tokensave"
# Stock never advertises message forwarding for registered tool handlers, so
# direct tool registration must degrade to a silent skip — the LCM surface
# stays reachable through the context engine schemas instead.
assert ctx.tools == []
lcm_names = [schema["name"] for schema in ctx.context_engine.get_tool_schemas()]
assert "lcm_status" in lcm_names and "lcm_grep" in lcm_names

# The stock-abstract ContextEngine methods round-trip.
engine = ctx.context_engine
engine.update_from_response({"prompt_tokens": 11, "completion_tokens": 4})
assert engine.last_total_tokens == 15

calls = []

def fake_call(name, args, **kwargs):
    calls.append((name, args))
    return json.dumps({"content": [{"type": "text", "text": json.dumps({"should_compress": False})}]})

original_call = plugin.tools.call_tokensave_tool
plugin.tools.call_tokensave_tool = fake_call
assert engine.should_compress(123) is False
assert calls and calls[0][0] == "tokensave_lcm_preflight"
plugin.tools.call_tokensave_tool = original_call

# Stock memory activation: plugins/memory drives register() through its
# _ProviderCollector (exactly these four methods — notably no
# register_command, register_context_engine, or register_skill).
class StockProviderCollector:
    def __init__(self):
        self.provider = None

    def register_memory_provider(self, provider):
        self.provider = provider

    def register_tool(self, *args, **kwargs):
        pass

    def register_hook(self, *args, **kwargs):
        pass

    def register_cli_command(self, *args, **kwargs):
        pass

collector = StockProviderCollector()
plugin.register(collector)
assert collector.provider is not None
assert isinstance(collector.provider, MemoryProvider)
assert collector.provider.name == "tokensave"
assert collector.provider.is_available() is True
assert collector.provider.get_tool_schemas()

# Stock fallback branch: when register() yields no provider, the loader
# instantiates any module-level MemoryProvider subclass directly.
fallback = plugin.TokensaveMemoryProvider()
assert isinstance(fallback, MemoryProvider)
assert fallback.name == "tokensave"
"#,
    )
    .unwrap();

    let output = Command::new("python3")
        .arg(&script)
        .arg(plugin_dir)
        .output()
        .expect("python3 should run stock Hermes API degradation check");
    assert!(
        output.status.success(),
        "generated plugin should degrade gracefully on the stock Hermes plugin API\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_hermes_global_install_and_uninstall_plugin() {
    let home = TempDir::new().unwrap();
    let ctx = make_install_ctx(home.path());

    HermesIntegration.install(&ctx).unwrap();

    let plugin_dir = home.path().join(".hermes/plugins/tokensave");
    assert!(plugin_dir.join("plugin.yaml").exists());
    assert!(plugin_dir.join("__init__.py").exists());
    let config = std::fs::read_to_string(home.path().join(".hermes/config.yaml")).unwrap();
    assert!(config.contains("- tokensave"));

    HermesIntegration.uninstall(&ctx).unwrap();
    assert!(
        !plugin_dir.exists(),
        "uninstall should remove only the tokensave Hermes plugin directory"
    );
    let config = std::fs::read_to_string(home.path().join(".hermes/config.yaml")).unwrap();
    assert!(
        !config.contains("- tokensave"),
        "uninstall should remove tokensave from plugins.enabled"
    );
    assert!(
        !config.contains("memory:\n"),
        "uninstall should remove the empty tokensave-created memory block"
    );
    assert!(
        !config.contains("engine: tokensave") && !config.contains("context:\n"),
        "uninstall should remove the tokensave context engine activation:\n{config}"
    );
}

#[test]
fn test_hermes_profile_install_targets_named_profile() {
    let home = TempDir::new().unwrap();
    let ctx = InstallContext {
        home: home.path().to_path_buf(),
        tokensave_bin: "/usr/local/bin/tokensave".to_string(),
        tool_permissions: expected_tool_perms(),
        profile: Some("Work_Profile".to_string()),
        project_root: None,
        dashboard: true,
    };

    HermesIntegration.install(&ctx).unwrap();

    let plugin_dir = home
        .path()
        .join(".hermes/profiles/work_profile/plugins/tokensave");
    assert!(plugin_dir.join("plugin.yaml").exists());
    assert!(!home.path().join(".hermes/plugins/tokensave").exists());
    let config = std::fs::read_to_string(
        home.path()
            .join(".hermes/profiles/work_profile/config.yaml"),
    )
    .expect("profile config should be written");
    assert_hermes_config_enables_tokensave_memory(
        &home
            .path()
            .join(".hermes/profiles/work_profile/config.yaml"),
    );
    assert!(config.contains("- tokensave"));

    HermesIntegration.uninstall(&ctx).unwrap();
    assert!(!plugin_dir.exists());
    assert!(home.path().join(".hermes/profiles/work_profile").exists());
}

#[test]
fn test_hermes_install_all_profiles_configures_default_and_named_profiles() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let default_profile = home.path().join(".hermes");
    let work_profile = home.path().join(".hermes/profiles/work");
    let personal_profile = home.path().join(".hermes/profiles/personal");
    std::fs::create_dir_all(&work_profile).unwrap();
    std::fs::create_dir_all(&personal_profile).unwrap();
    std::fs::write(
        default_profile.join("config.yaml"),
        "theme: dark\nplugins:\n  enabled:\n    - other\n",
    )
    .unwrap();
    std::fs::write(
        work_profile.join("config.yaml"),
        "theme: light\nmemory:\n  retention: session\nplugins:\n  disabled:\n    - tokensave\n    - other-disabled\n",
    )
    .unwrap();

    let output = tokensave_command(project.path(), home.path())
        .arg("install")
        .arg("--agent")
        .arg("hermes")
        .arg("--all-profiles")
        .output()
        .expect("run hermes all-profiles install");
    assert!(
        output.status.success(),
        "hermes all-profiles install should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    for profile in [&default_profile, &work_profile, &personal_profile] {
        assert!(
            profile.join("plugins/tokensave/plugin.yaml").exists(),
            "tokensave plugin should be installed in {}",
            profile.display()
        );
        assert_hermes_config_enables_tokensave_memory(&profile.join("config.yaml"));
    }
    let default_config = std::fs::read_to_string(default_profile.join("config.yaml")).unwrap();
    assert!(default_config.contains("    - other"));
    let work_config = std::fs::read_to_string(work_profile.join("config.yaml")).unwrap();
    assert!(work_config.contains("  retention: session"));
    assert!(!work_config.contains("  disabled:\n    - tokensave"));
    assert!(
        !project
            .path()
            .join(".hermes/plugins/tokensave/plugin.yaml")
            .exists(),
        "profile-level all-profiles install must not write a project-local plugin"
    );
}

#[test]
fn test_hermes_uninstall_all_profiles_cleans_only_tokensave_from_each_profile() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let default_profile = home.path().join(".hermes");
    let work_profile = home.path().join(".hermes/profiles/work");
    let personal_profile = home.path().join(".hermes/profiles/personal");

    for profile in [&default_profile, &work_profile, &personal_profile] {
        let plugin_dir = profile.join("plugins/tokensave");
        let other_plugin_dir = profile.join("plugins/other");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::create_dir_all(&other_plugin_dir).unwrap();
        std::fs::write(plugin_dir.join("plugin.yaml"), "name: tokensave\n").unwrap();
        std::fs::write(other_plugin_dir.join("plugin.yaml"), "name: other\n").unwrap();
        std::fs::write(
            profile.join("config.yaml"),
            "theme: dark\nmemory:\n  provider: tokensave\nplugins:\n  enabled:\n    - tokensave\n    - other\n",
        )
        .unwrap();
    }

    let output = tokensave_command(project.path(), home.path())
        .arg("uninstall")
        .arg("--agent")
        .arg("hermes")
        .arg("--all-profiles")
        .output()
        .expect("run hermes all-profiles uninstall");
    assert!(
        output.status.success(),
        "hermes all-profiles uninstall should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    for profile in [&default_profile, &work_profile, &personal_profile] {
        assert!(
            !profile.join("plugins/tokensave/plugin.yaml").exists(),
            "tokensave plugin should be removed from {}",
            profile.display()
        );
        assert!(
            profile.join("plugins/other/plugin.yaml").exists(),
            "uninstall must preserve unrelated plugins in {}",
            profile.display()
        );
        let config = std::fs::read_to_string(profile.join("config.yaml")).unwrap();
        assert!(config.contains("theme: dark"));
        assert!(config.contains("    - other"));
        assert!(!config.contains("    - tokensave"));
        assert!(!config.contains("provider: tokensave"));
    }
}

#[test]
fn test_hermes_local_install_with_profile_targets_named_profile() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();

    let output = tokensave_command(project.path(), home.path())
        .arg("install")
        .arg("--local")
        .arg("--agent")
        .arg("hermes")
        .arg("--profile")
        .arg("project")
        .output()
        .expect("run hermes local install with profile");
    assert!(
        output.status.success(),
        "hermes profile local install should succeed\nstderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(home
        .path()
        .join(".hermes/profiles/project/plugins/tokensave/plugin.yaml")
        .exists());
    assert_hermes_config_enables_tokensave_memory(
        &home.path().join(".hermes/profiles/project/config.yaml"),
    );
    assert!(
        !project
            .path()
            .join(".hermes/plugins/tokensave/plugin.yaml")
            .exists(),
        "--profile should target a profile instead of project plugin directory"
    );
}

#[test]
fn test_hermes_install_rejects_invalid_profile_names() {
    let home = TempDir::new().unwrap();
    let ctx = InstallContext {
        home: home.path().to_path_buf(),
        tokensave_bin: "/usr/local/bin/tokensave".to_string(),
        tool_permissions: expected_tool_perms(),
        profile: Some("_bad".to_string()),
        project_root: None,
        dashboard: true,
    };

    let err = HermesIntegration.install(&ctx).unwrap_err().to_string();
    assert!(err.contains("invalid Hermes profile"));
    assert!(!home.path().join(".hermes/profiles/_bad").exists());
}

#[test]
fn test_profile_flag_is_only_valid_for_hermes_install() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();

    let output = tokensave_command(project.path(), home.path())
        .arg("install")
        .arg("--agent")
        .arg("cursor")
        .arg("--profile")
        .arg("work")
        .output()
        .expect("run install with invalid --profile agent");

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr)
        .contains("`--profile` is only supported with `--agent hermes`"));
}

#[test]
fn test_profile_flag_is_valid_for_hermes_uninstall_only() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();

    let install = tokensave_command(project.path(), home.path())
        .arg("install")
        .arg("--agent")
        .arg("hermes")
        .arg("--profile")
        .arg("work")
        .output()
        .expect("run hermes profile install");
    assert!(
        install.status.success(),
        "hermes profile install should succeed\nstderr:\n{}",
        String::from_utf8_lossy(&install.stderr)
    );
    let plugin_dir = home.path().join(".hermes/profiles/work/plugins/tokensave");
    assert!(plugin_dir.exists());

    let uninstall = tokensave_command(project.path(), home.path())
        .arg("uninstall")
        .arg("--agent")
        .arg("hermes")
        .arg("--profile")
        .arg("work")
        .output()
        .expect("run hermes profile uninstall");
    assert!(
        uninstall.status.success(),
        "hermes profile uninstall should succeed\nstderr:\n{}",
        String::from_utf8_lossy(&uninstall.stderr)
    );
    assert!(!plugin_dir.exists());

    let invalid = tokensave_command(project.path(), home.path())
        .arg("uninstall")
        .arg("--agent")
        .arg("cursor")
        .arg("--profile")
        .arg("work")
        .output()
        .expect("run non-Hermes uninstall with profile");
    assert!(!invalid.status.success());
    assert!(String::from_utf8_lossy(&invalid.stderr)
        .contains("`--profile` is only supported with `--agent hermes`"));
}

#[test]
fn test_hermes_install_removes_tokensave_from_disabled_list() {
    let home = TempDir::new().unwrap();
    let hermes_dir = home.path().join(".hermes");
    std::fs::create_dir_all(&hermes_dir).unwrap();
    std::fs::write(
        hermes_dir.join("config.yaml"),
        "theme: dark\nplugins:\n  disabled:\n    - tokensave\n    - other\n",
    )
    .unwrap();

    HermesIntegration
        .install(&make_install_ctx(home.path()))
        .unwrap();

    let config = std::fs::read_to_string(hermes_dir.join("config.yaml")).unwrap();
    assert!(config.contains("theme: dark"));
    assert!(config.contains("enabled:"));
    assert!(config.contains("    - tokensave"));
    assert!(
        !config.contains("  disabled:\n    - tokensave"),
        "plugins.disabled must not keep tokensave because disabled wins"
    );
    assert!(config.contains("    - other"));
}

#[test]
fn test_hermes_install_matches_two_space_list_item_indent() {
    // Hermes itself writes sequence items at the same indent as the key
    // (`enabled:` + `  - item`); inserting a 4-space item into such a list
    // produces unparseable YAML, so the installer must match the style.
    let home = TempDir::new().unwrap();
    let hermes_dir = home.path().join(".hermes");
    std::fs::create_dir_all(&hermes_dir).unwrap();
    std::fs::write(
        hermes_dir.join("config.yaml"),
        "theme: dark\nplugins:\n  enabled:\n  - other\n  - second\n",
    )
    .unwrap();

    HermesIntegration
        .install(&make_install_ctx(home.path()))
        .unwrap();

    let config = std::fs::read_to_string(hermes_dir.join("config.yaml")).unwrap();
    assert!(
        config.contains("  enabled:\n  - tokensave\n  - other\n  - second\n"),
        "tokensave must be inserted with the existing 2-space item indent:\n{config}"
    );
    assert!(
        !config.contains("    - tokensave"),
        "no 4-space item may be mixed into a 2-space list:\n{config}"
    );

    // Idempotency: a second install must detect the 2-space item.
    HermesIntegration
        .install(&make_install_ctx(home.path()))
        .unwrap();
    let config = std::fs::read_to_string(hermes_dir.join("config.yaml")).unwrap();
    assert_eq!(
        config.matches("- tokensave").count(),
        1,
        "re-install must not duplicate the 2-space list item:\n{config}"
    );
}

#[test]
fn test_hermes_install_removes_two_space_indent_disabled_entry() {
    let home = TempDir::new().unwrap();
    let hermes_dir = home.path().join(".hermes");
    std::fs::create_dir_all(&hermes_dir).unwrap();
    std::fs::write(
        hermes_dir.join("config.yaml"),
        "plugins:\n  disabled:\n  - tokensave\n  - other\n  enabled:\n  - kept\n",
    )
    .unwrap();

    HermesIntegration
        .install(&make_install_ctx(home.path()))
        .unwrap();

    let config = std::fs::read_to_string(hermes_dir.join("config.yaml")).unwrap();
    assert!(
        config.contains("  disabled:\n  - other\n"),
        "tokensave must be removed from the 2-space disabled list:\n{config}"
    );
    assert!(
        config.contains("  enabled:\n  - tokensave\n  - kept\n"),
        "tokensave must be enabled with matching indent:\n{config}"
    );
}

#[test]
fn test_hermes_install_accepts_flow_style_empty_disabled_list() {
    // Hermes writes `disabled: []` for the empty list; the installer used to
    // reject it as "unsupported Hermes plugins config".
    let home = TempDir::new().unwrap();
    let hermes_dir = home.path().join(".hermes");
    std::fs::create_dir_all(&hermes_dir).unwrap();
    std::fs::write(
        hermes_dir.join("config.yaml"),
        "theme: dark\nplugins:\n  disabled: []\n  enabled:\n    - other\n",
    )
    .unwrap();

    HermesIntegration
        .install(&make_install_ctx(home.path()))
        .unwrap();

    let config = std::fs::read_to_string(hermes_dir.join("config.yaml")).unwrap();
    assert!(
        config.contains("  disabled: []"),
        "the empty flow-style disabled list must be preserved:\n{config}"
    );
    assert!(
        config.contains("  enabled:\n    - tokensave\n    - other\n"),
        "tokensave must be added to the enabled list:\n{config}"
    );
}

#[test]
fn test_hermes_install_rewrites_flow_style_empty_enabled_list() {
    let home = TempDir::new().unwrap();
    let hermes_dir = home.path().join(".hermes");
    std::fs::create_dir_all(&hermes_dir).unwrap();
    std::fs::write(
        hermes_dir.join("config.yaml"),
        "plugins:\n  enabled: []\n  disabled: []\n",
    )
    .unwrap();

    HermesIntegration
        .install(&make_install_ctx(home.path()))
        .unwrap();

    let config = std::fs::read_to_string(hermes_dir.join("config.yaml")).unwrap();
    assert!(
        config.contains("  enabled:\n    - tokensave\n"),
        "`enabled: []` must be rewritten into a block list with tokensave:\n{config}"
    );
    assert!(
        !config.contains("enabled: []"),
        "the empty flow-style enabled list must be replaced:\n{config}"
    );
    assert!(
        config.contains("  disabled: []"),
        "the untouched disabled flow list must survive:\n{config}"
    );
}

#[test]
fn test_hermes_install_still_rejects_non_empty_flow_lists() {
    let home = TempDir::new().unwrap();
    let hermes_dir = home.path().join(".hermes");
    std::fs::create_dir_all(&hermes_dir).unwrap();
    let original = "plugins:\n  enabled: [other]\n";
    std::fs::write(hermes_dir.join("config.yaml"), original).unwrap();

    let err = HermesIntegration
        .install(&make_install_ctx(home.path()))
        .unwrap_err()
        .to_string();

    assert!(err.contains("unsupported Hermes plugins config"));
    assert_eq!(
        std::fs::read_to_string(hermes_dir.join("config.yaml")).unwrap(),
        original,
        "non-empty flow lists must not be rewritten"
    );
}

#[test]
fn test_hermes_install_backs_up_existing_config() {
    let home = TempDir::new().unwrap();
    let hermes_dir = home.path().join(".hermes");
    std::fs::create_dir_all(&hermes_dir).unwrap();
    let original = "theme: dark\nplugins:\n  enabled:\n    - other\n";
    std::fs::write(hermes_dir.join("config.yaml"), original).unwrap();

    HermesIntegration
        .install(&make_install_ctx(home.path()))
        .unwrap();

    let backup = hermes_dir.join("config.yaml.bak");
    assert!(
        backup.exists(),
        "install should back up existing Hermes config"
    );
    assert_eq!(
        std::fs::read_to_string(backup).unwrap(),
        original,
        "backup should preserve the exact original config"
    );
}

#[test]
fn test_hermes_install_rejects_existing_memory_provider_without_rewrite() {
    let home = TempDir::new().unwrap();
    let hermes_dir = home.path().join(".hermes");
    std::fs::create_dir_all(&hermes_dir).unwrap();
    let original =
        "theme: dark\nmemory:\n  provider: other-memory\nplugins:\n  enabled:\n    - other\n";
    std::fs::write(hermes_dir.join("config.yaml"), original).unwrap();

    let err = HermesIntegration
        .install(&make_install_ctx(home.path()))
        .unwrap_err()
        .to_string();

    assert!(err.contains("Hermes memory provider already configured"));
    assert_eq!(
        std::fs::read_to_string(hermes_dir.join("config.yaml")).unwrap(),
        original,
        "install must not overwrite an existing Hermes memory provider"
    );
}

#[test]
fn test_hermes_install_rejects_existing_context_engine_without_rewrite() {
    let home = TempDir::new().unwrap();
    let hermes_dir = home.path().join(".hermes");
    std::fs::create_dir_all(&hermes_dir).unwrap();
    let original = "theme: dark\ncontext:\n  engine: other-engine\n";
    std::fs::write(hermes_dir.join("config.yaml"), original).unwrap();

    let err = HermesIntegration
        .install(&make_install_ctx(home.path()))
        .unwrap_err()
        .to_string();

    assert!(
        err.contains("Hermes context engine already configured"),
        "unexpected error: {err}"
    );
    assert_eq!(
        std::fs::read_to_string(hermes_dir.join("config.yaml")).unwrap(),
        original,
        "install must not overwrite a foreign Hermes context engine"
    );
}

#[test]
fn test_hermes_install_replaces_default_compressor_context_engine() {
    let home = TempDir::new().unwrap();
    let hermes_dir = home.path().join(".hermes");
    std::fs::create_dir_all(&hermes_dir).unwrap();
    // `compressor` is the built-in default the host falls back to anyway, so
    // replacing it is the activation step, not an overwrite.
    std::fs::write(
        hermes_dir.join("config.yaml"),
        "theme: dark\ncontext:\n  engine: compressor\n  other_key: 1\n",
    )
    .unwrap();

    HermesIntegration
        .install(&make_install_ctx(home.path()))
        .unwrap();

    let config = std::fs::read_to_string(hermes_dir.join("config.yaml")).unwrap();
    assert!(
        config.contains("  engine: tokensave"),
        "install must replace the default compressor engine:\n{config}"
    );
    assert!(
        config.contains("  other_key: 1"),
        "install must keep unrelated context keys:\n{config}"
    );

    // Uninstall removes only the engine selection, not the user's block.
    HermesIntegration
        .uninstall(&make_install_ctx(home.path()))
        .unwrap();
    let config = std::fs::read_to_string(hermes_dir.join("config.yaml")).unwrap();
    assert!(
        !config.contains("engine: tokensave"),
        "uninstall must deactivate the tokensave context engine:\n{config}"
    );
    assert!(
        config.contains("context:") && config.contains("  other_key: 1"),
        "uninstall must keep the user's remaining context block:\n{config}"
    );
}

#[test]
fn test_hermes_install_preserves_user_keys_in_tokensave_config_block() {
    let home = TempDir::new().unwrap();
    let hermes_dir = home.path().join(".hermes");
    std::fs::create_dir_all(&hermes_dir).unwrap();
    std::fs::write(
        hermes_dir.join("config.yaml"),
        "plugins:\n  tokensave:\n    summary_model: glm-4.7\n  enabled:\n    - other\n",
    )
    .unwrap();

    let ctx = InstallContext {
        home: home.path().to_path_buf(),
        tokensave_bin: "/usr/local/bin/tokensave".to_string(),
        tool_permissions: expected_tool_perms(),
        profile: None,
        project_root: Some(std::path::PathBuf::from("/pinned/project")),
        dashboard: true,
    };
    HermesIntegration.install(&ctx).unwrap();

    let config = std::fs::read_to_string(hermes_dir.join("config.yaml")).unwrap();
    assert!(
        config.contains("    project_root: \"/pinned/project\""),
        "install must add the pin to the existing plugins.tokensave block:\n{config}"
    );
    assert!(
        config.contains("    summary_model: glm-4.7"),
        "install must keep user keys in the plugins.tokensave block:\n{config}"
    );

    // Uninstall drops only the generated pin and keeps the user's keys.
    HermesIntegration
        .uninstall(&make_install_ctx(home.path()))
        .unwrap();
    let config = std::fs::read_to_string(hermes_dir.join("config.yaml")).unwrap();
    assert!(
        !config.contains("project_root:"),
        "uninstall must remove the generated pin:\n{config}"
    );
    assert!(
        config.contains("  tokensave:") && config.contains("    summary_model: glm-4.7"),
        "uninstall must keep user keys in the plugins.tokensave block:\n{config}"
    );
}

#[test]
fn test_hermes_healthcheck_warns_on_stale_plugin_and_missing_pin() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    HermesIntegration
        .install(&make_install_ctx(home.path()))
        .unwrap();

    let plugin_dir = home.path().join(".hermes/plugins/tokensave");
    let hctx = HealthcheckContext {
        home: home.path().to_path_buf(),
        project_path: project.path().to_path_buf(),
    };

    // Fresh install: version matches the binary, no pin — no warnings.
    let mut dc = DoctorCounters::new();
    HermesIntegration.healthcheck(&mut dc, &hctx);
    assert_eq!(dc.warnings, 0, "fresh install should be healthy");

    // Stale manifest version (an old generator wrote it) must warn.
    let manifest_path = plugin_dir.join("plugin.yaml");
    let manifest = std::fs::read_to_string(&manifest_path).unwrap();
    std::fs::write(
        &manifest_path,
        manifest.replace(
            &format!("version: {}", env!("CARGO_PKG_VERSION")),
            "version: 1.0.0",
        ),
    )
    .unwrap();
    let mut dc = DoctorCounters::new();
    HermesIntegration.healthcheck(&mut dc, &hctx);
    assert_eq!(
        dc.warnings, 1,
        "stale generated plugin version should warn once"
    );

    // A pinned project root that no longer exists must warn too.
    HermesIntegration
        .install(&InstallContext {
            home: home.path().to_path_buf(),
            tokensave_bin: "/usr/local/bin/tokensave".to_string(),
            tool_permissions: expected_tool_perms(),
            profile: None,
            project_root: Some(std::path::PathBuf::from("/missing/pinned/project")),
            dashboard: true,
        })
        .unwrap();
    let mut dc = DoctorCounters::new();
    HermesIntegration.healthcheck(&mut dc, &hctx);
    assert_eq!(
        dc.warnings, 1,
        "a dangling pinned project root should warn once"
    );
}

#[test]
fn test_hermes_install_rejects_inline_plugins_config_without_rewrite() {
    let home = TempDir::new().unwrap();
    let hermes_dir = home.path().join(".hermes");
    std::fs::create_dir_all(&hermes_dir).unwrap();
    let original = "theme: dark\nplugins: { enabled: [other] }\n";
    std::fs::write(hermes_dir.join("config.yaml"), original).unwrap();

    let err = HermesIntegration
        .install(&make_install_ctx(home.path()))
        .unwrap_err()
        .to_string();

    assert!(err.contains("unsupported Hermes plugins config"));
    assert_eq!(
        std::fs::read_to_string(hermes_dir.join("config.yaml")).unwrap(),
        original,
        "unsupported inline plugins config must not be rewritten or duplicated"
    );
}

#[test]
fn test_hermes_uninstall_preserves_other_profile_plugins_and_config() {
    let home = TempDir::new().unwrap();
    let profile = home.path().join(".hermes/profiles/work");
    let plugin_dir = profile.join("plugins/tokensave");
    let other_plugin = profile.join("plugins/other");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::create_dir_all(&other_plugin).unwrap();
    std::fs::write(plugin_dir.join("plugin.yaml"), "name: tokensave\n").unwrap();
    std::fs::write(other_plugin.join("plugin.yaml"), "name: other\n").unwrap();
    std::fs::write(
        profile.join("config.yaml"),
        "theme: dark\nplugins:\n  enabled:\n    - other\n    - tokensave\n",
    )
    .unwrap();

    let ctx = InstallContext {
        home: home.path().to_path_buf(),
        tokensave_bin: String::new(),
        tool_permissions: expected_tool_perms(),
        profile: Some("work".to_string()),
        project_root: None,
        dashboard: true,
    };

    HermesIntegration.uninstall(&ctx).unwrap();

    assert!(!plugin_dir.exists());
    assert!(other_plugin.join("plugin.yaml").exists());
    let config = std::fs::read_to_string(profile.join("config.yaml")).unwrap();
    assert!(config.contains("theme: dark"));
    assert!(config.contains("    - other"));
    assert!(!config.contains("    - tokensave"));
}

#[test]
fn test_hermes_uninstall_preserves_unknown_files_in_tokensave_plugin_dir() {
    let home = TempDir::new().unwrap();
    let plugin_dir = home.path().join(".hermes/plugins/tokensave");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::write(plugin_dir.join("plugin.yaml"), "name: tokensave\n").unwrap();
    std::fs::write(plugin_dir.join("user-notes.txt"), "keep me\n").unwrap();

    HermesIntegration
        .uninstall(&make_install_ctx(home.path()))
        .unwrap();

    assert!(
        plugin_dir.join("user-notes.txt").exists(),
        "uninstall should not delete unknown files in the tokensave plugin dir"
    );
    assert!(
        !plugin_dir.join("plugin.yaml").exists(),
        "uninstall should remove tokensave-generated files"
    );
}

#[test]
fn test_local_install_cursor_removes_legacy_project_mcp_hooks_and_rule() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();

    let cursor_dir = project.path().join(".cursor");
    std::fs::create_dir_all(cursor_dir.join("rules")).unwrap();
    std::fs::write(
        cursor_dir.join("mcp.json"),
        r#"{"mcpServers":{"tokensave":{"type":"stdio","command":"/old/tokensave","args":["serve","--path","."]}}}"#,
    )
    .unwrap();
    std::fs::write(
        cursor_dir.join("hooks.json"),
        r#"{"version":1,"hooks":{"afterFileEdit":[{"command":"/old/tokensave hook-cursor-after-file-edit","timeout":30}]}}"#,
    )
    .unwrap();
    std::fs::write(
        cursor_dir.join("rules/tokensave.mdc"),
        "---\ndescription: Prefer tokensave MCP tools for codebase exploration\nalwaysApply: true\n---\n\n# Prefer tokensave MCP tools\n",
    )
    .unwrap();

    assert_local_install_success("cursor", project.path(), home.path());
    assert_local_install_success("cursor", project.path(), home.path());

    assert!(
        !cursor_dir.join("mcp.json").exists(),
        "local install should remove legacy tokensave-only project MCP config"
    );
    assert!(
        !cursor_dir.join("hooks.json").exists(),
        "local install should remove legacy tokensave-only project hooks"
    );
    assert!(
        !cursor_dir.join("rules/tokensave.mdc").exists(),
        "local install should remove legacy tokensave project rule"
    );
}

#[cfg(unix)]
#[test]
fn test_local_install_cursor_rejects_symlinked_cursor_dir() {
    use std::os::unix::fs::symlink;

    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    symlink(outside.path(), project.path().join(".cursor")).unwrap();

    let output = run_local_install("cursor", project.path(), home.path());
    assert!(
        !output.status.success(),
        "local Cursor install should reject symlinked .cursor directories"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("symlink"),
        "error should explain the symlink refusal, got:\n{stderr}"
    );
}

#[test]
fn test_local_install_supported_agents_write_project_paths() {
    let cases = [
        (
            "claude",
            vec![".mcp.json", ".claude/settings.json", ".claude/CLAUDE.md"],
        ),
        (
            "codex",
            vec![".codex/config.toml", ".codex/hooks.json", "AGENTS.md"],
        ),
        ("gemini", vec![".gemini/settings.json", "GEMINI.md"]),
        (
            "kiro",
            vec![
                ".kiro/settings/mcp.json",
                ".kiro/steering/tokensave.md",
                ".kiro/agents/tokensave.json",
            ],
        ),
        ("opencode", vec!["opencode.json", "AGENTS.md"]),
        ("copilot", vec![".vscode/mcp.json"]),
        ("zed", vec![".zed/settings.json"]),
        ("roo-code", vec![".roo/mcp.json"]),
        ("kimi", vec![".kimi-code/mcp.json", "AGENTS.md"]),
        ("kilo", vec!["kilo.json"]),
        ("vibe", vec![".vibe/config.toml", ".vibe/prompts/cli.md"]),
    ];

    for (agent, paths) in cases {
        let home = TempDir::new().unwrap();
        let project = TempDir::new().unwrap();

        assert_local_install_success(agent, project.path(), home.path());

        for relative in paths {
            let path = project.path().join(relative);
            assert!(
                path.exists(),
                "{agent} local install should create project path {}",
                path.display()
            );
            let body = std::fs::read_to_string(&path).unwrap();
            assert!(
                body.contains("tokensave"),
                "{agent} local file {} should mention tokensave",
                path.display()
            );
            let is_instruction_file = matches!(
                path.extension().and_then(|ext| ext.to_str()),
                Some("md" | "mdc")
            );
            if is_instruction_file {
                assert!(
                    body.contains("tokensave_fact_store"),
                    "{agent} local instruction file {} should mention fact memory tools",
                    path.display()
                );
                assert!(
                    body.contains("tokensave_message_search"),
                    "{agent} local instruction file {} should mention transcript message search",
                    path.display()
                );
            }
            if !is_instruction_file {
                let expected = expected_tokensave_bin();
                assert!(
                    body.contains(&expected),
                    "{agent} local config {} should use the resolved absolute tokensave executable",
                    path.display()
                );
            }
        }

        assert!(
            !home.path().join(".tokensave/config.toml").exists(),
            "{agent} local install must not create or mutate user-level install tracking"
        );
    }
}

#[test]
fn test_local_install_rejects_antigravity_without_project_mutation() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();

    let output = run_local_install("antigravity", project.path(), home.path());

    assert!(
        !output.status.success(),
        "Antigravity local install should be rejected"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Antigravity") && stderr.contains("--local"),
        "unsupported-agent error should name Antigravity and --local, got:\n{stderr}"
    );
    assert!(
        !home.path().join(".tokensave/config.toml").exists(),
        "rejected local install must not mutate user-level install tracking"
    );
}

#[test]
fn test_local_install_rejects_cline_without_project_mutation() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();

    let output = run_local_install("cline", project.path(), home.path());

    assert!(
        !output.status.success(),
        "Cline local install should be rejected"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Cline") && stderr.contains("--local"),
        "unsupported-agent error should name Cline and --local, got:\n{stderr}"
    );
    assert!(
        !project.path().join(".cline_mcp_servers.json").exists(),
        "unsupported Cline local install must not write undocumented workspace config"
    );
    assert!(
        !home.path().join(".tokensave/config.toml").exists(),
        "rejected local install must not mutate user-level install tracking"
    );
}

#[test]
fn test_claude_install_creates_config() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let ctx = make_install_ctx(home);
    ClaudeIntegration.install(&ctx).unwrap();

    // Check ~/.claude.json exists and has mcpServers.tokensave
    let claude_json = home.join(".claude.json");
    assert!(
        claude_json.exists(),
        "~/.claude.json should exist after install"
    );
    let content: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&claude_json).unwrap()).unwrap();
    assert!(
        content.get("mcpServers").is_some(),
        "mcpServers key should exist"
    );
    assert!(
        content["mcpServers"]["tokensave"].is_object(),
        "mcpServers.tokensave should be an object"
    );
    // Verify args contain "serve"
    let args = content["mcpServers"]["tokensave"]["args"]
        .as_array()
        .unwrap();
    assert!(args.iter().any(|v| v.as_str() == Some("serve")));

    // Check ~/.claude/settings.json exists with hook and permissions
    let settings_path = home.join(".claude/settings.json");
    assert!(
        settings_path.exists(),
        "settings.json should exist after install"
    );
    let settings: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    // Check hook
    assert!(
        settings["hooks"]["PreToolUse"].is_array(),
        "PreToolUse hook should be an array"
    );
    // Check permissions
    assert!(
        settings["permissions"]["allow"].is_array(),
        "permissions.allow should be an array"
    );
    let allow = settings["permissions"]["allow"].as_array().unwrap();
    let allow_strs: Vec<&str> = allow.iter().filter_map(|v| v.as_str()).collect();
    for perm in expected_tool_perms() {
        assert!(
            allow_strs.contains(&perm.as_str()),
            "permissions.allow should contain {perm}"
        );
    }

    // Check CLAUDE.md exists with tokensave rules
    let claude_md = home.join(".claude/CLAUDE.md");
    assert!(claude_md.exists(), "CLAUDE.md should exist after install");
    let md_content = std::fs::read_to_string(&claude_md).unwrap();
    assert!(
        md_content.contains("tokensave"),
        "CLAUDE.md should mention tokensave"
    );
}

#[test]
fn test_gemini_install_creates_config() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let ctx = make_install_ctx(home);
    GeminiIntegration.install(&ctx).unwrap();

    // Check ~/.gemini/settings.json
    let settings_path = home.join(".gemini/settings.json");
    assert!(
        settings_path.exists(),
        "settings.json should exist after install"
    );
    let content: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    assert!(
        content["mcpServers"]["tokensave"].is_object(),
        "mcpServers.tokensave should exist"
    );
    // Verify trust flag
    assert_eq!(
        content["mcpServers"]["tokensave"]["trust"],
        serde_json::json!(true),
        "gemini should have trust: true"
    );

    // Check GEMINI.md
    let gemini_md = home.join(".gemini/GEMINI.md");
    assert!(gemini_md.exists(), "GEMINI.md should exist after install");
    let md_content = std::fs::read_to_string(&gemini_md).unwrap();
    assert!(md_content.contains("tokensave"));
}

#[test]
fn test_codex_install_creates_config() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let ctx = make_install_ctx(home);
    CodexIntegration.install(&ctx).unwrap();

    // Check ~/.codex/config.toml
    let config_path = home.join(".codex/config.toml");
    assert!(
        config_path.exists(),
        "config.toml should exist after install"
    );
    // Verify the file contains the expected content as text (the TOML output from
    // toml::to_string_pretty uses dotted headers which may not round-trip through
    // toml::Value::parse in all crate versions)
    let content = std::fs::read_to_string(&config_path).unwrap();
    assert!(
        content.contains("[mcp_servers.tokensave]"),
        "config.toml should contain [mcp_servers.tokensave]"
    );
    assert!(
        content.contains("TOKENSAVE_ENABLE_GLOBAL_DB = \"1\""),
        "global Codex config should opt into user-level global accounting"
    );
    assert!(
        content.contains("\"serve\""),
        "config.toml should contain \"serve\" in args"
    );
    for tool in tool_names() {
        let section = format!("[mcp_servers.tokensave.tools.{tool}]");
        let section_start = content.find(&section).unwrap_or_else(|| {
            panic!("Codex config should include auto-approval section {section}")
        });
        let section_body = content[section_start..]
            .split_once("\n[")
            .map_or(&content[section_start..], |(body, _)| body);
        assert!(
            section_body.contains("approval_mode = \"auto\""),
            "Codex should auto-approve tokensave tool {tool}"
        );
    }

    // Check AGENTS.md
    let agents_md = home.join(".codex/AGENTS.md");
    assert!(agents_md.exists(), "AGENTS.md should exist after install");
    let md_content = std::fs::read_to_string(&agents_md).unwrap();
    assert!(md_content.contains("tokensave"));
}

/// Returns true if any matcher group registered under `event` has a handler
/// whose `command` contains `needle`. Mirrors Codex's nested hooks.json shape:
/// `hooks[event][] -> { matcher?, hooks: [ { type, command, timeout } ] }`.
fn codex_event_has_handler(hooks: &serde_json::Value, event: &str, needle: &str) -> bool {
    hooks["hooks"][event].as_array().is_some_and(|groups| {
        groups.iter().any(|group| {
            group["hooks"].as_array().is_some_and(|handlers| {
                handlers.iter().any(|h| {
                    h["command"]
                        .as_str()
                        .is_some_and(|command| command.contains(needle))
                })
            })
        })
    })
}

/// Returns the matcher string for the group containing `needle` under `event`.
fn codex_matcher_for_handler(
    hooks: &serde_json::Value,
    event: &str,
    needle: &str,
) -> Option<String> {
    let groups = hooks["hooks"][event].as_array()?;
    for group in groups {
        let has = group["hooks"].as_array().is_some_and(|handlers| {
            handlers.iter().any(|h| {
                h["command"]
                    .as_str()
                    .is_some_and(|command| command.contains(needle))
            })
        });
        if has {
            return Some(group["matcher"].as_str().unwrap_or_default().to_string());
        }
    }
    None
}

fn assert_codex_hooks_registered(hooks: &serde_json::Value) {
    assert!(
        codex_event_has_handler(hooks, "SessionStart", "hook-codex-session-start"),
        "Codex SessionStart hook should steer toward tokensave MCP tools: {hooks}"
    );
    assert!(
        codex_event_has_handler(hooks, "UserPromptSubmit", "hook-codex-user-prompt-submit"),
        "Codex UserPromptSubmit hook should reset the counter and steer the agent: {hooks}"
    );
    assert!(
        codex_event_has_handler(hooks, "SubagentStart", "hook-codex-subagent-start"),
        "Codex SubagentStart hook should redirect research subagents: {hooks}"
    );
    assert!(
        codex_event_has_handler(hooks, "PostToolUse", "hook-codex-post-tool-use"),
        "Codex PostToolUse hook should keep the index fresh: {hooks}"
    );
    let matcher = codex_matcher_for_handler(hooks, "PostToolUse", "hook-codex-post-tool-use")
        .expect("PostToolUse handler should exist");
    assert!(
        matcher.contains("Bash") && matcher.contains("apply_patch"),
        "PostToolUse matcher should target Bash and apply_patch, got {matcher:?}"
    );
}

#[test]
fn test_codex_global_install_writes_hooks() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let ctx = make_install_ctx(home);
    CodexIntegration.install(&ctx).unwrap();

    let hooks_path = home.join(".codex/hooks.json");
    assert!(
        hooks_path.exists(),
        "global Codex install should write ~/.codex/hooks.json"
    );
    let hooks = read_json(&hooks_path);
    assert_codex_hooks_registered(&hooks);
}

#[test]
fn test_codex_local_install_writes_hooks() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();

    assert_local_install_success("codex", project.path(), home.path());

    let config_path = project.path().join(".codex/config.toml");
    let config = std::fs::read_to_string(&config_path).unwrap();
    assert!(
        config.contains("args = [\n    \"serve\",\n    \"--path\",\n    \".\",\n]"),
        "local Codex config should pin serve to the project root with --path ."
    );
    assert!(
        !config.contains("TOKENSAVE_DISABLE_GLOBAL_DB")
            && !config.contains("TOKENSAVE_ENABLE_GLOBAL_DB"),
        "local Codex config should not need env flags for repo-local mode"
    );

    let hooks_path = project.path().join(".codex/hooks.json");
    assert!(
        hooks_path.exists(),
        "local Codex install should write <project>/.codex/hooks.json"
    );
    let hooks = read_json(&hooks_path);
    assert_codex_hooks_registered(&hooks);
    // Local install must use the resolved absolute tokensave binary path.
    assert_command_contains_bin(&hooks, "SessionStart", "hook-codex-session-start");

    assert!(
        !home.path().join(".codex/hooks.json").exists(),
        "local install must not write the global Codex hooks config"
    );
}

fn assert_command_contains_bin(hooks: &serde_json::Value, event: &str, needle: &str) {
    let groups = hooks["hooks"][event].as_array().expect("event array");
    let command = groups
        .iter()
        .find_map(|group| {
            group["hooks"].as_array().and_then(|handlers| {
                handlers.iter().find_map(|h| {
                    h["command"]
                        .as_str()
                        .filter(|command| command.contains(needle))
                })
            })
        })
        .expect("handler command should exist");
    let expected = expected_tokensave_bin();
    assert!(
        command.contains(&expected),
        "Codex hook command must use the resolved absolute tokensave executable, got {command}"
    );
}

#[test]
fn test_codex_install_reconciles_hooks_idempotently() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();

    // Pre-seed a hooks.json with a stale tokensave PostToolUse group plus a
    // foreign hook that must be preserved across reinstall.
    let codex_dir = home.join(".codex");
    std::fs::create_dir_all(&codex_dir).unwrap();
    std::fs::write(
        codex_dir.join("hooks.json"),
        r#"{
          "hooks": {
            "PostToolUse": [
              { "matcher": "Bash", "hooks": [ { "type": "command", "command": "/old/tokensave hook-codex-post-tool-use", "timeout": 60 } ] },
              { "matcher": "Bash", "hooks": [ { "type": "command", "command": "/usr/bin/foreign-hook", "timeout": 10 } ] }
            ]
          }
        }"#,
    )
    .unwrap();

    let ctx = make_install_ctx(home);
    CodexIntegration.install(&ctx).unwrap();
    CodexIntegration.install(&ctx).unwrap();

    let hooks = read_json(&codex_dir.join("hooks.json"));
    let groups = hooks["hooks"]["PostToolUse"].as_array().unwrap();

    let tokensave_groups: Vec<_> = groups
        .iter()
        .filter(|group| {
            group["hooks"].as_array().is_some_and(|handlers| {
                handlers.iter().any(|h| {
                    h["command"]
                        .as_str()
                        .is_some_and(|c| c.contains("hook-codex-post-tool-use"))
                })
            })
        })
        .collect();
    assert_eq!(
        tokensave_groups.len(),
        1,
        "reinstall must keep exactly one tokensave PostToolUse group, got {groups:?}"
    );
    assert!(
        groups.iter().any(|group| {
            group["hooks"].as_array().is_some_and(|handlers| {
                handlers
                    .iter()
                    .any(|h| h["command"].as_str() == Some("/usr/bin/foreign-hook"))
            })
        }),
        "reinstall must preserve foreign hooks, got {groups:?}"
    );
}

#[test]
fn test_codex_uninstall_removes_hooks() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let ctx = make_install_ctx(home);

    CodexIntegration.install(&ctx).unwrap();
    let hooks_path = home.join(".codex/hooks.json");
    assert!(hooks_path.exists());

    CodexIntegration.uninstall(&ctx).unwrap();

    if hooks_path.exists() {
        let hooks = read_json(&hooks_path);
        assert!(
            !codex_event_has_handler(&hooks, "SessionStart", "hook-codex-session-start"),
            "uninstall should remove tokensave Codex hooks"
        );
    }
}

#[test]
fn test_kimi_install_creates_config() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let ctx = make_install_ctx(home);
    KimiIntegration.install(&ctx).unwrap();

    let mcp_path = home.join(".kimi/mcp.json");
    assert!(mcp_path.exists(), "mcp.json should exist after install");
    let content: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&mcp_path).unwrap()).unwrap();
    assert!(
        content["mcpServers"]["tokensave"].is_object(),
        "mcpServers.tokensave should be an object"
    );
    let args = content["mcpServers"]["tokensave"]["args"]
        .as_array()
        .unwrap();
    assert!(args.iter().any(|v| v.as_str() == Some("serve")));

    let agents_md = home.join(".kimi/AGENTS.md");
    assert!(agents_md.exists(), "AGENTS.md should exist after install");
    let md_content = std::fs::read_to_string(&agents_md).unwrap();
    assert!(md_content.contains("tokensave"));
}

#[test]
fn test_kimi_install_then_uninstall() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let ctx = make_install_ctx(home);

    KimiIntegration.install(&ctx).unwrap();
    let mcp_path = home.join(".kimi/mcp.json");
    assert!(mcp_path.exists());

    KimiIntegration.uninstall(&ctx).unwrap();

    assert!(
        !mcp_path.exists(),
        "mcp.json with only tokensave should be removed on uninstall"
    );

    let agents_md = home.join(".kimi/AGENTS.md");
    if agents_md.exists() {
        let content = std::fs::read_to_string(&agents_md).unwrap();
        assert!(
            !content.contains("## Prefer tokensave MCP tools"),
            "AGENTS.md should not have tokensave rules after uninstall"
        );
    }
}

#[test]
fn test_kimi_is_detected_and_has_tokensave() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();

    assert!(!KimiIntegration.is_detected(home));
    assert!(!KimiIntegration.has_tokensave(home));

    let ctx = make_install_ctx(home);
    KimiIntegration.install(&ctx).unwrap();

    assert!(KimiIntegration.is_detected(home));
    assert!(KimiIntegration.has_tokensave(home));
}

#[test]
fn test_cursor_install_creates_plugin() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let ctx = make_install_ctx(home);
    CursorIntegration.install(&ctx).unwrap();

    assert_cursor_plugin_bundle(&cursor_plugin_install_dir(home), &ctx.tokensave_bin);
    assert!(
        !home.join(".cursor/mcp.json").exists(),
        "Cursor plugin install should not write legacy ~/.cursor/mcp.json"
    );
}

#[test]
fn test_opencode_install_creates_config() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    // OpenCode uses ~/.config/opencode/opencode.json
    // Create the parent dir so install can discover it
    let ctx = make_install_ctx(home);
    OpenCodeIntegration.install(&ctx).unwrap();

    let config_path = home.join(".config/opencode/opencode.json");
    assert!(
        config_path.exists(),
        "opencode.json should exist after install"
    );
    let content: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
    assert!(content["mcp"]["tokensave"].is_object());
}

#[test]
fn test_zed_install_creates_config() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let ctx = make_install_ctx(home);
    ZedIntegration.install(&ctx).unwrap();

    // On macOS: ~/Library/Application Support/Zed/settings.json
    // On linux: ~/.config/zed/settings.json
    #[cfg(target_os = "macos")]
    let settings_path = home.join("Library/Application Support/Zed/settings.json");
    #[cfg(not(target_os = "macos"))]
    let settings_path = home.join(".config/zed/settings.json");

    assert!(
        settings_path.exists(),
        "Zed settings.json should exist after install"
    );
    let content: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    assert!(content["context_servers"]["tokensave"].is_object());
}

#[test]
fn test_cline_install_creates_config() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let ctx = make_install_ctx(home);
    ClineIntegration.install(&ctx).unwrap();

    // Cline uses VS Code extension global storage
    #[cfg(target_os = "macos")]
    let settings_path = home.join("Library/Application Support/Code/User/globalStorage/saoudrizwan.claude-dev/settings/cline_mcp_settings.json");
    #[cfg(target_os = "linux")]
    let settings_path = home.join(
        ".config/Code/User/globalStorage/saoudrizwan.claude-dev/settings/cline_mcp_settings.json",
    );
    #[cfg(target_os = "windows")]
    let settings_path = home.join("AppData/Roaming/Code/User/globalStorage/saoudrizwan.claude-dev/settings/cline_mcp_settings.json");
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    let settings_path = home.join(
        ".config/Code/User/globalStorage/saoudrizwan.claude-dev/settings/cline_mcp_settings.json",
    );

    assert!(
        settings_path.exists(),
        "Cline settings should exist after install"
    );
    let content: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    assert!(content["mcpServers"]["tokensave"].is_object());
}

#[test]
fn test_roo_code_install_creates_config() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let ctx = make_install_ctx(home);
    RooCodeIntegration.install(&ctx).unwrap();

    #[cfg(target_os = "macos")]
    let settings_path = home.join("Library/Application Support/Code/User/globalStorage/rooveterinaryinc.roo-cline/settings/cline_mcp_settings.json");
    #[cfg(target_os = "linux")]
    let settings_path = home.join(".config/Code/User/globalStorage/rooveterinaryinc.roo-cline/settings/cline_mcp_settings.json");
    #[cfg(target_os = "windows")]
    let settings_path = home.join("AppData/Roaming/Code/User/globalStorage/rooveterinaryinc.roo-cline/settings/cline_mcp_settings.json");
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    let settings_path = home.join(".config/Code/User/globalStorage/rooveterinaryinc.roo-cline/settings/cline_mcp_settings.json");

    assert!(
        settings_path.exists(),
        "Roo Code settings should exist after install"
    );
    let content: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    assert!(content["mcpServers"]["tokensave"].is_object());
}

#[test]
fn test_copilot_install_creates_config() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let ctx = make_install_ctx(home);
    CopilotIntegration.install(&ctx).unwrap();

    // Check VS Code settings.json
    #[cfg(target_os = "macos")]
    let vscode_settings = home.join("Library/Application Support/Code/User/settings.json");
    #[cfg(target_os = "linux")]
    let vscode_settings = home.join(".config/Code/User/settings.json");
    #[cfg(target_os = "windows")]
    let vscode_settings = home.join("AppData/Roaming/Code/User/settings.json");
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    let vscode_settings = home.join(".config/Code/User/settings.json");

    assert!(
        vscode_settings.exists(),
        "VS Code settings.json should exist"
    );
    let content: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&vscode_settings).unwrap()).unwrap();
    assert!(content["mcp"]["servers"]["tokensave"].is_object());

    // Check CLI config
    let cli_config = home.join(".copilot/mcp-config.json");
    assert!(cli_config.exists(), "Copilot CLI config should exist");
    let cli_content: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&cli_config).unwrap()).unwrap();
    assert!(cli_content["mcpServers"]["tokensave"].is_object());

    let cli_prompt = home.join(".copilot/copilot-instructions.md");
    let prompt = std::fs::read_to_string(&cli_prompt).unwrap();
    assert!(prompt.contains("tokensave_fact_store"));
    assert!(prompt.contains("memory_facts"));
    assert!(prompt.contains("sensitive or proprietary code"));
}

#[test]
fn test_vibe_install_creates_config() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let ctx = make_install_ctx(home);
    VibeIntegration.install(&ctx).unwrap();

    let config_path = home.join(".vibe/config.toml");
    assert!(
        config_path.exists(),
        "config.toml should exist after install"
    );
    let content = std::fs::read_to_string(&config_path).unwrap();
    assert!(
        content.contains("name = \"tokensave\""),
        "config should contain tokensave MCP server"
    );
    assert!(
        content.contains("transport = \"stdio\""),
        "config should use stdio transport"
    );
    assert!(
        content.contains("args = [\"serve\"]"),
        "config should have serve arg"
    );

    // Check prompt rules
    let prompt_path = home.join(".vibe/prompts/cli.md");
    assert!(
        prompt_path.exists(),
        "Vibe prompt should exist after install"
    );
    let prompt = std::fs::read_to_string(&prompt_path).unwrap();
    assert!(prompt.contains("tokensave"));
    assert!(prompt.contains("tokensave_fact_store"));
    assert!(prompt.contains("memory_facts"));
    assert!(prompt.contains("sensitive or proprietary code"));
}

// ---------------------------------------------------------------------------
// 4. Install followed by Uninstall
// ---------------------------------------------------------------------------

#[test]
fn test_claude_install_then_uninstall() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let ctx = make_install_ctx(home);

    // Install
    ClaudeIntegration.install(&ctx).unwrap();
    assert!(home.join(".claude.json").exists());

    // Uninstall
    ClaudeIntegration.uninstall(&ctx).unwrap();

    // ~/.claude.json should be removed (was only tokensave)
    // It may be removed entirely or have mcpServers removed
    if home.join(".claude.json").exists() {
        let content: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(home.join(".claude.json")).unwrap())
                .unwrap();
        // Should not have tokensave anymore
        let has_tokensave = content
            .get("mcpServers")
            .and_then(|v| v.get("tokensave"))
            .is_some();
        assert!(
            !has_tokensave,
            "tokensave should be removed from .claude.json after uninstall"
        );
    }
}

#[test]
fn test_gemini_install_then_uninstall() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let ctx = make_install_ctx(home);

    GeminiIntegration.install(&ctx).unwrap();
    let settings_path = home.join(".gemini/settings.json");
    assert!(settings_path.exists());

    GeminiIntegration.uninstall(&ctx).unwrap();

    // After uninstall, settings.json should be removed or not contain tokensave
    if settings_path.exists() {
        let content: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
        let has_tokensave = content
            .get("mcpServers")
            .and_then(|v| v.get("tokensave"))
            .is_some();
        assert!(
            !has_tokensave,
            "tokensave should be removed from settings.json"
        );
    }

    // GEMINI.md should be removed (was only tokensave rules)
    let gemini_md = home.join(".gemini/GEMINI.md");
    if gemini_md.exists() {
        let content = std::fs::read_to_string(&gemini_md).unwrap();
        assert!(
            !content.contains("## Prefer tokensave MCP tools"),
            "GEMINI.md should not contain tokensave rules after uninstall"
        );
    }
}

#[test]
fn test_codex_install_then_uninstall() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let ctx = make_install_ctx(home);

    CodexIntegration.install(&ctx).unwrap();
    let config_path = home.join(".codex/config.toml");
    assert!(config_path.exists());

    CodexIntegration.uninstall(&ctx).unwrap();

    // After uninstall, the config (which only contained tokensave) becomes
    // empty and is removed; or, if other content existed, the tokensave
    // server is dropped but the rest is preserved.
    assert!(
        !config_path.exists(),
        "config.toml with only tokensave should be removed on uninstall"
    );

    let agents_md = home.join(".codex/AGENTS.md");
    if agents_md.exists() {
        let content = std::fs::read_to_string(&agents_md).unwrap();
        assert!(
            !content.contains("## Prefer tokensave MCP tools"),
            "AGENTS.md should not have tokensave rules after uninstall"
        );
    }
}

#[test]
fn test_codex_install_preserves_existing_config() {
    // Regression test for issue #63: installing tokensave used to wipe out the
    // entire ~/.codex/config.toml because load_toml_file silently returned an
    // empty table.
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    std::fs::create_dir_all(home.join(".codex")).unwrap();
    let config_path = home.join(".codex/config.toml");
    let original = "\
model = \"o4-mini\"
approval_policy = \"on-failure\"

[mcp_servers.other]
command = \"other-bin\"
args = [\"--flag\"]
";
    std::fs::write(&config_path, original).unwrap();

    let ctx = make_install_ctx(home);
    CodexIntegration.install(&ctx).unwrap();

    // A backup of the original must exist.
    let backup = home.join(".codex/config.toml.bak");
    assert!(backup.exists(), "install must back up the existing config");
    assert_eq!(std::fs::read_to_string(&backup).unwrap(), original);

    // The new config must keep the user's settings.
    let new_contents = std::fs::read_to_string(&config_path).unwrap();
    let parsed: toml::Table = toml::from_str(&new_contents).unwrap();
    assert_eq!(
        parsed.get("model").and_then(|v| v.as_str()),
        Some("o4-mini"),
        "top-level user keys must be preserved"
    );
    assert_eq!(
        parsed.get("approval_policy").and_then(|v| v.as_str()),
        Some("on-failure"),
    );
    let servers = parsed
        .get("mcp_servers")
        .and_then(|v| v.as_table())
        .expect("mcp_servers should still be a table");
    assert!(
        servers.contains_key("other"),
        "pre-existing mcp_servers entries must be preserved"
    );
    assert!(
        servers.contains_key("tokensave"),
        "tokensave should be registered alongside existing servers"
    );
}

#[test]
fn test_codex_install_refuses_unparseable_config() {
    // Issue #63 guard: if the existing config can't be parsed, refuse to
    // overwrite rather than silently replacing the user's content.
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    std::fs::create_dir_all(home.join(".codex")).unwrap();
    let config_path = home.join(".codex/config.toml");
    let original = "this is not valid TOML {{{{";
    std::fs::write(&config_path, original).unwrap();

    let ctx = make_install_ctx(home);
    let result = CodexIntegration.install(&ctx);
    assert!(
        result.is_err(),
        "install must fail when existing config.toml is unparseable"
    );
    assert_eq!(
        std::fs::read_to_string(&config_path).unwrap(),
        original,
        "the broken config must be left untouched so the user can fix it"
    );
}

// ---------------------------------------------------------------------------
// Issue #63 regression: every agent must back up an existing config before
// overwriting it, and the user's pre-existing content must survive install.
// ---------------------------------------------------------------------------

/// Seed the agent's primary config with `original`, run install, then assert
/// that a `.bak` was created with the original bytes and that the new content
/// still contains the user's `marker` substring.
///
/// The path is taken from `agent.primary_config_path(home)` so a future change
/// to platform-conditional path logic (e.g. zed v4.3.15 Windows incident)
/// can't drift between tests and production.
fn assert_install_backs_up_and_preserves(
    agent: &dyn AgentIntegration,
    home: &Path,
    original: &str,
    marker: &str,
) {
    let config_path = agent
        .primary_config_path(home)
        .unwrap_or_else(|| panic!("{} must implement primary_config_path", agent.name()));
    std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
    std::fs::write(&config_path, original).unwrap();

    let ctx = make_install_ctx(home);
    agent.install(&ctx).expect("install should succeed");

    let mut backup = config_path.as_os_str().to_owned();
    backup.push(".bak");
    let backup = std::path::PathBuf::from(backup);
    assert!(
        backup.exists(),
        "{}: install must back up the existing config to {}",
        agent.name(),
        backup.display()
    );
    assert_eq!(
        std::fs::read_to_string(&backup).unwrap(),
        original,
        "{}: backup must contain the exact original bytes",
        agent.name()
    );

    let new = std::fs::read_to_string(&config_path).unwrap();
    assert!(
        new.contains(marker),
        "{}: user's pre-existing content (marker {marker:?}) must be preserved, got:\n{new}",
        agent.name(),
    );
}

#[test]
fn test_claude_install_preserves_existing_config() {
    let dir = TempDir::new().unwrap();
    let original = r#"{
  "theme": "solarized",
  "mcpServers": {
    "other": { "command": "other-bin", "args": ["--flag"] }
  }
}
"#;
    assert_install_backs_up_and_preserves(&ClaudeIntegration, dir.path(), original, "solarized");
}

#[test]
fn test_gemini_install_preserves_existing_config() {
    let dir = TempDir::new().unwrap();
    let original = r#"{
  "theme": "dark",
  "mcpServers": { "other": { "command": "other-bin" } }
}
"#;
    assert_install_backs_up_and_preserves(&GeminiIntegration, dir.path(), original, "\"theme\"");
}

#[test]
fn test_cursor_install_preserves_existing_legacy_mcp_config() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join(".cursor/mcp.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let original = r#"{
  "mcpServers": { "other": { "command": "other-bin" } }
}
"#;
    std::fs::write(&path, original).unwrap();

    CursorIntegration
        .install(&make_install_ctx(dir.path()))
        .unwrap();

    assert_cursor_plugin_bundle(
        &cursor_plugin_install_dir(dir.path()),
        &make_install_ctx(dir.path()).tokensave_bin,
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        original,
        "plugin install should not rewrite legacy Cursor MCP config"
    );
    assert!(
        !dir.path().join(".cursor/mcp.json.bak").exists(),
        "plugin install should not create backups for untouched legacy MCP config"
    );
}

#[test]
fn test_opencode_install_preserves_existing_config() {
    let dir = TempDir::new().unwrap();
    let original = r#"{
  "$schema": "https://opencode.ai/config.json",
  "mcp": { "other": { "type": "local", "command": ["other-bin"] } }
}
"#;
    assert_install_backs_up_and_preserves(&OpenCodeIntegration, dir.path(), original, "other-bin");
}

#[test]
fn test_zed_install_preserves_existing_config() {
    let dir = TempDir::new().unwrap();
    let original = r#"{
  "theme": "One Dark",
  "context_servers": { "other": { "command": { "path": "other-bin", "args": [] } } }
}
"#;
    assert_install_backs_up_and_preserves(&ZedIntegration, dir.path(), original, "One Dark");
}

#[test]
fn test_cline_install_preserves_existing_config() {
    let dir = TempDir::new().unwrap();
    let original = r#"{
  "mcpServers": { "other": { "command": "other-bin" } }
}
"#;
    assert_install_backs_up_and_preserves(&ClineIntegration, dir.path(), original, "other-bin");
}

#[test]
fn test_roo_code_install_preserves_existing_config() {
    let dir = TempDir::new().unwrap();
    let original = r#"{
  "mcpServers": { "other": { "command": "other-bin" } }
}
"#;
    assert_install_backs_up_and_preserves(&RooCodeIntegration, dir.path(), original, "other-bin");
}

#[test]
fn test_cursor_uninstall_backs_up_config_with_other_content() {
    // Regression for issue #63: uninstall paths must also back up the file
    // before rewriting, so a botched rewrite is recoverable.
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let ctx = make_install_ctx(home);

    let path = home.join(".cursor/mcp.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let original = r#"{
  "mcpServers": {
    "tokensave": { "command": "/usr/local/bin/tokensave", "args": ["serve"] },
    "other": { "command": "other-bin" }
  }
}
"#;
    std::fs::write(&path, original).unwrap();

    CursorIntegration.uninstall(&ctx).unwrap();

    let backup = home.join(".cursor/mcp.json.bak");
    assert!(
        backup.exists(),
        "uninstall must back up the existing config before rewriting it"
    );
    assert_eq!(
        std::fs::read_to_string(&backup).unwrap(),
        original,
        "backup must contain the exact pre-uninstall bytes"
    );
    let new = std::fs::read_to_string(&path).unwrap();
    assert!(
        new.contains("other-bin") && !new.contains("tokensave"),
        "uninstall must drop tokensave but keep other servers; got:\n{new}"
    );
}

#[test]
fn test_kilo_install_preserves_existing_config() {
    let dir = TempDir::new().unwrap();
    let original = r#"{
  // user comment about their workflow
  "mcp": { "other": { "type": "local", "command": ["other-bin"], "enabled": true } }
}
"#;
    assert_install_backs_up_and_preserves(&KiloIntegration, dir.path(), original, "other-bin");
}

#[test]
fn test_antigravity_install_preserves_existing_config() {
    let dir = TempDir::new().unwrap();
    let original = r#"{
  "mcpServers": { "other": { "command": "other-bin" } }
}
"#;
    assert_install_backs_up_and_preserves(
        &AntigravityIntegration,
        dir.path(),
        original,
        "other-bin",
    );
}

/// Regression for #85: `tokensave install --agent antigravity` must populate
/// both the IDE config and the CLI plugin file so the `agy` CLI can see the
/// server. Before the fix only the IDE path was written, which left the CLI
/// invisible in `/mcp`.
#[test]
fn test_antigravity_install_writes_cli_plugin() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let bin = "/usr/local/bin/tokensave";
    let ctx = InstallContext {
        home: home.to_path_buf(),
        tokensave_bin: bin.to_string(),
        tool_permissions: expected_tool_perms(),
        profile: None,
        project_root: None,
        dashboard: true,
    };

    AntigravityIntegration.install(&ctx).expect("install ok");

    let ide_path = home.join(".gemini/antigravity/mcp_config.json");
    let cli_path = home.join(".gemini/antigravity-cli/plugins/tokensave.json");
    assert!(
        ide_path.exists(),
        "IDE config must be written: {ide_path:?}"
    );
    assert!(
        cli_path.exists(),
        "CLI plugin must be written: {cli_path:?}"
    );

    for path in [&ide_path, &cli_path] {
        let body: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let server = body
            .get("mcpServers")
            .and_then(|v| v.get("tokensave"))
            .expect("tokensave entry");
        assert_eq!(
            server.get("command").and_then(|v| v.as_str()),
            Some(bin),
            "{path:?} must point at the install bin"
        );
        assert!(
            server
                .get("args")
                .and_then(|v| v.as_array())
                .is_some_and(|a| a.iter().any(|v| v.as_str() == Some("serve"))),
            "{path:?} must invoke `serve`"
        );
    }
}

/// Uninstall must remove the CLI plugin file outright (it belongs only to
/// tokensave) and remove the `tokensave` entry from the shared IDE config
/// without touching the user's other entries.
#[test]
fn test_antigravity_uninstall_removes_both_locations() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let bin = "/usr/local/bin/tokensave";
    let ctx = InstallContext {
        home: home.to_path_buf(),
        tokensave_bin: bin.to_string(),
        tool_permissions: expected_tool_perms(),
        profile: None,
        project_root: None,
        dashboard: true,
    };

    AntigravityIntegration.install(&ctx).unwrap();
    AntigravityIntegration.uninstall(&ctx).unwrap();

    let cli_path = home.join(".gemini/antigravity-cli/plugins/tokensave.json");
    assert!(
        !cli_path.exists(),
        "CLI plugin file must be deleted, still exists at {cli_path:?}"
    );

    let ide_path = home.join(".gemini/antigravity/mcp_config.json");
    // IDE config either deleted (empty) or rewritten without our entry —
    // both are acceptable; what's not acceptable is the entry persisting.
    if ide_path.exists() {
        let body: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&ide_path).unwrap()).unwrap();
        assert!(
            body.get("mcpServers")
                .and_then(|v| v.get("tokensave"))
                .is_none(),
            "tokensave entry must be removed from {ide_path:?}"
        );
    }
}

#[test]
fn test_copilot_install_preserves_existing_config() {
    let dir = TempDir::new().unwrap();
    let original = r#"{
  "editor.fontSize": 14,
  "workbench.colorTheme": "Default Dark+"
}
"#;
    assert_install_backs_up_and_preserves(
        &CopilotIntegration,
        dir.path(),
        original,
        "Default Dark+",
    );
}

/// Meta-test: every agent that goes through `assert_install_backs_up_and_preserves`
/// above must also actually return a path from `primary_config_path`. Catches
/// the case where a new integration is added without wiring up the method,
/// which would otherwise only surface as a confusing panic in CI.
#[test]
fn test_every_tested_agent_advertises_primary_config_path() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let agents: Vec<(&dyn AgentIntegration, &str)> = vec![
        (&ClaudeIntegration, "claude"),
        (&GeminiIntegration, "gemini"),
        (&CursorIntegration, "cursor"),
        (&OpenCodeIntegration, "opencode"),
        (&ZedIntegration, "zed"),
        (&ClineIntegration, "cline"),
        (&RooCodeIntegration, "roo-code"),
        (&CopilotIntegration, "copilot"),
        (&KiloIntegration, "kilo"),
        (&AntigravityIntegration, "antigravity"),
        (&CodexIntegration, "codex"),
        (&KiroIntegration, "kiro"),
        (&KimiIntegration, "kimi"),
    ];
    for (agent, id) in agents {
        let path = agent
            .primary_config_path(home)
            .unwrap_or_else(|| panic!("{id} must implement primary_config_path"));
        assert!(
            path.starts_with(home),
            "{id}: primary_config_path must be under the home arg, got {}",
            path.display()
        );
    }
}

#[test]
fn test_cursor_install_then_uninstall() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let ctx = make_install_ctx(home);

    CursorIntegration.install(&ctx).unwrap();
    let plugin_dir = cursor_plugin_install_dir(home);
    assert!(plugin_dir.exists());

    CursorIntegration.uninstall(&ctx).unwrap();

    assert!(
        !plugin_dir.exists(),
        "Cursor uninstall should remove the local plugin install"
    );
}

#[test]
fn test_copilot_install_then_uninstall() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let ctx = make_install_ctx(home);

    CopilotIntegration.install(&ctx).unwrap();
    CopilotIntegration.uninstall(&ctx).unwrap();

    // CLI config should be cleaned up
    let cli_config = home.join(".copilot/mcp-config.json");
    if cli_config.exists() {
        let content: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&cli_config).unwrap()).unwrap();
        let has_tokensave = content
            .get("mcpServers")
            .and_then(|v| v.get("tokensave"))
            .is_some();
        assert!(!has_tokensave);
    }
}

#[test]
fn test_vibe_install_then_uninstall() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let ctx = make_install_ctx(home);

    VibeIntegration.install(&ctx).unwrap();
    VibeIntegration.uninstall(&ctx).unwrap();

    let config_path = home.join(".vibe/config.toml");
    if config_path.exists() {
        let content = std::fs::read_to_string(&config_path).unwrap();
        assert!(
            !content.contains("name = \"tokensave\""),
            "tokensave should be removed from config.toml"
        );
    }

    let prompt_path = home.join(".vibe/prompts/cli.md");
    if prompt_path.exists() {
        let content = std::fs::read_to_string(&prompt_path).unwrap();
        assert!(
            !content.contains("tokensave"),
            "tokensave rules should be removed from prompt"
        );
    }
}

// ---------------------------------------------------------------------------
// 5. Healthcheck with tempdir
// ---------------------------------------------------------------------------

/// Creates a fake tokensave binary in a temp dir and returns the path string.
/// This allows healthchecks to verify binary existence.
fn make_install_ctx_with_real_bin(home: &Path) -> InstallContext {
    let bin_dir = home.join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let bin_path = bin_dir.join("tokensave");
    std::fs::write(&bin_path, "#!/bin/sh\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin_path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    InstallContext {
        home: home.to_path_buf(),
        tokensave_bin: bin_path.to_string_lossy().to_string(),
        tool_permissions: expected_tool_perms(),
        profile: None,
        project_root: None,
        dashboard: true,
    }
}

#[test]
fn test_healthcheck_claude_clean_install() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let ctx = make_install_ctx_with_real_bin(home);
    ClaudeIntegration.install(&ctx).unwrap();

    let mut dc = DoctorCounters::new();
    let hctx = HealthcheckContext {
        home: home.to_path_buf(),
        project_path: home.to_path_buf(),
    };
    ClaudeIntegration.healthcheck(&mut dc, &hctx);
    assert_eq!(dc.issues, 0, "clean Claude install should have no issues");
}

#[test]
fn test_healthcheck_gemini_clean_install() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let ctx = make_install_ctx(home);
    GeminiIntegration.install(&ctx).unwrap();

    let mut dc = DoctorCounters::new();
    let hctx = HealthcheckContext {
        home: home.to_path_buf(),
        project_path: home.to_path_buf(),
    };
    GeminiIntegration.healthcheck(&mut dc, &hctx);
    assert_eq!(dc.issues, 0, "clean Gemini install should have no issues");
}

#[test]
fn test_healthcheck_codex_after_install() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let ctx = make_install_ctx(home);
    CodexIntegration.install(&ctx).unwrap();

    let mut dc = DoctorCounters::new();
    let hctx = HealthcheckContext {
        home: home.to_path_buf(),
        project_path: home.to_path_buf(),
    };
    CodexIntegration.healthcheck(&mut dc, &hctx);
    assert_eq!(
        dc.issues, 0,
        "Codex healthcheck should pass after a clean install"
    );
}

#[test]
fn test_healthcheck_codex_local_install_checks_project_config() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    assert_local_install_success("codex", project.path(), home.path());

    let mut dc = DoctorCounters::new();
    let hctx = HealthcheckContext {
        home: home.path().to_path_buf(),
        project_path: project.path().to_path_buf(),
    };
    CodexIntegration.healthcheck(&mut dc, &hctx);
    assert_eq!(
        dc.issues, 0,
        "local Codex healthcheck should pass without global ~/.codex config"
    );
}

#[test]
fn test_healthcheck_codex_ignores_unrelated_project_agents_md() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    std::fs::write(
        project.path().join("AGENTS.md"),
        "Project-specific agent instructions without tokensave.\n",
    )
    .unwrap();
    let ctx = make_install_ctx(home.path());
    CodexIntegration.install(&ctx).unwrap();

    let mut dc = DoctorCounters::new();
    let hctx = HealthcheckContext {
        home: home.path().to_path_buf(),
        project_path: project.path().to_path_buf(),
    };
    CodexIntegration.healthcheck(&mut dc, &hctx);
    assert_eq!(
        dc.issues, 0,
        "global Codex healthcheck should be used when project AGENTS.md is unrelated"
    );
}

#[test]
fn test_healthcheck_cursor_clean_install() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let ctx = make_install_ctx(home);
    CursorIntegration.install(&ctx).unwrap();

    let mut dc = DoctorCounters::new();
    let hctx = HealthcheckContext {
        home: home.to_path_buf(),
        project_path: home.to_path_buf(),
    };
    CursorIntegration.healthcheck(&mut dc, &hctx);
    assert_eq!(dc.issues, 0, "clean Cursor install should have no issues");
}

#[test]
fn test_healthcheck_cursor_local_install_checks_project_config() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    assert_local_install_success("cursor", project.path(), home.path());

    let mut dc = DoctorCounters::new();
    let hctx = HealthcheckContext {
        home: home.path().to_path_buf(),
        project_path: project.path().to_path_buf(),
    };
    CursorIntegration.healthcheck(&mut dc, &hctx);
    assert_eq!(
        dc.issues, 0,
        "local Cursor healthcheck should pass without global ~/.cursor config"
    );
}

#[test]
fn test_healthcheck_hermes_profile_install_checks_named_profiles() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let ctx = InstallContext {
        home: home.path().to_path_buf(),
        tokensave_bin: "/usr/local/bin/tokensave".to_string(),
        tool_permissions: expected_tool_perms(),
        profile: Some("work".to_string()),
        project_root: None,
        dashboard: true,
    };
    HermesIntegration.install(&ctx).unwrap();

    let mut dc = DoctorCounters::new();
    let hctx = HealthcheckContext {
        home: home.path().to_path_buf(),
        project_path: project.path().to_path_buf(),
    };
    HermesIntegration.healthcheck(&mut dc, &hctx);
    assert_eq!(dc.issues, 0, "Hermes profile install should have no issues");
    assert_eq!(
        dc.warnings, 0,
        "Hermes healthcheck should recognize named profile installs"
    );
}

#[test]
fn test_healthcheck_hermes_local_install_checks_project_hermes_home() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    assert_local_install_success("hermes", project.path(), home.path());

    let mut dc = DoctorCounters::new();
    let hctx = HealthcheckContext {
        home: home.path().to_path_buf(),
        project_path: project.path().to_path_buf(),
    };
    HermesIntegration.healthcheck(&mut dc, &hctx);
    assert_eq!(dc.issues, 0, "Hermes local install should have no issues");
    assert_eq!(
        dc.warnings, 0,
        "Hermes healthcheck should recognize project-local HERMES_HOME installs"
    );
}

#[test]
fn test_healthcheck_opencode_clean_install() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let ctx = make_install_ctx(home);
    OpenCodeIntegration.install(&ctx).unwrap();

    let mut dc = DoctorCounters::new();
    let hctx = HealthcheckContext {
        home: home.to_path_buf(),
        project_path: home.to_path_buf(),
    };
    OpenCodeIntegration.healthcheck(&mut dc, &hctx);
    assert_eq!(dc.issues, 0, "clean OpenCode install should have no issues");
}

#[test]
fn test_healthcheck_no_install_warns() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();

    // Healthcheck without installing should produce warnings (not crashes)
    let mut dc = DoctorCounters::new();
    let hctx = HealthcheckContext {
        home: home.to_path_buf(),
        project_path: home.to_path_buf(),
    };
    ClaudeIntegration.healthcheck(&mut dc, &hctx);
    // Should have issues (missing config files)
    assert!(
        dc.issues > 0 || dc.warnings > 0,
        "healthcheck on empty dir should report issues or warnings"
    );
}

#[test]
fn test_doctor_counters() {
    let mut dc = DoctorCounters::new();
    assert_eq!(dc.issues, 0);
    assert_eq!(dc.warnings, 0);

    dc.pass("this is fine");
    assert_eq!(dc.issues, 0);
    assert_eq!(dc.warnings, 0);

    dc.fail("something broke");
    assert_eq!(dc.issues, 1);
    assert_eq!(dc.warnings, 0);

    dc.warn("be careful");
    assert_eq!(dc.issues, 1);
    assert_eq!(dc.warnings, 1);

    dc.info("just info");
    assert_eq!(dc.issues, 1);
    assert_eq!(dc.warnings, 1);

    dc.fail("another failure");
    assert_eq!(dc.issues, 2);
    assert_eq!(dc.warnings, 1);
}

// ---------------------------------------------------------------------------
// 6. Helper function tests
// ---------------------------------------------------------------------------

#[test]
fn test_load_json_file_missing() {
    let val = load_json_file(Path::new("/nonexistent/file.json"));
    assert!(val.is_object());
    assert!(val.as_object().unwrap().is_empty());
}

#[test]
fn test_load_json_file_valid() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("test.json");
    std::fs::write(&path, r#"{"key": "value"}"#).unwrap();
    let val = load_json_file(&path);
    assert_eq!(val["key"], "value");
}

#[test]
fn test_load_json_file_invalid_returns_empty() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("bad.json");
    std::fs::write(&path, "not valid json").unwrap();
    let val = load_json_file(&path);
    assert!(val.is_object());
    assert!(val.as_object().unwrap().is_empty());
}

#[test]
fn test_load_json_file_strict_missing() {
    let result = load_json_file_strict(Path::new("/nonexistent/file.json"));
    assert!(result.is_ok());
    let val = result.unwrap();
    assert!(val.is_object());
    assert!(val.as_object().unwrap().is_empty());
}

#[test]
fn test_load_json_file_strict_empty_file() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("empty.json");
    std::fs::write(&path, "").unwrap();
    let result = load_json_file_strict(&path);
    assert!(result.is_ok());
    let val = result.unwrap();
    assert!(val.as_object().unwrap().is_empty());
}

#[test]
fn test_load_json_file_strict_whitespace_only() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("ws.json");
    std::fs::write(&path, "   \n  \t  ").unwrap();
    let result = load_json_file_strict(&path);
    assert!(result.is_ok());
}

#[test]
fn test_load_json_file_strict_invalid() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("bad.json");
    std::fs::write(&path, "not valid json").unwrap();
    assert!(load_json_file_strict(&path).is_err());
}

#[test]
fn test_load_json_file_strict_valid() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("good.json");
    std::fs::write(&path, r#"{"hello": "world"}"#).unwrap();
    let val = load_json_file_strict(&path).unwrap();
    assert_eq!(val["hello"], "world");
}

#[test]
fn test_backup_config_file() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("config.json");
    std::fs::write(&path, r#"{"original": true}"#).unwrap();
    let backup = backup_config_file(&path).unwrap();
    assert!(backup.is_some());
    let backup_path = backup.unwrap();
    assert!(backup_path.exists());
    // Verify backup content matches original
    let backup_content = std::fs::read_to_string(&backup_path).unwrap();
    assert_eq!(backup_content, r#"{"original": true}"#);
}

#[test]
fn test_backup_config_file_missing() {
    let result = backup_config_file(Path::new("/nonexistent/file.json")).unwrap();
    assert!(result.is_none());
}

#[test]
fn test_safe_write_json_file() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("output.json");
    let value = serde_json::json!({"hello": "world"});
    safe_write_json_file(&path, &value, None).unwrap();
    let content: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(content["hello"], "world");
}

#[test]
fn test_safe_write_json_file_creates_parent_dirs() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("deep/nested/dir/output.json");
    let value = serde_json::json!({"nested": true});
    safe_write_json_file(&path, &value, None).unwrap();
    assert!(path.exists());
}

#[test]
fn test_safe_write_json_file_overwrites_existing() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("existing.json");
    std::fs::write(&path, r#"{"old": true}"#).unwrap();
    let value = serde_json::json!({"new": true});
    safe_write_json_file(&path, &value, None).unwrap();
    let content: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(content["new"], true);
    assert!(content.get("old").is_none());
}

#[test]
fn test_write_json_file() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("write_test.json");
    let value = serde_json::json!({"test": 42});
    write_json_file(&path, &value).unwrap();
    let content: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(content["test"], 42);
}

#[test]
fn test_load_toml_file_missing() {
    let val = load_toml_file(Path::new("/nonexistent/file.toml")).unwrap();
    assert!(val.is_table());
    assert!(val.as_table().unwrap().is_empty());
}

#[test]
fn test_load_toml_file_valid() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("test.toml");
    std::fs::write(&path, "key = \"value\"\nnumber = 42\n").unwrap();
    let val = load_toml_file(&path).expect("valid TOML should parse as document");
    let table = val.as_table().expect("top-level should be a table");
    assert_eq!(table.get("key").and_then(|v| v.as_str()), Some("value"));
    assert_eq!(table.get("number").and_then(|v| v.as_integer()), Some(42));
}

#[test]
fn test_load_toml_file_invalid_returns_err() {
    // Bug #63: invalid TOML used to silently return an empty table, which let
    // install_mcp_server wipe out the user's config. Now it must surface an
    // error so the caller refuses to overwrite.
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("bad.toml");
    std::fs::write(&path, "{{{{not valid toml").unwrap();
    assert!(
        load_toml_file(&path).is_err(),
        "unparseable TOML must surface as error, not silently empty"
    );
}

#[test]
fn test_load_toml_file_empty_file_returns_empty_table() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("empty.toml");
    std::fs::write(&path, "").unwrap();
    let val = load_toml_file(&path).expect("empty file should be treated as empty table");
    assert!(val.as_table().unwrap().is_empty());
}

#[test]
fn test_write_toml_file() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("output.toml");
    let mut table = toml::map::Map::new();
    table.insert("key".to_string(), toml::Value::String("value".to_string()));
    let val = toml::Value::Table(table);
    write_toml_file(&path, &val).unwrap();
    assert!(path.exists());
    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.contains("key"));
    assert!(content.contains("value"));
}

#[test]
fn test_write_toml_file_backs_up_existing() {
    // Issue #63: overwriting an existing config must always leave a .bak copy
    // so the user can recover if anything goes wrong.
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("config.toml");
    let original = "preserved = \"keep me\"\n";
    std::fs::write(&path, original).unwrap();

    let mut table = toml::map::Map::new();
    table.insert(
        "new".to_string(),
        toml::Value::String("content".to_string()),
    );
    write_toml_file(&path, &toml::Value::Table(table)).unwrap();

    let backup = dir.path().join("config.toml.bak");
    assert!(
        backup.exists(),
        "write must create a .bak of the prior file"
    );
    assert_eq!(
        std::fs::read_to_string(&backup).unwrap(),
        original,
        "the backup must contain the exact previous bytes"
    );
}

#[test]
fn test_write_toml_file_no_backup_when_no_prior_file() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("fresh.toml");
    let mut table = toml::map::Map::new();
    table.insert("k".to_string(), toml::Value::String("v".to_string()));
    write_toml_file(&path, &toml::Value::Table(table)).unwrap();

    let backup = dir.path().join("fresh.toml.bak");
    assert!(
        !backup.exists(),
        "no backup should be created on first write"
    );
}

// ---------------------------------------------------------------------------
// JSONC helpers
// ---------------------------------------------------------------------------

#[test]
fn test_load_jsonc_file_missing() {
    let val = load_jsonc_file(Path::new("/nonexistent/file.jsonc"));
    assert!(val.is_object());
    assert!(val.as_object().unwrap().is_empty());
}

#[test]
fn test_load_jsonc_file_with_comments() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("test.jsonc");
    std::fs::write(
        &path,
        r#"{
        // This is a comment
        "key": "value", // trailing comment
        /* block comment */
        "number": 42,
    }"#,
    )
    .unwrap();
    let val = load_jsonc_file(&path);
    assert_eq!(val["key"], "value");
    assert_eq!(val["number"], 42);
}

#[test]
fn test_load_jsonc_file_strict_missing() {
    let result = load_jsonc_file_strict(Path::new("/nonexistent/file.jsonc"));
    assert!(result.is_ok());
}

#[test]
fn test_load_jsonc_file_strict_with_comments() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("test.jsonc");
    std::fs::write(
        &path,
        r#"{
        // comment
        "key": "value"
    }"#,
    )
    .unwrap();
    let val = load_jsonc_file_strict(&path).unwrap();
    assert_eq!(val["key"], "value");
}

#[test]
fn test_parse_jsonc() {
    let input = r#"{
        // line comment
        "a": 1,
        /* block */ "b": 2,
    }"#;
    let val = parse_jsonc(input);
    assert_eq!(val["a"], 1);
    assert_eq!(val["b"], 2);
}

// ---------------------------------------------------------------------------
// 7. is_detected / has_tokensave tests
// ---------------------------------------------------------------------------

#[test]
fn test_is_detected_claude() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    assert!(!ClaudeIntegration.is_detected(home));
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    assert!(ClaudeIntegration.is_detected(home));
}

#[test]
fn test_is_detected_codex() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    assert!(!CodexIntegration.is_detected(home));
    std::fs::create_dir_all(home.join(".codex")).unwrap();
    assert!(CodexIntegration.is_detected(home));
}

#[test]
fn test_is_detected_gemini() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    assert!(!GeminiIntegration.is_detected(home));
    std::fs::create_dir_all(home.join(".gemini")).unwrap();
    assert!(GeminiIntegration.is_detected(home));
}

#[test]
fn test_is_detected_cursor() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    assert!(!CursorIntegration.is_detected(home));
    std::fs::create_dir_all(home.join(".cursor")).unwrap();
    assert!(CursorIntegration.is_detected(home));
}

#[test]
fn test_is_detected_opencode() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    assert!(!OpenCodeIntegration.is_detected(home));
    std::fs::create_dir_all(home.join(".config/opencode")).unwrap();
    assert!(OpenCodeIntegration.is_detected(home));
}

#[test]
fn test_is_detected_zed() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    assert!(!ZedIntegration.is_detected(home));
    #[cfg(target_os = "macos")]
    std::fs::create_dir_all(home.join("Library/Application Support/Zed")).unwrap();
    #[cfg(not(target_os = "macos"))]
    std::fs::create_dir_all(home.join(".config/zed")).unwrap();
    assert!(ZedIntegration.is_detected(home));
}

#[test]
fn test_is_detected_copilot() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    // Copilot is detected when either VS Code User dir or .copilot dir exists
    assert!(!CopilotIntegration.is_detected(home));
    std::fs::create_dir_all(home.join(".copilot")).unwrap();
    assert!(CopilotIntegration.is_detected(home));
}

#[test]
fn test_has_tokensave_claude() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    // No config => false
    assert!(!ClaudeIntegration.has_tokensave(home));

    // After install => true
    let ctx = make_install_ctx(home);
    ClaudeIntegration.install(&ctx).unwrap();
    assert!(ClaudeIntegration.has_tokensave(home));

    // After uninstall => false
    ClaudeIntegration.uninstall(&ctx).unwrap();
    assert!(!ClaudeIntegration.has_tokensave(home));
}

#[test]
fn test_has_tokensave_gemini() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    assert!(!GeminiIntegration.has_tokensave(home));

    let ctx = make_install_ctx(home);
    GeminiIntegration.install(&ctx).unwrap();
    assert!(GeminiIntegration.has_tokensave(home));
}

#[test]
fn test_has_tokensave_codex() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    assert!(!CodexIntegration.has_tokensave(home));

    let ctx = make_install_ctx(home);
    CodexIntegration.install(&ctx).unwrap();
    assert!(home.join(".codex/config.toml").exists());
    assert!(
        CodexIntegration.has_tokensave(home),
        "has_tokensave should detect tokensave after a clean install"
    );
}

#[test]
fn test_has_tokensave_cursor() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    assert!(!CursorIntegration.has_tokensave(home));

    let ctx = make_install_ctx(home);
    CursorIntegration.install(&ctx).unwrap();
    assert!(CursorIntegration.has_tokensave(home));
}

#[test]
fn test_has_tokensave_opencode() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    assert!(!OpenCodeIntegration.has_tokensave(home));

    let ctx = make_install_ctx(home);
    OpenCodeIntegration.install(&ctx).unwrap();
    assert!(OpenCodeIntegration.has_tokensave(home));
}

#[test]
fn test_has_tokensave_copilot() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    assert!(!CopilotIntegration.has_tokensave(home));

    let ctx = make_install_ctx(home);
    CopilotIntegration.install(&ctx).unwrap();
    assert!(CopilotIntegration.has_tokensave(home));
}

// ---------------------------------------------------------------------------
// 8. Idempotency tests
// ---------------------------------------------------------------------------

#[test]
fn test_claude_install_idempotent() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let ctx = make_install_ctx(home);

    // Install twice should not fail
    ClaudeIntegration.install(&ctx).unwrap();
    ClaudeIntegration.install(&ctx).unwrap();

    // Config should still be valid
    let claude_json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(home.join(".claude.json")).unwrap()).unwrap();
    assert!(claude_json["mcpServers"]["tokensave"].is_object());
}

#[test]
fn test_gemini_install_idempotent() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let ctx = make_install_ctx(home);

    GeminiIntegration.install(&ctx).unwrap();
    GeminiIntegration.install(&ctx).unwrap();

    let settings: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(home.join(".gemini/settings.json")).unwrap())
            .unwrap();
    assert!(settings["mcpServers"]["tokensave"].is_object());
}

#[test]
fn test_uninstall_without_install_does_not_crash() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let ctx = make_install_ctx(home);

    // Uninstalling when nothing is installed should not panic or error
    ClaudeIntegration.uninstall(&ctx).unwrap();
    GeminiIntegration.uninstall(&ctx).unwrap();
    CodexIntegration.uninstall(&ctx).unwrap();
    CursorIntegration.uninstall(&ctx).unwrap();
    CopilotIntegration.uninstall(&ctx).unwrap();
    OpenCodeIntegration.uninstall(&ctx).unwrap();
    ZedIntegration.uninstall(&ctx).unwrap();
    ClineIntegration.uninstall(&ctx).unwrap();
    RooCodeIntegration.uninstall(&ctx).unwrap();
    KiroIntegration.uninstall(&ctx).unwrap();
    VibeIntegration.uninstall(&ctx).unwrap();
}

// ---------------------------------------------------------------------------
// 9. Install preserves existing config
// ---------------------------------------------------------------------------

#[test]
fn test_claude_install_preserves_existing_claude_json() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();

    // Pre-populate .claude.json with other data
    let claude_json_path = home.join(".claude.json");
    std::fs::write(
        &claude_json_path,
        r#"{"mcpServers": {"other-server": {"command": "foo"}}, "customKey": 42}"#,
    )
    .unwrap();

    let ctx = make_install_ctx(home);
    ClaudeIntegration.install(&ctx).unwrap();

    let content: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&claude_json_path).unwrap()).unwrap();
    // tokensave added
    assert!(content["mcpServers"]["tokensave"].is_object());
    // existing server preserved
    assert!(content["mcpServers"]["other-server"].is_object());
    // custom key preserved
    assert_eq!(content["customKey"], 42);
}

#[test]
fn test_gemini_install_preserves_existing_settings() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();

    let settings_path = home.join(".gemini/settings.json");
    std::fs::create_dir_all(home.join(".gemini")).unwrap();
    std::fs::write(
        &settings_path,
        r#"{"mcpServers": {"other": {"command": "bar"}}, "theme": "dark"}"#,
    )
    .unwrap();

    let ctx = make_install_ctx(home);
    GeminiIntegration.install(&ctx).unwrap();

    let content: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    assert!(content["mcpServers"]["tokensave"].is_object());
    assert!(content["mcpServers"]["other"].is_object());
    assert_eq!(content["theme"], "dark");
}

// ---------------------------------------------------------------------------
// 10. Constants sanity
// ---------------------------------------------------------------------------

#[test]
fn test_tool_names_not_empty() {
    let names = tool_names();
    assert!(!names.is_empty());
    for name in &names {
        assert!(
            name.starts_with("tokensave_"),
            "tool name should start with tokensave_: {name}"
        );
    }
}

#[test]
fn test_read_only_tool_names_excludes_mutating_tools() {
    let read_only = read_only_tool_names();
    let read_only_set: std::collections::HashSet<&str> =
        read_only.iter().map(String::as_str).collect();
    let known_tools: std::collections::HashSet<String> = tool_names().into_iter().collect();
    assert!(!read_only.is_empty());

    for name in &read_only {
        assert!(
            known_tools.contains(name),
            "read-only tool should be a known MCP tool: {name}"
        );
    }

    for mutating in [
        "tokensave_str_replace",
        "tokensave_multi_str_replace",
        "tokensave_insert_at",
        "tokensave_ast_grep_rewrite",
        "tokensave_replace_symbol",
        "tokensave_insert_at_symbol",
        "tokensave_run_affected_tests",
        "tokensave_session_start",
        "tokensave_session_end",
        "tokensave_fact_store",
        "tokensave_fact_feedback",
    ] {
        assert!(
            !read_only_set.contains(mutating),
            "mutating tool should not be read-only: {mutating}"
        );
    }
}

#[test]
fn test_expected_tool_perms_not_empty() {
    let perms = expected_tool_perms();
    assert!(!perms.is_empty());
    for perm in &perms {
        assert!(
            perm.starts_with("mcp__tokensave__"),
            "tool perm should start with mcp__tokensave__: {perm}"
        );
    }
}

#[test]
fn test_tool_perms_match_tool_names() {
    let names = tool_names();
    let perms = expected_tool_perms();
    assert_eq!(
        names.len(),
        perms.len(),
        "tool_names and expected_tool_perms should have same length"
    );
    for name in &names {
        let expected_perm = format!("mcp__tokensave__{name}");
        assert!(
            perms.contains(&expected_perm),
            "missing permission for tool {name}: expected {expected_perm}"
        );
    }
}

// ---------------------------------------------------------------------------
// 11. restore_config_backup
// ---------------------------------------------------------------------------

#[test]
fn test_restore_config_backup_restores_content() {
    let dir = TempDir::new().unwrap();
    let original_path = dir.path().join("config.json");
    let backup_path = dir.path().join("config.json.bak");

    // Create original and backup
    std::fs::write(&original_path, r#"{"version": 1}"#).unwrap();
    std::fs::write(&backup_path, r#"{"version": 1}"#).unwrap();

    // Corrupt the original
    std::fs::write(&original_path, "CORRUPTED").unwrap();

    // Restore from backup
    restore_config_backup(&original_path, &backup_path);

    let restored = std::fs::read_to_string(&original_path).unwrap();
    assert_eq!(
        restored, r#"{"version": 1}"#,
        "restored content should match the backup"
    );
}

#[test]
fn test_restore_config_backup_to_missing_original() {
    let dir = TempDir::new().unwrap();
    let original_path = dir.path().join("config.json");
    let backup_path = dir.path().join("config.json.bak");

    // Only create backup, not original
    std::fs::write(&backup_path, r#"{"saved": true}"#).unwrap();

    restore_config_backup(&original_path, &backup_path);

    assert!(
        original_path.exists(),
        "original should be created from backup"
    );
    let content = std::fs::read_to_string(&original_path).unwrap();
    assert_eq!(content, r#"{"saved": true}"#);
}

#[test]
fn test_restore_config_backup_missing_backup_does_not_panic() {
    let dir = TempDir::new().unwrap();
    let original_path = dir.path().join("config.json");
    let backup_path = dir.path().join("config.json.bak");

    std::fs::write(&original_path, "original").unwrap();

    // Restore with a nonexistent backup — should not panic
    restore_config_backup(&original_path, &backup_path);

    // Original should remain untouched since backup failed
    let content = std::fs::read_to_string(&original_path).unwrap();
    assert_eq!(content, "original");
}

// ---------------------------------------------------------------------------
// 12. which_tokensave
// ---------------------------------------------------------------------------

#[test]
fn test_which_tokensave_returns_some_or_none() {
    // which_tokensave checks current_exe and PATH — we just verify it
    // doesn't panic and returns a sensible result.
    let result = which_tokensave();
    // In a test environment, the current exe is the test runner, not tokensave,
    // so it may return None (unless tokensave is on PATH). Either way, no panic.
    if let Some(ref path) = result {
        assert!(!path.is_empty(), "path should not be empty if Some");
    }
    // Test passes regardless of Some or None — just ensures no panic.
}

// ---------------------------------------------------------------------------
// 13. home_dir
// ---------------------------------------------------------------------------

#[test]
fn test_home_dir_returns_some() {
    let result = home_dir();
    assert!(
        result.is_some(),
        "home_dir should return Some on most systems"
    );
    let home = result.unwrap();
    assert!(home.is_absolute(), "home dir should be an absolute path");
}

// ---------------------------------------------------------------------------
// 14. migrate_installed_agents
// ---------------------------------------------------------------------------

#[test]
fn test_migrate_installed_agents_skips_when_already_populated() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let mut config = tokensave::user_config::UserConfig {
        installed_agents: vec!["claude".to_string()],
        ..Default::default()
    };

    // Should return immediately since installed_agents is non-empty
    migrate_installed_agents(home, &mut config);

    // The existing list should be unchanged
    assert_eq!(config.installed_agents, vec!["claude".to_string()]);
}

#[test]
fn test_migrate_installed_agents_detects_installed_agents() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();

    // Install copilot so it can be detected
    let ctx = make_install_ctx(home);
    CopilotIntegration.install(&ctx).unwrap();

    let mut config = tokensave::user_config::UserConfig::default();
    assert!(config.installed_agents.is_empty());

    // migrate will scan and detect copilot is installed
    // Note: save() will try to write to ~/.tokensave/config.toml which may fail
    // in CI, but the function still populates installed_agents in memory.
    migrate_installed_agents(home, &mut config);

    assert!(
        config.installed_agents.contains(&"copilot".to_string()),
        "copilot should be detected, got: {:?}",
        config.installed_agents
    );
}

#[test]
fn test_migrate_installed_agents_empty_home_no_change() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let mut config = tokensave::user_config::UserConfig::default();

    migrate_installed_agents(home, &mut config);

    // No agents installed in empty home, list should remain empty
    assert!(
        config.installed_agents.is_empty(),
        "installed_agents should remain empty when no agents detected"
    );
}

// ---------------------------------------------------------------------------
// 15. pick_integrations_interactive (no-agent-detected error path)
// ---------------------------------------------------------------------------

#[test]
fn test_pick_integrations_interactive_no_agents_detected() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();

    // Empty home — no agents detected
    let result = pick_integrations_interactive(home, &[]);
    assert!(
        result.is_err(),
        "pick_integrations_interactive should error when no agents detected"
    );

    let err_msg = format!("{}", result.unwrap_err());
    assert!(
        err_msg.contains("No supported agents detected"),
        "error should mention no agents detected, got: {err_msg}"
    );
}

#[test]
fn test_pick_integrations_interactive_single_uninstalled_agent() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();

    // Create only the .copilot dir so exactly one agent is detected
    std::fs::create_dir_all(home.join(".copilot")).unwrap();

    // Single detected agent that is NOT installed => fast path returns it directly
    let result = pick_integrations_interactive(home, &[]);
    assert!(
        result.is_ok(),
        "should succeed with single uninstalled agent"
    );
    let (to_install, to_uninstall) = result.unwrap();
    assert_eq!(to_install, vec!["copilot".to_string()]);
    assert!(to_uninstall.is_empty());
}

// ---------------------------------------------------------------------------
// 16. vscode_data_dir / copilot_cli_dir
// ---------------------------------------------------------------------------

#[test]
fn test_vscode_data_dir_is_under_home() {
    let home = Path::new("/fake/home");
    let dir = tokensave::agents::vscode_data_dir(home);
    assert!(
        dir.starts_with("/fake/home"),
        "vscode_data_dir should be under home: {}",
        dir.display()
    );
}

#[test]
fn test_copilot_cli_dir_is_under_home() {
    let home = Path::new("/fake/home");
    let dir = tokensave::agents::copilot_cli_dir(home);
    assert_eq!(
        dir,
        Path::new("/fake/home/.copilot"),
        "copilot_cli_dir should be home/.copilot"
    );
}

// ---------------------------------------------------------------------------
// 17. parse_jsonc edge cases
// ---------------------------------------------------------------------------

#[test]
fn test_parse_jsonc_empty_string() {
    let val = parse_jsonc("");
    assert!(val.is_object());
    assert!(val.as_object().unwrap().is_empty());
}

#[test]
fn test_parse_jsonc_only_comments() {
    let input = "// just a comment\n/* block */\n";
    let val = parse_jsonc(input);
    assert!(val.is_object());
    assert!(val.as_object().unwrap().is_empty());
}

#[test]
fn test_parse_jsonc_nested_comments() {
    let input = r#"{
        "a": "hello // not a comment",
        /* this is a real comment */
        "b": true
    }"#;
    let val = parse_jsonc(input);
    assert_eq!(val["a"].as_str().unwrap(), "hello // not a comment");
    assert_eq!(val["b"], true);
}

#[test]
fn test_parse_jsonc_trailing_comma_in_object() {
    let input = r#"{"a": 1, "b": 2,}"#;
    let val = parse_jsonc(input);
    assert_eq!(val["a"], 1);
    assert_eq!(val["b"], 2);
}

#[test]
fn test_parse_jsonc_trailing_comma_in_array() {
    let input = r#"{"arr": [1, 2, 3,]}"#;
    let val = parse_jsonc(input);
    let arr = val["arr"].as_array().unwrap();
    assert_eq!(arr.len(), 3);
}

// ---------------------------------------------------------------------------
// 18. backup + safe_write round-trip
// ---------------------------------------------------------------------------

#[test]
fn test_backup_and_safe_write_round_trip() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("roundtrip.json");

    // Create initial file
    let initial = serde_json::json!({"name": "tokensave", "version": 1});
    safe_write_json_file(&path, &initial, None).unwrap();

    // Create backup
    let backup = backup_config_file(&path).unwrap();
    assert!(backup.is_some());
    let backup_path = backup.unwrap();

    // Overwrite with new content
    let updated = serde_json::json!({"name": "tokensave", "version": 2});
    safe_write_json_file(&path, &updated, Some(&backup_path)).unwrap();

    // Verify new content
    let content: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(content["version"], 2);

    // Verify backup still has old content
    let backup_content: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&backup_path).unwrap()).unwrap();
    assert_eq!(backup_content["version"], 1);

    // Restore from backup
    restore_config_backup(&path, &backup_path);
    let restored: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(restored["version"], 1);
}

#[test]
fn test_hermes_install_writes_and_preserves_project_root_pin() {
    let home = TempDir::new().unwrap();
    let pinned = InstallContext {
        home: home.path().to_path_buf(),
        tokensave_bin: "/usr/local/bin/tokensave".to_string(),
        tool_permissions: expected_tool_perms(),
        profile: None,
        project_root: Some(std::path::PathBuf::from("/pinned/project")),
        dashboard: true,
    };
    HermesIntegration.install(&pinned).unwrap();

    let tools_path = home.path().join(".hermes/plugins/tokensave/tools.py");
    let config_path = home.path().join(".hermes/config.yaml");
    let tools_py = std::fs::read_to_string(&tools_path).unwrap();
    assert!(
        tools_py.contains("PINNED_PROJECT_ROOT = \"/pinned/project\""),
        "install --project-root must write the pin into tools.py:\n{tools_py}"
    );
    let config = std::fs::read_to_string(&config_path).unwrap();
    assert!(
        config.contains("  tokensave:") && config.contains("    project_root: \"/pinned/project\""),
        "install --project-root must write the pin into the conventional plugins.tokensave config block:\n{config}"
    );

    // A reinstall without the flag must keep the pin (no silent unpinning).
    HermesIntegration
        .install(&make_install_ctx(home.path()))
        .unwrap();
    let tools_py = std::fs::read_to_string(&tools_path).unwrap();
    assert!(
        tools_py.contains("PINNED_PROJECT_ROOT = \"/pinned/project\""),
        "reinstall without --project-root must preserve the existing pin:\n{tools_py}"
    );
    let config = std::fs::read_to_string(&config_path).unwrap();
    assert!(
        config.contains("    project_root: \"/pinned/project\""),
        "reinstall must preserve the plugins.tokensave config pin:\n{config}"
    );

    // The config block is the authoritative pin home: a reinstall must
    // restore the pin even when the generated tools.py was deleted.
    std::fs::remove_file(&tools_path).unwrap();
    HermesIntegration
        .install(&make_install_ctx(home.path()))
        .unwrap();
    let tools_py = std::fs::read_to_string(&tools_path).unwrap();
    assert!(
        tools_py.contains("PINNED_PROJECT_ROOT = \"/pinned/project\""),
        "reinstall must restore the pin from plugins.tokensave.project_root:\n{tools_py}"
    );

    // Uninstall removes the generated pin from the config block.
    HermesIntegration
        .uninstall(&make_install_ctx(home.path()))
        .unwrap();
    let config = std::fs::read_to_string(&config_path).unwrap();
    assert!(
        !config.contains("project_root:") && !config.contains("  tokensave:"),
        "uninstall must remove the generated plugins.tokensave pin block:\n{config}"
    );

    // A fresh install without a pin stays unpinned.
    let fresh_home = TempDir::new().unwrap();
    HermesIntegration
        .install(&make_install_ctx(fresh_home.path()))
        .unwrap();
    let tools_py =
        std::fs::read_to_string(fresh_home.path().join(".hermes/plugins/tokensave/tools.py"))
            .unwrap();
    assert!(
        tools_py.contains("PINNED_PROJECT_ROOT = None"),
        "fresh install without --project-root must leave the plugin unpinned:\n{tools_py}"
    );
    let fresh_config =
        std::fs::read_to_string(fresh_home.path().join(".hermes/config.yaml")).unwrap();
    assert!(
        !fresh_config.contains("project_root:"),
        "fresh install without --project-root must not write a config pin:\n{fresh_config}"
    );
}

#[test]
fn test_hermes_generated_python_reads_plugins_tokensave_config_block() {
    let home = TempDir::new().unwrap();
    HermesIntegration
        .install(&make_install_ctx_with_real_bin(home.path()))
        .unwrap();

    let hermes_home = home.path().join(".hermes");
    let plugin_dir = hermes_home.join("plugins/tokensave");
    let script = plugin_dir.join("check_config_block.py");
    std::fs::write(
        &script,
        r#"
import importlib.machinery
import importlib.util
import json
import os
import pathlib
import sys

plugin_dir = pathlib.Path(sys.argv[1])
hermes_home = plugin_dir.parent.parent
os.environ["HERMES_HOME"] = str(hermes_home)

# Simulate a user (or the installer) putting settings in the conventional
# plugins.tokensave config block.
import yaml
config_path = hermes_home / "config.yaml"
config = yaml.safe_load(config_path.read_text()) or {}
plugins_cfg = config.setdefault("plugins", {})
plugins_cfg["tokensave"] = {
    "project_root": "/config/block/project",
    "summary_model": "glm-4.7",
}
config_path.write_text(yaml.dump(config, default_flow_style=False))

parent_name = "_hermes_user_config_block"
parent_spec = importlib.machinery.ModuleSpec(parent_name, None, is_package=True)
parent_spec.submodule_search_locations = []
sys.modules[parent_name] = importlib.util.module_from_spec(parent_spec)
module_name = f"{parent_name}.tokensave"
spec = importlib.util.spec_from_file_location(
    module_name,
    plugin_dir / "__init__.py",
    submodule_search_locations=[str(plugin_dir)],
)
plugin = importlib.util.module_from_spec(spec)
sys.modules[module_name] = plugin
spec.loader.exec_module(plugin)

# The subprocess dispatch path resolves the pin from the config block even
# though tools.py was generated without a PINNED_PROJECT_ROOT.
assert plugin.tools.PINNED_PROJECT_ROOT is None
assert plugin.tools.config_pinned_project_root() == "/config/block/project"

captured = {}

class Result:
    returncode = 0
    stdout = "{}"
    stderr = ""

def fake_run(argv, **kwargs):
    captured["argv"] = argv
    return Result()

plugin.tools.subprocess.run = fake_run
plugin.tools.call_tokensave_tool("tokensave_status", {})
argv = captured["argv"]
assert "--project" in argv, argv
assert argv[argv.index("--project") + 1] == "/config/block/project", argv

# The context engine layers the block under the host config: block values
# fill gaps, host-provided values always win.
engine = plugin.TokenSaveContextEngine()
assert engine.project_root == "/config/block/project", engine.project_root
assert plugin._lcm_str_setting(engine.config, "LCM_SUMMARY_MODEL", "summary_model", default="") == "glm-4.7"

host_engine = plugin.TokenSaveContextEngine(config={"project_root": "/host/wins", "summary_model": "host-model"})
assert host_engine.project_root == "/host/wins"
assert plugin._lcm_str_setting(host_engine.config, "LCM_SUMMARY_MODEL", "summary_model", default="") == "host-model"

# Attribute-style host configs chain through to the block too.
class HostConfig:
    summary_model = None
    fresh_tail_count = 16

attr_engine = plugin.TokenSaveContextEngine(config=HostConfig())
assert plugin._lcm_str_setting(attr_engine.config, "LCM_SUMMARY_MODEL", "summary_model", default="") == "glm-4.7"
assert plugin._configured_int(attr_engine.config, "fresh_tail_count") == 16

# Engines bound to a different profile home do not inherit this block.
other_engine = plugin.TokenSaveContextEngine(hermes_home="/tmp/definitely-missing-hermes-home")
assert other_engine.project_root is None
"#,
    )
    .unwrap();

    let mut check = Command::new("python3");
    check.arg(&script).arg(&plugin_dir);
    if let Some(shim_dir) = pyyaml_shim_pythonpath(home.path()) {
        check.env("PYTHONPATH", shim_dir);
    }
    let output = check
        .output()
        .expect("python3 should run generated Hermes config block check");
    assert!(
        output.status.success(),
        "generated plugin should read the plugins.tokensave config block\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
