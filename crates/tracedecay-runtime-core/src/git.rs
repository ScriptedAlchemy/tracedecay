//! Process-wide resolution of the `git` binary.
//!
//! The daemon and CLI spawn `git` from ~13 sites. A bare `Command::new("git")`
//! makes the OS re-walk `PATH` on every spawn — cheap on Linux/macOS but
//! ~100-300ms per spawn on Windows. This module resolves the `git` binary to an
//! absolute path exactly once (cached in a [`OnceLock`]) and hands every product
//! spawn site that cached path, so the long-running daemon never re-walks `PATH`.
//!
//! The gix-first read paths in [`crate::branch`] and [`crate::worktree`] are
//! unaffected: they still prefer in-process `gix` and only reach a `git`
//! subprocess as a gated fallback. This module only changes *which* program those
//! fallbacks (and the one-shot spawn sites) exec.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;

/// The literal used when resolution fails, preserving today's behavior (the OS
/// PATH-walks per spawn, but callers keep working).
const GIT_LITERAL: &str = "git";

/// Returns the resolved `git` program to spawn, as a cached `&'static OsStr`.
///
/// Resolution order (performed once, then cached):
///   1. The `GIT` environment variable, if set and non-empty (explicit override,
///      matching git's own habit of honoring a program override).
///   2. An absolute path found by a which-style walk of `PATH` (+ `PATHEXT` on
///      Windows).
///   3. The literal `"git"` fallback, so behavior is never worse than a bare
///      `Command::new("git")`.
///
/// Callers pass the result straight to `Command::new(..)` (both `std` and
/// `tokio` accept `impl AsRef<OsStr>`).
pub fn git_program() -> &'static OsStr {
    static PROGRAM: OnceLock<OsString> = OnceLock::new();
    PROGRAM.get_or_init(resolve_git_program).as_os_str()
}

fn resolve_git_program() -> OsString {
    // 1. Explicit override wins. Empty values are ignored so an accidental
    //    `GIT=` does not break spawns.
    if let Some(value) = std::env::var_os("GIT") {
        if !value.is_empty() {
            return value;
        }
    }

    // 2. which-style lookup over PATH (+ PATHEXT on Windows).
    if let Some(path) = find_in_path(GIT_LITERAL) {
        return path.into_os_string();
    }

    // 3. Fallback: let the OS resolve it per-spawn, as before.
    OsString::from(GIT_LITERAL)
}

/// Minimal `which`-style lookup: find `name` as an executable on `PATH`.
///
/// On Windows, each `PATH` entry is probed with every `PATHEXT` suffix (and the
/// bare name) so `git.exe` resolves from `git`. On Unix, the bare name is probed
/// and the entry must be a file (execute-permission is not separately checked —
/// git's own PATH lookup does not either, and a false positive simply degrades to
/// today's per-spawn PATH walk on exec failure).
fn find_in_path(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        if let Some(found) = probe_dir(&dir, name) {
            return Some(found);
        }
    }
    None
}

#[cfg(windows)]
fn probe_dir(dir: &Path, name: &str) -> Option<PathBuf> {
    // PATHEXT holds the executable suffixes (";"-separated), e.g.
    // ".COM;.EXE;.BAT;.CMD". Fall back to a sane default when unset.
    let pathext = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string());

    // If the name already carries an extension, try it verbatim first.
    let bare = dir.join(name);
    if bare.is_file() {
        return Some(bare);
    }
    for ext in pathext.split(';') {
        let ext = ext.trim();
        if ext.is_empty() {
            continue;
        }
        let candidate = dir.join(format!("{name}{ext}"));
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

#[cfg(not(windows))]
fn probe_dir(dir: &Path, name: &str) -> Option<PathBuf> {
    let candidate = dir.join(name);
    candidate.is_file().then_some(candidate)
}

/// Runs `git <args>` in `repo_root` with the resolved [`git_program`], returning
/// the command [`Output`] on a zero exit status, or `None` on spawn failure or a
/// non-zero exit. Use this when the raw, untrimmed stdout matters (multi-line
/// output such as `git reflog` or `git log`).
pub fn git_output(repo_root: &Path, args: &[&str]) -> Option<Output> {
    let output = Command::new(git_program())
        .args(args)
        .current_dir(repo_root)
        .output()
        .ok()?;
    output.status.success().then_some(output)
}

/// Runs `git <args>` in `repo_root` and returns the trimmed stdout as a
/// `String`, or `None` on spawn failure, non-zero exit, non-UTF-8 output, or
/// empty (after trimming) output. Convenience wrapper over [`git_output`] for
/// the common single-value reads (`rev-parse`, `config --get`, ...).
pub fn git_capture(repo_root: &Path, args: &[&str]) -> Option<String> {
    let output = git_output(repo_root, args)?;
    let text = String::from_utf8(output.stdout).ok()?;
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn git_program_is_stable_and_resolves() {
        // Cached: two calls return the identical pointer/value.
        let first = git_program();
        let second = git_program();
        assert_eq!(first, second);

        // Either an existing absolute path was found, or we fell back to the
        // literal "git" — never worse than a bare Command::new("git").
        let resolved = Path::new(first);
        assert!(
            resolved == Path::new(GIT_LITERAL) || resolved.is_file(),
            "git_program() should be the \"git\" fallback or an existing file, got {}",
            resolved.display()
        );
    }

    #[test]
    fn git_env_override_is_honored() {
        // resolve_git_program() reads GIT directly; test it in isolation so the
        // process-wide OnceLock cache in git_program() is untouched.
        let sentinel = "/nonexistent/tracedecay-test-git-override";
        unsafe {
            std::env::set_var("GIT", sentinel);
        }
        let resolved = resolve_git_program();
        unsafe {
            std::env::remove_var("GIT");
        }
        assert_eq!(resolved, OsString::from(sentinel));

        // An empty GIT is ignored (falls through to PATH lookup / literal).
        unsafe {
            std::env::set_var("GIT", "");
        }
        let resolved_empty = resolve_git_program();
        unsafe {
            std::env::remove_var("GIT");
        }
        assert_ne!(resolved_empty, OsString::from(""));
    }
}
