use std::path::Path;
use std::process::Command;

use tempfile::TempDir;
use tokensave::agents::{AgentIntegration, HermesIntegration, InstallContext};

fn make_install_ctx(home: &Path) -> InstallContext {
    InstallContext {
        home: home.to_path_buf(),
        tokensave_bin: "/usr/local/bin/tokensave".to_string(),
        tool_permissions: Vec::new(),
        profile: None,
    }
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

#[test]
fn generated_context_engine_registers_when_supported() {
    let home = TempDir::new().unwrap();
    HermesIntegration
        .install(&make_install_ctx(home.path()))
        .unwrap();

    let plugin_dir = home.path().join(".hermes/plugins/tokensave");
    assert_python_compiles(&[
        &plugin_dir.join("tools.py"),
        &plugin_dir.join("schemas.py"),
        &plugin_dir.join("__init__.py"),
    ]);

    let script = plugin_dir.join("check_context_engine.py");
    std::fs::write(
        &script,
        r#"
import importlib.machinery
import importlib.util
import pathlib
import sys

plugin_dir = pathlib.Path(sys.argv[1])

parent_name = "_hermes_user_context"
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
    def __init__(self):
        self.tools = []
        self.hooks = []
        self.context_engines = []

    def register_tool(self, **kwargs):
        self.tools.append(kwargs)

    def register_hook(self, name, handler):
        self.hooks.append((name, handler))

    def register_context_engine(self, engine):
        self.context_engines.append(engine)

ctx = FullCtx()
plugin.register(ctx)
assert len(ctx.context_engines) == 1
engine = ctx.context_engines[0]
assert isinstance(engine, plugin.TokenSaveContextEngine)

engine.initialize(
    session_id="session-123",
    hermes_home="/tmp/hermes-profile",
    project_root="/tmp/project",
)
assert engine.active_session_id == "session-123"
assert engine.hermes_home == "/tmp/hermes-profile"
assert engine.project_root == "/tmp/project"

local_args = plugin._storage_args(project_root="/tmp/project", hermes_home="/tmp/hermes-profile")
assert local_args == {
    "storage_scope": "project_local",
    "project_root": "/tmp/project",
}

profile_args = plugin._storage_args(hermes_home="/tmp/hermes-profile")
assert profile_args == {
    "storage_scope": "hermes_profile",
    "hermes_home": "/tmp/hermes-profile",
}

fallback_args = plugin._storage_args()
assert fallback_args == {"storage_scope": "hermes_profile"}

class LegacyCtx:
    def register_tool(self, *args, **kwargs):
        pass

    def register_hook(self, *args, **kwargs):
        pass

legacy = LegacyCtx()
plugin.register(legacy)
"#,
    )
    .unwrap();

    let output = Command::new("python3")
        .arg(&script)
        .arg(plugin_dir)
        .output()
        .expect("python3 should run generated Hermes context engine check");
    assert!(
        output.status.success(),
        "generated plugin should register a Hermes context engine when supported\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
