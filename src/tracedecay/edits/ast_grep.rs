//! Structural rewrite via the `ast-grep` CLI, with a built-in literal-text
//! fallback when the tool is not installed and the pattern needs no
//! structural matching. `ast-grep` is only ever consulted for its read-only
//! structured replacement plan; the exact post-edit bytes are reconstructed
//! locally and applied through the same commit-or-preview gate every other
//! primitive uses.

use std::io::Write;
use std::path::Path;

use crate::errors::{Result, TraceDecayError};
use crate::types::AstGrepResult;

use super::super::TraceDecay;
use super::file_authority::SourceEditFileAuthority;
use super::preview::edit_success_message;

impl TraceDecay {
    /// Performs structural rewrite using ast-grep CLI.
    pub(crate) async fn ast_grep_rewrite(
        &self,
        path: &str,
        pattern: &str,
        rewrite: &str,
        dry_run: bool,
    ) -> Result<AstGrepResult> {
        let rel_path = self
            .resolve_path(path)
            .ok_or_else(|| TraceDecayError::Config {
                message: "path is not within the project".to_string(),
            })?;
        let file = SourceEditFileAuthority::open(&self.project_root, Path::new(&rel_path))?;
        let (source, source_identity) = file.read_to_string(path)?;

        let check_output = crate::external_tools::ast_grep_command()
            .args(["--version"])
            .output();

        if check_output.is_err() {
            if can_use_literal_rewrite_fallback(pattern) {
                if !source.contains(pattern) {
                    return Ok(AstGrepResult {
                        success: false,
                        file_path: rel_path.clone(),
                        pattern: pattern.to_string(),
                        rewrite: rewrite.to_string(),
                        dry_run,
                        diff: None,
                        message: "pattern not found (built-in literal fallback)".to_string(),
                    });
                }
                let modified = source.replace(pattern, rewrite);
                let diff = self
                    .commit_or_preview_edit(
                        &rel_path,
                        &file,
                        &source_identity,
                        &source,
                        &modified,
                        dry_run,
                    )
                    .await?;
                return Ok(AstGrepResult {
                    success: true,
                    file_path: rel_path,
                    pattern: pattern.to_string(),
                    rewrite: rewrite.to_string(),
                    dry_run,
                    diff,
                    message: edit_success_message(
                        dry_run,
                        "literal rewrite completed using built-in fallback",
                    ),
                });
            }
            return Ok(AstGrepResult {
                success: false,
                file_path: rel_path.clone(),
                pattern: pattern.to_string(),
                rewrite: rewrite.to_string(),
                dry_run,
                diff: None,
                message: "ast-grep is not installed and this pattern needs SGPattern matching. Simple literal rewrites are handled by the built-in fallback.".to_string(),
            });
        }

        // Always ask ast-grep for its read-only structured replacement plan.
        // Reconstructing the exact post-edit bytes here keeps dry-run capture
        // and real application behind the same write authority.
        let suffix = Path::new(&rel_path)
            .extension()
            .and_then(|extension| extension.to_str())
            .map_or_else(String::new, |extension| format!(".{extension}"));
        let mut snapshot = tempfile::Builder::new()
            .prefix("tracedecay-source-edit-")
            .suffix(&suffix)
            .tempfile()
            .map_err(|error| TraceDecayError::Config {
                message: format!("failed to stage source edit analysis snapshot: {error}"),
            })?;
        snapshot
            .write_all(source.as_bytes())
            .and_then(|()| snapshot.flush())
            .map_err(|error| TraceDecayError::Config {
                message: format!("failed to write source edit analysis snapshot: {error}"),
            })?;
        let snapshot_path_arg = snapshot.path().to_string_lossy();
        let mut ast_grep_args: Vec<&str> =
            vec!["run", "-p", pattern, "-r", rewrite, "--json=compact"];
        ast_grep_args.push(snapshot_path_arg.as_ref());
        let output = crate::external_tools::ast_grep_command()
            .args(&ast_grep_args)
            .output()
            .map_err(|e| TraceDecayError::Config {
                message: format!("failed to run ast-grep: {e}"),
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr_trim = stderr.trim();
            let stdout_trim = stdout.trim();
            let exit = output
                .status
                .code()
                .map_or_else(|| "killed by signal".to_string(), |c| c.to_string());
            let message = if !stderr_trim.is_empty() {
                format!("ast-grep failed (exit {exit}): {stderr_trim}")
            } else if !stdout_trim.is_empty() {
                format!("ast-grep failed (exit {exit}). stdout: {stdout_trim}")
            } else {
                format!(
                    "ast-grep failed (exit {exit}) with no output. Likely causes: \
                     pattern matched 0 nodes, language not inferred from file extension \
                     (e.g. .txt has no parser), or invalid pattern syntax. \
                     File: {rel_path}, pattern: {pattern:?}"
                )
            };
            return Ok(AstGrepResult {
                success: false,
                file_path: rel_path.clone(),
                pattern: pattern.to_string(),
                rewrite: rewrite.to_string(),
                dry_run,
                diff: None,
                message,
            });
        }

        let modified = reconstruct_ast_grep_rewrite(&source, &output.stdout)?;
        let diff = self
            .commit_or_preview_edit(
                &rel_path,
                &file,
                &source_identity,
                &source,
                &modified,
                dry_run,
            )
            .await?;

        Ok(AstGrepResult {
            success: true,
            file_path: rel_path,
            pattern: pattern.to_string(),
            rewrite: rewrite.to_string(),
            dry_run,
            diff,
            message: edit_success_message(dry_run, "ast-grep rewrite completed"),
        })
    }
}

#[derive(serde::Deserialize)]
struct AstGrepJsonReplacement {
    text: String,
    replacement: String,
    #[serde(rename = "replacementOffsets")]
    replacement_offsets: AstGrepJsonOffsets,
}

#[derive(serde::Deserialize)]
struct AstGrepJsonOffsets {
    start: usize,
    end: usize,
}

fn reconstruct_ast_grep_rewrite(source: &str, output: &[u8]) -> Result<String> {
    let mut replacements: Vec<AstGrepJsonReplacement> = if output.is_empty() {
        Vec::new()
    } else {
        serde_json::from_slice(output).map_err(|error| TraceDecayError::Config {
            message: format!("ast-grep returned invalid replacement JSON: {error}"),
        })?
    };
    replacements.sort_by_key(|candidate| {
        (
            candidate.replacement_offsets.start,
            candidate.replacement_offsets.end,
        )
    });

    let mut modified = String::with_capacity(source.len());
    let mut cursor = 0;
    for candidate in replacements {
        let start = candidate.replacement_offsets.start;
        let end = candidate.replacement_offsets.end;
        if start < cursor || end < start {
            return Err(TraceDecayError::Config {
                message: "ast-grep returned overlapping or reversed replacement offsets"
                    .to_string(),
            });
        }
        let Some(matched) = source.get(start..end) else {
            return Err(TraceDecayError::Config {
                message: "ast-grep returned replacement offsets outside UTF-8 source boundaries"
                    .to_string(),
            });
        };
        if matched != candidate.text {
            return Err(TraceDecayError::Config {
                message: "ast-grep replacement offsets did not match the source bytes".to_string(),
            });
        }
        modified.push_str(&source[cursor..start]);
        modified.push_str(&candidate.replacement);
        cursor = end;
    }
    modified.push_str(&source[cursor..]);
    Ok(modified)
}

fn can_use_literal_rewrite_fallback(pattern: &str) -> bool {
    let trimmed = pattern.trim();
    !trimmed.is_empty()
        && trimmed == pattern
        && !pattern.contains('$')
        && !pattern.contains('\n')
        && !pattern.contains('\r')
}

#[cfg(test)]
mod tests {
    use super::reconstruct_ast_grep_rewrite;

    #[test]
    fn ast_grep_reconstruction_uses_exact_validated_offsets() {
        let output =
            br#"[{"text":"old","replacement":"new","replacementOffsets":{"start":3,"end":6}}]"#;
        assert_eq!(
            reconstruct_ast_grep_rewrite("fn old() {}\n", output).unwrap(),
            "fn new() {}\n"
        );
    }

    #[test]
    fn ast_grep_reconstruction_rejects_mismatched_source() {
        let output =
            br#"[{"text":"not-old","replacement":"new","replacementOffsets":{"start":3,"end":6}}]"#;
        assert!(reconstruct_ast_grep_rewrite("fn old() {}\n", output).is_err());
    }
}
