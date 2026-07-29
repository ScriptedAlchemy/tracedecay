//! Shared fixtures and assertion helpers for the agent-integration test
//! modules in this binary. Extracted from the former monolithic
//! `agent_test.rs`; each per-area module imports these via
//! `use crate::agent_test_support::*`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;

use tempfile::TempDir;
use tracedecay::agents::*;
use tracedecay::automation::managed_skills::{
    ManagedSkillDraft, ManagedSkillProvenance, ManagedSkillSource,
};
use tracedecay::config::USER_DATA_DIR_ENV;

// 3. Install / config creation tests (with tempdir)
// ---------------------------------------------------------------------------

/// Install contexts in this suite disable the Hermes dashboard-wrapper
/// deploy: none of these tests assert on the deployed `dashboard/` page
/// (that coverage lives in `hermes_dashboard_test`), and skipping it avoids
/// rewriting ~300KB of embedded UI bundles per install — a real cost on
/// Windows CI. Agents other than Hermes ignore the flag entirely.
pub fn make_install_ctx(home: &Path) -> InstallContext {
    InstallContext {
        home: home.to_path_buf(),
        tracedecay_bin: "/usr/local/bin/tracedecay".to_string(),
        tool_permissions: expected_tool_perms(),
        project_root: None,
        dashboard: false,
    }
}

/// The Hermes plugin generated for [`make_install_ctx`] is identical for
/// every empty test home (fixed fake binary path, no profile, no pin), so
/// render it once per process and copy the resulting `.hermes` tree into
/// each test home instead of re-running template + tool-schema generation.
/// Tests that pre-seed `~/.hermes/config.yaml` (config-merge coverage) must
/// keep calling `HermesIntegration.install` directly.
pub static HERMES_DEFAULT_INSTALL_TEMPLATE: std::sync::OnceLock<
    Vec<(std::path::PathBuf, Vec<u8>)>,
> = std::sync::OnceLock::new();

pub fn install_hermes_default(home: &Path) {
    let files = HERMES_DEFAULT_INSTALL_TEMPLATE.get_or_init(|| {
        let template_home = TempDir::new().unwrap();
        HermesIntegration
            .install(&make_install_ctx(template_home.path()))
            .unwrap();
        let root = template_home.path().join(".hermes");
        let mut files = Vec::new();
        collect_files_recursive(&root, &root, &mut files);
        assert!(
            !files.is_empty(),
            "hermes install template should contain generated files"
        );
        files
    });
    for (relative, contents) in files {
        let path = home.join(".hermes").join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }
}

pub fn collect_files_recursive(
    root: &Path,
    dir: &Path,
    out: &mut Vec<(std::path::PathBuf, Vec<u8>)>,
) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect_files_recursive(root, &path, out);
        } else {
            let relative = path.strip_prefix(root).unwrap().to_path_buf();
            out.push((relative, std::fs::read(&path).unwrap()));
        }
    }
}

pub fn managed_skill_draft(id: &str, title: &str) -> ManagedSkillDraft {
    ManagedSkillDraft {
        id: id.to_string(),
        title: title.to_string(),
        summary: format!("{title} summary"),
        category: "workflow".to_string(),
        targets: tracedecay::automation::managed_skills::default_managed_skill_targets(),
        body_markdown: format!("Use {title} for repeated workflows."),
        support_files: Vec::new(),
        provenance: ManagedSkillProvenance {
            source: ManagedSkillSource::UserDraft,
            actor: "test".to_string(),
            run_id: None,
        },
    }
}

pub fn tracedecay_command(project: &Path, home: &Path) -> Command {
    let tracedecay_bin = Path::new(env!("CARGO_BIN_EXE_tracedecay"));
    let mut command = Command::new(tracedecay_bin);
    command
        .current_dir(project)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env(USER_DATA_DIR_ENV, home.join(".tracedecay"))
        .env("KIRO_HOME", home.join(".kiro"))
        .env("VIBE_HOME", home.join(".vibe"));
    if let Some(bin_dir) = tracedecay_bin.parent() {
        command.env("PATH", std::env::join_paths([bin_dir]).unwrap());
    }
    command
}

pub fn run_local_install(agent: &str, project: &Path, home: &Path) -> std::process::Output {
    tracedecay_command(project, home)
        .arg("install")
        .arg("--local")
        .arg("--agent")
        .arg(agent)
        .output()
        .unwrap_or_else(|e| panic!("failed to run local install for {agent}: {e}"))
}

pub fn assert_local_install_success(
    agent: &str,
    project: &Path,
    home: &Path,
) -> std::process::Output {
    let output = run_local_install(agent, project, home);
    assert!(
        output.status.success(),
        "local install for {agent} should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

pub fn read_json(path: &Path) -> serde_json::Value {
    serde_json::from_str(
        &std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("failed to read JSON {}: {e}", path.display())),
    )
    .unwrap_or_else(|e| panic!("failed to parse JSON {}: {e}", path.display()))
}

pub fn seed_memory_digest_target(
    profile_root: &Path,
    target: tracedecay::automation::skill_targets::SkillInstallTarget,
    output: &Path,
) {
    let path = profile_root.join("agent_managed/memory_digest_targets.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        path,
        serde_json::to_string_pretty(&serde_json::json!({
            "targets": [{
                "target": target,
                "output": output,
            }]
        }))
        .unwrap(),
    )
    .unwrap();
}

pub fn expected_tracedecay_bin() -> String {
    let path = std::fs::canonicalize(env!("CARGO_BIN_EXE_tracedecay"))
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_BIN_EXE_tracedecay")))
        .to_string_lossy()
        .replace('\\', "/");
    if cfg!(windows) {
        path.strip_prefix("//?/").unwrap_or(&path).to_string()
    } else {
        path
    }
}

pub fn expected_tracedecay_bin_variants() -> Vec<String> {
    let raw = PathBuf::from(env!("CARGO_BIN_EXE_tracedecay"));
    let canonical = std::fs::canonicalize(&raw).unwrap_or_else(|_| raw.clone());
    let mut variants = Vec::new();
    for path in [raw, canonical] {
        let native = path.to_string_lossy().to_string();
        let slash = native.replace('\\', "/");
        if !variants.contains(&native) {
            variants.push(native);
        }
        if !variants.contains(&slash) {
            variants.push(slash);
        }
    }
    variants
}

pub fn contains_expected_tracedecay_bin(body: &str) -> bool {
    let slash_body = body.replace('\\', "/");
    expected_tracedecay_bin_variants().iter().any(|expected| {
        body.contains(expected) || slash_body.contains(&expected.replace('\\', "/"))
    })
}

pub fn comparable_command_path(command: &str) -> String {
    command
        .strip_prefix("//?/")
        .unwrap_or(command)
        .replace('\\', "/")
}

pub fn assert_command_eq(actual: &serde_json::Value, expected: &str) {
    let actual = actual
        .as_str()
        .unwrap_or_else(|| panic!("command should be a string: {actual}"));
    assert_eq!(
        comparable_command_path(actual),
        comparable_command_path(expected)
    );
}

/// Python snippet that py_compiles the generated plugin sources inside the
/// same interpreter that runs a test's check script, instead of the separate
/// `python3 -m py_compile` process `assert_python_compiles` spawns. On
/// Windows CI every python launch goes through a .cmd shim and costs ~1s, so
/// folding the compile check into the check script halves those tests'
/// interpreter launches while keeping compile failures attributable.
/// `plugin_dir_expr` is a Python expression evaluating to the plugin dir.
pub fn python_compile_check(plugin_dir_expr: &str) -> String {
    format!(
        r#"
import pathlib as _compile_pathlib
import py_compile as _py_compile
import sys as _compile_sys

for _name in ("tools.py", "schemas.py", "__init__.py"):
    try:
        _py_compile.compile(str(({plugin_dir_expr}) / _name), doraise=True)
    except _py_compile.PyCompileError as _exc:
        print(f"generated Python should compile: {{_name}}: {{_exc}}", file=_compile_sys.stderr)
        _compile_sys.exit(1)
"#
    )
}

pub fn python_command() -> Command {
    static PYTHON: LazyLock<PathBuf> = LazyLock::new(|| {
        if cfg!(windows) {
            for var in ["Python_ROOT_DIR", "pythonLocation"] {
                if let Some(root) = std::env::var_os(var) {
                    let exe = Path::new(&root).join("python.exe");
                    if exe.is_file() {
                        return exe;
                    }
                }
            }
        }
        PathBuf::from("python3")
    });
    Command::new(&*PYTHON)
}

pub fn assert_python_compiles(paths: &[&Path]) {
    let output = python_command()
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

pub fn cursor_plugin_install_dir(home: &Path) -> std::path::PathBuf {
    home.join(".cursor/plugins/local/tracedecay")
}

pub fn codex_plugin_install_dir(home: &Path) -> std::path::PathBuf {
    home.join("plugins/tracedecay")
}

pub fn codex_cached_plugin_install_dir(home: &Path) -> std::path::PathBuf {
    home.join(".codex/plugins/cache/personal/tracedecay")
        .join(env!("CARGO_PKG_VERSION"))
}

pub fn codex_stale_cached_plugin_install_dir(home: &Path) -> std::path::PathBuf {
    home.join(".codex/plugins/cache/personal/tracedecay/0.0.4")
}

pub fn codex_legacy_cached_plugin_install_dir(home: &Path) -> std::path::PathBuf {
    home.join(".codex/plugins/cache/caveman-home/tracedecay/0.0.4")
}

pub fn codex_personal_marketplace_path(home: &Path) -> std::path::PathBuf {
    home.join(".agents/plugins/marketplace.json")
}

pub fn write_codex_personal_marketplace(home: &Path, name: &str, display_name: &str) {
    std::fs::create_dir_all(home.join(".agents/plugins")).unwrap();
    std::fs::write(
        codex_personal_marketplace_path(home),
        format!(
            r#"{{"interface":{{"displayName":"{display_name}"}},"name":"{name}","plugins":[{{"name":"tracedecay","source":{{"source":"local","path":"./plugins/tracedecay"}}}}]}}"#
        ),
    )
    .unwrap();
}

pub fn codex_repo_marketplace_path(project: &Path) -> std::path::PathBuf {
    project.join(".agents/plugins/marketplace.json")
}

pub fn codex_project_plugin_install_dir(project: &Path) -> std::path::PathBuf {
    project.join("plugins/tracedecay")
}

pub fn write_codex_plugin_manifest(plugin_dir: &Path, version: &str) {
    std::fs::create_dir_all(plugin_dir.join(".codex-plugin")).unwrap();
    std::fs::write(
        plugin_dir.join(".codex-plugin/plugin.json"),
        format!(r#"{{"name":"tracedecay","version":"{version}"}}"#),
    )
    .unwrap();
}

pub fn write_stale_codex_skill(plugin_dir: &Path) {
    std::fs::create_dir_all(plugin_dir.join("skills/stale-skill")).unwrap();
    std::fs::write(
        plugin_dir.join("skills/stale-skill/SKILL.md"),
        "---\nname: tracedecay:stale-skill\n---\n",
    )
    .unwrap();
}

pub fn assert_codex_plugin_bundle(
    plugin_dir: &Path,
    expected_command: &str,
    expected_args: serde_json::Value,
    expected_global_bundle: bool,
) {
    let manifest = read_json(&plugin_dir.join(".codex-plugin/plugin.json"));
    assert_eq!(manifest["name"], "tracedecay");
    assert_eq!(manifest["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(manifest["license"], "MIT");
    assert_eq!(manifest["skills"], "./skills/");
    assert_eq!(manifest["mcpServers"], "./.mcp.json");
    if expected_global_bundle {
        assert_eq!(manifest["hooks"], "./hooks/hooks.json");
    } else {
        assert!(
            manifest.get("hooks").is_none(),
            "repo-local Codex plugin should not declare lifecycle hooks"
        );
    }

    let mcp = read_json(&plugin_dir.join(".mcp.json"));
    let server = &mcp["mcpServers"]["graph"];
    assert_eq!(server["type"], "stdio");
    assert_command_eq(&server["command"], expected_command);
    assert_eq!(server["args"], expected_args);
    assert_eq!(server["startup_timeout_sec"], 120);
    assert_eq!(server["tool_timeout_sec"], 900);
    if expected_global_bundle {
        assert_eq!(server["env"]["TRACEDECAY_ENABLE_GLOBAL_DB"], "1");
    } else {
        assert!(
            server.get("env").is_none(),
            "project-local Codex plugin MCP config should not opt into global DB"
        );
    }

    let hooks_path = plugin_dir.join("hooks/hooks.json");
    if expected_global_bundle {
        let hooks = read_json(&hooks_path);
        assert_codex_hooks_registered(&hooks);
        assert_command_contains_expected_bin(
            &hooks,
            "SessionStart",
            "hook-codex-session-start",
            expected_command,
        );
    } else {
        assert!(
            !hooks_path.exists(),
            "repo-local Codex plugin should not ship lifecycle hooks"
        );
    }

    let skill = std::fs::read_to_string(plugin_dir.join("skills/exploring-code/SKILL.md"))
        .expect("Codex plugin should ship tracedecay steering skills");
    assert!(skill.contains("tracedecay"));
}

pub fn assert_codex_marketplace_entry(
    marketplace_path: &Path,
    expected_name: &str,
    expected_display_name: &str,
    expected_source_path: &str,
) {
    let marketplace = read_json(marketplace_path);
    assert_eq!(marketplace["name"], expected_name);
    assert_eq!(
        marketplace["interface"]["displayName"],
        expected_display_name
    );
    let plugins = marketplace["plugins"]
        .as_array()
        .expect("marketplace plugins should be an array");
    let entry = plugins
        .iter()
        .find(|entry| entry["name"] == "tracedecay")
        .expect("marketplace should contain tracedecay");
    assert_eq!(entry["source"]["source"], "local");
    assert_eq!(entry["source"]["path"], expected_source_path);
    assert_eq!(entry["policy"]["installation"], "AVAILABLE");
    assert_eq!(entry["policy"]["authentication"], "ON_INSTALL");
    assert_eq!(entry["category"], "Productivity");
}

pub fn assert_codex_personal_marketplace_entry(home: &Path) {
    assert_codex_marketplace_entry(
        &codex_personal_marketplace_path(home),
        "personal",
        "Personal",
        "./plugins/tracedecay",
    );
}

pub fn assert_codex_repo_marketplace_entry(project: &Path) {
    assert_codex_marketplace_entry(
        &codex_repo_marketplace_path(project),
        "local-repo",
        "Local Repo",
        "./plugins/tracedecay",
    );
}

pub fn assert_cursor_plugin_bundle(
    plugin_dir: &Path,
    expected_command: &str,
    expected_version: &str,
) {
    let manifest = read_json(&plugin_dir.join(".cursor-plugin/plugin.json"));
    assert_eq!(manifest["name"], "tracedecay");
    assert_eq!(manifest["version"], expected_version);
    assert_eq!(manifest["license"], "MIT");
    assert_eq!(manifest["mcpServers"], "mcp.json");
    assert_eq!(manifest["hooks"], "hooks/hooks.json");
    // Documented manifest metadata shown in Cursor's plugin surfaces.
    assert_eq!(manifest["displayName"], "TraceDecay");
    assert_eq!(manifest["category"], "Developer Tools");
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
    assert_eq!(
        manifest["commands"], "commands/",
        "the manifest must declare the native Cursor commands surface"
    );
    assert!(
        manifest["rules"]
            .as_array()
            .is_some_and(|rules| rules.iter().any(|rule| rule == "rules/tracedecay.mdc")),
        "plugin manifest should reference the tracedecay Cursor rule"
    );

    let mcp = read_json(&plugin_dir.join("mcp.json"));
    let server = &mcp["mcpServers"]["tracedecay"];
    assert_eq!(server["type"], "stdio");
    assert_command_eq(&server["command"], expected_command);
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
        ("postToolUse", "hook-cursor-post-tool-use"),
        ("preCompact", "hook-cursor-pre-compact"),
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
                .is_some_and(|command| comparable_command_path(command)
                    .contains(&comparable_command_path(expected_command))),
            "plugin hook commands should use the installed tracedecay binary"
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

    let rule = std::fs::read_to_string(plugin_dir.join("rules/tracedecay.mdc")).unwrap();
    assert!(rule.contains("alwaysApply: true"));
    assert!(rule.contains("tracedecay MCP tools"));
    assert!(rule.to_lowercase().contains("fall back"));
    assert!(plugin_dir.join("README.md").exists());
}

pub fn valid_single_quoted_yaml_scalar(value: &str) -> bool {
    let Some(inner) = value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')) else {
        return false;
    };

    let mut chars = inner.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\'' && chars.next() != Some('\'') {
            return false;
        }
    }
    true
}

pub fn assert_hermes_config_enables_tracedecay_memory(config_path: &Path) -> String {
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
        config.contains("  provider: tracedecay"),
        "missing tracedecay memory provider:\n{config}"
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
        config.contains("- tracedecay"),
        "missing tracedecay plugin enablement:\n{config}"
    );
    assert!(
        config.contains("context:"),
        "missing context block (context.engine selects the plugin engine):\n{config}"
    );
    assert!(
        config.contains("  engine: tracedecay"),
        "missing tracedecay context engine activation:\n{config}"
    );
    config
}

/// Shared body of the per-agent symlink-containment tests. A `--local` install
/// must never follow a symlinked project-local parent (or file) that escapes
/// the project root: the guard has to fire *before* any bytes are written, so
/// the outside directory stays byte-for-byte untouched and the command fails
/// with a symlink-refusal error.
#[cfg(unix)]
pub fn assert_local_install_rejects_symlinked_target(
    agent: &str,
    link_relative: &str,
    link_is_dir: bool,
) {
    use std::os::unix::fs::symlink;

    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    let sentinel = outside.path().join("keep.txt");
    std::fs::write(&sentinel, "untouched\n").unwrap();

    let link = project.path().join(link_relative);
    if let Some(parent) = link.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let target = if link_is_dir {
        outside.path().to_path_buf()
    } else {
        sentinel.clone()
    };
    symlink(&target, &link).unwrap();

    let output = run_local_install(agent, project.path(), home.path());
    assert!(
        !output.status.success(),
        "{agent} local install must reject a symlinked project-local target ({link_relative})"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("symlink"),
        "{agent} error should explain the symlink refusal, got:\n{stderr}"
    );
    // Nothing was written through the symlink into the outside directory.
    assert_eq!(
        std::fs::read_to_string(&sentinel).unwrap(),
        "untouched\n",
        "{agent} must not write through the symlink"
    );
    let outside_entries = std::fs::read_dir(outside.path()).unwrap().count();
    assert_eq!(
        outside_entries, 1,
        "{agent} must not create files behind the symlinked target"
    );
}

/// Shared body of the per-agent `test_local_install_*_writes_project_paths`
/// tests. Split into one test per agent (instead of a single loop) so the
/// eleven CLI install spawns run in parallel; each spawn costs noticeable
/// wall time on Windows CI.
pub fn assert_local_install_writes_project_paths(agent: &str, paths: &[&str]) {
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
            body.contains("tracedecay"),
            "{agent} local file {} should mention tracedecay",
            path.display()
        );
        let is_instruction_file = matches!(
            path.extension().and_then(|ext| ext.to_str()),
            Some("md" | "mdc")
        );
        if is_instruction_file && agent != "codex" {
            assert!(
                body.contains("tracedecay_fact_store"),
                "{agent} local instruction file {} should mention fact memory tools",
                path.display()
            );
            assert!(
                body.contains("tracedecay_message_search"),
                "{agent} local instruction file {} should mention transcript message search",
                path.display()
            );
        }
        let is_codex_metadata = agent == "codex"
            && (*relative == ".agents/plugins/marketplace.json"
                || relative.ends_with(".codex-plugin/plugin.json"));
        if !is_instruction_file && !is_codex_metadata {
            assert!(
                contains_expected_tracedecay_bin(&body),
                "{agent} local config {} should use the resolved absolute tracedecay executable",
                path.display()
            );
        }
    }

    assert!(
        !home.path().join(".tracedecay/config.toml").exists(),
        "{agent} local install must not create or mutate user-level install tracking"
    );
}

/// Returns true if any matcher group registered under `event` has a handler
/// whose `command` contains `needle`. Mirrors Codex's nested hooks.json shape:
/// `hooks[event][] -> { matcher?, hooks: [ { type, command, timeout } ] }`.
pub fn codex_event_has_handler(hooks: &serde_json::Value, event: &str, needle: &str) -> bool {
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
pub fn codex_matcher_for_handler(
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

pub fn assert_codex_hooks_registered(hooks: &serde_json::Value) {
    assert!(
        codex_event_has_handler(hooks, "SessionStart", "hook-codex-session-start"),
        "Codex SessionStart hook should steer toward tracedecay MCP tools: {hooks}"
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
    assert!(
        codex_event_has_handler(hooks, "PostCompact", "hook-codex-post-compact"),
        "Codex PostCompact hook should generate app-server LCM summaries: {hooks}"
    );
    assert!(
        codex_event_has_handler(hooks, "Stop", "hook-codex-stop"),
        "Codex Stop hook should ingest and review the final user-scoped turn: {hooks}"
    );
    let matcher = codex_matcher_for_handler(hooks, "PostToolUse", "hook-codex-post-tool-use")
        .expect("PostToolUse handler should exist");
    assert!(
        matcher.contains("Bash") && matcher.contains("apply_patch"),
        "PostToolUse matcher should target Bash and apply_patch, got {matcher:?}"
    );
    let compact_matcher =
        codex_matcher_for_handler(hooks, "PostCompact", "hook-codex-post-compact")
            .expect("PostCompact handler should exist");
    assert!(
        compact_matcher.contains("auto") && compact_matcher.contains("manual"),
        "PostCompact matcher should target auto and manual compactions, got {compact_matcher:?}"
    );
}

pub fn assert_command_contains_expected_bin(
    hooks: &serde_json::Value,
    event: &str,
    needle: &str,
    expected: &str,
) {
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
    assert!(
        comparable_command_path(command).contains(&comparable_command_path(expected)),
        "Codex hook command must use the resolved absolute tracedecay executable, got {command}"
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
pub fn assert_install_backs_up_and_preserves(
    agent: &dyn AgentIntegration,
    home: &Path,
    original: &str,
    marker: &str,
) {
    // Serializes env-mutating installs; callers must not hold AgentEnvLock.
    let _agent_env = crate::common::AgentEnvLock::pin(home);
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

/// Creates a fake tracedecay binary in a temp dir and returns the path string.
/// This allows healthchecks to verify binary existence.
pub fn make_install_ctx_with_real_bin(home: &Path) -> InstallContext {
    let bin_dir = home.join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let bin_path = bin_dir.join("tracedecay");
    std::fs::write(&bin_path, "#!/bin/sh\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin_path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    InstallContext {
        home: home.to_path_buf(),
        tracedecay_bin: bin_path.to_string_lossy().to_string(),
        tool_permissions: expected_tool_perms(),
        project_root: None,
        dashboard: false,
    }
}
