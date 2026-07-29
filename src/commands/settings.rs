use super::daemon::daemon_tool_json;

pub(crate) fn handle_upload_counter(enable: bool) {
    let mut config = tracedecay::user_config::UserConfig::load();
    config.upload_enabled = enable;
    match config.save_with_recovery() {
        Ok(Some(backup)) => eprintln!(
            "note: corrupt config.toml backed up to {} before regenerating",
            backup.display()
        ),
        Ok(None) => {}
        Err(err) => eprintln!("warning: could not save tracedecay config: {err}"),
    }
    if enable {
        eprintln!("Worldwide counter upload enabled.");
    } else {
        eprintln!(
            "Worldwide counter upload disabled. You can re-enable with `tracedecay enable-upload-counter`."
        );
    }
}

pub(crate) async fn handle_gitignore(
    path: Option<String>,
    action: Option<String>,
) -> tracedecay::errors::Result<()> {
    let project_path = tracedecay::config::resolve_path(path);
    match action.as_deref() {
        Some("on") => {
            let current = tracedecay::config::cached_runtime_configuration(&project_path)?;
            let mut config = current.config.clone();
            config.git_ignore = true;
            tracedecay::config::mutate_pinned_runtime_configuration(&current, config).await?;
            eprintln!("gitignore enabled — .gitignore rules will be respected during indexing.");
            eprintln!("Run `tracedecay sync` to re-index with the new setting.");
        }
        Some("off") => {
            let current = tracedecay::config::cached_runtime_configuration(&project_path)?;
            let mut config = current.config.clone();
            config.git_ignore = false;
            tracedecay::config::mutate_pinned_runtime_configuration(&current, config).await?;
            eprintln!("gitignore disabled — .gitignore rules will be ignored during indexing.");
            eprintln!("Run `tracedecay sync` to re-index with the new setting.");
        }
        Some(other) => {
            return Err(tracedecay::errors::TraceDecayError::Config {
                message: format!("unknown action '{other}': expected 'on' or 'off'"),
            });
        }
        None => {
            let resolved = super::scope::resolve_project_scope(project_path).await?;
            let response = daemon_tool_json(
                Some(&resolved.project_path),
                "tracedecay_admin_project",
                serde_json::json!({ "action": "gitignore_status" }),
            )
            .await?;
            let enabled = response
                .get("git_ignore")
                .and_then(serde_json::Value::as_bool)
                .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
                    message: "daemon gitignore status omitted git_ignore".to_string(),
                })?;
            let status = if enabled { "on" } else { "off" };
            eprintln!("gitignore: {status}");
        }
    }
    Ok(())
}
