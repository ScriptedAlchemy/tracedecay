//! CLI project-scope resolution through the application boundary types.
//!
//! Query-facing commands resolve their project scope ONCE, through the
//! daemon-brokered project registry, into the transport-neutral
//! `tracedecay_contracts::ResolvedScope`. Every failure state is explicit:
//! an unregistered exact root, an unusable selector, a malformed registry
//! response, or a sibling-root resolution fails closed, the CLI never
//! substitutes another project (no CWD or sibling fallback).
//!
//! This module owns only the CLI-specific brokering: the daemon handshake,
//! the registry status taxonomy, and payload field extraction. The resolution
//! guards (canonicalization, sibling-root authorization, digest revalidation)
//! and the daemon-owned identity delegation live in the single canonical
//! path (`tracedecay_session_memory::context::RegisteredScopeResolver`).

use std::path::{Path, PathBuf};
use tracedecay_runtime_core::config::ProfileRoot;

use tracedecay_contracts::retrieval::{
    AdminCliRegistryContextV1, AdminCliResultV1, AdminCliSurfaceRequestV1,
};

use super::daemon::{admin_cli_result, admin_cli_result_mismatch};

/// A CLI command's resolved project scope: the registered project identity
/// and canonical root used by the daemon application boundary.
#[derive(Debug)]
pub(crate) struct ResolvedCliScope {
    pub(crate) project_id: tracedecay_domain::ProjectId,
    pub(crate) project_path: PathBuf,
}

/// Resolves an already-selected explicit root, or a path inside one, into the
/// registry's canonical root and exact application scope. This helper never
/// discovers or substitutes a path from the process CWD.
pub(crate) async fn resolve_project_scope(
    profile: &ProfileRoot,
    project_path: PathBuf,
) -> tracedecay_domain::errors::Result<ResolvedCliScope> {
    let request = AdminCliSurfaceRequestV1::RegistryContext {
        project_arg: Some(project_path.clone()),
    };
    match admin_cli_result(profile, None, request).await? {
        AdminCliResultV1::RegistryContext(context) => {
            scope_from_registry_context(&project_path, &context)
        }
        _ => Err(admin_cli_result_mismatch("registry_context")),
    }
}

fn scope_from_registry_context(
    requested: &Path,
    context: &AdminCliRegistryContextV1,
) -> tracedecay_domain::errors::Result<ResolvedCliScope> {
    let project = match context {
        AdminCliRegistryContextV1::Ok { project, .. } => project,
        AdminCliRegistryContextV1::NotFound { .. } => {
            return Err(config_error(format!(
                "no registered TraceDecay project at exact root '{}'; run `tracedecay init` there (no fallback project is substituted)",
                requested.display()
            )));
        }
        AdminCliRegistryContextV1::Invalid { .. } => {
            return Err(config_error(format!(
                "'{}' is not a usable project selector",
                requested.display()
            )));
        }
    };
    let canonical = canonicalize_absolute_root(
        &PathBuf::from(&project.canonical_root),
        "registered project root",
        requested,
    )?;
    let project_id =
        tracedecay_domain::ProjectId::new(project.project_id.as_str()).map_err(|error| {
            config_error(format!(
                "registry project id for '{}' is not canonical: {error}",
                requested.display()
            ))
        })?;
    // The requested-root canonicalization, sibling-root authorization, and
    // scope-digest revalidation all live in the single canonical resolver; the
    // CLI keeps only the registry brokering and selector taxonomy above.
    let project_path =
        tracedecay_session_memory::context::RegisteredScopeResolver::canonical_scope_root(
            &canonical,
            requested,
            &project_id,
        )
        .map_err(|error| {
            config_error(format!(
                "failed to resolve exact transport root for '{}': {error}",
                requested.display()
            ))
        })?;
    tracedecay_session_memory::context::RegisteredScopeResolver::resolve(
        &canonical,
        &project_path,
        &project_id,
    )
    .map_err(|error| {
        config_error(format!(
            "failed to resolve exact application scope for '{}': {error}",
            project_path.display()
        ))
    })?;
    Ok(ResolvedCliScope {
        project_id,
        project_path,
    })
}

fn canonicalize_absolute_root(
    root: &Path,
    role: &str,
    requested: &Path,
) -> tracedecay_domain::errors::Result<PathBuf> {
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

fn config_error(message: String) -> tracedecay_domain::errors::TraceDecayError {
    tracedecay_domain::errors::TraceDecayError::Config { message }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use tracedecay_contracts::retrieval::AdminCliRegistryContextV1;
    use tracedecay_runtime_core::path_safety::canonical_existing_identity;

    use super::{ResolvedCliScope, scope_from_registry_context};

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

    fn ok_payload(canonical_root: &Path) -> AdminCliRegistryContextV1 {
        serde_json::from_value(serde_json::json!({
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
        }))
        .unwrap()
    }

    #[test]
    fn exact_root_resolves_same_project_and_scope_via_application_type() {
        let temp = tempfile::TempDir::new().unwrap();
        let root = canonical_existing_identity(temp.path()).unwrap();
        git_init(&root);
        write_identity_marker(&root, "project.cli-scope-test");

        let first: ResolvedCliScope =
            scope_from_registry_context(&root, &ok_payload(&root)).unwrap();
        let second = scope_from_registry_context(&root, &ok_payload(&root)).unwrap();

        assert_eq!(first.project_path, root);
        assert_eq!(second.project_path, root);
    }

    #[test]
    fn subdirectory_request_converges_to_registered_canonical_root() {
        let temp = tempfile::TempDir::new().unwrap();
        let root = canonical_existing_identity(temp.path()).unwrap();
        git_init(&root);
        write_identity_marker(&root, "project.cli-scope-test");
        let subdir = root.join("src/deep");
        std::fs::create_dir_all(&subdir).unwrap();

        let resolved = scope_from_registry_context(&subdir, &ok_payload(&root)).unwrap();

        assert_eq!(
            resolved.project_path, root,
            "a path inside the registered root converges onto its canonical root"
        );
    }

    #[test]
    fn linked_worktree_request_preserves_authorized_transport_root() {
        let temp = tempfile::TempDir::new().unwrap();
        let registered = temp.path().join("registered");
        let linked = temp.path().join("linked");
        std::fs::create_dir_all(&registered).unwrap();
        git_init(&registered);
        std::fs::write(registered.join("README.md"), "linked scope fixture\n").unwrap();
        for args in [
            &["config", "user.email", "test@example.com"][..],
            &["config", "user.name", "Test User"][..],
            &["add", "README.md"][..],
            &["commit", "-q", "-m", "initial commit"][..],
        ] {
            let output = Command::new("git")
                .current_dir(&registered)
                .args(args)
                .output()
                .expect("git runs");
            assert!(
                output.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let output = Command::new("git")
            .current_dir(&registered)
            .args([
                "worktree",
                "add",
                "-q",
                "-b",
                "feature/linked-scope",
                linked.to_str().unwrap(),
                "HEAD",
            ])
            .output()
            .expect("git worktree add runs");
        assert!(
            output.status.success(),
            "git worktree add failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let registered = canonical_existing_identity(&registered).unwrap();
        let linked = canonical_existing_identity(&linked).unwrap();
        write_identity_marker(&registered, "project.cli-scope-test");

        let resolved = scope_from_registry_context(&linked, &ok_payload(&registered)).unwrap();

        assert_eq!(resolved.project_path, linked);
        assert_eq!(resolved.project_id.as_str(), "project.cli-scope-test");
    }

    #[test]
    fn unregistered_exact_root_fails_closed_without_cwd_fallback() {
        let temp = tempfile::TempDir::new().unwrap();
        let root = temp.path().join("unregistered");
        std::fs::create_dir_all(&root).unwrap();
        let payload = AdminCliRegistryContextV1::NotFound { project: () };

        let error = scope_from_registry_context(&root, &payload).unwrap_err();

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
        let payload = AdminCliRegistryContextV1::Invalid { project: () };

        let error = scope_from_registry_context(&root, &payload).unwrap_err();

        assert!(
            error.to_string().contains("not a usable project selector"),
            "unexpected error: {error}"
        );
    }

    /// A registry answer outside the context contract (an unknown status, or
    /// an `ok` without its project or profile) never reaches scope resolution.
    #[test]
    fn registry_answers_outside_the_context_contract_are_refused() {
        for (body, refusal) in [
            (
                serde_json::json!({ "status": "ambiguous", "project": null }),
                "unknown variant `ambiguous`, expected one of `ok`, `invalid`, `not_found`",
            ),
            (
                serde_json::json!({ "project": null }),
                "missing field `status`",
            ),
            (
                serde_json::json!({
                    "status": "ok",
                    "profile_id": "profile.cli-scope-test",
                    "project": null,
                    "aliases": [],
                    "stores": [],
                }),
                "invalid type: null, expected struct PublicCodeProject",
            ),
            (
                serde_json::json!({
                    "status": "ok",
                    "project": { "project_id": "project.cli-scope-test" },
                    "aliases": [],
                    "stores": [],
                }),
                "missing field `label`",
            ),
        ] {
            assert_eq!(
                serde_json::from_value::<AdminCliRegistryContextV1>(body)
                    .unwrap_err()
                    .to_string(),
                refusal
            );
        }
    }

    #[test]
    fn noncanonical_project_id_fails_closed_without_normalization() {
        let temp = tempfile::TempDir::new().unwrap();
        let root = canonical_existing_identity(temp.path()).unwrap();
        let mut payload = ok_payload(&root);
        if let AdminCliRegistryContextV1::Ok { project, .. } = &mut payload {
            project.project_id = " project.cli-scope-test".to_owned();
        }

        let error = scope_from_registry_context(&root, &payload).unwrap_err();

        assert!(
            error.to_string().contains("not canonical"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn missing_registered_root_fails_closed_without_lexical_fallback() {
        let temp = tempfile::TempDir::new().unwrap();
        let root = temp.path().join("missing");

        let error = scope_from_registry_context(&root, &ok_payload(&root)).unwrap_err();

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

        let error = scope_from_registry_context(&sibling, &ok_payload(&registered)).unwrap_err();

        assert!(
            error.to_string().contains("sibling root"),
            "a resolution that names a different root must fail closed: {error}"
        );
    }
}
