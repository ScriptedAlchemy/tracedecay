//! CLI presentation for the composed semantic activation journey.
//!
//! The daemon owns every stage (native evaluation, publication, and the
//! configuration compare-and-swap); this module only renders the staged
//! journey and its typed receipt or failure.

use crate::cli::SemanticAction;

pub(crate) async fn run(action: SemanticAction) -> tracedecay_domain::errors::Result<()> {
    match action {
        SemanticAction::Activate {
            profile,
            no_rollback,
            project,
            json,
        } => activate(profile, !no_rollback, project, json).await,
    }
}

#[hotpath::measure(label = "cli.semantic.activate", future = true)]
async fn activate(
    profile: String,
    set_rollback: bool,
    project: Option<String>,
    json: bool,
) -> tracedecay_domain::errors::Result<()> {
    let project_root = tracedecay::config::resolve_path_with_discovery(project);
    let handshake = tracedecay::daemon::handshake_for_current_client(
        Some(project_root.clone()),
        None,
        false,
        false,
    )?;
    let client = tracedecay_daemon_identity::invocation_client_for_current(handshake)?;
    if json {
        eprintln!(
            "semantic activate: evaluating profile '{profile}' natively for {} \
             (current+10x workload; typically minutes)",
            project_root.display()
        );
    } else {
        println!(
            "Evaluating semantic profile '{profile}' for {} with the native \
             FastEmbed evaluator (current+10x workload; this typically takes \
             minutes). Watch `tracedecay tool runtime` for the runtime state.",
            project_root.display()
        );
    }
    let receipt = hotpath::future!(
        client.activate_semantic_profile(&profile, set_rollback),
        label = "cli.semantic.activate.request"
    )
    .await?;
    if json {
        println!("{}", serde_json::to_string(&receipt)?);
        return Ok(());
    }
    println!(
        "Published: profile {} (report {})",
        receipt.profile_digest, receipt.report_digest
    );
    match &receipt.rollback_profile_id {
        Some(rollback) => println!(
            "Activated: configuration revision {} (rollback profile: {rollback})",
            receipt.configuration_revision
        ),
        None => println!(
            "Activated: configuration revision {} (no rollback profile recorded)",
            receipt.configuration_revision
        ),
    }
    println!(
        "Semantic runtime state: {}",
        serde_json::to_string(&receipt.runtime_state)?
    );
    Ok(())
}
