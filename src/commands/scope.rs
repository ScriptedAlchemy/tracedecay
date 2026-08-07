//! CLI project-scope resolution through the application boundary types.
//!
//! Query-facing commands resolve their project scope ONCE, through the
//! daemon-brokered project registry, into the transport-neutral
//! `tracedecay_application::ResolvedScope`. Every failure state is explicit:
//! an unregistered exact root, an unusable selector, a malformed registry
//! response, or a sibling-root resolution fails closed — the CLI never
//! substitutes another project (no CWD or sibling fallback).
//!
//! This module owns only the CLI-specific brokering: the daemon handshake,
//! the registry status taxonomy, and payload field extraction. The resolution
//! guards (canonicalization, sibling-root authorization, digest revalidation)
//! and the daemon-owned identity delegation live in the single canonical
//! path (`tracedecay::application::context::RegisteredScopeResolver`).

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::daemon::daemon_tool_json;

/// A CLI command's resolved project scope: the registered profile/project
/// identities and canonical root used by the daemon application boundary.
#[derive(Debug)]
pub(crate) struct ResolvedCliScope {
    pub(crate) profile_id: tracedecay_domain::configuration::UserProfileId,
    pub(crate) project_id: tracedecay_domain::ProjectId,
    pub(crate) project_path: PathBuf,
}

/// Resolves an already-selected explicit root, or a path inside one, into the
/// registry's canonical root and exact application scope. This helper never
/// discovers or substitutes a path from the process CWD.
pub(crate) async fn resolve_project_scope(
    project_path: PathBuf,
) -> tracedecay::errors::Result<ResolvedCliScope> {
    let payload = daemon_tool_json(
        None,
        "tracedecay_admin_cli",
        serde_json::json!({
            "action": "registry_context",
            "project_arg": project_path,
        }),
    )
    .await?;
    scope_from_registry_payload(&project_path, &payload)
}

fn scope_from_registry_payload(
    requested: &Path,
    payload: &Value,
) -> tracedecay::errors::Result<ResolvedCliScope> {
    match payload.get("status").and_then(Value::as_str) {
        Some("ok") => {}
        Some("not_found") => {
            return Err(config_error(format!(
                "no registered TraceDecay project at exact root '{}'; run `tracedecay init` there (no fallback project is substituted)",
                requested.display()
            )));
        }
        Some("invalid") => {
            return Err(config_error(format!(
                "'{}' is not a usable project selector",
                requested.display()
            )));
        }
        Some("ambiguous") => {
            return Err(config_error(format!(
                "project selector '{}' is ambiguous; refusing to choose a project implicitly",
                requested.display()
            )));
        }
        Some(other) => {
            return Err(config_error(format!(
                "project registry returned unknown status '{other}' for '{}'",
                requested.display()
            )));
        }
        None => {
            return Err(config_error(format!(
                "project registry response for '{}' omitted status",
                requested.display()
            )));
        }
    }
    let project = payload
        .get("project")
        .filter(|project| !project.is_null())
        .ok_or_else(|| {
            config_error(format!(
                "project registry response for '{}' omitted the project record",
                requested.display()
            ))
        })?;
    let profile_id = payload
        .get("profile_id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            config_error(format!(
                "project registry response for '{}' omitted profile_id",
                requested.display()
            ))
        })
        .and_then(|profile_id| {
            tracedecay_domain::configuration::UserProfileId::new(profile_id).map_err(|error| {
                config_error(format!(
                    "registry profile id for '{}' is not canonical: {error}",
                    requested.display()
                ))
            })
        })?;
    let project_id = required_project_str(project, "project_id", requested)?;
    let canonical_root = required_project_str(project, "canonical_root", requested)?;
    let canonical = canonicalize_absolute_root(
        &PathBuf::from(canonical_root),
        "registered project root",
        requested,
    )?;
    let project_id = tracedecay_domain::ProjectId::new(project_id).map_err(|error| {
        config_error(format!(
            "registry project id for '{}' is not canonical: {error}",
            requested.display()
        ))
    })?;
    // The requested-root canonicalization, sibling-root authorization, and
    // scope-digest revalidation all live in the single canonical resolver; the
    // CLI keeps only the registry brokering and selector taxonomy above.
    tracedecay::application::context::RegisteredScopeResolver::resolve(
        &canonical,
        requested,
        &project_id,
    )
    .map_err(|error| {
        config_error(format!(
            "failed to resolve exact application scope for '{}': {error}",
            canonical.display()
        ))
    })?;
    Ok(ResolvedCliScope {
        profile_id,
        project_id,
        project_path: canonical,
    })
}

fn required_project_str<'a>(
    project: &'a Value,
    field: &str,
    requested: &Path,
) -> tracedecay::errors::Result<&'a str> {
    project
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            config_error(format!(
                "project registry response for '{}' omitted project.{field}",
                requested.display()
            ))
        })
}

fn canonicalize_absolute_root(
    root: &Path,
    role: &str,
    requested: &Path,
) -> tracedecay::errors::Result<PathBuf> {
    if !root.is_absolute() {
        return Err(config_error(format!(
            "{role} '{}' for project selector '{}' is not absolute; refusing CWD-relative scope resolution",
            root.display(),
            requested.display()
        )));
    }
    root.canonicalize().map_err(|error| {
        config_error(format!(
            "{role} '{}' for project selector '{}' could not be canonicalized: {error}",
            root.display(),
            requested.display()
        ))
    })
}

fn config_error(message: String) -> tracedecay::errors::TraceDecayError {
    tracedecay::errors::TraceDecayError::Config { message }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use serde_json::Value;

    use super::{ResolvedCliScope, scope_from_registry_payload};

    fn git_init(root: &Path) {
        let output = Command::new("git")
            .current_dir(root)
            .args(["init", "-q", "-b", "main"])
            .output()
            .expect("git runs");
        assert!(
            output.status.success(),
            "git init failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn write_identity_marker(root: &Path, project_id: &str) {
        let written =
            tracedecay_runtime_core::storage::write_repository_identity_marker(root, project_id)
                .expect("write repository identity marker");
        assert!(
            written,
            "repository identity marker must land in the git common dir of '{}'",
            root.display()
        );
    }

    fn ok_payload(canonical_root: &Path) -> Value {
        serde_json::json!({
            "status": "ok",
            "profile_id": "profile.cli-scope-test",
            "project": {
                "project_id": "project.cli-scope-test",
                "label": "cli-scope-test",
                "project_root": canonical_root.to_string_lossy(),
                "display_root": canonical_root.to_string_lossy(),
                "canonical_root": canonical_root.to_string_lossy(),
                "git_common_dir": null,
                "default_branch": "main",
                "created_at": 1,
                "last_seen_at": 2,
            },
            "aliases": [],
            "stores": [],
        })
    }

    #[test]
    fn exact_root_resolves_same_project_and_scope_via_application_type() {
        let temp = tempfile::TempDir::new().unwrap();
        let root = temp.path().canonicalize().unwrap();
        git_init(&root);
        write_identity_marker(&root, "project.cli-scope-test");

        let first: ResolvedCliScope =
            scope_from_registry_payload(&root, &ok_payload(&root)).unwrap();
        let second = scope_from_registry_payload(&root, &ok_payload(&root)).unwrap();

        assert_eq!(first.project_path, root);
        assert_eq!(second.project_path, root);
        assert_eq!(first.profile_id.as_str(), "profile.cli-scope-test");
        assert_eq!(second.profile_id.as_str(), "profile.cli-scope-test");
    }

    #[test]
    fn subdirectory_request_converges_to_registered_canonical_root() {
        let temp = tempfile::TempDir::new().unwrap();
        let root = temp.path().canonicalize().unwrap();
        git_init(&root);
        write_identity_marker(&root, "project.cli-scope-test");
        let subdir = root.join("src/deep");
        std::fs::create_dir_all(&subdir).unwrap();

        let resolved = scope_from_registry_payload(&subdir, &ok_payload(&root)).unwrap();

        assert_eq!(
            resolved.project_path, root,
            "a path inside the registered root converges onto its canonical root"
        );
    }

    #[test]
    fn unregistered_exact_root_fails_closed_without_cwd_fallback() {
        let temp = tempfile::TempDir::new().unwrap();
        let root = temp.path().join("unregistered");
        std::fs::create_dir_all(&root).unwrap();
        let payload = serde_json::json!({ "status": "not_found", "project": null });

        let error = scope_from_registry_payload(&root, &payload).unwrap_err();

        let message = error.to_string();
        assert!(
            message.contains("no registered TraceDecay project"),
            "unexpected error: {message}"
        );
        assert!(
            message.contains(&root.display().to_string()),
            "error must name the requested root, not a fallback: {message}"
        );
    }

    #[test]
    fn unusable_selector_fails_closed() {
        let root = PathBuf::from("/nonexistent/selector");
        let payload = serde_json::json!({ "status": "invalid", "project": null });

        let error = scope_from_registry_payload(&root, &payload).unwrap_err();

        assert!(
            error.to_string().contains("not a usable project selector"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn ambiguous_selector_fails_closed() {
        let root = PathBuf::from("/ambiguous/selector");
        let payload = serde_json::json!({ "status": "ambiguous", "project": null });

        let error = scope_from_registry_payload(&root, &payload).unwrap_err();

        assert!(
            error.to_string().contains("ambiguous"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn missing_or_unknown_registry_status_fails_closed() {
        let root = PathBuf::from("/nonexistent/root");
        for payload in [
            serde_json::json!({ "project": null }),
            serde_json::json!({ "status": "surprise", "project": null }),
        ] {
            let error = scope_from_registry_payload(&root, &payload).unwrap_err();
            assert!(
                error.to_string().contains("status"),
                "malformed status must fail closed: {error}"
            );
        }
    }

    #[test]
    fn ok_payload_with_missing_project_fields_fails_closed() {
        let temp = tempfile::TempDir::new().unwrap();
        let root = temp.path().canonicalize().unwrap();
        for payload in [
            serde_json::json!({
                "status": "ok",
                "profile_id": "profile.cli-scope-test",
                "project": null
            }),
            serde_json::json!({
                "status": "ok",
                "profile_id": "profile.cli-scope-test",
                "project": { "canonical_root": root.to_string_lossy() },
            }),
            serde_json::json!({
                "status": "ok",
                "profile_id": "profile.cli-scope-test",
                "project": { "project_id": "project.cli-scope-test" },
            }),
        ] {
            let error = scope_from_registry_payload(&root, &payload).unwrap_err();
            assert!(
                error.to_string().contains("project"),
                "incomplete registry payload must fail closed: {error}"
            );
        }
    }

    #[test]
    fn missing_profile_identity_fails_closed() {
        let temp = tempfile::TempDir::new().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let mut payload = ok_payload(&root);
        payload
            .as_object_mut()
            .expect("registry payload object")
            .remove("profile_id");

        let error = scope_from_registry_payload(&root, &payload).unwrap_err();

        assert!(
            error.to_string().contains("omitted profile_id"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn noncanonical_project_id_fails_closed_without_normalization() {
        let temp = tempfile::TempDir::new().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let mut payload = ok_payload(&root);
        payload["project"]["project_id"] = Value::String(" project.cli-scope-test".to_string());

        let error = scope_from_registry_payload(&root, &payload).unwrap_err();

        assert!(
            error.to_string().contains("not canonical"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn missing_registered_root_fails_closed_without_lexical_fallback() {
        let temp = tempfile::TempDir::new().unwrap();
        let root = temp.path().join("missing");

        let error = scope_from_registry_payload(&root, &ok_payload(&root)).unwrap_err();

        assert!(
            error.to_string().contains("could not be canonicalized"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn sibling_root_resolution_fails_closed() {
        let temp = tempfile::TempDir::new().unwrap();
        let registered = temp.path().join("registered");
        let sibling = temp.path().join("sibling");
        std::fs::create_dir_all(&registered).unwrap();
        std::fs::create_dir_all(&sibling).unwrap();
        git_init(&registered);
        git_init(&sibling);

        let error = scope_from_registry_payload(&sibling, &ok_payload(&registered)).unwrap_err();

        assert!(
            error.to_string().contains("sibling root"),
            "a resolution that names a different root must fail closed: {error}"
        );
    }
}
