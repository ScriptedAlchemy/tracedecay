pub(crate) mod config;
mod facts;
mod runs;
mod skills;

use crate::cli::AutomationAction;

async fn daemon_project_dashboard_root(
    project_path: &std::path::Path,
) -> tracedecay::errors::Result<std::path::PathBuf> {
    let context = crate::commands::daemon_tool_json(
        Some(project_path),
        "tracedecay_active_project",
        serde_json::json!({ "format": "json" }),
    )
    .await?;
    let data_root = context
        .get("storage")
        .and_then(|storage| storage.get("data_root"))
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
            message: "managed daemon returned no active project data_root".to_string(),
        })?;
    Ok(std::path::PathBuf::from(data_root).join("dashboard"))
}

async fn daemon_automation_action(
    project_path: &std::path::Path,
    args: serde_json::Value,
) -> tracedecay::errors::Result<serde_json::Value> {
    crate::commands::daemon_tool_json(Some(project_path), "tracedecay_admin_project", args).await
}

pub(crate) async fn handle_automation_command(
    action: AutomationAction,
) -> tracedecay::errors::Result<()> {
    match action {
        AutomationAction::Config { action } => {
            config::handle_automation_config_command(action).await
        }
        AutomationAction::Run { action } => runs::handle_automation_run_command(action).await,
        AutomationAction::Runs { action } => runs::handle_automation_runs_command(action).await,
        AutomationAction::Skills { action } => {
            skills::handle_automation_skills_command(action).await
        }
        AutomationAction::Facts { action } => facts::handle_automation_facts_command(action).await,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        facts::{
            fact_apply_rpc_args, fact_list_rpc_args, fact_reject_rpc_args, fact_view_rpc_args,
        },
        runs::*,
    };
    use crate::cli::AutomationRunAction;
    use tracedecay::automation::config::AutomationConfigPatch;

    #[test]
    fn automation_rpc_requests_preserve_fact_and_manual_run_arguments() {
        assert_eq!(
            fact_apply_rpc_args("fact-7"),
            serde_json::json!({ "action": "fact_apply", "id": "fact-7" })
        );
        assert_eq!(
            fact_list_rpc_args(Some("pending_approval"), 50),
            serde_json::json!({
                "action": "fact_list",
                "state": "pending_approval",
                "limit": 50,
            })
        );
        assert_eq!(
            fact_view_rpc_args("fact-7"),
            serde_json::json!({ "action": "fact_view", "id": "fact-7" })
        );
        assert_eq!(
            fact_reject_rpc_args("fact-7", Some("not durable")),
            serde_json::json!({
                "action": "fact_reject",
                "id": "fact-7",
                "reason": "not durable",
            })
        );

        let (path, request) = automation_run_rpc_request(AutomationRunAction::MemoryCuration {
            max_clusters: 9,
            min_confidence: 0.7,
            path: Some("/repo".to_string()),
        })
        .unwrap();
        assert_eq!(path.as_deref(), Some("/repo"));
        assert_eq!(
            request,
            serde_json::json!({
                "action": "automation_run",
                "task": "memory_curation",
                "options": { "max_clusters": 9, "min_confidence": 0.7 },
            })
        );

        let (path, request) = automation_run_rpc_request(AutomationRunAction::SessionReflection {
            provider: "claude".to_string(),
            query: "decisions".to_string(),
            evidence_limit: 11,
            scope: "session".to_string(),
            session_id: Some("session-3".to_string()),
            include_summaries: false,
            sort: "hybrid".to_string(),
            source: Some("assistant".to_string()),
            role: Some("user".to_string()),
            start_time: Some(10),
            end_time: Some(20),
            path: None,
        })
        .unwrap();
        assert_eq!(path, None);
        assert_eq!(
            request,
            serde_json::json!({
                "action": "automation_run",
                "task": "session_reflection",
                "options": {
                    "provider": "claude",
                    "query": "decisions",
                    "evidence_limit": 11,
                    "scope": "session",
                    "session_id": "session-3",
                    "include_summaries": false,
                    "sort": "hybrid",
                    "source": "assistant",
                    "role": "user",
                    "start_time": 10,
                    "end_time": 20,
                },
            })
        );

        let (_, request) = automation_run_rpc_request(AutomationRunAction::SkillWriting {
            provider: "all".to_string(),
            query: "repeated workflow".to_string(),
            evidence_limit: 13,
            path: None,
        })
        .unwrap();
        assert_eq!(
            request,
            serde_json::json!({
                "action": "automation_run",
                "task": "skill_writing",
                "options": {
                    "provider": "all",
                    "query": "repeated workflow",
                    "evidence_limit": 13,
                },
            })
        );
    }

    #[test]
    fn automation_rpc_preserves_response_shape() {
        let payload = serde_json::json!({ "run": { "run_id": "run-5", "status": "ok" } });
        assert_eq!(automation_run_result(&payload).unwrap(), &payload["run"]);
        assert!(automation_run_result(&serde_json::json!({})).is_err());
    }

    #[test]
    fn automation_config_reconcile_is_change_sensitive() {
        let current = AutomationConfigPatch {
            enabled: Some(true),
            ..AutomationConfigPatch::default()
        };
        assert!(!super::config::automation_config_changed(
            Some(&current),
            &current
        ));

        let changed = AutomationConfigPatch {
            scheduler_tick_secs: Some(17),
            ..current.clone()
        };
        assert!(super::config::automation_config_changed(
            Some(&current),
            &changed
        ));
    }

    #[test]
    fn automation_reconcile_requests_bind_project_and_profile_scopes() {
        assert_eq!(
            super::config::project_automation_reconcile_args(),
            serde_json::json!({
                "action": "automation_reconcile",
                "scope": "project"
            })
        );
        assert_eq!(
            super::config::profile_automation_reconcile_args(),
            serde_json::json!({
                "action": "automation_reconcile",
                "scope": "profile"
            })
        );
    }
}
