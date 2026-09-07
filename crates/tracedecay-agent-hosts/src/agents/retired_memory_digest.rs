//! One-way cleanup for the removed profile-wide memory digest exporter.
//!
//! Project facts are delivered only through the exact project-bound runtime
//! guidance path. These helpers remove artifacts written by the former static
//! exporter during the next host install, update, or uninstall.

use std::path::Path;

use crate::errors::{Result, TraceDecayError};

const START: &str = "<!-- TRACEDECAY MEMORY DIGEST START -->";
const END: &str = "<!-- TRACEDECAY MEMORY DIGEST END -->";

pub(crate) fn remove_state(profile_root: &Path) -> Result<()> {
    for path in [
        profile_root.join("agent_managed/memory_digest.json"),
        profile_root.join("agent_managed/memory_digest_targets.json"),
    ] {
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

pub(crate) fn remove_prompt_block(prompt_path: &Path) -> Result<()> {
    let existing = match std::fs::read_to_string(prompt_path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let starts = existing.match_indices(START).collect::<Vec<_>>();
    let ends = existing.match_indices(END).collect::<Vec<_>>();
    let (start, end) = match (starts.as_slice(), ends.as_slice()) {
        ([], []) => return Ok(()),
        ([(start, _)], [(end, _)]) if start <= end => (*start, *end + END.len()),
        _ => {
            return Err(TraceDecayError::Config {
                message: format!(
                    "retired memory digest markers are ambiguous in {}",
                    prompt_path.display()
                ),
            });
        }
    };
    let mut updated = existing[..start].trim_end().to_owned();
    let suffix = existing[end..].trim_start();
    if !updated.is_empty() && !suffix.is_empty() {
        updated.push_str("\n\n");
    }
    updated.push_str(suffix);
    if !updated.is_empty() && !updated.ends_with('\n') {
        updated.push('\n');
    }
    if updated.trim().is_empty() {
        std::fs::remove_file(prompt_path)?;
    } else {
        super::safe_write_text_file(prompt_path, &updated, None)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleanup_preserves_operator_prompt_content() {
        let dir = tempfile::tempdir().unwrap();
        let prompt = dir.path().join("AGENTS.md");
        std::fs::write(
            &prompt,
            format!("operator before\n\n{START}\nproject fact\n{END}\n\noperator after\n"),
        )
        .unwrap();

        remove_prompt_block(&prompt).unwrap();

        assert_eq!(
            std::fs::read_to_string(prompt).unwrap(),
            "operator before\n\noperator after\n"
        );
    }

    #[test]
    fn ambiguous_markers_fail_without_writing() {
        let dir = tempfile::tempdir().unwrap();
        let prompt = dir.path().join("AGENTS.md");
        let contents = format!("{START}\none\n{START}\ntwo\n{END}\n");
        std::fs::write(&prompt, &contents).unwrap();

        let error = remove_prompt_block(&prompt).unwrap_err();

        assert!(error.to_string().contains("ambiguous"));
        assert_eq!(std::fs::read_to_string(prompt).unwrap(), contents);
    }

    #[test]
    fn state_cleanup_deletes_only_generated_state() {
        let profile = tempfile::tempdir().unwrap();
        let managed = profile.path().join("agent_managed");
        std::fs::create_dir_all(&managed).unwrap();
        let snapshot = managed.join("memory_digest.json");
        let targets = managed.join("memory_digest_targets.json");
        let preserved = managed.join("operator.json");
        for path in [&snapshot, &targets, &preserved] {
            std::fs::write(path, "generated").unwrap();
        }

        remove_state(profile.path()).unwrap();

        assert!(!snapshot.exists());
        assert!(!targets.exists());
        assert!(preserved.exists());
    }
}
