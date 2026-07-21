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
    let current = tracedecay::config::cached_runtime_configuration(&project_path)?;
    match action.as_deref() {
        Some("on") => {
            let mut config = current.config.clone();
            config.git_ignore = true;
            tracedecay::config::mutate_pinned_runtime_configuration(&current, config).await?;
            eprintln!("gitignore enabled — .gitignore rules will be respected during indexing.");
            eprintln!("Run `tracedecay sync` to re-index with the new setting.");
        }
        Some("off") => {
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
            let status = if current.config.git_ignore {
                "on"
            } else {
                "off"
            };
            eprintln!("gitignore: {status}");
        }
    }
    Ok(())
}
