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

    let project = &context.project;
    println!("Project: {}", project.project_id);
    println!("root: {}", project.display_root);
    if let Some(branch) = &project.default_branch {
        println!("default branch: {branch}");
    }
    if let Some(git_common_dir) = &project.git_common_dir {
        println!("git common dir: {git_common_dir}");
    }
    println!("last seen: {}", project.last_seen_at);

    if !context.aliases.is_empty() {
        println!();
        println!("Aliases:");
        for alias in &context.aliases {
            println!("  {}", alias.alias_path);
        }
    }

    if !context.stores.is_empty() {
        println!();
        println!("Stores:");
        for store_context in &context.stores {
            let store = &store_context.store;
            println!(
                "  {} [{} / {}] {}",
                store.store_id, store.store_kind, store.storage_mode, store.store_relpath
            );
            for scope in &store_context.graph_scopes {
                println!(
                    "    scope {} branch={} db={} writable={}",
                    scope.graph_scope_id, scope.branch_name, scope.db_relpath, scope.writable
                );
            }
            for artifact in &store_context.artifacts {
                let size = artifact
                    .size_bytes
                    .map(|bytes| bytes.to_string())
                    .unwrap_or_else(|| "-".to_string());
                println!(
                    "    artifact {} path={} size={}",
                    artifact.artifact_kind, artifact.relpath, size
                );
            }
        }
    }
    Ok(())
}
