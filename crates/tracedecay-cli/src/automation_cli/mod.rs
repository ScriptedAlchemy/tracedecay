pub(crate) mod config;
mod facts;
mod runs;
mod skills;

use crate::cli::AutomationAction;
use tracedecay_runtime_core::config::ProfileRoot;

pub(crate) async fn handle_automation_command(
    profile: &ProfileRoot,
    action: AutomationAction,
) -> tracedecay_domain::errors::Result<()> {
    match action {
        AutomationAction::Config { action } => {
            tracing::Instrument::instrument(
                config::handle_automation_config_command(profile, action),
                tracing::trace_span!("cli.automation.config"),
            )
            .await
        }
        AutomationAction::Runs { action } => {
            tracing::Instrument::instrument(
                runs::handle_automation_runs_command(profile, action),
                tracing::trace_span!("cli.automation.runs"),
            )
            .await
        }
        AutomationAction::Skills { action } => {
            tracing::Instrument::instrument(
                skills::handle_automation_skills_command(profile, action),
                tracing::trace_span!("cli.automation.skills"),
            )
            .await
        }
        AutomationAction::Facts { action } => {
            tracing::Instrument::instrument(
                facts::handle_automation_facts_command(profile, action),
                tracing::trace_span!("cli.automation.facts"),
            )
            .await
        }
    }
}
