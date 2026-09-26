//! Shared ast-grep executable authority for capability probes and source edits.

use std::path::{Path, PathBuf};
use std::process::Command;

const AST_GREP_BIN_ENV: &str = "TRACEDECAY_AST_GREP_BIN";

pub fn ast_grep_command() -> Command {
    Command::new(resolve_ast_grep_bin())
}

fn resolve_ast_grep_bin() -> PathBuf {
    if let Some(path) = std::env::var_os(AST_GREP_BIN_ENV).filter(|path| !path.is_empty()) {
        return PathBuf::from(path);
    }

    crate::git::find_executable_on_path("ast-grep")
        .map(|found| npm_native_binary(&found).unwrap_or(found))
        .or_else(|| {
            common_tool_paths("ast-grep")
                .into_iter()
                .find(|path| is_executable_file(path))
        })
        .unwrap_or_else(|| PathBuf::from("ast-grep"))
}

/// npm's Windows launcher is a `.cmd` that re-enters node, and `Command`
/// refuses multi-line arguments to a batch file.
fn npm_native_binary(launcher: &Path) -> Option<PathBuf> {
    let is_batch = launcher
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("cmd"));
    let native = launcher
        .parent()?
        .join("node_modules/@ast-grep/cli/ast-grep.exe");
    (is_batch && native.is_file()).then_some(native)
}

fn common_tool_paths(tool: &str) -> Vec<PathBuf> {
    let tool = format!("{tool}{}", std::env::consts::EXE_SUFFIX);
    let tool = tool.as_str();
    let mut candidates = Vec::new();

    if let Ok(current_exe) = std::env::current_exe()
        && let Some(parent) = current_exe.parent()
    {
        candidates.push(parent.join(tool));
    }

    candidates.push(PathBuf::from("/usr/local/bin").join(tool));
    candidates.push(PathBuf::from("/opt/homebrew/bin").join(tool));
    candidates.push(PathBuf::from("/usr/bin").join(tool));
    candidates.push(PathBuf::from("/bin").join(tool));

    candidates
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }

    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::ast_grep_command;

    #[test]
    fn explicit_binary_override_is_shared_by_probes_and_edits() {
        const CHILD: &str = "TRACEDECAY_TEST_AST_GREP_RESOLVER";
        if std::env::var_os(CHILD).is_some() {
            assert_eq!(ast_grep_command().get_program(), "explicit-ast-grep");
            return;
        }
        // A child isolates environment authority from concurrent tests.
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "ast_grep::tests::explicit_binary_override_is_shared_by_probes_and_edits",
            ])
            .env(CHILD, "1")
            .env("TRACEDECAY_AST_GREP_BIN", "explicit-ast-grep")
            .env_remove("PATH")
            .status()
            .unwrap();
        assert!(status.success());
    }
}
