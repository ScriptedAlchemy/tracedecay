use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use tracedecay::profile_registry_maintenance::ProfileRegistryMaintenanceRuntime;
use tracedecay_application::{ProjectRegistryView, render_project_registry_view};
use tracedecay_domain::errors::{Result, TraceDecayError};
#[cfg(test)]
use tracedecay_global_db::ProjectRegistryContext;
use tracedecay_global_db::RegisteredGlobalDb;

use crate::cli::ProjectsAction;
use crate::commands::{ProfileOfflineAuthority, join_outcome_and_restore, take_profile_offline};

const MAX_LIMIT: usize = 1_000;

#[hotpath::measure(label = "cli.projects.dispatch", future = true)]
pub(crate) async fn handle_projects_action(
    action: ProjectsAction,
    assume_yes: bool,
    dry_run: bool,
) -> Result<()> {
    match action {
        ProjectsAction::List { limit, json } => {
            let limit = bounded_limit(limit);
            let payload = call_registry_admin(json!({
                "action": "registry_list",
                "limit": limit,
                "query": null,
            }))
            .await?;
            print_registry_list(&payload, "registered projects", json)?;
        }
        ProjectsAction::Search { query, limit, json } => {
            let limit = bounded_limit(limit);
            let payload = call_registry_admin(json!({
                "action": "registry_list",
                "limit": limit,
                "query": query,
            }))
            .await?;
            print_registry_list(&payload, &format!("projects matching \"{query}\""), json)?;
        }
        ProjectsAction::Context { selector, json } => {
            let payload = call_registry_admin(json!({
                "action": "registry_context",
                "project_arg": selector,
            }))
            .await?;
            if payload["status"] != "ok" {
                return Err(TraceDecayError::Config {
                    message: format!(
                        "registered project not found for '{selector}'; try `tracedecay projects search {selector}`"
                    ),
                });
            }
            if json {
                println!("{}", serde_json::to_string_pretty(&payload)?);
            } else {
                print!("{}", render_project_context_payload(&payload));
            }
        }
        ProjectsAction::Forget {
            selector,
            keep_store,
        } => {
            handle_projects_forget(&selector, keep_store, assume_yes, dry_run).await?;
        }
    }
    Ok(())
}

/// Forgets exactly one registered project (#730): retires its registry rows
/// and deletes its profile store directories without touching any other
/// project or any repo-local file.
///
/// The destructive path runs offline inside the bounded profile-offline
/// window, so it completes even when the project's runtime is wedged in a
/// terminal activation loop — the managed daemon service is stopped (the
/// supervisor bounds the stop) and restored afterward. The preview runs
/// through the daemon like every other read-only `projects` subcommand and
/// never stops the service.
#[hotpath::measure(label = "cli.projects.forget", future = true)]
async fn handle_projects_forget(
    selector: &str,
    keep_store: bool,
    assume_yes: bool,
    dry_run: bool,
) -> Result<()> {
    let selector_arg = forget_selector_argument(selector)?;
    if dry_run {
        return preview_projects_forget(selector, &selector_arg, keep_store).await;
    }
    if !assume_yes {
        return Err(TraceDecayError::Config {
            message: format!(
                "forgetting project '{selector}' retires its registry rows and deletes its \
                 profile store directories; re-run with --yes to confirm, use --dry-run to \
                 preview the exact removal, or add --keep-store to keep the store bytes"
            ),
        });
    }
    let profile_root = tracedecay_runtime_core::storage::default_profile_root()?;
    let profile_offline = take_profile_offline(&profile_root, "projects forget")?;
    let outcome = forget_under_profile_offline(
        selector,
        &selector_arg,
        keep_store,
        &profile_root,
        &profile_offline,
    )
    .await;
    let restore = profile_offline.finish();
    join_outcome_and_restore("projects forget", outcome, restore)
}

/// The destructive forget body, run inside the profile-offline window. The
/// maintenance scope and registry handles drop before the caller restores the
/// daemon service.
async fn forget_under_profile_offline(
    selector: &str,
    selector_arg: &Path,
    keep_store: bool,
    profile_root: &Path,
    profile_offline: &ProfileOfflineAuthority,
) -> Result<()> {
    let _database_scope = tracedecay_runtime_core::db::enter_maintenance_database_scope(
        profile_offline.lease()?,
        profile_root,
        "projects forget",
    )?;
    let Some(registry) = ProfileRegistryMaintenanceRuntime::try_open_existing(profile_root).await?
    else {
        return Err(TraceDecayError::Config {
            message: "no profile registry exists; there is nothing to forget".to_string(),
        });
    };
    let Some(context) = registry.resolve_registered_project(selector_arg).await? else {
        return Err(forget_selector_not_found(selector));
    };
    let report = registry
        .forget_project(profile_root, &context, keep_store)
        .await?;
    println!(
        "Forgot project {} ({}).",
        report.project_id, context.project.display_root
    );
    for store_dir in &report.removed_store_dirs {
        println!("  removed store {}", store_dir.display());
    }
    for store_dir in &report.absent_store_dirs {
        println!("  store was already absent: {}", store_dir.display());
    }
    for store_dir in &report.kept_store_dirs {
        println!("  kept store {} (--keep-store)", store_dir.display());
    }
    println!(
        "  retired {} registry identity row(s) and {} token-ledger row(s); aliases, store \
         instances, graph scopes, and artifacts cascaded with the identity",
        report.rows.code_projects_deleted, report.rows.path_ledger_rows_deleted
    );
    Ok(())
}

/// Read-only forget preview, brokered through the daemon registry like the
/// other `projects` reads. Prints exactly what a confirmed run would remove.
async fn preview_projects_forget(
    selector: &str,
    selector_arg: &Path,
    keep_store: bool,
) -> Result<()> {
    let payload = call_registry_admin(json!({
        "action": "registry_context",
        "project_arg": selector_arg,
    }))
    .await?;
    if payload["status"] != "ok" {
        return Err(forget_selector_not_found(selector));
    }
    let profile_root = tracedecay_runtime_core::storage::default_profile_root()?;
    let project = &payload["project"];
    println!(
        "Would forget project {} ({}).",
        project["project_id"].as_str().unwrap_or("-"),
        project["display_root"].as_str().unwrap_or("-")
    );
    let alias_count = payload["aliases"].as_array().map_or(0, Vec::len);
    let stores = payload["stores"].as_array().cloned().unwrap_or_default();
    println!(
        "  would retire the registry identity row, {alias_count} alias(es), and {} store \
         instance(s)",
        stores.len()
    );
    for store in &stores {
        let Some(relpath) = store["store"]["store_relpath"].as_str() else {
            continue;
        };
        let data_root = profile_root.join(relpath);
        if keep_store {
            println!("  would keep store {} (--keep-store)", data_root.display());
        } else {
            println!("  would delete store {}", data_root.display());
        }
    }
    println!("Re-run with --yes (without --dry-run) to apply.");
    Ok(())
}

fn forget_selector_not_found(selector: &str) -> TraceDecayError {
    TraceDecayError::Config {
        message: format!(
            "no registered project matches '{selector}'; try `tracedecay projects search \
             {selector}`"
        ),
    }
}

/// Path-shaped selectors are absolutized against the CLI's own working
/// directory before reaching any resolver, so `.` names the operator's
/// directory rather than the daemon's or the maintenance process's.
fn forget_selector_argument(selector: &str) -> Result<PathBuf> {
    let trimmed = selector.trim();
    if trimmed.is_empty() {
        return Err(TraceDecayError::Config {
            message: "projects forget requires a project id or path selector".to_string(),
        });
    }
    if RegisteredGlobalDb::is_explicit_project_path_selector(trimmed) {
        std::path::absolute(trimmed).map_err(|error| TraceDecayError::Config {
            message: format!("could not absolutize selector path '{trimmed}': {error}"),
        })
    } else {
        Ok(PathBuf::from(trimmed))
    }
}

fn bounded_limit(limit: usize) -> usize {
    limit.clamp(1, MAX_LIMIT)
}

#[hotpath::measure(label = "cli.projects.render")]
fn print_registry_list(payload: &Value, label: &str, json_output: bool) -> Result<()> {
    if json_output {
        println!("{}", serde_json::to_string_pretty(payload)?);
        return Ok(());
    }
    let view: ProjectRegistryView = serde_json::from_value(json!({
        "summary": payload["summary"],
        "project_tree": payload["project_tree"],
    }))?;
    print!("{}", render_project_registry_view(label, &view));
    Ok(())
}

fn render_project_context_payload(payload: &Value) -> String {
    let mut out = String::new();
    let project = &payload["project"];
    let _ = writeln!(
        out,
        "Project: {}",
        project["project_id"].as_str().unwrap_or("-")
    );
    let _ = writeln!(
        out,
        "root: {}",
        project["display_root"].as_str().unwrap_or("-")
    );
    if let Some(branch) = project["default_branch"].as_str() {
        let _ = writeln!(out, "default branch: {branch}");
    }
    if let Some(git_common_dir) = project["git_common_dir"].as_str() {
        let _ = writeln!(out, "git common dir: {git_common_dir}");
    }
    let _ = writeln!(out, "last seen: {}", project["last_seen_at"]);

    if let Some(aliases) = payload["aliases"]
        .as_array()
        .filter(|aliases| !aliases.is_empty())
    {
        out.push_str("\nAliases:\n");
        for alias in aliases {
            let _ = writeln!(out, "  {}", alias["alias_path"].as_str().unwrap_or("-"));
        }
    }

    if let Some(stores) = payload["stores"]
        .as_array()
        .filter(|stores| !stores.is_empty())
    {
        out.push_str("\nStores:\n");
        for store_context in stores {
            let store = &store_context["store"];
            let _ = writeln!(
                out,
                "  {} [{} / {}] {}",
                store["store_id"].as_str().unwrap_or("-"),
                store["store_kind"].as_str().unwrap_or("-"),
                store["storage_mode"].as_str().unwrap_or("-"),
                store["store_relpath"].as_str().unwrap_or("-")
            );
            for scope in store_context["graph_scopes"]
                .as_array()
                .into_iter()
                .flatten()
            {
                let _ = writeln!(
                    out,
                    "    scope {} branch={} db={} writable={}",
                    scope["graph_scope_id"].as_str().unwrap_or("-"),
                    scope["branch_name"].as_str().unwrap_or("-"),
                    scope["db_relpath"].as_str().unwrap_or("-"),
                    scope["writable"].as_bool().unwrap_or(false)
                );
            }
            for artifact in store_context["artifacts"].as_array().into_iter().flatten() {
                let size = artifact["size_bytes"]
                    .as_u64()
                    .map_or_else(|| "-".to_string(), |bytes| bytes.to_string());
                let _ = writeln!(
                    out,
                    "    artifact {} path={} size={}",
                    artifact["artifact_kind"].as_str().unwrap_or("-"),
                    artifact["relpath"].as_str().unwrap_or("-"),
                    size
                );
            }
        }
    }
    out
}

#[hotpath::measure(label = "cli.projects.request", future = true)]
async fn call_registry_admin(arguments: Value) -> Result<Value> {
    let cwd = std::env::current_dir()?;
    let project_root = tracedecay::config::discover_project_root(&cwd);
    let handshake =
        tracedecay::daemon::handshake_for_current_client(project_root, None, false, false)?;
    let result =
        tracedecay::daemon::call_default_tool(&handshake, "tracedecay_admin_cli", arguments)
            .await?;
    tracedecay::daemon::tool_json_payload(&result, "tracedecay_admin_cli")
}

/// Renders the plain-text `projects context` view. Deliberately omits
/// `project.git_remote_url` — a git remote URL can embed credentials
/// (`https://user:token@host/...`), so it must never be printed here or
/// serialized into the JSON view (see `PublicCodeProject`).
#[cfg(test)]
fn render_project_context_text(context: &ProjectRegistryContext) -> String {
    let mut out = String::new();
    let project = &context.project;
    let _ = writeln!(out, "Project: {}", project.project_id);
    let _ = writeln!(out, "root: {}", project.display_root);
    if let Some(branch) = &project.default_branch {
        let _ = writeln!(out, "default branch: {branch}");
    }
    if let Some(git_common_dir) = &project.git_common_dir {
        let _ = writeln!(out, "git common dir: {git_common_dir}");
    }
    let _ = writeln!(out, "last seen: {}", project.last_seen_at);

    if !context.aliases.is_empty() {
        out.push('\n');
        out.push_str("Aliases:\n");
        for alias in &context.aliases {
            let _ = writeln!(out, "  {}", alias.alias_path);
        }
    }

    if !context.stores.is_empty() {
        out.push('\n');
        out.push_str("Stores:\n");
        for store_context in &context.stores {
            let store = &store_context.store;
            let _ = writeln!(
                out,
                "  {} [{} / {}] {}",
                store.store_id, store.store_kind, store.storage_mode, store.store_relpath
            );
            for scope in &store_context.graph_scopes {
                let _ = writeln!(
                    out,
                    "    scope {} branch={} db={} writable={}",
                    scope.graph_scope_id, scope.branch_name, scope.db_relpath, scope.writable
                );
            }
            for artifact in &store_context.artifacts {
                let size = artifact
                    .size_bytes
                    .map_or_else(|| "-".to_string(), |bytes| bytes.to_string());
                let _ = writeln!(
                    out,
                    "    artifact {} path={} size={}",
                    artifact.artifact_kind, artifact.relpath, size
                );
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_dashboard_api::project_registry::PublicProjectRegistryContext;
    use tracedecay_global_db::{
        CodeProjectRecord, GraphScopeRecord, ProjectAliasRecord, ProjectStoreContext,
        StoreArtifactRecord, StoreInstanceRecord,
    };

    const CREDENTIAL_REMOTE_URL: &str =
        "https://user:sekret-token@github.com/example/private-repo.git";

    fn context_with_credential_remote() -> ProjectRegistryContext {
        ProjectRegistryContext {
            project: CodeProjectRecord {
                project_id: "proj_test".to_string(),
                canonical_root: "/repo".to_string(),
                display_root: "/repo".to_string(),
                git_common_dir: Some("/repo/.git".to_string()),
                git_remote_url: Some(CREDENTIAL_REMOTE_URL.to_string()),
                default_branch: Some("main".to_string()),
                created_at: 100,
                last_seen_at: 200,
            },
            aliases: vec![ProjectAliasRecord {
                alias_path: "/repo".to_string(),
                project_id: "proj_test".to_string(),
                last_seen_at: 200,
            }],
            stores: vec![ProjectStoreContext {
                store: StoreInstanceRecord {
                    store_id: "store:test".to_string(),
                    project_id: "proj_test".to_string(),
                    store_kind: "code_project".to_string(),
                    storage_mode: "profile_sharded".to_string(),
                    store_relpath: "projects/proj_test".to_string(),
                    manifest_relpath: None,
                    created_at: 110,
                    last_verified_at: Some(210),
                    last_write_at: Some(220),
                },
                graph_scopes: vec![GraphScopeRecord {
                    graph_scope_id: "store:test:branch:main".to_string(),
                    project_id: "proj_test".to_string(),
                    store_id: "store:test".to_string(),
                    branch_name: "main".to_string(),
                    db_relpath: "projects/proj_test/branches/main.db".to_string(),
                    parent_scope_id: None,
                    last_synced_at: Some(230),
                    writable: true,
                }],
                artifacts: vec![StoreArtifactRecord {
                    store_id: "store:test".to_string(),
                    artifact_kind: "graph_db".to_string(),
                    relpath: "projects/proj_test/branches/main.db".to_string(),
                    size_bytes: Some(4096),
                    schema_version: None,
                    updated_at: Some(240),
                }],
            }],
        }
    }

    #[test]
    fn plain_text_context_omits_credential_bearing_remote_url() {
        let context = context_with_credential_remote();
        let text = render_project_context_text(&context);

        assert!(
            !text.contains("sekret-token"),
            "plain-text projects context leaked a credential: {text}"
        );
        assert!(
            !text.contains(CREDENTIAL_REMOTE_URL),
            "plain-text projects context leaked the remote URL: {text}"
        );
        assert!(
            !text.to_lowercase().contains("git_remote_url")
                && !text.to_lowercase().contains("remote:"),
            "plain-text projects context should not print remote metadata: {text}"
        );
        // Sanity: the rest of the context still renders as expected, so
        // this isn't just an empty-output false pass.
        assert!(text.contains("Project: proj_test"));
        assert!(text.contains("root: /repo"));
    }

    #[test]
    fn json_context_omits_credential_bearing_remote_url() {
        let context = context_with_credential_remote();
        let payload = PublicProjectRegistryContext::new(&context, None);
        let json = serde_json::to_string(&payload).expect("payload should serialize");

        assert!(
            !json.contains("sekret-token"),
            "JSON projects context leaked a credential: {json}"
        );
        assert!(
            !json.contains(CREDENTIAL_REMOTE_URL),
            "JSON projects context leaked the remote URL: {json}"
        );
        assert!(
            !json.contains("git_remote_url"),
            "JSON projects context should not include the git_remote_url field: {json}"
        );
        // Sanity: the rest of the context still serializes as expected.
        assert!(json.contains("proj_test"));
    }

    #[test]
    fn daemon_context_payload_preserves_registry_details() {
        let context = context_with_credential_remote();
        let public = PublicProjectRegistryContext::new(&context, None);
        let payload = serde_json::json!({
            "project": public.project,
            "aliases": context.aliases,
            "stores": context.stores,
        });

        let text = render_project_context_payload(&payload);

        assert!(text.contains("Aliases:\n  /repo"));
        assert!(text.contains("Stores:\n  store:test [code_project / profile_sharded]"));
        assert!(text.contains("scope store:test:branch:main branch=main"));
        assert!(text.contains("artifact graph_db path=projects/proj_test/branches/main.db"));
        assert!(!text.contains("sekret-token"));
    }
}
