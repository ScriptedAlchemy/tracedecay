use std::path::Path;

use serde_json::json;
use tracedecay::errors::{Result, TraceDecayError};
use tracedecay::global_db::{CodeProjectRecord, GlobalDb, ProjectRegistryContext};
use tracedecay::project_registry::{
    PublicCodeProject, PublicProjectRegistryContext, build_project_registry_view,
    render_project_registry_view,
};

use crate::cli::ProjectsAction;

const MAX_LIMIT: usize = 1_000;

pub(crate) async fn handle_projects_action(action: ProjectsAction) -> Result<()> {
    let db = GlobalDb::open()
        .await
        .ok_or_else(|| TraceDecayError::Config {
            message:
                "no TraceDecay global registry found; run `tracedecay init` in a project first"
                    .to_string(),
        })?;

    match action {
        ProjectsAction::List { limit, json } => {
            let limit = bounded_limit(limit);
            let mut projects = db.list_code_projects(limit + 1).await;
            let truncated = projects.len() > limit;
            projects.truncate(limit);
            let active_project_id = active_project_id(&db).await;
            print_projects(
                &db,
                projects,
                ProjectPrintOptions {
                    label: "registered projects",
                    limit,
                    truncated,
                    active_project_id: active_project_id.as_deref(),
                    query: None,
                    json_output: json,
                },
            )
            .await?;
        }
        ProjectsAction::Search { query, limit, json } => {
            let limit = bounded_limit(limit);
            let mut projects = db.search_code_projects(&query, limit + 1).await;
            let truncated = projects.len() > limit;
            projects.truncate(limit);
            let active_project_id = active_project_id(&db).await;
            print_projects(
                &db,
                projects,
                ProjectPrintOptions {
                    label: &format!("projects matching \"{query}\""),
                    limit,
                    truncated,
                    active_project_id: active_project_id.as_deref(),
                    query: Some(("query", query.as_str())),
                    json_output: json,
                },
            )
            .await?;
        }
        ProjectsAction::Context { selector, json } => {
            let context = project_context(&db, &selector).await.ok_or_else(|| {
                TraceDecayError::Config {
                    message: format!(
                        "registered project not found for '{selector}'; try `tracedecay projects search {selector}`"
                    ),
                }
            })?;
            print_project_context(&context, json)?;
        }
    }
    Ok(())
}

fn bounded_limit(limit: usize) -> usize {
    limit.clamp(1, MAX_LIMIT)
}

async fn project_context(db: &GlobalDb, selector: &str) -> Option<ProjectRegistryContext> {
    if let Some(context) = db.project_registry_context_by_id(selector).await {
        return Some(context);
    }
    let selector_path = Path::new(selector);
    if let Some(context) = db.project_registry_context_by_alias(selector_path).await {
        return Some(context);
    }
    if !GlobalDb::is_explicit_project_path_selector(selector) {
        return None;
    }
    let git_common_dir = tracedecay::worktree::git_common_dir(selector_path);
    db.project_registry_context_by_identity(selector_path, git_common_dir.as_deref())
        .await
}

async fn active_project_id(db: &GlobalDb) -> Option<String> {
    let cwd = std::env::current_dir().ok()?;
    let git_common_dir = tracedecay::worktree::git_common_dir(&cwd);
    db.project_registry_context_by_identity(&cwd, git_common_dir.as_deref())
        .await
        .map(|context| context.project.project_id)
}

struct ProjectPrintOptions<'a> {
    label: &'a str,
    limit: usize,
    truncated: bool,
    active_project_id: Option<&'a str>,
    query: Option<(&'a str, &'a str)>,
    json_output: bool,
}

async fn print_projects(
    db: &GlobalDb,
    projects: Vec<CodeProjectRecord>,
    options: ProjectPrintOptions<'_>,
) -> Result<()> {
    let contexts = db.project_registry_contexts_for_projects(&projects).await;
    let view = build_project_registry_view(&contexts, options.active_project_id, options.truncated);
    if options.json_output {
        let projects = projects
            .iter()
            .map(|project| PublicCodeProject::from_record(project, options.active_project_id))
            .collect::<Vec<_>>();
        let mut payload = json!({
            "limit": options.limit,
            "truncated": options.truncated,
            "summary": view.summary,
            "project_tree": view.project_tree,
            "projects": projects,
        });
        if let Some((key, value)) = options.query {
            payload[key] = json!(value);
        }
        println!("{}", serde_json::to_string_pretty(&payload)?);
    } else {
        print!("{}", render_project_registry_view(options.label, &view));
    }
    Ok(())
}

fn print_project_context(context: &ProjectRegistryContext, json_output: bool) -> Result<()> {
    if json_output {
        let payload = PublicProjectRegistryContext::new(context, None);
        println!("{}", serde_json::to_string_pretty(&payload)?);
        return Ok(());
    }
    print!("{}", render_project_context_text(context));
    Ok(())
}

/// Renders the plain-text `projects context` view. Deliberately omits
/// `project.git_remote_url` — a git remote URL can embed credentials
/// (`https://user:token@host/...`), so it must never be printed here or
/// serialized into the JSON view (see `PublicCodeProject`).
fn render_project_context_text(context: &ProjectRegistryContext) -> String {
    let mut out = String::new();
    let project = &context.project;
    out.push_str(&format!("Project: {}\n", project.project_id));
    out.push_str(&format!("root: {}\n", project.display_root));
    if let Some(branch) = &project.default_branch {
        out.push_str(&format!("default branch: {branch}\n"));
    }
    if let Some(git_common_dir) = &project.git_common_dir {
        out.push_str(&format!("git common dir: {git_common_dir}\n"));
    }
    out.push_str(&format!("last seen: {}\n", project.last_seen_at));

    if !context.aliases.is_empty() {
        out.push('\n');
        out.push_str("Aliases:\n");
        for alias in &context.aliases {
            out.push_str(&format!("  {}\n", alias.alias_path));
        }
    }

    if !context.stores.is_empty() {
        out.push('\n');
        out.push_str("Stores:\n");
        for store_context in &context.stores {
            let store = &store_context.store;
            out.push_str(&format!(
                "  {} [{} / {}] {}\n",
                store.store_id, store.store_kind, store.storage_mode, store.store_relpath
            ));
            for scope in &store_context.graph_scopes {
                out.push_str(&format!(
                    "    scope {} branch={} db={} writable={}\n",
                    scope.graph_scope_id, scope.branch_name, scope.db_relpath, scope.writable
                ));
            }
            for artifact in &store_context.artifacts {
                let size = artifact
                    .size_bytes
                    .map(|bytes| bytes.to_string())
                    .unwrap_or_else(|| "-".to_string());
                out.push_str(&format!(
                    "    artifact {} path={} size={}\n",
                    artifact.artifact_kind, artifact.relpath, size
                ));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay::global_db::{
        GraphScopeRecord, ProjectAliasRecord, ProjectStoreContext, StoreArtifactRecord,
        StoreInstanceRecord,
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
}
