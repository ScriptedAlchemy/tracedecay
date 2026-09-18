//! Optional global git `post-commit` hook that runs `tracedecay sync`.
//!
//! Installed only when the operator passes `--git-hook`. Setting
//! `core.hooksPath` can redirect every repository away from `.git/hooks`, so
//! this stays an explicit authorization rather than a prompt. The install path
//! never reads stdin.

use std::path::{Path, PathBuf};

use super::host_config_io::{home_dir, quote_posix_command_arg};

/// The marker comment used to identify tracedecay's section in a hook script.
///
/// NOTE: Legacy hooks written by the old "tracedecay" binary used the marker
/// "# tracedecay: auto-sync". Those are not detected by this constant, so
/// existing tracedecay git hooks will not be treated as already-present and a
/// second tracedecay block may be appended on offer. This is intentional
/// (install path only writes new identity), users can manually remove the
/// old block.
const HOOK_MARKER: &str = "# tracedecay: auto-sync";

/// The hook snippet appended to (or written as) the post-commit script.
fn post_commit_snippet(tracedecay_bin: &str) -> String {
    let bin = quote_posix_command_arg(&tracedecay_bin.replace('\\', "/"));
    format!(
        "{HOOK_MARKER}\n\
         {bin} sync >/dev/null 2>&1 &\n"
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitPostCommitHookStatus {
    Present,
    Absent,
}

/// What installing the hook did. `Installed` includes the case where
/// `core.hooksPath` was unset and this call set it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitPostCommitHookInstall {
    AlreadyPresent,
    Installed { hooks_path_set: bool },
}

/// Report the hook without reading stdin or changing gitconfig.
///
/// Setting `core.hooksPath` redirects repositories away from `.git/hooks`, so
/// that step stays behind `--git-hook`. Supervision is the printed receipt, not a prompt.
#[hotpath::measure(label = "agent_hosts.agents.git.report_post_commit")]
pub fn report_git_post_commit_hook_status() {
    let Some(home) = home_dir() else {
        return;
    };
    match git_post_commit_hook_status(&home) {
        GitPostCommitHookStatus::Present => {
            eprintln!("  Global git post-commit hook already contains tracedecay");
        }
        GitPostCommitHookStatus::Absent => {
            eprintln!(
                "Git post-commit hook left unchanged. Pass --git-hook to install it. \
                 Setting core.hooksPath can redirect every repository away from .git/hooks, \
                 so that step stays explicit and never waits on stdin."
            );
        }
    }
}

/// Install the reversible sync hook. Never reads stdin.
#[hotpath::measure(label = "agent_hosts.agents.git.install_post_commit")]
pub fn install_git_post_commit_hook(tracedecay_bin: &str) -> std::result::Result<(), String> {
    let home = home_dir().ok_or_else(|| "could not determine home directory".to_string())?;
    match install_git_post_commit_hook_at(&home, tracedecay_bin)? {
        GitPostCommitHookInstall::AlreadyPresent => {
            eprintln!("  Global git post-commit hook already contains tracedecay, skipping");
        }
        GitPostCommitHookInstall::Installed { hooks_path_set } => {
            if hooks_path_set {
                eprintln!(
                    "\x1b[32m✔\x1b[0m Set git core.hooksPath for the tracedecay post-commit hook"
                );
            }
            eprintln!("\x1b[32m✔\x1b[0m Installed global git post-commit hook");
        }
    }
    Ok(())
}

pub(crate) fn install_git_post_commit_hook_at(
    home: &Path,
    tracedecay_bin: &str,
) -> std::result::Result<GitPostCommitHookInstall, String> {
    let hooks_dir = read_global_hooks_path(home);
    let (hooks_dir, need_set_hookspath) = match hooks_dir {
        Some(dir) => (dir, false),
        None => (home.join(".config").join("git").join("hooks"), true),
    };
    let hook_path = hooks_dir.join("post-commit");
    if hook_contains_marker(&hook_path) {
        return Ok(GitPostCommitHookInstall::AlreadyPresent);
    }
    write_git_post_commit_hook(
        home,
        &hooks_dir,
        need_set_hookspath,
        &hook_path,
        tracedecay_bin,
    )?;
    Ok(GitPostCommitHookInstall::Installed {
        hooks_path_set: need_set_hookspath,
    })
}

fn git_post_commit_hook_status(home: &Path) -> GitPostCommitHookStatus {
    let Some(hooks_dir) = read_global_hooks_path(home) else {
        let default = home
            .join(".config")
            .join("git")
            .join("hooks")
            .join("post-commit");
        return if hook_contains_marker(&default) {
            GitPostCommitHookStatus::Present
        } else {
            GitPostCommitHookStatus::Absent
        };
    };
    if hook_contains_marker(&hooks_dir.join("post-commit")) {
        GitPostCommitHookStatus::Present
    } else {
        GitPostCommitHookStatus::Absent
    }
}

fn hook_contains_marker(hook_path: &Path) -> bool {
    hook_path.exists()
        && std::fs::read_to_string(hook_path).is_ok_and(|contents| contents.contains(HOOK_MARKER))
}

fn write_git_post_commit_hook(
    home: &Path,
    hooks_dir: &Path,
    need_set_hookspath: bool,
    hook_path: &Path,
    tracedecay_bin: &str,
) -> std::result::Result<(), String> {
    std::fs::create_dir_all(hooks_dir)
        .map_err(|error| format!("Failed to create {}: {error}", hooks_dir.display()))?;

    // If no global hooksPath was configured, set it in ~/.gitconfig.
    if need_set_hookspath {
        let gitconfig_path = home.join(".gitconfig");
        set_global_hooks_path(&gitconfig_path, hooks_dir)?;
    }

    let snippet = post_commit_snippet(tracedecay_bin);
    if hook_path.exists() {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(hook_path)
            .map_err(|error| {
                format!(
                    "Failed to open {} for writing: {error}",
                    hook_path.display()
                )
            })?;
        write!(file, "\n{snippet}")
            .map_err(|error| format!("Failed to write to {}: {error}", hook_path.display()))?;
    } else {
        let contents = format!("#!/bin/sh\n{snippet}");
        std::fs::write(hook_path, contents)
            .map_err(|error| format!("Failed to create {}: {error}", hook_path.display()))?;
    }

    // Make executable (Unix).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(hook_path, std::fs::Permissions::from_mode(0o755)).map_err(
            |error| format!("Failed to mark {} executable: {error}", hook_path.display()),
        )?;
    }
    Ok(())
}

/// Reads `core.hooksPath` from the global gitconfig files.
///
/// Checks `~/.gitconfig` first, then `~/.config/git/config` (the XDG
/// location). Returns the resolved absolute path, or `None` if the key
/// is absent from both files.
fn read_global_hooks_path(home: &Path) -> Option<PathBuf> {
    let candidates = [
        home.join(".gitconfig"),
        home.join(".config").join("git").join("config"),
    ];
    for path in &candidates {
        if let Some(value) = parse_gitconfig_value(path, "core", "hookspath") {
            let expanded = expand_tilde(&value, home);
            let p = PathBuf::from(&expanded);
            if p.is_absolute() {
                return Some(p);
            }
            // Relative paths in gitconfig are relative to the home dir.
            return Some(home.join(p));
        }
    }
    None
}

/// Minimal gitconfig parser: finds the value of `key` under `[section]`.
///
/// Key matching is case-insensitive (git config keys are case-insensitive).
/// Handles `key = value`, `key=value`, and quoted values.
fn parse_gitconfig_value(path: &Path, section: &str, key: &str) -> Option<String> {
    let contents = std::fs::read_to_string(path).ok()?;
    let section_lower = section.to_ascii_lowercase();
    let key_lower = key.to_ascii_lowercase();

    let mut in_section = false;
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            // Parse section header: [core], [core "subsection"], etc.
            let header = trimmed
                .trim_start_matches('[')
                .split(']')
                .next()
                .unwrap_or("")
                .trim();
            let section_name = header.split_whitespace().next().unwrap_or("");
            in_section = section_name.eq_ignore_ascii_case(&section_lower);
            continue;
        }
        if !in_section {
            continue;
        }
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
            continue;
        }
        // Parse key = value
        if let Some((k, v)) = trimmed.split_once('=')
            && k.trim().to_ascii_lowercase() == key_lower
        {
            let v = v.trim();
            let v = v
                .strip_prefix('"')
                .and_then(|s| s.strip_suffix('"'))
                .unwrap_or(v);
            return Some(v.to_string());
        }
    }
    None
}

/// Appends `core.hooksPath` to the global gitconfig file, creating it if
/// necessary. Appends to an existing `[core]` section if one exists,
/// otherwise adds a new one at the end of the file.
fn set_global_hooks_path(
    gitconfig_path: &Path,
    hooks_dir: &Path,
) -> std::result::Result<(), String> {
    let hooks_str = hooks_dir.to_string_lossy().replace('\\', "/");
    let contents = if gitconfig_path.exists() {
        std::fs::read_to_string(gitconfig_path)
            .map_err(|e| format!("Failed to read {}: {e}", gitconfig_path.display()))?
    } else {
        String::new()
    };

    let new_contents = insert_gitconfig_value(&contents, "core", "hooksPath", &hooks_str);

    if let Some(parent) = gitconfig_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create {}: {e}", parent.display()))?;
    }
    std::fs::write(gitconfig_path, new_contents)
        .map_err(|e| format!("Failed to write {}: {e}", gitconfig_path.display()))?;
    Ok(())
}

/// Inserts `key = value` under `[section]` in gitconfig content.
/// If the section exists, appends the key after the last line of that section.
/// Otherwise appends a new section at the end.
fn insert_gitconfig_value(contents: &str, section: &str, key: &str, value: &str) -> String {
    let section_lower = section.to_ascii_lowercase();
    let lines: Vec<&str> = contents.lines().collect();
    let mut result = Vec::with_capacity(lines.len() + 3);
    let entry = format!("\t{key} = {value}");

    // Find the target section and the line index just before the next section.
    let mut section_end: Option<usize> = None;
    let mut in_section = false;
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            if in_section {
                // We've hit the next section, insert before it.
                section_end = Some(i);
                break;
            }
            let header = trimmed
                .trim_start_matches('[')
                .split(']')
                .next()
                .unwrap_or("")
                .trim();
            let name = header.split_whitespace().next().unwrap_or("");
            if name.eq_ignore_ascii_case(&section_lower) {
                in_section = true;
            }
        }
    }
    if in_section && section_end.is_none() {
        // Section runs to end of file.
        section_end = Some(lines.len());
    }

    if let Some(insert_at) = section_end {
        for (i, line) in lines.iter().enumerate() {
            if i == insert_at {
                result.push(entry.as_str());
            }
            result.push(line);
        }
        // If inserting at end-of-file.
        if insert_at == lines.len() {
            result.push(&entry);
        }
    } else {
        // Section doesn't exist, append it.
        for line in &lines {
            result.push(line);
        }
        if !contents.is_empty() && !contents.ends_with('\n') {
            result.push("");
        }
        let section_header = format!("[{section}]");
        // We need to own these strings for the result.
        // Re-build as a String directly instead.
        let mut out = result.join("\n");
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&section_header);
        out.push('\n');
        out.push_str(&entry);
        out.push('\n');
        return out;
    }

    let mut out = result.join("\n");
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// Expand a leading `~` to the given home directory.
fn expand_tilde(s: &str, home: &Path) -> String {
    if let Some(rest) = s.strip_prefix("~/") {
        return home.join(rest).to_string_lossy().replace('\\', "/");
    }
    if s == "~" {
        return home.to_string_lossy().to_string();
    }
    s.to_string()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn post_commit_snippet_quotes_posix_binary_paths_with_spaces() {
        let snippet = post_commit_snippet("/tmp/bin with spaces/tracedecay");

        assert!(snippet.contains("'/tmp/bin with spaces/tracedecay' sync"));
    }

    #[test]
    fn insert_into_existing_section() {
        let config = "[user]\n\tname = Test\n[core]\n\tautocrlf = true\n";
        let result = insert_gitconfig_value(config, "core", "hooksPath", "/tmp/hooks");
        assert!(result.contains("\thooksPath = /tmp/hooks"));
        assert!(result.contains("[core]"));
        assert!(result.contains("autocrlf = true"));
    }

    #[test]
    fn insert_new_section() {
        let config = "[user]\n\tname = Test\n";
        let result = insert_gitconfig_value(config, "core", "hooksPath", "/tmp/hooks");
        assert!(result.contains("[core]\n\thooksPath = /tmp/hooks"));
    }

    #[test]
    fn insert_before_next_section() {
        let config = "[core]\n\tautocrlf = true\n[user]\n\tname = Test\n";
        let result = insert_gitconfig_value(config, "core", "hooksPath", "/tmp/hooks");
        // hooksPath should appear after autocrlf but before [user]
        let hooks_pos = result.find("hooksPath").unwrap();
        let user_pos = result.find("[user]").unwrap();
        let autocrlf_pos = result.find("autocrlf").unwrap();
        assert!(hooks_pos > autocrlf_pos);
        assert!(hooks_pos < user_pos);
    }

    #[test]
    fn expand_tilde_with_slash() {
        let home = Path::new("/home/test");
        assert_eq!(expand_tilde("~/hooks", home), "/home/test/hooks");
    }

    #[test]
    fn install_writes_the_hook_without_reading_stdin_and_is_idempotent() {
        let home = tempfile::tempdir().unwrap();
        let first = install_git_post_commit_hook_at(home.path(), "/usr/bin/tracedecay").unwrap();
        assert_eq!(
            first,
            GitPostCommitHookInstall::Installed {
                hooks_path_set: true
            }
        );
        let hook = std::fs::read_to_string(
            home.path()
                .join(".config")
                .join("git")
                .join("hooks")
                .join("post-commit"),
        )
        .unwrap();
        assert!(hook.contains(HOOK_MARKER));
        assert!(hook.contains("/usr/bin/tracedecay"));
        let gitconfig = std::fs::read_to_string(home.path().join(".gitconfig")).unwrap();
        assert!(gitconfig.contains("hooksPath"));
        assert_eq!(
            install_git_post_commit_hook_at(home.path(), "/usr/bin/tracedecay").unwrap(),
            GitPostCommitHookInstall::AlreadyPresent
        );
        assert_eq!(
            git_post_commit_hook_status(home.path()),
            GitPostCommitHookStatus::Present
        );
    }
}
