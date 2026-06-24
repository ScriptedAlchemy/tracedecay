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

    find_on_path("ast-grep")
        .or_else(|| {
            common_tool_paths("ast-grep")
                .into_iter()
                .find(|path| is_executable_file(path))
        })
        .unwrap_or_else(|| PathBuf::from("ast-grep"))
}

fn find_on_path(tool: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths)
        .map(|dir| dir.join(tool))
        .find(|path| is_executable_file(path))
}

fn common_tool_paths(tool: &str) -> Vec<PathBuf> {
    let mut candidates = Vec::new();

    if let Ok(current_exe) = std::env::current_exe() {
        if let Some(parent) = current_exe.parent() {
            candidates.push(parent.join(tool));
        }
    }

    if let Some(home) = std::env::var_os("HOME").filter(|home| !home.is_empty()) {
        let home = PathBuf::from(home);
        candidates.push(home.join(".local/bin").join(tool));
        candidates.push(home.join(".cargo/bin").join(tool));
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
