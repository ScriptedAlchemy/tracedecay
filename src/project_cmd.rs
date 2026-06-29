use std::path::Path;

use serde_json::json;
use tracedecay::errors::{Result, TraceDecayError};
use tracedecay::global_db::{CodeProjectRecord, GlobalDb, ProjectRegistryContext};

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
            let projects = db.list_code_projects(limit).await;
            print_projects("registered projects", projects, limit, json)?;
        }
        ProjectsAction::Search { query, limit, json } => {
            let limit = bounded_limit(limit);
            let projects = db.search_code_projects(&query, limit).await;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "query": query,
                        "limit": limit,
                        "projects": projects,
                    }))?
                );
            } else {
                print_project_table(&format!("projects matching \"{query}\""), &projects);
            }
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
    db.project_registry_context_by_alias(Path::new(selector))
        .await
}

fn print_projects(
    label: &str,
    projects: Vec<CodeProjectRecord>,
    limit: usize,
    json_output: bool,
) -> Result<()> {
    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "limit": limit,
                "projects": projects,
            }))?
        );
    } else {
        print_project_table(label, &projects);
    }
    Ok(())
}

fn print_project_table(label: &str, projects: &[CodeProjectRecord]) {
    if projects.is_empty() {
        println!("No {label} found.");
        return;
    }

    let id_width = projects
        .iter()
        .map(|project| project.project_id.len())
        .max()
        .unwrap_or("project_id".len())
        .max("project_id".len());
    let branch_width = projects
        .iter()
        .map(|project| {
            project
                .default_branch
                .as_deref()
                .unwrap_or("-")
                .chars()
                .count()
        })
        .max()
        .unwrap_or("branch".len())
        .max("branch".len());

    println!("Found {} {label}:", projects.len());
    println!();
    println!(
        "  {id:<id_width$}  {branch:<branch_width$}  root",
        id = "project_id",
        branch = "branch",
    );
    for project in projects {
        let branch = project.default_branch.as_deref().unwrap_or("-");
        println!(
            "  {id:<id_width$}  {branch:<branch_width$}  {root}",
            id = project.project_id,
            root = project.display_root,
        );
    }
}

fn print_project_context(context: &ProjectRegistryContext, json_output: bool) -> Result<()> {
    if json_output {
        println!("{}", serde_json::to_string_pretty(context)?);
        return Ok(());
    }

    let project = &context.project;
    println!("Project: {}", project.project_id);
    println!("root: {}", project.display_root);
    if let Some(branch) = &project.default_branch {
        println!("default branch: {branch}");
    }
    if let Some(remote) = &project.git_remote_url {
        println!("remote: {remote}");
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
