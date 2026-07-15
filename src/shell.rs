//! Shared shell-command parsing helpers for hooks and analytics classifiers.

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ShellInvocation {
    pub(crate) base: String,
    pub(crate) args: Vec<String>,
}

/// Splits a shell command line into words.
///
/// On Unix-like hosts this delegates to `shell-words` for POSIX-compatible
/// shell word parsing. Windows keeps the local fallback because unquoted paths
/// such as `C:\Users\me\repo` are common in hook payloads and POSIX parsers
/// interpret those backslashes as escapes.
pub(crate) fn shell_words(command: &str) -> Vec<String> {
    shell_words_for_platform(command, cfg!(windows))
}

pub(crate) fn shell_invocations(command: &str) -> Vec<ShellInvocation> {
    let tokens = shell_words(command);
    let mut invocations = Vec::new();
    let mut idx = 0;
    while idx < tokens.len() {
        let token = clean_shell_token(&tokens[idx]);
        let lower = token.to_ascii_lowercase();
        if is_shell_separator(&lower) || is_env_assignment(&token) {
            idx += 1;
            continue;
        }
        if matches!(lower.as_str(), "env" | "time" | "command" | "noglob") {
            idx += 1;
            continue;
        }
        if matches!(lower.as_str(), "cd" | "pushd") {
            idx += 1;
            while idx < tokens.len() {
                let separator = clean_shell_token(&tokens[idx]);
                idx += 1;
                if is_shell_separator(&separator) {
                    break;
                }
            }
            continue;
        }
        if matches!(lower.as_str(), "bash" | "sh" | "zsh")
            && tokens
                .get(idx + 1)
                .is_some_and(|token| matches!(token.as_str(), "-lc" | "-c"))
        {
            if let Some(nested) = tokens.get(idx + 2) {
                invocations.extend(shell_invocations(nested));
            }
            idx += 3;
            continue;
        }

        let base = program_base(&token);
        idx += 1;
        let mut args = Vec::new();
        while idx < tokens.len() {
            let arg = clean_shell_token(&tokens[idx]);
            if is_shell_separator(&arg) {
                break;
            }
            args.push(arg);
            idx += 1;
        }
        invocations.push(ShellInvocation { base, args });
    }
    invocations
}

fn shell_words_for_platform(command: &str, windows: bool) -> Vec<String> {
    if !windows && let Ok(words) = shell_words::split(command) {
        return words;
    }
    fallback_shell_words(command, windows)
}

fn fallback_shell_words(command: &str, windows: bool) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;

    for c in command.chars() {
        if escaped {
            current.push(c);
            escaped = false;
            continue;
        }

        match quote {
            Some('\'') => {
                if c == '\'' {
                    quote = None;
                } else {
                    current.push(c);
                }
            }
            Some('"') => match c {
                '"' => quote = None,
                '\\' => escaped = true,
                _ => current.push(c),
            },
            _ => match c {
                '\'' | '"' => quote = Some(c),
                '\\' if windows => current.push(c),
                '\\' => escaped = true,
                c if c.is_whitespace() => {
                    if !current.is_empty() {
                        words.push(std::mem::take(&mut current));
                    }
                }
                _ => current.push(c),
            },
        }
    }

    if escaped {
        current.push('\\');
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

fn clean_shell_token(token: &str) -> String {
    token
        .trim_matches(|c| matches!(c, '(' | ')' | '"' | '\''))
        .to_string()
}

fn is_shell_separator(token: &str) -> bool {
    matches!(token, "&&" | "||" | "|" | ";" | "then" | "do")
}

fn is_env_assignment(token: &str) -> bool {
    let Some((key, _)) = token.split_once('=') else {
        return false;
    };
    !key.is_empty()
        && key
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

fn program_base(program: &str) -> String {
    let program = program.trim_start_matches('(').to_ascii_lowercase();
    program
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(&program)
        .to_string()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn shell_words_preserves_unquoted_windows_paths() {
        assert_eq!(
            shell_words_for_platform(r"git --work-tree=C:\Users\me\repo pull", true),
            vec!["git", r"--work-tree=C:\Users\me\repo", "pull"]
        );
        assert_eq!(
            shell_words_for_platform(r"git --work-tree=C:\Users\me\repo pull", false),
            vec!["git", r"--work-tree=C:Usersmerepo", "pull"]
        );
    }

    #[test]
    fn shell_words_uses_posix_quoting_on_unix() {
        assert_eq!(
            shell_words_for_platform(r#"rg foo\ bar "src/hooks""#, false),
            vec!["rg", "foo bar", "src/hooks"]
        );
    }

    #[test]
    fn shell_words_falls_back_for_malformed_input() {
        assert_eq!(
            shell_words_for_platform(r#"grep "unterminated src"#, false),
            vec!["grep", "unterminated src"]
        );
    }

    #[test]
    fn shell_invocations_extract_nested_commands() {
        assert_eq!(
            shell_invocations(r#"cd /tmp && FOO=1 bash -lc "rg 'foo bar' src""#),
            vec![ShellInvocation {
                base: "rg".to_string(),
                args: vec!["foo bar".to_string(), "src".to_string()],
            }]
        );
    }
}
