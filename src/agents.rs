//! Root composition façade for agent-host integrations.
//!
//! Host behavior lives in `tracedecay-agent-hosts`; the path-based Hermes
//! profile adapter remains here because it owns filesystem backup/error policy.

pub use tracedecay_agent_hosts::agents::{
    AgentIntegration, AntigravityIntegration, ClaudeIntegration, ClineIntegration,
    CodexIntegration, CopilotIntegration, CursorIntegration, DoctorCounters, GeminiIntegration,
    HealthcheckContext, HermesIntegration, InstallContext, KiloIntegration, KimiIntegration,
    KiroIntegration,
    ManagedSkillExportReport, OpenCodeIntegration, RooCodeIntegration, UpdatePluginOutcome,
    VibeIntegration, ZedIntegration, available_integrations,
    backup_and_write_json, backup_config_file, copilot_cli_dir, detect_missing_installed_agents,
    export_managed_skills_to_agent_hosts, export_managed_skills_to_agents, home_dir,
    kiro_data_dir, load_json_file, load_json_file_strict,
    load_jsonc_file, load_jsonc_file_strict, load_toml_file, offer_git_post_commit_hook,
    parse_jsonc, pick_integrations_interactive, restore_config_backup, safe_write_json_file,
    safe_write_text_file, vscode_data_dir,
    vscode_insiders_data_dir, which_tracedecay, write_json_file, write_toml_file,
    CLI_FALLBACK_PROMPT_RULES,
};
pub use tracedecay_agent_hosts::agents::{
    antigravity, claude, cline, codex, copilot, cursor, gemini, kilo, kimi, kiro, opencode,
    plugin_bundle, prompt_rules, roo_code, vibe, zed,
};

/// Compatibility module retaining the root-owned Hermes profile I/O seam.
pub mod hermes {
    pub use tracedecay_agent_hosts::agents::HermesIntegration;

    pub mod profile_config {
        pub use tracedecay_agent_hosts::agents::hermes::profile_config::*;
    }

    pub(crate) use crate::hermes_profile_config::read_config_pinned_project_root;
}

pub(crate) fn configure_root_ports() {
    tracedecay_agent_hosts::ports::install_root_ports(tracedecay_agent_hosts::ports::RootPorts {
        tool_definitions: root_tool_definitions,
        format_capable_tool_names: root_format_capable_tool_names,
        cursor_catch_up_ingest_max_bytes: root_cursor_catch_up_ingest_max_bytes,
        cursor_post_install: root_cursor_post_install,
        cursor_session_health: root_cursor_session_health,
        memory_injection_enabled: crate::hooks::memory_inject::memory_injection_enabled,
        degraded_serve_stderr_marker: || crate::serve::DEGRADED_SERVE_STDERR_MARKER,
        user_memory_curator: root_user_memory_curator,
        project_analytics_events: root_project_analytics_events,
        latest_session_activity: root_latest_session_activity,
    });
}

fn root_tool_definitions() -> Vec<tracedecay_agent_hosts::ports::ToolDescriptor> {
    crate::mcp::tools::get_tool_definitions()
        .into_iter()
        .map(|tool| tracedecay_agent_hosts::ports::ToolDescriptor {
            name: tool.name,
            description: tool.description,
            input_schema: tool.input_schema,
            read_only: tool
                .annotations
                .as_ref()
                .and_then(|annotations| annotations.get("readOnlyHint"))
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
        })
        .collect()
}

fn root_format_capable_tool_names() -> Vec<String> {
    crate::mcp::tools::format_capable_tool_names()
        .iter()
        .map(|name| (*name).to_string())
        .collect()
}

fn root_cursor_catch_up_ingest_max_bytes() -> u64 {
    crate::hooks::CURSOR_CATCH_UP_INGEST_MAX_BYTES
}

fn root_cursor_post_install(
    project_path: std::path::PathBuf,
) -> tracedecay_agent_hosts::ports::CursorPostInstallFuture {
    Box::pin(async move {
        crate::hooks::memory_inject::regenerate_cursor_memory_rule(&project_path).await;
        let Some(branch_name) = crate::branch::current_branch(&project_path) else {
            return;
        };
        match crate::tracedecay::TraceDecay::add_branch_tracking(&project_path, &branch_name).await {
            Ok(crate::branch::BranchAddOutcome::Added) => {
                eprintln!("\x1b[32m✔\x1b[0m Tracked Cursor branch '{branch_name}' for tracedecay indexing");
            }
            Ok(
                crate::branch::BranchAddOutcome::AlreadyTracked
                | crate::branch::BranchAddOutcome::Deferred
                | crate::branch::BranchAddOutcome::NotIndexed,
            ) => {}
            Err(error) => {
                eprintln!("\x1b[33mwarning:\x1b[0m could not track Cursor branch '{branch_name}' for tracedecay indexing: {error}");
            }
        }
    })
}

fn root_cursor_session_health(
    project_path: &std::path::Path,
) -> Option<tracedecay_agent_hosts::ports::CursorSessionHealth> {
    let db_path = crate::sessions::cursor::project_session_db_path(project_path);
    if !db_path.exists() {
        return None;
    }
    let handle = tokio::runtime::Handle::try_current().ok()?;
    tokio::task::block_in_place(|| {
        handle.block_on(async {
            let db = crate::sessions::cursor::open_project_session_db(project_path).await?;
            let health = db.session_ingest_health_for_provider(Some("cursor")).await;
            Some(tracedecay_agent_hosts::ports::CursorSessionHealth {
                max_transcript_pending_bytes: health.max_transcript_pending_bytes,
                pending_bytes: health.pending_bytes,
                pending_transcripts: health.pending_transcripts,
                tracked_transcripts: health.tracked_transcripts,
                literal_workspace_placeholder_paths: db
                    .literal_workspace_placeholder_transcript_paths(10)
                    .await,
            })
        })
    })
}

fn root_user_memory_curator<'a>(
    profile_root: &'a std::path::Path,
    config: &'a crate::automation::config::AutomationConfig,
    backend: &'a dyn crate::automation::backend::AgentTaskBackend,
    options: crate::automation::memory_curator::MemoryCuratorAutomationOptions,
) -> tracedecay_agent_hosts::ports::UserMemoryCuratorFuture<'a> {
    Box::pin(crate::automation::run_user_memory_curator_with_backend(
        profile_root,
        config,
        backend,
        options,
    ))
}

fn root_project_analytics_events<'a>(
    project_root: &'a std::path::Path,
    limit: usize,
) -> tracedecay_agent_hosts::ports::AnalyticsEventsFuture<'a> {
    Box::pin(async move {
        let Some(db) = crate::global_db::GlobalDb::open().await else {
            return Ok(Vec::new());
        };
        let events = db
            .query_analytics_events(&crate::global_db::AnalyticsEventQuery {
                provider: None,
                project_id: Some(crate::global_db::GlobalDb::canonical_project_key(project_root)),
                session_id: None,
                event_kind: None,
                since: None,
                limit,
            })
            .await
            .map_err(|message| crate::errors::TraceDecayError::Config {
                message: format!("failed to import project analytics into skill usage ledger: {message}"),
            })?;
        Ok(events
            .into_iter()
            .map(|event| tracedecay_agent_hosts::ports::AnalyticsEventRecord {
                id: event.id,
                provider: event.provider,
                project_id: event.project_id,
                session_id: event.session_id,
                timestamp: event.timestamp,
                event_kind: event.event_kind,
                hook_name: event.hook_name,
                tool_name: event.tool_name,
                tool_category: event.tool_category,
                skill_name: event.skill_name,
                hint_category: event.hint_category,
                hint_id: event.hint_id,
                outcome: event.outcome,
                metadata_json: event.metadata_json,
            })
            .collect())
    })
}

fn root_latest_session_activity<'a>(
    sessions_db_path: &'a std::path::Path,
) -> tracedecay_agent_hosts::ports::SessionActivityFuture<'a> {
    Box::pin(async move {
        crate::global_db::GlobalDb::open_read_only_at(sessions_db_path)
            .await?
            .latest_session_activity_secs()
            .await
    })
}

pub fn get_integration(id: &str) -> crate::errors::Result<Box<dyn AgentIntegration>> {
    configure_root_ports();
    tracedecay_agent_hosts::agents::get_integration(id)
}

pub fn all_integrations() -> Vec<Box<dyn AgentIntegration>> {
    configure_root_ports();
    tracedecay_agent_hosts::agents::all_integrations()
}

pub fn tool_names() -> Vec<String> {
    configure_root_ports();
    tracedecay_agent_hosts::agents::tool_names()
}

pub fn read_only_tool_names() -> Vec<String> {
    configure_root_ports();
    tracedecay_agent_hosts::agents::read_only_tool_names()
}

pub fn expected_tool_perms() -> Vec<String> {
    configure_root_ports();
    tracedecay_agent_hosts::agents::expected_tool_perms()
}

/// Backfill `installed_agents` without leaking the root `UserConfig` into the
/// lower host crate.
pub fn migrate_installed_agents(
    home: &std::path::Path,
    config: &mut crate::user_config::UserConfig,
) {
    let additions = detect_missing_installed_agents(home, &config.installed_agents);
    if additions.is_empty() {
        return;
    }
    config.installed_agents.extend(additions);
    if let Err(error) = config.save() {
        eprintln!("warning: could not save tracedecay config: {error}");
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    fn embedded_plugin_tool_mentions() -> std::collections::BTreeSet<String> {
        let mut mentions = std::collections::BTreeSet::new();
        for (_, contents) in
            tracedecay_agent_hosts::agents::cursor::embedded_plugin_files()
        {
            let bytes = contents.as_bytes();
            let mut search_from = 0;
            while let Some(found) = contents[search_from..].find("tracedecay_") {
                let start = search_from + found;
                let mut end = start + "tracedecay_".len();
                while end < bytes.len()
                    && (bytes[end].is_ascii_lowercase()
                        || bytes[end].is_ascii_digit()
                        || bytes[end] == b'_')
                {
                    end += 1;
                }
                let token = contents[start..end].trim_end_matches('_');
                if token.len() > "tracedecay_".len() {
                    mentions.insert(token.to_string());
                }
                search_from = end;
            }
        }
        mentions
    }

    fn registered_tool_names() -> std::collections::BTreeSet<String> {
        let mut names: std::collections::BTreeSet<String> =
            crate::mcp::tools::get_tool_definitions()
                .into_iter()
                .map(|definition| definition.name)
                .collect();
        names.insert("tracedecay_ast_grep_rewrite".to_string());
        names
    }

    #[test]
    fn plugin_tool_mentions_resolve_to_registered_tools() {
        const NON_TOOL_MENTIONS: &[&str] = &["tracedecay_metrics"];
        let known = registered_tool_names();
        let unknown: Vec<String> = embedded_plugin_tool_mentions()
            .into_iter()
            .filter(|mention| {
                !known.contains(mention) && !NON_TOOL_MENTIONS.contains(&mention.as_str())
            })
            .collect();
        assert!(
            unknown.is_empty(),
            "cursor-plugin mentions tool names missing from get_tool_definitions(): {unknown:?}"
        );
    }

    #[test]
    fn registered_tools_are_referenced_by_the_plugin_bundle() {
        const TOOLS_WITHOUT_PLUGIN_REFERENCE: &[&str] = &[];
        let mentions = embedded_plugin_tool_mentions();
        let missing: Vec<String> = registered_tool_names()
            .into_iter()
            .filter(|name| {
                !mentions.contains(name) && !TOOLS_WITHOUT_PLUGIN_REFERENCE.contains(&name.as_str())
            })
            .collect();
        assert!(
            missing.is_empty(),
            "tools registered in get_tool_definitions() but referenced nowhere under \
             cursor-plugin/ (reference them in a skill or allow-list them): {missing:?}"
        );
    }

    #[test]
    fn session_context_skill_index_matches_bundle_skills() {
        let mut bundled: Vec<String> =
            tracedecay_agent_hosts::agents::cursor::embedded_plugin_files()
                .into_iter()
                .filter_map(|(relative, contents)| {
                    let name = relative
                        .strip_prefix("skills/")
                        .and_then(|rest| rest.strip_suffix("/SKILL.md"))?;
                    (!contents.contains("disable-model-invocation: true"))
                        .then(|| name.to_string())
                })
                .collect();
        bundled.sort();
        let mut listed: Vec<String> = crate::hooks::CURSOR_PLUGIN_SKILLS
            .iter()
            .map(|skill| (*skill).to_string())
            .collect();
        listed.sort();
        assert_eq!(
            bundled, listed,
            "hooks::CURSOR_PLUGIN_SKILLS must list exactly the model-invocable bundled skills"
        );
    }

    #[test]
    fn readme_mcp_allowlist_matches_read_only_tools() {
        let files = tracedecay_agent_hosts::agents::cursor::embedded_plugin_files();
        let readme = files
            .iter()
            .find(|&&(relative, _)| relative == "README.md")
            .map(|&(_, contents)| contents)
            .expect("plugin README must be embedded");

        let mut listed: Vec<String> = readme
            .lines()
            .filter_map(|line| {
                let entry = line.trim().trim_end_matches(',').trim_matches('"');
                entry
                    .strip_prefix("tracedecay:")
                    .filter(|tool| tool.starts_with("tracedecay_"))
                    .map(str::to_string)
            })
            .collect();
        listed.sort();
        listed.dedup();

        let mut read_only: Vec<String> = crate::mcp::tools::get_tool_definitions()
            .into_iter()
            .filter(|definition| {
                definition
                    .annotations
                    .as_ref()
                    .and_then(|annotations| annotations.get("readOnlyHint"))
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false)
            })
            .map(|definition| definition.name)
            .collect();
        read_only.sort();

        assert_eq!(
            listed, read_only,
            "the README mcpAllowlist snippet must list exactly the readOnlyHint=true tools"
        );
    }
}
