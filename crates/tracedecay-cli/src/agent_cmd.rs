use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use tracedecay_agent_hosts::agents::host_component_registration::CatalogHostComponentRegistrationAuthority;
use tracedecay_session_memory::user_config::UserConfig;

mod automation;
#[cfg(test)]
mod host_cli_fixture;
#[cfg(test)]
#[path = "../../../tests/support/isolated_profile.rs"]
mod isolated_profile;
pub(crate) use automation::CodexAutomationInstall;
#[cfg(test)]
use automation::broker_codex_daemon_automation_project;
use automation::{
    install_codex_daemon_automation, validate_codex_automation_flags,
    validate_codex_automation_project_path,
};
mod feedback_component;
mod feedback_rollback;
pub(crate) use feedback_rollback::handle_feedback_rollback_command;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HostBundleCliOperation {
    Install,
    Update,
    Repair,
    Uninstall,
}

#[derive(Debug)]
pub(crate) enum AgentReinstallOutcome {
    Installed,
}

/// The one host lifecycle entry point behind `install`, `update-plugin`,
/// `reinstall`, and `uninstall`. Without `--component` it runs every
/// component of each host's canonical set; with it, only the named one.
pub(crate) async fn handle_host_lifecycle_command(
    agent: Option<String>,
    operation: HostBundleCliOperation,
    options: crate::cli::HostBundleCliOptions,
    no_dashboard: bool,
    automation: Option<CodexAutomationInstall>,
) -> tracedecay_domain::errors::Result<()> {
    validate_codex_automation_flags(agent.as_deref(), automation)?;
    if component_mutation_still_requires_yes(
        operation,
        options.component.is_some(),
        options.dry_run,
        options.yes,
    ) {
        return Err(tracedecay_domain::errors::TraceDecayError::Config {
            message: "host component uninstall requires --yes; use --dry-run to preview. \
                      install, update, and repair proceed from the named command \
                      and only stop for competing extension claims or --adopt"
                .to_string(),
        });
    }
    let home = tracedecay_agent_hosts::agents::home_dir().ok_or_else(|| {
        tracedecay_domain::errors::TraceDecayError::Config {
            message: "could not determine home directory".to_string(),
        }
    })?;
    let lifecycle_root = resolved_lifecycle_root()?;
    let mut user_config = load_host_lifecycle_user_config()?;
    let explicitly_scoped = agent.is_some();
    let agent_ids = match agent {
        Some(agent) => vec![agent],
        // Nothing to configure is an ordinary first-run state: this is the
        // command a user runs before any agent exists.
        None if operation == HostBundleCliOperation::Install => {
            match tracedecay_agent_hosts::agents::select_detected_integrations(
                &home,
                &user_config.installed_agents,
            ) {
                Some(ids) => ids,
                None => {
                    eprintln!();
                    eprintln!(
                        "{}",
                        tracedecay_agent_hosts::agents::no_detected_integrations_notice(&home)
                    );
                    return Ok(());
                }
            }
        }
        None => {
            // A tracked id that no longer resolves (a release renamed or
            // removed it) would otherwise be retried forever.
            let before = user_config.installed_agents.len();
            user_config
                .installed_agents
                .retain(|id| tracedecay_agent_hosts::agents::get_integration(id).is_ok());
            if user_config.installed_agents.len() != before
                && let Err(err) = user_config.save()
            {
                eprintln!("warning: could not save tracedecay config: {err}");
            }
            if user_config.installed_agents.is_empty() {
                eprintln!("No installed agents found. Run `tracedecay install` first.");
                return Ok(());
            }
            user_config.installed_agents.clone()
        }
    };
    if agent_ids.is_empty() {
        eprintln!("No changes.");
    }

    // Each agent runs independently: one host whose CLI is missing or whose
    // files conflict must not strand the others.
    let mut failures: Vec<String> = Vec::new();
    let mut refreshed: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for agent_id in &agent_ids {
        if !explicitly_scoped {
            if let Some(component) = options.component
                && component_is_not_applicable(agent_id, component)
            {
                eprintln!(
                    "not applicable: agent {agent_id:?} does not support the requested {component:?} component; continuing sweep"
                );
                continue;
            }
            // A skipped host is a reported unavailable result, never a silent
            // success for the sweep that contained it.
            if host_kind_for_agent(agent_id).is_err() {
                eprintln!(
                    "skipped: {}",
                    unsupported_host_component_set_message(agent_id)
                );
                continue;
            }
        }
        let dashboard = match operation {
            HostBundleCliOperation::Install => !no_dashboard,
            // Removal must cover everything an install could have written.
            HostBundleCliOperation::Uninstall => true,
            HostBundleCliOperation::Update | HostBundleCliOperation::Repair => {
                user_config.dashboard_enabled_for_agent(agent_id)
            }
        };
        let mut result = run_host_component_lifecycle(
            agent_id,
            operation,
            &options,
            &home,
            &lifecycle_root,
            &ComponentSetApplyContext::resolved_with_dashboard(dashboard),
        );
        if result.is_ok()
            && !options.dry_run
            && agent_id == "codex"
            && let Some(automation) = automation
        {
            result = match validate_codex_automation_project_path() {
                Ok(project_path) => {
                    hotpath::future!(
                        install_codex_daemon_automation(&project_path, &home, automation),
                        label = "cli.agent.automation"
                    )
                    .await
                }
                Err(error) => Err(error),
            };
        }
        if let Err(error) = result {
            failures.push(if explicitly_scoped {
                error.to_string()
            } else {
                format!("{agent_id}: {error}")
            });
            continue;
        }
        if options.dry_run {
            continue;
        }
        refreshed.insert(agent_id.clone());
        match operation {
            HostBundleCliOperation::Install => {
                if !user_config.installed_agents.contains(agent_id) {
                    user_config.installed_agents.push(agent_id.clone());
                }
                user_config
                    .agent_dashboard_enabled
                    .insert(agent_id.clone(), !no_dashboard);
            }
            // Removing one component leaves the rest of the host tracked.
            HostBundleCliOperation::Uninstall if options.component.is_none() => {
                user_config.installed_agents.retain(|id| id != agent_id);
                user_config.agent_dashboard_enabled.remove(agent_id);
            }
            HostBundleCliOperation::Uninstall
            | HostBundleCliOperation::Update
            | HostBundleCliOperation::Repair => {}
        }
    }
    if !options.dry_run {
        user_config
            .save()
            .map_err(|err| tracedecay_domain::errors::TraceDecayError::Config {
                message: format!("failed to save user config: {err}"),
            })?;
    }
    if !failures.is_empty() {
        let message = if explicitly_scoped {
            failures.join("; ")
        } else {
            format!(
                "agent {} failed for: {}",
                operation_verb(operation),
                failures.join("; ")
            )
        };
        return Err(tracedecay_domain::errors::TraceDecayError::Config { message });
    }
    if options.dry_run {
        return Ok(());
    }
    if options.component.is_none()
        && matches!(
            operation,
            HostBundleCliOperation::Install | HostBundleCliOperation::Repair
        )
    {
        // A pass may disarm the startup silent reinstall (`previous_version`)
        // only when every agent still tracked was refreshed by this very
        // pass. Anything less advances `last_installed_version` alone: after
        // an upgrade, the untouched agents still need the silent refresh.
        if crate::update_cmd::install_pass_covers_tracked_agents(
            &user_config.installed_agents,
            &refreshed,
        ) {
            crate::update_cmd::record_completed_reinstall_pass(&mut user_config)?;
        } else if operation == HostBundleCliOperation::Install {
            user_config.last_installed_version = env!("CARGO_PKG_VERSION").to_string();
            user_config
                .save()
                .map_err(|err| tracedecay_domain::errors::TraceDecayError::Config {
                    message: format!("failed to save user config: {err}"),
                })?;
        }
    }
    // An install pass converges the managed-skill exports against the store,
    // so a host never keeps advertising a skill the store no longer holds.
    if operation == HostBundleCliOperation::Install {
        crate::update_cmd::deploy_managed_skills_after_lifecycle();
    }
    Ok(())
}

fn operation_verb(operation: HostBundleCliOperation) -> &'static str {
    match operation {
        HostBundleCliOperation::Install => "install",
        HostBundleCliOperation::Update => "update",
        HostBundleCliOperation::Repair => "reinstall",
        HostBundleCliOperation::Uninstall => "uninstall",
    }
}

fn resolved_lifecycle_root() -> tracedecay_domain::errors::Result<PathBuf> {
    tracedecay_agent_hosts::agents::host_bundle::resolved_host_bundle_lifecycle_root().map_err(
        |error| tracedecay_domain::errors::TraceDecayError::Config {
            message: format!("could not resolve host lifecycle root: {error}"),
        },
    )
}

/// Runs one agent's canonical component set, or the single `--component`
/// the options name, as one receipt-backed transaction (or its dry run).
fn run_host_component_lifecycle(
    agent_id: &str,
    operation: HostBundleCliOperation,
    options: &crate::cli::HostBundleCliOptions,
    home: &Path,
    lifecycle_root: &Path,
    context: &ComponentSetApplyContext,
) -> tracedecay_domain::errors::Result<()> {
    let now_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| tracedecay_domain::errors::TraceDecayError::Config {
            message: "system clock is before the Unix epoch".to_string(),
        })?
        .as_secs();
    let component_set = canonical_host_component_set_with_tracedecay_bin(
        agent_id,
        options.component,
        now_unix,
        &context.tracedecay_bin,
    )?
    .ok_or_else(|| tracedecay_domain::errors::TraceDecayError::Config {
        message: unsupported_host_component_set_message(agent_id),
    })?;
    if options.dry_run {
        dry_run_canonical_component_set(
            agent_id,
            operation,
            &component_set,
            options,
            home,
            lifecycle_root,
            context,
        )
    } else {
        apply_canonical_component_set(
            agent_id,
            operation,
            &component_set,
            options,
            home,
            lifecycle_root,
            context,
        )
    }
}

/// Truthful reason a host component set is unavailable, so a skipped or
/// refused agent never reads as an empty success.
fn unsupported_host_component_set_message(agent: &str) -> String {
    match host_kind_for_agent(agent).ok().and_then(
        tracedecay_agent_hosts::agents::host_bundle_registry::unsupported_host_component_set_reason,
    ) {
        Some(reason) => {
            format!("agent {agent:?} has no installable first-party host component set: {reason:?}")
        }
        None => format!("agent {agent:?} has no canonical first-party host component set"),
    }
}

#[cfg(test)]
fn canonical_host_component_set(
    agent: &str,
    component: Option<crate::cli::HostBundleComponentArg>,
    now_unix: u64,
) -> tracedecay_domain::errors::Result<
    Option<
        tracedecay_agent_hosts::agents::host_bundle_registry::VerifiedEmbeddedHostComponentSetV1,
    >,
> {
    let tracedecay_bin = tracedecay_agent_hosts::agents::which_tracedecay()
        .unwrap_or_else(|| "tracedecay".to_string());
    canonical_host_component_set_with_tracedecay_bin(agent, component, now_unix, &tracedecay_bin)
}

fn component_is_not_applicable(agent: &str, component: crate::cli::HostBundleComponentArg) -> bool {
    host_kind_for_agent(agent).is_ok_and(|host| {
        !tracedecay_agent_hosts::agents::host_bundle_registry::supported_components(host)
            .contains(&host_bundle_component(component))
    })
}

fn canonical_host_component_set_with_tracedecay_bin(
    agent: &str,
    component: Option<crate::cli::HostBundleComponentArg>,
    now_unix: u64,
    tracedecay_bin: &str,
) -> tracedecay_domain::errors::Result<
    Option<
        tracedecay_agent_hosts::agents::host_bundle_registry::VerifiedEmbeddedHostComponentSetV1,
    >,
> {
    let host = match host_kind_for_agent(agent) {
        Ok(host) => host,
        Err(_) => return Ok(None),
    };
    let requested = component.map(host_bundle_component).map_or_else(
        || tracedecay_agent_hosts::agents::host_bundle_registry::default_components(host),
        |component| vec![component],
    );
    if requested.is_empty() {
        return Err(tracedecay_domain::errors::TraceDecayError::Config {
            message: unsupported_host_component_set_message(agent),
        });
    }
    tracedecay_agent_hosts::agents::host_bundle_registry::verified_embedded_host_component_set_with_tracedecay_bin(
        host,
        &requested,
        now_unix,
        tracedecay_bin,
        crate::product_runtime::PRODUCT_FULL_SHA,
    )
    .map(Some)
    .map_err(|error| tracedecay_domain::errors::TraceDecayError::Config {
        message: format!("first-party {agent:?} component set is unavailable: {error}"),
    })
}

/// Uninstall removes host registration. That stays behind `--yes`.
/// Install, update, and repair are the command the operator already ran.
pub(crate) fn component_mutation_still_requires_yes(
    operation: HostBundleCliOperation,
    component_selected: bool,
    dry_run: bool,
    yes: bool,
) -> bool {
    component_selected && !dry_run && !yes && operation == HostBundleCliOperation::Uninstall
}

/// The named verb authorizes a reversible plan. `--yes` is still required to
/// accept competing third-party claims, and uninstall still requires it.
pub(crate) fn lifecycle_invocation_confirms_plan(
    operation: HostBundleCliOperation,
    component_selected: bool,
    yes: bool,
) -> bool {
    yes || !component_selected || operation != HostBundleCliOperation::Uninstall
}

fn lifecycle_operation(
    operation: HostBundleCliOperation,
) -> tracedecay_agent_hosts::agents::host_bundle::HostBundleLifecycleOpV1 {
    match operation {
        HostBundleCliOperation::Install => {
            tracedecay_agent_hosts::agents::host_bundle::HostBundleLifecycleOpV1::Install
        }
        HostBundleCliOperation::Update => {
            tracedecay_agent_hosts::agents::host_bundle::HostBundleLifecycleOpV1::Update
        }
        HostBundleCliOperation::Repair => {
            tracedecay_agent_hosts::agents::host_bundle::HostBundleLifecycleOpV1::Repair
        }
        HostBundleCliOperation::Uninstall => {
            tracedecay_agent_hosts::agents::host_bundle::HostBundleLifecycleOpV1::Uninstall
        }
    }
}

fn component_set_request(
    component_set: &tracedecay_agent_hosts::agents::host_bundle_registry::VerifiedEmbeddedHostComponentSetV1,
    operation: HostBundleCliOperation,
    explicit_confirmation: bool,
    explicit_adoption: bool,
) -> tracedecay_domain::errors::Result<
    tracedecay_agent_hosts::agents::host_bundle::HostComponentSetExecutionRequestV1,
> {
    let operation_id = tracedecay_contracts::request_identity::mint_global_operation_id(
        tracedecay_contracts::request_identity::GlobalOperationIdentityKind::HostComponentSet,
    )
    .map_err(|error| tracedecay_domain::errors::TraceDecayError::Config {
        message: format!("could not generate host lifecycle operation id: {error}"),
    })?;
    let host = component_set.component_set.host;
    Ok(
        tracedecay_agent_hosts::agents::host_bundle::HostComponentSetExecutionRequestV1 {
            lifecycle:
                tracedecay_agent_hosts::agents::host_bundle::HostComponentSetLifecycleRequestV1 {
                    operation: lifecycle_operation(operation),
                    expected_host: host,
                    expected_components: component_set
                        .component_set
                        .components
                        .iter()
                        .map(|component| component.manifest.component)
                        .collect(),
                    explicit_confirmation,
                    hermes_profile_bindings: u8::from(
                        host == tracedecay_agent_hosts::agents::host_bundle::HostKindV1::Hermes,
                    ),
                    explicit_adoption,
                },
            operation_id,
        },
    )
}

fn dry_run_canonical_component_set(
    agent_id: &str,
    operation: HostBundleCliOperation,
    component_set: &tracedecay_agent_hosts::agents::host_bundle_registry::VerifiedEmbeddedHostComponentSetV1,
    options: &crate::cli::HostBundleCliOptions,
    home: &Path,
    lifecycle_root: &Path,
    context: &ComponentSetApplyContext,
) -> tracedecay_domain::errors::Result<()> {
    let preview = preview_canonical_component_set(
        agent_id,
        operation,
        component_set,
        options,
        home,
        lifecycle_root,
        Some(context),
    )?;
    eprintln!(
        "{} {:?}: plan={}, registration_base={}, registration_current={}, artifacts={}, confirmation={}",
        agent_id,
        operation,
        hex::encode(preview.plan_digest),
        hex::encode(preview.base_registration_revision),
        hex::encode(preview.current_registration_revision),
        hex::encode(preview.artifact_state_revision),
        preview.confirmation_required
    );
    for claim in &preview.competing_extension_claims {
        eprintln!(
            "  competing {:?} claim by {:?} (evidence {})",
            claim.capability,
            claim.extension_id,
            hex::encode(claim.evidence_digest)
        );
    }
    for plan in &preview.component_plans {
        eprintln!(
            "  {:?}: {} mutation(s), rollback={}",
            plan.component,
            plan.mutations.len(),
            plan.rollback_required
        );
        let owned = receipt_owned_paths(
            lifecycle_root,
            component_set.component_set.host,
            plan.component,
        );
        for mutation in &plan.mutations {
            eprintln!(
                "  {:?} {} [{}]",
                mutation.action,
                mutation.relative_path,
                artifact_disposition(&mutation.action, &owned, &mutation.relative_path)
            );
        }
    }
    Ok(())
}

/// Deploy paths the durable receipt for this component already claims. A path
/// missing from this set is one no receipt records, so replacing an existing
/// file there is an adoption rather than an ordinary refresh.
fn receipt_owned_paths(
    lifecycle_root: &Path,
    host: tracedecay_agent_hosts::agents::host_bundle::HostKindV1,
    component: tracedecay_agent_hosts::agents::host_bundle::HostComponentV1,
) -> std::collections::BTreeSet<String> {
    tracedecay_agent_hosts::agents::host_bundle::latest_host_component_receipt_at(
        lifecycle_root,
        host,
        component,
    )
    .ok()
    .flatten()
    .map(|receipt| {
        receipt
            .artifacts
            .into_iter()
            .map(|artifact| artifact.relative_path)
            .collect()
    })
    .unwrap_or_default()
}

/// Per-path disposition for the dry run. A foreign claim never reaches here:
/// the planner refuses the whole preview with a typed ownership conflict, so
/// `refuse-foreign` surfaces as that error rather than as a plan entry.
fn artifact_disposition(
    action: &tracedecay_agent_hosts::agents::host_bundle::HostArtifactActionV1,
    receipt_owned: &std::collections::BTreeSet<String>,
    relative_path: &str,
) -> &'static str {
    use tracedecay_agent_hosts::agents::host_bundle::HostArtifactActionV1 as Action;

    match action {
        Action::Noop if receipt_owned.contains(relative_path) => "unchanged",
        Action::Noop => "adopt",
        Action::WriteNew => "write-new",
        Action::Remove => "remove",
        Action::Replace if receipt_owned.contains(relative_path) => "replace",
        Action::Replace => "adopt",
    }
}

fn preview_canonical_component_set(
    agent_id: &str,
    operation: HostBundleCliOperation,
    component_set: &tracedecay_agent_hosts::agents::host_bundle_registry::VerifiedEmbeddedHostComponentSetV1,
    options: &crate::cli::HostBundleCliOptions,
    home: &Path,
    lifecycle_root: &Path,
    context: Option<&ComponentSetApplyContext>,
) -> tracedecay_domain::errors::Result<
    tracedecay_agent_hosts::agents::host_bundle::HostComponentSetLifecyclePreviewV1,
> {
    let request = component_set_request(component_set, operation, options.yes, options.adopt)?;
    let mut registration = match context {
        Some(context) => {
            CatalogHostComponentRegistrationAuthority::new_with_tracedecay_bin_and_dashboard(
                agent_id,
                home,
                request.lifecycle.operation,
                context.tracedecay_bin.clone(),
                context.dashboard,
            )?
        }
        None => CatalogHostComponentRegistrationAuthority::new(
            agent_id,
            home,
            request.lifecycle.operation,
        )?,
    };
    tracedecay_agent_hosts::agents::host_bundle::dry_run_host_component_set_lifecycle_with_lifecycle_root_at(
        home,
        lifecycle_root,
        &component_set.component_set,
        &request,
        component_set,
        &mut registration,
    )
    .map_err(|error| host_bundle_error_for_agent(agent_id, error))
}

/// How one component-set lifecycle reaches the outside world: which
/// `tracedecay` binary the written registrations invoke, and whether the
/// dashboard component is registered alongside them.
#[derive(Clone, Debug)]
struct ComponentSetApplyContext {
    tracedecay_bin: String,
    dashboard: bool,
}

impl ComponentSetApplyContext {
    /// The production context: the resolved installed binary, dashboard on.
    #[cfg(test)]
    fn resolved() -> Self {
        Self::resolved_with_dashboard(true)
    }

    /// The production binary with the dashboard registration decided by the
    /// caller, which is what the lifecycle commands pass through from
    /// `--no-dashboard` and the per-agent dashboard policy.
    fn resolved_with_dashboard(dashboard: bool) -> Self {
        Self {
            tracedecay_bin: tracedecay_agent_hosts::agents::which_tracedecay()
                .unwrap_or_else(|| "tracedecay".to_string()),
            dashboard,
        }
    }

    /// A pinned fixture binary, dashboard on exactly as in production.
    #[cfg(test)]
    fn with_tracedecay_bin(tracedecay_bin: &str) -> Self {
        Self {
            tracedecay_bin: tracedecay_bin.to_string(),
            dashboard: true,
        }
    }
}

#[hotpath::measure(label = "cli.agent.component.apply")]
fn apply_canonical_component_set(
    agent_id: &str,
    operation: HostBundleCliOperation,
    component_set: &tracedecay_agent_hosts::agents::host_bundle_registry::VerifiedEmbeddedHostComponentSetV1,
    options: &crate::cli::HostBundleCliOptions,
    home: &Path,
    lifecycle_root: &Path,
    context: &ComponentSetApplyContext,
) -> tracedecay_domain::errors::Result<()> {
    let ComponentSetApplyContext {
        tracedecay_bin,
        dashboard,
    } = context;
    let dashboard = *dashboard;
    let request = component_set_request(
        component_set,
        operation,
        lifecycle_invocation_confirms_plan(operation, options.component.is_some(), options.yes),
        options.adopt,
    )?;
    let mut writer =
        tracedecay_agent_hosts::agents::host_bundle::HostBundleWriterV1::open_with_lifecycle_root(
            home,
            lifecycle_root,
        )
        .map_err(|error| host_bundle_error_for_agent(agent_id, error))?;
    let mut transaction =
        tracedecay_agent_hosts::agents::host_bundle::HostComponentSetTransactionV1::new(
            &mut writer,
        );
    let mut registration =
        CatalogHostComponentRegistrationAuthority::new_with_tracedecay_bin_and_dashboard(
            agent_id,
            home,
            request.lifecycle.operation,
            tracedecay_bin.to_string(),
            dashboard,
        )?;
    let preview = transaction
        .preview(
            &component_set.component_set,
            &request,
            component_set,
            &mut registration,
        )
        .map_err(|error| host_bundle_error_for_agent(agent_id, error))?;
    // Receiptless-adoption authority is enforced inside the planner: without
    // `--adopt`, a receiptless file at a cataloged path is adopted only when
    // it matches the staged bytes, and anything else is refused as a typed
    // ownership conflict naming the `--yes --adopt` remedy. Reaching this point means every planned
    // adoption was authorized, so no separate CLI gate re-litigates it.
    // A full canonical set registers alongside competing third-party claims
    // (OpenCode's analyzer ownership projection accounts for them). A single
    // `--component` names one surface, so a claim on it demands an explicit
    // `--yes` rather than being resolved on the operator's behalf.
    if !preview.competing_extension_claims.is_empty()
        && options.component.is_some()
        && !options.yes
    {
        return Err(tracedecay_domain::errors::TraceDecayError::Config {
            message: format!(
                "agent {agent_id:?} already has {} third-party extension claim(s) on a surface \
                 this component set registers ({}); review `--dry-run` and re-run with `--yes` to \
                 confirm this exact plan",
                preview.competing_extension_claims.len(),
                preview
                    .competing_extension_claims
                    .iter()
                    .map(|claim| claim.extension_id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        });
    }
    let receipt = transaction
        .execute_confirmed(
            &component_set.component_set,
            &request,
            &preview,
            component_set,
            &mut registration,
        )
        .map_err(|error| host_bundle_error_for_agent(agent_id, error))?;
    eprintln!(
        "\x1b[32m✔\x1b[0m {} {:?}: {} component(s), receipt {}",
        agent_id,
        request.lifecycle.operation,
        receipt.component_receipts.len(),
        hex::encode(receipt.operation_id)
    );
    // The receipt owns the staged source; the host still has to activate it.
    if let Some(remediation) = registration.deferred_activation() {
        return Err(tracedecay_domain::errors::TraceDecayError::Config {
            message: remediation.to_string(),
        });
    }
    // Hook trust is the one Codex activation step that stays host-owned, so a
    // successful (re)install finishes with the exact remaining action.
    if agent_id == "codex"
        && request.lifecycle.operation
            != tracedecay_agent_hosts::agents::host_bundle::HostBundleLifecycleOpV1::Uninstall
        && let Some(followup) =
            tracedecay_agent_hosts::agents::codex::codex_hook_trust_followup(home)
    {
        eprintln!("  {followup}");
    }
    Ok(())
}

fn load_host_lifecycle_user_config() -> tracedecay_domain::errors::Result<UserConfig> {
    UserConfig::load_strict().map_err(|error| tracedecay_domain::errors::TraceDecayError::Config {
        message: format!("failed to load host lifecycle policy: {error}"),
    })
}

pub(crate) async fn handle_project_local_lifecycle_command(
    agent_id: String,
    operation: HostBundleCliOperation,
) -> tracedecay_domain::errors::Result<()> {
    if !matches!(agent_id.as_str(), "devin" | "zed" | "vibe") {
        return Err(project_local_host_lifecycle_unavailable());
    }
    let home = tracedecay_agent_hosts::agents::home_dir().ok_or_else(|| {
        tracedecay_domain::errors::TraceDecayError::Config {
            message: "could not determine home directory".to_string(),
        }
    })?;
    let tracedecay_bin = tracedecay_agent_hosts::agents::which_tracedecay().ok_or_else(|| {
        tracedecay_domain::errors::TraceDecayError::Config {
            message: "tracedecay not found on PATH. Install the checksummed GitHub release:\n  \
                      https://github.com/ScriptedAlchemy/tracedecay/releases/latest"
                .to_string(),
        }
    })?;
    let project_path = std::env::current_dir().map_err(|error| {
        tracedecay_domain::errors::TraceDecayError::Config {
            message: format!("could not determine project directory: {error}"),
        }
    })?;
    let integration = tracedecay_agent_hosts::agents::get_integration(&agent_id)?;
    if !integration.supports_local_install() {
        return Err(project_local_host_lifecycle_unavailable());
    }
    let context = tracedecay_agent_hosts::agents::InstallContext {
        home: home.clone(),
        tracedecay_bin,
        project_root: Some(project_path.clone()),
        dashboard: false,
    };
    // Project-local lifecycle installs the host's default component set:
    // Devin and Zed carry only the MCP registration, while Vibe also owns a
    // project prompt-rules document.
    let components = tracedecay_agent_hosts::agents::host_bundle_registry::default_components(
        host_kind_for_agent(&agent_id)?,
    );
    let _registration_paths =
        integration.project_host_component_registration_paths(&components, &home, &project_path)?;
    match operation {
        HostBundleCliOperation::Install
        | HostBundleCliOperation::Update
        | HostBundleCliOperation::Repair => {
            integration.activate_project_host_component_registration(
                &components,
                &context,
                &project_path,
            )?;
            eprintln!(
                "\x1b[32m+\x1b[0m {} project MCP registration",
                integration.name()
            );
        }
        HostBundleCliOperation::Uninstall => {
            integration.deactivate_project_host_component_registration(
                &components,
                &context,
                &project_path,
            )?;
            eprintln!(
                "\x1b[31m-\x1b[0m {} project MCP registration",
                integration.name()
            );
        }
    }
    Ok(())
}

fn project_local_host_lifecycle_unavailable() -> tracedecay_domain::errors::TraceDecayError {
    tracedecay_domain::errors::TraceDecayError::Config {
        message: "project-local host lifecycle is unavailable; install the canonical user-level host component set instead"
            .to_string(),
    }
}

fn host_bundle_component(
    component: crate::cli::HostBundleComponentArg,
) -> tracedecay_agent_hosts::agents::host_bundle::HostComponentV1 {
    match component {
        crate::cli::HostBundleComponentArg::Core => {
            tracedecay_agent_hosts::agents::host_bundle::HostComponentV1::Core
        }
        crate::cli::HostBundleComponentArg::Agent => {
            tracedecay_agent_hosts::agents::host_bundle::HostComponentV1::Agent
        }
        crate::cli::HostBundleComponentArg::ContextMcp => {
            tracedecay_agent_hosts::agents::host_bundle::HostComponentV1::ContextMcp
        }
        crate::cli::HostBundleComponentArg::OperatorMcp => {
            tracedecay_agent_hosts::agents::host_bundle::HostComponentV1::OperatorMcp
        }
    }
}

/// Inverse of `integration_id_for_host`, derived from the stock host list so
/// the two cannot drift apart.
///
/// That mapping is many-to-one, so the alias hosts that share an id with a
/// canonical one are skipped: `cursor` resolves to the desktop host and `cline`
/// to the single Cline host rather than the family.
fn host_kind_for_agent(
    agent: &str,
) -> tracedecay_domain::errors::Result<tracedecay_agent_hosts::agents::host_bundle::HostKindV1> {
    use tracedecay_agent_hosts::agents::host_bundle::HostKindV1;

    const ALIASED_HOSTS: [HostKindV1; 2] = [HostKindV1::CursorCloud, HostKindV1::ClineFamily];

    tracedecay_agent_hosts::agents::host_bundle::stock_host_kinds()
        .into_iter()
        .filter(|host| !ALIASED_HOSTS.contains(host))
        .find(|host| tracedecay_agent_hosts::agents::integration_id_for_host(*host) == agent)
        .ok_or_else(|| tracedecay_domain::errors::TraceDecayError::Config {
            message: format!("agent {agent:?} has no embedded first-party host component"),
        })
}

fn host_bundle_error(
    error: tracedecay_agent_hosts::agents::host_bundle::HostBundleError,
) -> tracedecay_domain::errors::TraceDecayError {
    tracedecay_domain::errors::TraceDecayError::Config {
        message: format!("host bundle lifecycle failed: {error}"),
    }
}

fn host_bundle_error_for_agent(
    agent_id: &str,
    error: tracedecay_agent_hosts::agents::host_bundle::HostBundleError,
) -> tracedecay_domain::errors::TraceDecayError {
    if error == tracedecay_agent_hosts::agents::host_bundle::HostBundleError::NativeUpdateRequired {
        let message = match agent_id {
            "claude" => {
                "Claude Code's loaded TraceDecay cache is stale. Run `claude plugin update \
                 tracedecay@tracedecay`, restart Claude Code, then retry the TraceDecay lifecycle."
            }
            "codex" => {
                "Codex's loaded TraceDecay cache is stale. Run `codex plugin add \
                 tracedecay@personal` to reinstall it, re-trust changed hooks, then retry the TraceDecay lifecycle."
            }
            _ => {
                "The host-native TraceDecay plugin cache is stale; update it through the host and retry."
            }
        };
        return tracedecay_domain::errors::TraceDecayError::Config {
            message: message.to_string(),
        };
    }
    if agent_id == "codex"
        && error
            == tracedecay_agent_hosts::agents::host_bundle::HostBundleError::UnsupportedCapability
    {
        return tracedecay_domain::errors::TraceDecayError::Config {
            message: "Codex activates plugins through its native cache, which TraceDecay drives \
                      with `codex plugin add tracedecay@personal` after deploying the source \
                      package. Confirm the `codex` CLI is on PATH and retry; hook trust still \
                      requires `/hooks` inside Codex after a successful add."
                .to_string(),
        };
    }
    if matches!(
        &error,
        tracedecay_agent_hosts::agents::host_bundle::HostBundleError::HostCliUnavailable { .. }
    ) && agent_id == "codex"
    {
        return tracedecay_domain::errors::TraceDecayError::Config {
            message: "Codex activates plugins through its native cache, which TraceDecay drives \
                      with `codex plugin add tracedecay@personal` after deploying the source \
                      package. Install the `codex` CLI or add it to PATH, then retry."
                .to_string(),
        };
    }
    host_bundle_error(error)
}

pub(crate) async fn handle_install_command(
    agent: Option<String>,
    local: bool,
    no_dashboard: bool,
    automation: Option<CodexAutomationInstall>,
    adopt: bool,
    git_hook: bool,
) -> tracedecay_domain::errors::Result<()> {
    validate_codex_automation_flags(agent.as_deref(), automation)?;
    if local {
        let agent_id = agent.ok_or_else(|| tracedecay_domain::errors::TraceDecayError::Config {
            message: "`tracedecay install --local` requires a project-capable `--agent`"
                .to_string(),
        })?;
        handle_project_local_lifecycle_command(agent_id, HostBundleCliOperation::Install).await?;
        if git_hook {
            install_requested_git_hook()?;
        }
        return Ok(());
    }
    let home = tracedecay_agent_hosts::agents::home_dir().ok_or_else(|| {
        tracedecay_domain::errors::TraceDecayError::Config {
            message: "could not determine home directory".to_string(),
        }
    })?;
    let tracedecay_bin = tracedecay_agent_hosts::agents::which_tracedecay().ok_or_else(|| {
        tracedecay_domain::errors::TraceDecayError::Config {
            message: "tracedecay not found on PATH. Install the checksummed GitHub release:\n  \
                          https://github.com/ScriptedAlchemy/tracedecay/releases/latest"
                .to_string(),
        }
    })?;
    let mut user_cfg = load_host_lifecycle_user_config()?;

    let mut installed_names: Vec<String> = Vec::new();
    // Ids this pass actually (re)installed at the current binary version. The
    // tail uses this to decide whether the pass covered every tracked agent
    // and may disarm the startup silent reinstall.
    let mut refreshed_ids: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    if let Some(id) = agent {
        let ag = tracedecay_agent_hosts::agents::get_integration(&id)?;
        let name = ag.name().to_string();
        let context = tracedecay_agent_hosts::agents::InstallContext {
            home: home.clone(),
            tracedecay_bin: tracedecay_bin.clone(),
            project_root: None,
            dashboard: !no_dashboard,
        };
        prepare_native_activation_if_needed(ag.as_ref(), &context)?;
        apply_default_canonical_component_set(
            &id,
            HostBundleCliOperation::Install,
            &home,
            !no_dashboard,
            adopt,
        )?;
        refreshed_ids.insert(id.clone());
        if let Some(options) = automation.filter(|_| id == "codex") {
            let scoped_project_path = validate_codex_automation_project_path()?;
            hotpath::future!(
                install_codex_daemon_automation(&scoped_project_path, &home, options),
                label = "cli.agent.automation"
            )
            .await?;
        }
        user_cfg
            .agent_dashboard_enabled
            .insert(id.clone(), !no_dashboard);
        if !user_cfg.installed_agents.contains(&id) {
            user_cfg.installed_agents.push(id);
            installed_names.push(name);
        }
        user_cfg
            .save()
            .map_err(|err| tracedecay_domain::errors::TraceDecayError::Config {
                message: format!("failed to save user config: {err}"),
            })?;
    } else {
        // Nothing to configure is an ordinary first-run state, not a failure:
        // this is the command a user runs before any agent exists. An explicit
        // `--agent` that cannot be satisfied still fails, above.
        let Some(to_install) = tracedecay_agent_hosts::agents::select_detected_integrations(
            &home,
            &user_cfg.installed_agents,
        ) else {
            eprintln!();
            eprintln!(
                "{}",
                tracedecay_agent_hosts::agents::no_detected_integrations_notice(&home)
            );
            if git_hook {
                install_requested_git_hook()?;
            }
            return Ok(());
        };

        // Each detected host installs independently: one host whose CLI is
        // missing or whose files conflict must not strand the others.
        let mut failures: Vec<String> = Vec::new();
        for id in &to_install {
            let ag = tracedecay_agent_hosts::agents::get_integration(id)?;
            let context = tracedecay_agent_hosts::agents::InstallContext {
                home: home.clone(),
                tracedecay_bin: tracedecay_bin.clone(),
                project_root: None,
                dashboard: !no_dashboard,
            };
            let installed =
                prepare_native_activation_if_needed(ag.as_ref(), &context).and_then(|()| {
                    apply_default_canonical_component_set(
                        id,
                        HostBundleCliOperation::Install,
                        &home,
                        !no_dashboard,
                        adopt,
                    )
                });
            if let Err(error) = installed {
                failures.push(format!("{id}: {error}"));
                continue;
            }
            refreshed_ids.insert(id.clone());
            installed_names.push(ag.name().to_string());
            if !user_cfg.installed_agents.contains(id) {
                user_cfg.installed_agents.push(id.clone());
            }
            user_cfg
                .agent_dashboard_enabled
                .insert(id.clone(), !no_dashboard);
        }
        user_cfg
            .save()
            .map_err(|err| tracedecay_domain::errors::TraceDecayError::Config {
                message: format!("failed to save user config: {err}"),
            })?;
        if !failures.is_empty() {
            for name in &installed_names {
                eprintln!("\x1b[32m+\x1b[0m {name}");
            }
            return Err(tracedecay_domain::errors::TraceDecayError::Config {
                message: format!("agent install failed for: {}", failures.join("; ")),
            });
        }
    }

    eprintln!();
    if installed_names.is_empty() {
        eprintln!("No changes.");
    } else {
        for name in &installed_names {
            eprintln!("\x1b[32m+\x1b[0m {name}");
        }
    }

    // An explicit install pass only refreshes its selection delta, so it may
    // disarm the startup silent reinstall (`previous_version`) only when every
    // agent still tracked was (re)installed by this very pass. Anything less
    // advances `last_installed_version` alone and leaves the arming intact:
    // after an upgrade, the untouched agents still need the silent refresh.
    if crate::update_cmd::install_pass_covers_tracked_agents(
        &user_cfg.installed_agents,
        &refreshed_ids,
    ) {
        crate::update_cmd::record_completed_reinstall_pass(&mut user_cfg)?;
    } else {
        user_cfg.last_installed_version = env!("CARGO_PKG_VERSION").to_string();
        user_cfg
            .save()
            .map_err(|err| tracedecay_domain::errors::TraceDecayError::Config {
                message: format!("failed to save user config: {err}"),
            })?;
    }

    // An install pass is a lifecycle pass: converge the managed-skill exports
    // (prompt indexes and materialized files) against the store, so a host is
    // never left advertising a skill the store no longer holds.
    crate::update_cmd::deploy_managed_skills_after_lifecycle();

    if git_hook {
        install_requested_git_hook()?;
    } else {
        tracedecay_agent_hosts::agents::report_git_post_commit_hook_status();
    }
    Ok(())
}

pub(crate) fn install_requested_git_hook() -> tracedecay_domain::errors::Result<()> {
    let tracedecay_bin = tracedecay_agent_hosts::agents::which_tracedecay().ok_or_else(|| {
        tracedecay_domain::errors::TraceDecayError::Config {
            message: "tracedecay not found on PATH".to_string(),
        }
    })?;
    tracedecay_agent_hosts::agents::install_git_post_commit_hook(&tracedecay_bin)
        .map_err(|message| tracedecay_domain::errors::TraceDecayError::Config { message })
}

pub(crate) async fn handle_reinstall_command(adopt: bool) -> tracedecay_domain::errors::Result<()> {
    let home = tracedecay_agent_hosts::agents::home_dir().ok_or_else(|| {
        tracedecay_domain::errors::TraceDecayError::Config {
            message: "could not determine home directory".to_string(),
        }
    })?;
    let tracedecay_bin = tracedecay_agent_hosts::agents::which_tracedecay().ok_or_else(|| {
        tracedecay_domain::errors::TraceDecayError::Config {
            message: "tracedecay not found on PATH".to_string(),
        }
    })?;
    let mut user_cfg = load_host_lifecycle_user_config()?;

    if user_cfg.installed_agents.is_empty() {
        eprintln!("No installed agents found. Run `tracedecay install` first.");
    } else {
        // Drop tracked ids that no longer resolve to an integration (a release
        // renamed or removed one, or a typo landed in `installed_agents`).
        // Without this the stale id is retried forever. Mirrors
        // `run_post_update_mutations`.
        let before = user_cfg.installed_agents.len();
        user_cfg
            .installed_agents
            .retain(|id| tracedecay_agent_hosts::agents::get_integration(id).is_ok());
        if user_cfg.installed_agents.len() != before
            && let Err(err) = user_cfg.save()
        {
            eprintln!("warning: could not save tracedecay config: {err}");
        }
        let agents = user_cfg.installed_agents.clone();
        eprintln!(
            "Reinstalling {} agent(s): {}",
            agents.len(),
            agents.join(", ")
        );
        let results = reinstall_agent_integrations_with_dashboard_policies(
            &agents,
            &home,
            &tracedecay_bin,
            &user_cfg.agent_dashboard_enabled,
            adopt,
        )
        .await;
        // Reporting lives in `partition_reinstall_results`, which every
        // reinstall pass shares. Keep the reason with the name, a bare id list
        // left "failed for: claude, cursor, hermes, kimi" undiagnosable.
        match crate::update_cmd::partition_reinstall_results(results) {
            crate::update_cmd::ReinstallOutcome::AllOk => {
                eprintln!("\x1b[32m✔\x1b[0m All agents reinstalled");
            }
            crate::update_cmd::ReinstallOutcome::PartialFailure { failed } => {
                return Err(tracedecay_domain::errors::TraceDecayError::Config {
                    message: format!("failed to reinstall agent(s): {}", failed.join("; ")),
                });
            }
        }
        // Advance BOTH markers: `previous_version` is what arms the startup
        // silent reinstall, so recording only `last_installed_version` here
        // left this explicit pass re-running on the next ordinary command.
        crate::update_cmd::record_completed_reinstall_pass(&mut user_cfg)?;
    }
    Ok(())
}

pub(crate) async fn handle_update_plugin_command(
    adopt: bool,
) -> tracedecay_domain::errors::Result<()> {
    let home = tracedecay_agent_hosts::agents::home_dir().ok_or_else(|| {
        tracedecay_domain::errors::TraceDecayError::Config {
            message: "could not determine home directory".to_string(),
        }
    })?;
    let tracedecay_bin = tracedecay_agent_hosts::agents::which_tracedecay().ok_or_else(|| {
        tracedecay_domain::errors::TraceDecayError::Config {
            message: "tracedecay not found on PATH".to_string(),
        }
    })?;
    let user_cfg = load_host_lifecycle_user_config()?;

    for id in &user_cfg.installed_agents {
        let integration = tracedecay_agent_hosts::agents::get_integration(id)?;
        let dashboard = user_cfg.dashboard_enabled_for_agent(id);
        let context = tracedecay_agent_hosts::agents::InstallContext {
            home: home.clone(),
            tracedecay_bin: tracedecay_bin.clone(),
            project_root: None,
            dashboard,
        };
        prepare_native_activation_if_needed(integration.as_ref(), &context)?;
        apply_default_canonical_component_set(
            id,
            HostBundleCliOperation::Update,
            &home,
            dashboard,
            adopt,
        )?;
    }
    Ok(())
}

#[hotpath::measure(label = "cli.agent.preflight")]
pub(crate) fn handle_reinstall_preflight_command() -> tracedecay_domain::errors::Result<()> {
    let home = tracedecay_agent_hosts::agents::home_dir().ok_or_else(|| {
        tracedecay_domain::errors::TraceDecayError::Config {
            message: "could not determine home directory".to_string(),
        }
    })?;
    let tracedecay_bin = tracedecay_agent_hosts::agents::which_tracedecay().ok_or_else(|| {
        tracedecay_domain::errors::TraceDecayError::Config {
            message: "could not resolve the canonical preflight binary".to_string(),
        }
    })?;
    let user_config = load_host_lifecycle_user_config()?;
    let agent_ids = user_config.installed_agents.clone();
    let project_path = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let health_context = tracedecay_agent_hosts::agents::HealthcheckContext {
        home: home.clone(),
        project_path,
    };
    let mut failures = Vec::new();
    let mut checked = 0_usize;
    let mut deferred = 0_usize;

    eprintln!(
        "Read-only integration refresh preflight ({} tracked).",
        agent_ids.len()
    );
    for id in &agent_ids {
        let integration = match tracedecay_agent_hosts::agents::get_integration(id) {
            Ok(integration) => integration,
            Err(_) => {
                eprintln!("  \x1b[33m!\x1b[0m {id}: unknown tracked id; refresh will skip it");
                continue;
            }
        };
        checked += 1;
        let dashboard = user_config.dashboard_enabled_for_agent(id);
        let install_context = tracedecay_agent_hosts::agents::InstallContext {
            home: home.clone(),
            tracedecay_bin: tracedecay_bin.clone(),
            project_root: None,
            dashboard,
        };
        match integration.preflight_non_interactive_install(&install_context) {
            Ok(tracedecay_agent_hosts::agents::NonInteractiveInstallOutcome::Ready) => {}
            Ok(
                tracedecay_agent_hosts::agents::NonInteractiveInstallOutcome::DeferredUserAction(
                    action,
                ),
            ) => {
                deferred += 1;
                eprintln!(
                    "  \x1b[32m✔\x1b[0m {id}: deferred manual activation is accepted ({})",
                    action.remediation
                );
                continue;
            }
            Err(error) => {
                eprintln!("  \x1b[31m✘\x1b[0m {id}: {error}");
                failures.push(format!("{id}: {error}"));
                continue;
            }
        }

        match preflight_agent_integration(
            id,
            integration.as_ref(),
            &home,
            &health_context,
            &install_context,
        ) {
            Ok(summary) => eprintln!("  \x1b[32m✔\x1b[0m {id}: {summary}"),
            Err(error) => {
                eprintln!("  \x1b[31m✘\x1b[0m {id}: {error}");
                failures.push(format!("{id}: {error}"));
            }
        }
    }

    if failures.is_empty() {
        eprintln!("Integration refresh preflight passed: {checked} checked, {deferred} deferred.");
        return Ok(());
    }
    Err(tracedecay_domain::errors::TraceDecayError::Config {
        message: format!(
            "integration refresh preflight failed for: {}",
            failures.join(", ")
        ),
    })
}

fn preflight_agent_integration(
    agent_id: &str,
    integration: &dyn tracedecay_agent_hosts::agents::AgentIntegration,
    home: &Path,
    health_context: &tracedecay_agent_hosts::agents::HealthcheckContext,
    install_context: &tracedecay_agent_hosts::agents::InstallContext,
) -> tracedecay_domain::errors::Result<String> {
    use tracedecay_agent_hosts::agents::host_bundle::HostBundleRegistrationStateV1;

    let now_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| tracedecay_domain::errors::TraceDecayError::Config {
            message: "system clock is before the Unix epoch".to_string(),
        })?
        .as_secs();
    let Some(component_set) = canonical_host_component_set(agent_id, None, now_unix)? else {
        return Err(tracedecay_domain::errors::TraceDecayError::Config {
            message: unsupported_host_component_set_message(agent_id),
        });
    };

    let mut registration_states = Vec::new();
    for component in &component_set.component_set.components {
        let state = integration.host_component_registration_for_lifecycle(
            component.manifest.component,
            health_context,
            install_context,
        );
        if state == HostBundleRegistrationStateV1::Corrupt {
            return Err(tracedecay_domain::errors::TraceDecayError::Config {
                message: format!(
                    "{:?} registration config is corrupt",
                    component.manifest.component
                ),
            });
        }
        registration_states.push(format!(
            "{:?}={}",
            component.manifest.component,
            registration_state_label(state)
        ));
    }
    let lifecycle_root =
        tracedecay_agent_hosts::agents::host_bundle::resolved_host_bundle_lifecycle_root()
            .map_err(|error| tracedecay_domain::errors::TraceDecayError::Config {
                message: format!("could not resolve host lifecycle root: {error}"),
            })?;
    preview_canonical_component_set(
        agent_id,
        HostBundleCliOperation::Repair,
        &component_set,
        &crate::cli::HostBundleCliOptions {
            component: None,
            dry_run: true,
            yes: true,
            adopt: false,
        },
        home,
        &lifecycle_root,
        Some(install_context),
    )?;
    Ok(format!(
        "signed repair plan valid; registration {}",
        registration_states.join(", ")
    ))
}

fn registration_state_label(
    state: tracedecay_agent_hosts::agents::host_bundle::HostBundleRegistrationStateV1,
) -> &'static str {
    use tracedecay_agent_hosts::agents::host_bundle::HostBundleRegistrationStateV1;

    match state {
        HostBundleRegistrationStateV1::Current => "current",
        HostBundleRegistrationStateV1::Repairable => "repairable",
        HostBundleRegistrationStateV1::Missing => "missing",
        HostBundleRegistrationStateV1::Corrupt => "corrupt",
    }
}

/// Reinstalls tracked integrations while reusing lifecycle authority already
/// held by post-update maintenance.
pub(crate) async fn reinstall_agent_integrations_under_lease(
    agent_ids: &[String],
    home: &Path,
    tracedecay_bin: &str,
    lifecycle: &tracedecay_runtime_core::lifecycle_lease::LifecycleLease,
) -> Vec<(
    String,
    tracedecay_domain::errors::Result<AgentReinstallOutcome>,
)> {
    let _ = lifecycle;
    reinstall_agent_integrations_with_persisted_dashboard_policies(agent_ids, home, tracedecay_bin)
        .await
}

async fn reinstall_agent_integrations_with_persisted_dashboard_policies(
    agent_ids: &[String],
    home: &Path,
    tracedecay_bin: &str,
) -> Vec<(
    String,
    tracedecay_domain::errors::Result<AgentReinstallOutcome>,
)> {
    let user_config = match load_host_lifecycle_user_config() {
        Ok(config) => config,
        Err(error) => {
            let message = error.to_string();
            return agent_ids
                .iter()
                .map(|id| {
                    (
                        id.clone(),
                        Err(tracedecay_domain::errors::TraceDecayError::Config {
                            message: message.clone(),
                        }),
                    )
                })
                .collect();
        }
    };
    reinstall_agent_integrations_with_dashboard_policies(
        agent_ids,
        home,
        tracedecay_bin,
        &user_config.agent_dashboard_enabled,
        false,
    )
    .await
}

async fn reinstall_agent_integrations_with_dashboard_policies(
    agent_ids: &[String],
    home: &Path,
    tracedecay_bin: &str,
    dashboard_policies: &std::collections::BTreeMap<String, bool>,
    adopt: bool,
) -> Vec<(
    String,
    tracedecay_domain::errors::Result<AgentReinstallOutcome>,
)> {
    let mut results = Vec::new();
    for id in agent_ids {
        let ag = match tracedecay_agent_hosts::agents::get_integration(id) {
            Ok(ag) => ag,
            Err(_) => {
                tracing::warn!(
                    agent_id = id,
                    "skipping unknown tracked agent id; it will not gate the version-marker refresh"
                );
                continue;
            }
        };
        let dashboard = dashboard_policies.get(id).copied().unwrap_or(true);
        let context = tracedecay_agent_hosts::agents::InstallContext {
            home: home.to_path_buf(),
            tracedecay_bin: tracedecay_bin.to_string(),
            project_root: None,
            dashboard,
        };
        if let Err(error) = prepare_native_activation_if_needed(ag.as_ref(), &context) {
            results.push((id.clone(), Err(error)));
            continue;
        }
        match apply_default_canonical_component_set(
            id,
            HostBundleCliOperation::Repair,
            home,
            dashboard,
            adopt,
        ) {
            Ok(()) => {
                results.push((id.clone(), Ok(AgentReinstallOutcome::Installed)));
                continue;
            }
            Err(error) => {
                results.push((id.clone(), Err(error)));
                continue;
            }
        }
    }
    results
}

pub(crate) async fn handle_uninstall_command(
    agent: Option<String>,
) -> tracedecay_domain::errors::Result<()> {
    let home = tracedecay_agent_hosts::agents::home_dir().ok_or_else(|| {
        tracedecay_domain::errors::TraceDecayError::Config {
            message: "could not determine home directory".to_string(),
        }
    })?;
    let mut user_cfg = load_host_lifecycle_user_config()?;

    if let Some(id) = agent {
        apply_default_canonical_component_set(
            &id,
            HostBundleCliOperation::Uninstall,
            &home,
            true,
            false,
        )?;
        user_cfg.installed_agents.retain(|a| a != &id);
        user_cfg.agent_dashboard_enabled.remove(&id);
        user_cfg
            .save()
            .map_err(|err| tracedecay_domain::errors::TraceDecayError::Config {
                message: format!("failed to save user config: {err}"),
            })?;
    } else {
        for id in user_cfg.installed_agents.clone() {
            apply_default_canonical_component_set(
                &id,
                HostBundleCliOperation::Uninstall,
                &home,
                true,
                false,
            )?;
        }
        user_cfg.installed_agents.clear();
        user_cfg.agent_dashboard_enabled.clear();
        user_cfg
            .save()
            .map_err(|err| tracedecay_domain::errors::TraceDecayError::Config {
                message: format!("failed to save user config: {err}"),
            })?;
        eprintln!("All agent integrations removed.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    use super::{
        AgentReinstallOutcome, CatalogHostComponentRegistrationAuthority, ComponentSetApplyContext,
        HostBundleCliOperation, apply_canonical_component_set,
        apply_default_canonical_component_set, broker_codex_daemon_automation_project,
        canonical_host_component_set, canonical_host_component_set_with_tracedecay_bin,
        component_is_not_applicable, component_mutation_still_requires_yes, component_set_request,
        lifecycle_invocation_confirms_plan,
        reinstall_agent_integrations_with_persisted_dashboard_policies,
    };
    use tracedecay_agent_hosts::agents::host_bundle::{
        CompetingHostExtensionClaimV1, HostBundleError, HostComponentSetExecutionRequestV1,
        HostComponentSetLifecyclePreviewV1, HostComponentSetRegistrationV1, HostComponentSetV1,
    };

    #[test]
    fn reversible_component_commands_do_not_wait_for_a_second_yes() {
        for operation in [
            HostBundleCliOperation::Install,
            HostBundleCliOperation::Update,
            HostBundleCliOperation::Repair,
        ] {
            assert!(
                !component_mutation_still_requires_yes(operation, true, false, false),
                "{operation:?} must proceed from the named command"
            );
            assert!(lifecycle_invocation_confirms_plan(operation, true, false));
        }
        assert!(component_mutation_still_requires_yes(
            HostBundleCliOperation::Uninstall,
            true,
            false,
            false
        ));
        assert!(!lifecycle_invocation_confirms_plan(
            HostBundleCliOperation::Uninstall,
            true,
            false
        ));
        assert!(lifecycle_invocation_confirms_plan(
            HostBundleCliOperation::Uninstall,
            true,
            true
        ));
        assert!(!component_mutation_still_requires_yes(
            HostBundleCliOperation::Uninstall,
            true,
            true,
            false
        ));
    }

    const OPENCODE_UNRELATED_CONFIG: &[u8] = br#"{"lsp":{"other":{"command":["tracedecay","lsp","bridge","--stdio"]}},"unrelated":{"keep":true}}
"#;
    const OPENCODE_CONTEXT_CONFIG: &[u8] = br#"{"mcp":{"tracedecay":{"type":"local","command":["tracedecay","serve"]},"other":{"type":"local","command":["other"]}},"unrelated":{"keep":true}}
"#;
    /// [`PinnedUserDataDir`] gives each test its own profile root (and its own
    /// `HOME`) for the duration of the guard, and holds the crate-wide
    /// user-data-dir lock while the override is installed, the same lock every
    /// other profile-mutating test takes, so the mutation cannot be observed
    /// half-applied. Hold it for as long as any `home` fixture is alive.
    ///
    /// This is also the serialization point for the other process-global
    /// variables these tests set (`PATH`, `KIMI_CODE_HOME`, and the host
    /// registration fault injectors): one lock for all of them keeps their
    /// windows from overlapping each other or a profile pin.
    fn pinned_host_profile() -> tracedecay_runtime_core::config::PinnedUserDataDir {
        tracedecay_runtime_core::config::PinnedUserDataDir::new()
    }

    #[test]
    fn unscoped_component_sweep_distinguishes_unsupported_from_optional() {
        assert!(component_is_not_applicable(
            "cline",
            crate::cli::HostBundleComponentArg::Core
        ));
        assert!(!component_is_not_applicable(
            "cline",
            crate::cli::HostBundleComponentArg::ContextMcp
        ));
        assert!(!component_is_not_applicable(
            "claude",
            crate::cli::HostBundleComponentArg::OperatorMcp
        ));
        assert!(
            canonical_host_component_set(
                "claude",
                Some(crate::cli::HostBundleComponentArg::OperatorMcp),
                0,
            )
            .is_ok(),
            "a supported optional component remains selectable"
        );
        assert!(
            canonical_host_component_set(
                "cline",
                Some(crate::cli::HostBundleComponentArg::Core),
                0,
            )
            .is_err(),
            "an explicitly scoped incompatible component remains a typed refusal"
        );
    }

    /// A `home` fixture for tests that drive a real host-native plugin CLI
    /// (`codex plugin add`/`remove` via [`run_host_cli`]) rather than only
    /// writing files themselves.
    ///
    /// `run_host_cli` launches the host CLI with `HOME` set to exactly this
    /// path, and at least one first-party `codex` build refuses to create its
    /// PATH-alias helper binaries once its resolved `codex_home` falls under
    /// the literal system temp directory (typically `/tmp`) -- a sandboxing
    /// precaution against a world-writable, shared temp root. A `home` fixture
    /// placed under the crate's own `target/` directory keeps the same
    /// per-test isolation `tempfile::tempdir()` gives, without that host
    /// safeguard misreading a fresh test fixture as an unsafe shared location.
    fn host_cli_tempdir() -> tempfile::TempDir {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("target")
            .join("host-cli-test-homes");
        std::fs::create_dir_all(&root)
            .unwrap_or_else(|error| panic!("failed to create {}: {error}", root.display()));
        tempfile::Builder::new()
            .prefix(".tmp")
            .tempdir_in(&root)
            .unwrap_or_else(|error| panic!("failed to create host CLI test home: {error}"))
    }

    fn copy_test_bundle(source: &std::path::Path, destination: &std::path::Path) {
        for entry in std::fs::read_dir(source).unwrap() {
            let entry = entry.unwrap();
            let source_path = entry.path();
            let destination_path = destination.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                std::fs::create_dir_all(&destination_path).unwrap();
                copy_test_bundle(&source_path, &destination_path);
            } else {
                std::fs::create_dir_all(destination_path.parent().unwrap()).unwrap();
                std::fs::copy(source_path, destination_path).unwrap();
            }
        }
    }

    /// `--yes` confirms the plan the preview showed; taking ownership of bytes
    /// no receipt records is a separate decision, so the dry run must name it
    /// as `adopt` rather than folding it into an ordinary refresh.
    #[test]
    fn dry_run_separates_adoption_from_an_ordinary_refresh() {
        use tracedecay_agent_hosts::agents::host_bundle::HostArtifactActionV1 as Action;

        let owned = ["plugins/tracedecay.json".to_string()]
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>();

        assert_eq!(
            super::artifact_disposition(&Action::Replace, &owned, "plugins/tracedecay.json"),
            "replace"
        );
        assert_eq!(
            super::artifact_disposition(&Action::Replace, &owned, "plugins/unowned.json"),
            "adopt"
        );
        assert_eq!(
            super::artifact_disposition(&Action::WriteNew, &owned, "plugins/unowned.json"),
            "write-new"
        );
        assert_eq!(
            super::artifact_disposition(&Action::Remove, &owned, "plugins/tracedecay.json"),
            "remove"
        );
        assert_eq!(
            super::artifact_disposition(&Action::Noop, &owned, "plugins/tracedecay.json"),
            "unchanged"
        );
        assert_eq!(
            super::artifact_disposition(&Action::Noop, &owned, "plugins/unowned.json"),
            "adopt"
        );
    }

    /// An explicit component repair that would claim an unrecorded file is
    /// refused without `--adopt`, even
    /// at preview time, which stays read-only, and the refusal names the
    /// contested path plus the explicit adoption remedy.
    #[tokio::test]
    async fn explicit_component_repair_refuses_adoption_without_the_adopt_flag() {
        let _profile = pinned_host_profile();
        let home = tempfile::tempdir().unwrap();
        let lifecycle = tempfile::tempdir().unwrap();
        let component_set = canonical_host_component_set(
            "cursor",
            Some(crate::cli::HostBundleComponentArg::Core),
            0,
        )
        .unwrap()
        .unwrap();
        // A receiptless deployment: the cataloged path exists on disk with
        // foreign bytes and no receipt records it.
        let adopted =
            &component_set.component_set.components[0].manifest.artifacts[0].relative_path;
        let deployed = home.path().join(adopted);
        std::fs::create_dir_all(deployed.parent().unwrap()).unwrap();
        std::fs::write(&deployed, b"pre-receipt").unwrap();
        let options = crate::cli::HostBundleCliOptions {
            component: Some(crate::cli::HostBundleComponentArg::Core),
            dry_run: false,
            yes: true,
            adopt: false,
        };

        let error = super::preview_canonical_component_set(
            "cursor",
            HostBundleCliOperation::Repair,
            &component_set,
            &options,
            home.path(),
            lifecycle.path(),
            None,
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains(adopted.as_str()), "{error}");
        assert!(error.contains("--yes --adopt"), "{error}");
        assert_eq!(
            std::fs::read(&deployed).unwrap(),
            b"pre-receipt",
            "the refusing preview is read-only"
        );

        let confirmed = crate::cli::HostBundleCliOptions {
            adopt: true,
            ..options
        };
        super::preview_canonical_component_set(
            "cursor",
            HostBundleCliOperation::Repair,
            &component_set,
            &confirmed,
            home.path(),
            lifecycle.path(),
            None,
        )
        .expect("explicit adoption authority must let the same repair plan");
        assert_eq!(
            std::fs::read(&deployed).unwrap(),
            b"pre-receipt",
            "the preview is read-only even with adoption authority"
        );
    }

    #[test]
    fn component_apply_refuses_receiptless_bytes_without_adoption_authority() {
        let _profile = pinned_host_profile();
        let home = tempfile::tempdir().unwrap();
        let lifecycle = tempfile::tempdir().unwrap();
        let component_set = canonical_host_component_set_with_tracedecay_bin(
            "cursor",
            Some(crate::cli::HostBundleComponentArg::Core),
            0,
            KIRO_FIXTURE_BIN,
        )
        .unwrap()
        .unwrap();
        let relative =
            &component_set.component_set.components[0].manifest.artifacts[0].relative_path;
        let deployed = home.path().join(relative);
        std::fs::create_dir_all(deployed.parent().unwrap()).unwrap();
        std::fs::write(&deployed, b"operator-owned").unwrap();

        let error = super::apply_canonical_component_set(
            "cursor",
            HostBundleCliOperation::Install,
            &component_set,
            &crate::cli::HostBundleCliOptions {
                component: Some(crate::cli::HostBundleComponentArg::Core),
                dry_run: false,
                yes: true,
                adopt: false,
            },
            home.path(),
            lifecycle.path(),
            &ComponentSetApplyContext::with_tracedecay_bin(KIRO_FIXTURE_BIN),
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains(relative.as_str()), "{error}");
        assert!(error.contains("--adopt"), "{error}");
        assert_eq!(std::fs::read(deployed).unwrap(), b"operator-owned");
    }

    #[test]
    fn default_component_apply_honors_explicit_adoption_authority() {
        let _profile = pinned_host_profile();
        let home = tempfile::tempdir().unwrap();
        let component_set = canonical_host_component_set("cursor", None, 0)
            .unwrap()
            .unwrap();
        let relative =
            &component_set.component_set.components[0].manifest.artifacts[0].relative_path;
        let deployed = home.path().join(relative);
        std::fs::create_dir_all(deployed.parent().unwrap()).unwrap();
        std::fs::write(&deployed, b"pre-receipt").unwrap();

        let error = apply_default_canonical_component_set(
            "cursor",
            HostBundleCliOperation::Install,
            home.path(),
            true,
            false,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("--adopt"), "{error}");
        assert_eq!(std::fs::read(&deployed).unwrap(), b"pre-receipt");

        apply_default_canonical_component_set(
            "cursor",
            HostBundleCliOperation::Install,
            home.path(),
            true,
            true,
        )
        .unwrap();
        assert_ne!(std::fs::read(deployed).unwrap(), b"pre-receipt");
    }

    struct VerifyFailureRegistration {
        inner: CatalogHostComponentRegistrationAuthority,
        stale_export_path: PathBuf,
        stale_export_present_at_verify: bool,
        verify_failure_injected: bool,
    }

    impl HostComponentSetRegistrationV1 for VerifyFailureRegistration {
        fn current_revision(
            &self,
            component_set: &HostComponentSetV1,
            request: &HostComponentSetExecutionRequestV1,
        ) -> Result<[u8; 32], HostBundleError> {
            self.inner.current_revision(component_set, request)
        }

        fn discover_competing_extension_claims(
            &self,
            component_set: &HostComponentSetV1,
            request: &HostComponentSetExecutionRequestV1,
        ) -> Result<Vec<CompetingHostExtensionClaimV1>, HostBundleError> {
            self.inner
                .discover_competing_extension_claims(component_set, request)
        }

        fn confirm_preview(
            &mut self,
            component_set: &HostComponentSetV1,
            request: &HostComponentSetExecutionRequestV1,
            preview: &HostComponentSetLifecyclePreviewV1,
        ) -> Result<(), HostBundleError> {
            self.inner.confirm_preview(component_set, request, preview)
        }

        fn declare_artifact_writes(
            &mut self,
            component_set: &HostComponentSetV1,
            request: &HostComponentSetExecutionRequestV1,
            paths: &[PathBuf],
        ) -> Result<(), HostBundleError> {
            self.inner
                .declare_artifact_writes(component_set, request, paths)
        }

        fn preflight(
            &mut self,
            component_set: &HostComponentSetV1,
            request: &HostComponentSetExecutionRequestV1,
        ) -> Result<(), HostBundleError> {
            self.inner.preflight(component_set, request)
        }

        fn stage(
            &mut self,
            component_set: &HostComponentSetV1,
            request: &HostComponentSetExecutionRequestV1,
        ) -> Result<(), HostBundleError> {
            self.inner.stage(component_set, request)
        }

        fn apply(
            &mut self,
            component_set: &HostComponentSetV1,
            request: &HostComponentSetExecutionRequestV1,
        ) -> Result<(), HostBundleError> {
            self.inner.apply(component_set, request)
        }

        fn verify(
            &mut self,
            component_set: &HostComponentSetV1,
            request: &HostComponentSetExecutionRequestV1,
        ) -> Result<(), HostBundleError> {
            self.inner.verify(component_set, request)?;
            self.stale_export_present_at_verify = self.stale_export_path.exists();
            self.verify_failure_injected = true;
            Err(tracedecay_host_integration::host_bundle_storage_failure!())
        }

        fn commit(
            &mut self,
            component_set: &HostComponentSetV1,
            request: &HostComponentSetExecutionRequestV1,
        ) -> Result<(), HostBundleError> {
            self.inner.commit(component_set, request)
        }

        fn rollback(
            &mut self,
            component_set: &HostComponentSetV1,
            request: &HostComponentSetExecutionRequestV1,
        ) -> Result<(), HostBundleError> {
            self.inner.rollback(component_set, request)
        }
    }

    /// Pinned binary path for the Kiro fixtures. Resolving it from `PATH`
    /// would let a sibling test that swaps `PATH` change these artifacts
    /// mid-test.
    const KIRO_FIXTURE_BIN: &str = "/usr/local/bin/tracedecay";

    use super::isolated_profile::EnvVarGuard;

    /// Keep Kiro lifecycle tests on the native `kiro-cli` route. The compiled
    /// fixture is a real executable so Windows runners do not rename a shell
    /// script to `.exe` or depend on an ambient Kiro install.
    fn write_fake_kiro_cli(path: &Path) {
        let dir = path.parent().expect("kiro-cli fixture path has a parent");
        super::host_cli_fixture::install_compiled_host_cli_fixture(dir, "kiro-cli");
    }

    /// Install a compiled `codex` fixture on `PATH` for Core lifecycle tests.
    ///
    /// Core activation drives Codex's own `codex plugin add`
    /// (`plugin_registry::require_codex_plugin_cli`), which is a *requirement*,
    /// not a preference: the host-capability doctrine forbids a fallback that
    /// edits Codex-owned files behind the host's back. CI runners carry no
    /// `codex` binary, so a test that exercises activation has to supply the
    /// host CLI the same way the Kiro tests supply theirs. Only host program
    /// resolution sees the fixture directory; the process `PATH` is untouched,
    /// so `which_tracedecay` and sibling tests keep the ambient environment.
    fn install_fake_codex_cli(
        dir: &std::path::Path,
    ) -> tracedecay_runtime_core::config::HostProgramSearchPathGuard {
        super::host_cli_fixture::install_compiled_host_cli_fixture(dir, "codex");
        tracedecay_runtime_core::config::HostProgramSearchPathGuard::set(dir)
    }

    /// The installed fixture has to resolve to the executable itself. A link
    /// to a path that does not exist is created without error on Unix, and
    /// host program resolution then reads it as no host CLI at all.
    #[test]
    fn installing_the_host_cli_fixture_resolves_to_an_executable() {
        let dir = tempfile::tempdir().unwrap();
        let installed =
            super::host_cli_fixture::install_compiled_host_cli_fixture(dir.path(), "kiro-cli");
        assert!(
            installed.is_file(),
            "installed host-CLI fixture at {} resolves to nothing",
            installed.display()
        );
    }

    fn seed_opencode_non_context_state(home: &std::path::Path) -> (PathBuf, PathBuf, PathBuf) {
        let config_path = home.join(".config/opencode/opencode.json");
        let core_path = home.join(".config/opencode/plugins/tracedecay.ts");
        let agent_set = canonical_host_component_set(
            "opencode",
            Some(crate::cli::HostBundleComponentArg::Agent),
            0,
        )
        .unwrap()
        .unwrap();
        let agent_path =
            home.join(&agent_set.component_set.components[0].manifest.artifacts[0].relative_path);
        for path in [&config_path, &core_path, &agent_path] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        }
        std::fs::write(&config_path, OPENCODE_UNRELATED_CONFIG).unwrap();
        std::fs::write(&core_path, b"core-sentinel\n").unwrap();
        std::fs::write(&agent_path, b"agent-sentinel\n").unwrap();
        (config_path, core_path, agent_path)
    }

    fn assert_opencode_non_context_state(paths: &(PathBuf, PathBuf, PathBuf)) {
        let config: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&paths.0).unwrap()).unwrap();
        assert_eq!(config["unrelated"]["keep"], true);
        assert_eq!(
            config["lsp"]["other"]["command"],
            serde_json::json!(["tracedecay", "lsp", "bridge", "--stdio"])
        );
        assert_eq!(std::fs::read(&paths.1).unwrap(), b"core-sentinel\n");
        assert_eq!(std::fs::read(&paths.2).unwrap(), b"agent-sentinel\n");
        assert!(
            !PathBuf::from(format!("{}.bak", paths.0.display())).exists(),
            "component lifecycle must not leave a legacy config backup"
        );
    }

    #[tokio::test]
    async fn codex_automation_project_initializes_through_daemon() {
        crate::product_runtime::register_for_tests();
        let project = tempfile::tempdir().unwrap();
        let project_path = project.path().to_path_buf();
        let expected_project_path = project_path.clone();
        let expected_dashboard = project_path.join("dashboard");
        let actual = broker_codex_daemon_automation_project(
            &project_path,
            move |handshake| async move {
                assert_eq!(
                    handshake.project_path.as_deref(),
                    Some(expected_project_path.as_path())
                );
                assert!(handshake.allow_init);
                Ok(())
            },
            |_| Ok(expected_dashboard.clone()),
        )
        .await
        .unwrap();

        assert_eq!(actual, expected_dashboard);
    }

    #[tokio::test]
    async fn unavailable_daemon_does_not_resolve_or_open_local_project() {
        // Without a registered provider the handshake fails first, and this
        // test's injected `daemon unavailable` failure never runs.
        crate::product_runtime::register_for_tests();
        let project = tempfile::tempdir().unwrap();
        let resolved = Arc::new(AtomicBool::new(false));
        let resolver_called = Arc::clone(&resolved);
        let error = broker_codex_daemon_automation_project(
            project.path(),
            |_| async {
                Err(tracedecay_domain::errors::TraceDecayError::Config {
                    message: "daemon unavailable".to_string(),
                })
            },
            move |_| {
                resolver_called.store(true, Ordering::SeqCst);
                Ok(PathBuf::from("unreachable"))
            },
        )
        .await
        .unwrap_err();

        assert!(error.to_string().contains("daemon unavailable"));
        assert!(!resolved.load(Ordering::SeqCst));
        assert!(std::fs::read_dir(project.path()).unwrap().next().is_none());
    }

    #[test]
    fn canonical_host_component_selection_uses_default_or_explicit_set() {
        let default = canonical_host_component_set("opencode", None, 0)
            .unwrap()
            .expect("OpenCode has a first-party default set");
        assert_eq!(default.component_set.components.len(), 3);

        let explicit = canonical_host_component_set(
            "opencode",
            Some(crate::cli::HostBundleComponentArg::ContextMcp),
            0,
        )
        .unwrap()
        .expect("explicit component uses a one-element set");
        assert_eq!(explicit.component_set.components.len(), 1);
        assert_eq!(
            explicit.component_set.components[0].manifest.component,
            tracedecay_agent_hosts::agents::host_bundle::HostComponentV1::ContextMcp
        );
        let hermes = canonical_host_component_set("hermes", None, 0)
            .unwrap()
            .expect("Hermes has a first-party default Core set");
        assert_eq!(hermes.component_set.components.len(), 1);
        assert_eq!(
            hermes.component_set.components[0].manifest.component,
            tracedecay_agent_hosts::agents::host_bundle::HostComponentV1::Core
        );
        // Kiro's supported route is its MCP registration alone; the degraded
        // hook route lives in Core and stays out of the default set.
        let kiro = canonical_host_component_set("kiro", None, 0)
            .unwrap()
            .expect("Kiro's MCP registration is a supported first-party route");
        assert_eq!(
            kiro.component_set
                .components
                .iter()
                .map(|component| component.manifest.component)
                .collect::<Vec<_>>(),
            vec![tracedecay_agent_hosts::agents::host_bundle::HostComponentV1::ContextMcp]
        );
        // Gemini's extension carries the MCP server and declares no hook, so
        // its default set is the separable MCP route and Core is a typed
        // refusal rather than a silently skipped agent.
        let gemini = canonical_host_component_set("gemini", None, 0)
            .unwrap()
            .expect("Gemini's extension registration is a supported first-party route");
        assert_eq!(
            gemini
                .component_set
                .components
                .iter()
                .map(|component| component.manifest.component)
                .collect::<Vec<_>>(),
            vec![tracedecay_agent_hosts::agents::host_bundle::HostComponentV1::ContextMcp]
        );
        assert!(
            canonical_host_component_set(
                "gemini",
                Some(crate::cli::HostBundleComponentArg::Core),
                0,
            )
            .is_err(),
            "a component the host cannot carry is refused, not reported unavailable"
        );
    }

    #[test]
    fn explicit_context_component_lifecycle_preserves_other_opencode_state() {
        let _profile = pinned_host_profile();
        let home = tempfile::tempdir().unwrap();
        let lifecycle = tempfile::tempdir().unwrap();
        let preserved = seed_opencode_non_context_state(home.path());
        let component_set = canonical_host_component_set(
            "opencode",
            Some(crate::cli::HostBundleComponentArg::ContextMcp),
            0,
        )
        .unwrap()
        .unwrap();
        let options = crate::cli::HostBundleCliOptions {
            component: Some(crate::cli::HostBundleComponentArg::ContextMcp),
            dry_run: false,
            yes: true,
            adopt: false,
        };

        apply_canonical_component_set(
            "opencode",
            HostBundleCliOperation::Install,
            &component_set,
            &options,
            home.path(),
            lifecycle.path(),
            &ComponentSetApplyContext::resolved(),
        )
        .unwrap();
        assert_opencode_non_context_state(&preserved);

        apply_canonical_component_set(
            "opencode",
            HostBundleCliOperation::Uninstall,
            &component_set,
            &options,
            home.path(),
            lifecycle.path(),
            &ComponentSetApplyContext::resolved(),
        )
        .unwrap();
        assert_opencode_non_context_state(&preserved);
    }

    /// Kiro's global MCP registry is owned by `kiro-cli`; the component
    /// transaction must drive that native command while retaining peer
    /// servers instead of editing the registry behind Kiro's back.
    #[cfg(unix)]
    #[test]
    fn kiro_context_mcp_component_set_applies_non_interactively_and_repeats() {
        let _profile = pinned_host_profile();
        #[cfg(unix)]
        let kiro_cli_dir = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        let kiro_cli_path = kiro_cli_dir.path().join("kiro-cli");
        #[cfg(unix)]
        write_fake_kiro_cli(&kiro_cli_path);
        #[cfg(unix)]
        let _kiro_path =
            tracedecay_runtime_core::config::HostProgramSearchPathGuard::set(kiro_cli_dir.path());
        let home = tempfile::tempdir().unwrap();
        let lifecycle = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join(".kiro")).unwrap();
        let component_set =
            canonical_host_component_set_with_tracedecay_bin("kiro", None, 0, KIRO_FIXTURE_BIN)
                .unwrap()
                .unwrap();
        let options = crate::cli::HostBundleCliOptions {
            component: None,
            dry_run: false,
            yes: true,
            adopt: false,
        };

        // Install then Repair is exactly the non-interactive update loop:
        // `reinstall_agent_integrations` re-runs the canonical component set
        // as `Repair` on every update.
        for operation in [
            HostBundleCliOperation::Install,
            HostBundleCliOperation::Repair,
            HostBundleCliOperation::Repair,
        ] {
            apply_canonical_component_set(
                "kiro",
                operation,
                &component_set,
                &options,
                home.path(),
                lifecycle.path(),
                &ComponentSetApplyContext::with_tracedecay_bin(KIRO_FIXTURE_BIN),
            )
            .unwrap_or_else(|error| panic!("kiro {operation:?} must apply cleanly: {error}"));
        }

        let registration_path = home.path().join(".kiro/settings/mcp.json");
        let registered: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&registration_path).unwrap()).unwrap();
        assert!(
            registered["mcpServers"]["tracedecay"].is_object(),
            "kiro MCP registration must survive the transaction: {registered}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn kiro_doctor_accepts_the_canonical_mcp_only_install() {
        use tracedecay_agent_hosts::agents::{
            AgentIntegration, DoctorCounters, HealthcheckContext, KiroIntegration,
        };

        let _profile = pinned_host_profile();
        let kiro_cli_dir = tempfile::tempdir().unwrap();
        write_fake_kiro_cli(&kiro_cli_dir.path().join("kiro-cli"));
        let _kiro_path =
            tracedecay_runtime_core::config::HostProgramSearchPathGuard::set(kiro_cli_dir.path());
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let lifecycle = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join(".kiro")).unwrap();

        let component_set =
            canonical_host_component_set_with_tracedecay_bin("kiro", None, 0, KIRO_FIXTURE_BIN)
                .unwrap()
                .unwrap();
        apply_canonical_component_set(
            "kiro",
            HostBundleCliOperation::Install,
            &component_set,
            &crate::cli::HostBundleCliOptions {
                component: None,
                dry_run: false,
                yes: true,
                adopt: false,
            },
            home.path(),
            lifecycle.path(),
            &ComponentSetApplyContext::with_tracedecay_bin(KIRO_FIXTURE_BIN),
        )
        .unwrap();

        // Global install is MCP-only: no steering or managed agent is written.
        assert!(!home.path().join(".kiro/steering/tracedecay.md").exists());
        assert!(!home.path().join(".kiro/agents/tracedecay.json").exists());
        let registered: serde_json::Value = serde_json::from_slice(
            &std::fs::read(home.path().join(".kiro/settings/mcp.json")).unwrap(),
        )
        .unwrap();
        assert!(
            registered["mcpServers"]["tracedecay"].is_object(),
            "global install must register the MCP server: {registered}"
        );

        let mut counters = DoctorCounters::new();
        KiroIntegration.healthcheck(
            &mut counters,
            &HealthcheckContext {
                home: home.path().to_path_buf(),
                project_path: project.path().to_path_buf(),
            },
        );
        assert_eq!(counters.issues, 0);
        assert_eq!(counters.warnings, 0);
    }

    /// A standing refusal such as an ownership conflict must reach the operator
    /// under its own name. Reporting it as `StalePreview` tells them to retry
    /// something that can never succeed and hides the only actionable
    /// diagnostic they have.
    #[test]
    fn confirmed_apply_reports_an_ownership_conflict_as_itself() {
        use tracedecay_agent_hosts::agents::host_bundle::HostBundleError;

        let _profile = pinned_host_profile();
        let kiro_cli_dir = tempfile::tempdir().unwrap();
        let kiro_cli_path = kiro_cli_dir.path().join("kiro-cli");
        write_fake_kiro_cli(&kiro_cli_path);
        let _kiro_path =
            tracedecay_runtime_core::config::HostProgramSearchPathGuard::set(kiro_cli_dir.path());
        let home = tempfile::tempdir().unwrap();
        let lifecycle = tempfile::tempdir().unwrap();
        let component_set =
            canonical_host_component_set_with_tracedecay_bin("kiro", None, 0, KIRO_FIXTURE_BIN)
                .unwrap()
                .unwrap();
        std::fs::create_dir_all(home.path().join(".kiro")).unwrap();
        let install_options = crate::cli::HostBundleCliOptions {
            component: None,
            dry_run: false,
            yes: true,
            adopt: false,
        };
        // A receipt-backed install claims the artifact path first: only a
        // receipt makes a later foreign edit a standing conflict rather than
        // an adoptable pre-receipt deployment.
        apply_canonical_component_set(
            "kiro",
            HostBundleCliOperation::Install,
            &component_set,
            &install_options,
            home.path(),
            lifecycle.path(),
            &ComponentSetApplyContext::with_tracedecay_bin(KIRO_FIXTURE_BIN),
        )
        .expect("the packaged Kiro set must install cleanly");

        let request =
            component_set_request(&component_set, HostBundleCliOperation::Update, true, false)
                .unwrap();
        let mut writer =
            tracedecay_agent_hosts::agents::host_bundle::HostBundleWriterV1::open_with_lifecycle_root(
                home.path(),
                lifecycle.path(),
            )
            .unwrap();
        let mut transaction =
            tracedecay_agent_hosts::agents::host_bundle::HostComponentSetTransactionV1::new(
                &mut writer,
            );
        let mut registration = CatalogHostComponentRegistrationAuthority::new(
            "kiro",
            home.path(),
            request.lifecycle.operation,
        )
        .unwrap();
        let preview = transaction
            .preview(
                &component_set.component_set,
                &request,
                &component_set,
                &mut registration,
            )
            .unwrap();

        // Somebody else rewrites the receipt-owned bytes between preview and
        // apply. The bytes now match neither the catalog nor the receipt, so
        // no retry can ever clear this.
        let artifact_path = home
            .path()
            .join(&component_set.component_set.components[0].manifest.artifacts[0].relative_path);
        std::fs::write(&artifact_path, b"{\"owner\":\"somebody else\"}").unwrap();

        let error = transaction
            .execute_confirmed(
                &component_set.component_set,
                &request,
                &preview,
                &component_set,
                &mut registration,
            )
            .expect_err("a foreign edit to a receipt-owned file must refuse the apply");
        assert!(
            !matches!(error, HostBundleError::StalePreview(_)),
            "a standing refusal must not be laundered into a retryable staleness report"
        );
        assert!(matches!(error, HostBundleError::OwnershipConflict(_)));
    }

    #[test]
    fn absent_kiro_cli_is_typed_unavailability_not_an_ownership_conflict() {
        let _profile = pinned_host_profile();
        let empty_path = tempfile::tempdir().unwrap();
        let _path =
            tracedecay_runtime_core::config::HostProgramSearchPathGuard::set(empty_path.path());
        let home = tempfile::tempdir().unwrap();
        let lifecycle = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join(".kiro")).unwrap();
        let component_set =
            canonical_host_component_set_with_tracedecay_bin("kiro", None, 0, KIRO_FIXTURE_BIN)
                .unwrap()
                .unwrap();
        let options = crate::cli::HostBundleCliOptions {
            component: None,
            dry_run: false,
            yes: true,
            adopt: false,
        };
        let error = apply_canonical_component_set(
            "kiro",
            HostBundleCliOperation::Install,
            &component_set,
            &options,
            home.path(),
            lifecycle.path(),
            &ComponentSetApplyContext::with_tracedecay_bin(KIRO_FIXTURE_BIN),
        )
        .expect_err("an absent Kiro CLI must refuse the lifecycle");
        let message = error.to_string();
        assert!(
            message.contains("unavailable"),
            "missing executable must stay a typed unavailability: {message}"
        );
        assert!(
            !message.contains("ownership conflict"),
            "absence must not be reported as an ownership conflict: {message}"
        );
    }

    #[test]
    fn malformed_kiro_fixture_output_is_not_unavailability_or_ownership_conflict() {
        let _profile = pinned_host_profile();
        let kiro_cli_dir = tempfile::tempdir().unwrap();
        write_fake_kiro_cli(&kiro_cli_dir.path().join("kiro-cli"));
        let _kiro_path =
            tracedecay_runtime_core::config::HostProgramSearchPathGuard::set(kiro_cli_dir.path());
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join(".tracedecay-host-cli-fixture")).unwrap();
        std::fs::write(
            home.path().join(".tracedecay-host-cli-fixture/malformed"),
            b"",
        )
        .unwrap();
        std::fs::create_dir_all(home.path().join(".kiro")).unwrap();
        let lifecycle = tempfile::tempdir().unwrap();
        let component_set =
            canonical_host_component_set_with_tracedecay_bin("kiro", None, 0, KIRO_FIXTURE_BIN)
                .unwrap()
                .unwrap();
        let options = crate::cli::HostBundleCliOptions {
            component: None,
            dry_run: false,
            yes: true,
            adopt: false,
        };
        let error = apply_canonical_component_set(
            "kiro",
            HostBundleCliOperation::Install,
            &component_set,
            &options,
            home.path(),
            lifecycle.path(),
            &ComponentSetApplyContext::with_tracedecay_bin(KIRO_FIXTURE_BIN),
        )
        .expect_err("malformed helper output must fail the lifecycle");
        let message = error.to_string();
        assert!(
            !message.contains("unavailable"),
            "a present helper that writes garbage must not look like a missing CLI: {message}"
        );
        assert!(
            !message.contains("ownership conflict"),
            "malformed helper output must not be laundered into an ownership conflict: {message}"
        );
    }

    /// A foreign edit landing between `stage` and `apply` must still abort the
    /// transaction. Scoping the apply-time recheck to the paths this
    /// transaction did not declare must not weaken that.
    #[test]
    fn foreign_registration_edit_between_stage_and_apply_still_aborts() {
        use tracedecay_agent_hosts::agents::host_bundle::{
            HostBundleError, HostComponentSetLifecyclePreviewV1, HostComponentSetRegistrationV1,
        };

        let _profile = pinned_host_profile();
        let home = tempfile::tempdir().unwrap();
        let component_set = canonical_host_component_set("opencode", None, 0)
            .unwrap()
            .unwrap();
        let request =
            component_set_request(&component_set, HostBundleCliOperation::Install, true, false)
                .unwrap();
        let mut registration = CatalogHostComponentRegistrationAuthority::new(
            "opencode",
            home.path(),
            request.lifecycle.operation,
        )
        .unwrap();

        // No declared writes: every registration path stays foreign, so the
        // scoped recheck is the full revision.
        registration
            .declare_artifact_writes(&component_set.component_set, &request, &[])
            .unwrap();
        registration
            .preflight(&component_set.component_set, &request)
            .unwrap();
        let revision = registration
            .current_revision(&component_set.component_set, &request)
            .unwrap();
        let preview = HostComponentSetLifecyclePreviewV1 {
            operation_id: request.operation_id,
            plan_digest: [7; 32],
            base_registration_revision: revision,
            current_registration_revision: revision,
            artifact_state_revision: [8; 32],
            component_plans: Vec::new(),
            competing_extension_claims: Vec::new(),
            confirmation_required: false,
        };
        registration
            .confirm_preview(&component_set.component_set, &request, &preview)
            .unwrap();
        registration
            .stage(&component_set.component_set, &request)
            .unwrap();

        // Somebody else rewrites the registration surface mid-transaction.
        let registration_path = home.path().join(".config/opencode/opencode.json");
        std::fs::create_dir_all(registration_path.parent().unwrap()).unwrap();
        std::fs::write(&registration_path, b"{\"external\":true}").unwrap();

        assert!(
            matches!(
                registration.apply(&component_set.component_set, &request),
                Err(HostBundleError::StalePreview(_))
            ),
            "a foreign mid-transaction edit must still abort the apply"
        );
    }

    /// The transaction's own declared write must not read back as foreign
    /// drift, but a foreign edit to a path it did *not* declare still must.
    #[test]
    fn declared_artifact_write_is_not_foreign_drift() {
        use tracedecay_agent_hosts::agents::host_bundle::{
            HostBundleError, HostComponentSetLifecyclePreviewV1, HostComponentSetRegistrationV1,
        };

        let component_set = canonical_host_component_set("opencode", None, 0)
            .unwrap()
            .unwrap();

        // `declare_the_write` decides only whether the mid-transaction write to
        // the registration surface is attributed to this transaction. Every
        // other input is identical, and each run gets its own home so the two
        // outcomes cannot influence each other.
        let drive = |declare_the_write: bool| {
            let _profile = pinned_host_profile();
            let home = tempfile::tempdir().unwrap();
            let registration_path = home.path().join(".config/opencode/opencode.json");
            // Create the directory up front: a registration directory that is
            // absent at stage and present at apply is its own (unrelated)
            // staleness signal, and this test is about the file's content.
            std::fs::create_dir_all(registration_path.parent().unwrap()).unwrap();
            let request =
                component_set_request(&component_set, HostBundleCliOperation::Install, true, false)
                    .unwrap();
            let mut registration = CatalogHostComponentRegistrationAuthority::new(
                "opencode",
                home.path(),
                request.lifecycle.operation,
            )
            .unwrap();
            let declared: Vec<PathBuf> = if declare_the_write {
                vec![registration_path.clone()]
            } else {
                Vec::new()
            };
            registration
                .declare_artifact_writes(&component_set.component_set, &request, &declared)
                .unwrap();
            registration
                .preflight(&component_set.component_set, &request)
                .unwrap();
            let revision = registration
                .current_revision(&component_set.component_set, &request)
                .unwrap();
            let preview = HostComponentSetLifecyclePreviewV1 {
                operation_id: request.operation_id,
                plan_digest: [7; 32],
                base_registration_revision: revision,
                current_registration_revision: revision,
                artifact_state_revision: [8; 32],
                component_plans: Vec::new(),
                competing_extension_claims: Vec::new(),
                confirmation_required: false,
            };
            registration
                .confirm_preview(&component_set.component_set, &request, &preview)
                .unwrap();
            registration
                .stage(&component_set.component_set, &request)
                .unwrap();
            // The write the transaction makes to its own registration surface.
            std::fs::write(&registration_path, b"{\"written\":\"by the transaction\"}").unwrap();
            registration.apply(&component_set.component_set, &request)
        };

        assert!(
            !matches!(drive(true), Err(HostBundleError::StalePreview(_))),
            "the transaction's own declared write must not read back as drift"
        );
        assert!(
            matches!(drive(false), Err(HostBundleError::StalePreview(_))),
            "the same write is foreign drift when the transaction did not declare it"
        );
    }

    #[test]
    fn explicit_context_component_rollback_preserves_other_opencode_state() {
        use tracedecay_agent_hosts::agents::host_bundle::HostComponentSetRegistrationV1;

        let _profile = pinned_host_profile();
        let home = tempfile::tempdir().unwrap();
        let preserved = seed_opencode_non_context_state(home.path());
        let component_set = canonical_host_component_set(
            "opencode",
            Some(crate::cli::HostBundleComponentArg::ContextMcp),
            0,
        )
        .unwrap()
        .unwrap();
        let request =
            component_set_request(&component_set, HostBundleCliOperation::Install, true, false)
                .unwrap();
        let mut registration = CatalogHostComponentRegistrationAuthority::new(
            "opencode",
            home.path(),
            request.lifecycle.operation,
        )
        .unwrap();

        registration
            .preflight(&component_set.component_set, &request)
            .unwrap();
        registration
            .stage(&component_set.component_set, &request)
            .unwrap();
        let prompt_path = home.path().join(".config/opencode/AGENTS.md");
        std::fs::write(&prompt_path, b"concurrent prompt edit\n").unwrap();
        registration
            .apply(&component_set.component_set, &request)
            .unwrap();
        registration
            .rollback(&component_set.component_set, &request)
            .unwrap();

        assert_opencode_non_context_state(&preserved);
        assert_eq!(
            std::fs::read(&prompt_path).unwrap(),
            b"concurrent prompt edit\n"
        );
    }

    #[test]
    fn opencode_non_owner_component_cannot_remove_context_registration() {
        let _profile = pinned_host_profile();
        let home = tempfile::tempdir().unwrap();
        let config_path = home.path().join(".config/opencode/opencode.json");
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        std::fs::write(&config_path, OPENCODE_CONTEXT_CONFIG).unwrap();
        let integration = tracedecay_agent_hosts::agents::get_integration("opencode").unwrap();
        let context = tracedecay_agent_hosts::agents::InstallContext {
            home: home.path().to_path_buf(),
            tracedecay_bin: "tracedecay".to_string(),
            project_root: None,
            dashboard: true,
        };

        integration
            .activate_deployed_host_component_registration(
                &[tracedecay_agent_hosts::agents::host_bundle::HostComponentV1::OperatorMcp],
                &context,
            )
            .unwrap();
        integration
            .deactivate_deployed_host_component_registration(
                &[tracedecay_agent_hosts::agents::host_bundle::HostComponentV1::OperatorMcp],
                &context,
            )
            .unwrap();

        assert_eq!(
            std::fs::read(&config_path).unwrap(),
            OPENCODE_CONTEXT_CONFIG
        );
    }

    #[test]
    fn current_opencode_context_install_is_byte_preserving() {
        use tracedecay_agent_hosts::agents::host_bundle::HostComponentSetRegistrationV1;

        let _profile = pinned_host_profile();
        let home = tempfile::tempdir().unwrap();
        let config_path = home.path().join(".config/opencode/opencode.json");
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        std::fs::write(&config_path, OPENCODE_CONTEXT_CONFIG).unwrap();
        let component_set = canonical_host_component_set(
            "opencode",
            Some(crate::cli::HostBundleComponentArg::ContextMcp),
            0,
        )
        .unwrap()
        .unwrap();
        let request =
            component_set_request(&component_set, HostBundleCliOperation::Install, true, false)
                .unwrap();
        let mut registration = CatalogHostComponentRegistrationAuthority::new(
            "opencode",
            home.path(),
            request.lifecycle.operation,
        )
        .unwrap();

        registration
            .preflight(&component_set.component_set, &request)
            .unwrap();
        registration
            .stage(&component_set.component_set, &request)
            .unwrap();
        registration
            .apply(&component_set.component_set, &request)
            .unwrap();
        registration
            .commit(&component_set.component_set, &request)
            .unwrap();

        assert_eq!(
            std::fs::read(&config_path).unwrap(),
            OPENCODE_CONTEXT_CONFIG
        );
    }

    #[test]
    fn opencode_core_rollback_restores_every_registration_side_effect() {
        use tracedecay_agent_hosts::agents::host_bundle::HostComponentSetRegistrationV1;

        let _profile = pinned_host_profile();
        let home = tempfile::tempdir().unwrap();
        let config_path = home.path().join(".config/opencode/opencode.json");
        let prompt_path = home.path().join(".config/opencode/AGENTS.md");
        for path in [&config_path, &prompt_path] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        }
        let original_config = b"{\"unrelated\":\"keep\"}\n";
        let original_prompt = b"user prompt\n";
        std::fs::write(&config_path, original_config).unwrap();
        std::fs::write(&prompt_path, original_prompt).unwrap();
        let component_set = canonical_host_component_set(
            "opencode",
            Some(crate::cli::HostBundleComponentArg::Core),
            0,
        )
        .unwrap()
        .unwrap();
        let request =
            component_set_request(&component_set, HostBundleCliOperation::Repair, true, false)
                .unwrap();
        let mut registration = CatalogHostComponentRegistrationAuthority::new(
            "opencode",
            home.path(),
            request.lifecycle.operation,
        )
        .unwrap();

        registration
            .preflight(&component_set.component_set, &request)
            .unwrap();
        registration
            .stage(&component_set.component_set, &request)
            .unwrap();
        registration
            .apply(&component_set.component_set, &request)
            .unwrap();
        registration
            .rollback(&component_set.component_set, &request)
            .unwrap();

        assert_eq!(std::fs::read(&config_path).unwrap(), original_config);
        assert_eq!(std::fs::read(&prompt_path).unwrap(), original_prompt);
    }

    #[cfg(unix)]
    #[test]
    fn codex_core_rollback_restores_generated_agent_exports_byte_for_byte() {
        let _profile = pinned_host_profile();
        // Core `apply` drives Codex's own `codex plugin add`, which is a hard
        // requirement of that path. Supply the host CLI rather than depending
        // on whatever the machine happens to have installed.
        let codex_cli_dir = tempfile::tempdir().unwrap();
        let _codex_path = install_fake_codex_cli(codex_cli_dir.path());
        let home = host_cli_tempdir();
        // Same filesystem as `home`: the receipt transaction backs up a
        // staged artifact by renaming it into `lifecycle`, and rename cannot
        // cross a filesystem boundary.
        let lifecycle = host_cli_tempdir();
        // One bin for stage + component-set render + registration activate so
        // Codex hook-trust's safety valve sees matching commands (not
        // `which_tracedecay` vs `current_exe` drift under cargo-test).
        let tracedecay_bin = tracedecay_agent_hosts::agents::which_tracedecay()
            .unwrap_or_else(|| "tracedecay".to_string());
        let agents_dir = home.path().join(".codex/agents");
        std::fs::create_dir_all(&agents_dir).unwrap();
        let stale_path = agents_dir.join("tracedecay-legacy.toml");
        let current_path = agents_dir.join("tracedecay-code-explorer.toml");
        let user_path = agents_dir.join("user-agent.toml");
        let manifest_path = agents_dir.join(".tracedecay-managed-agents.json");
        let stale_bytes = b"model = \"legacy\"\n";
        let current_bytes = b"model = \"preexisting-current\"\n";
        let user_bytes = b"model = \"user\"\n";
        let manifest_bytes = format!(
            "{{\"version\":1,\"exported\":[{{\"id\":\"legacy\",\"path\":{}}}]}}\n",
            serde_json::to_string(&stale_path).unwrap()
        );
        std::fs::write(&stale_path, stale_bytes).unwrap();
        std::fs::write(&current_path, current_bytes).unwrap();
        std::fs::write(&user_path, user_bytes).unwrap();
        std::fs::write(&manifest_path, manifest_bytes.as_bytes()).unwrap();

        // Core-component `apply` now drives Codex's own `codex plugin add`,
        // which requires the plugin to already be registered in Codex's
        // marketplace. Stage it exactly as the ordinary install path does.
        let integration = tracedecay_agent_hosts::agents::get_integration("codex").unwrap();
        integration
            .prepare_non_interactive_install(&tracedecay_agent_hosts::agents::InstallContext {
                home: home.path().to_path_buf(),
                tracedecay_bin: tracedecay_bin.clone(),
                project_root: None,
                dashboard: true,
            })
            .unwrap();

        let component_set = canonical_host_component_set_with_tracedecay_bin(
            "codex",
            Some(crate::cli::HostBundleComponentArg::Core),
            0,
            &tracedecay_bin,
        )
        .unwrap()
        .unwrap();
        let request =
            component_set_request(&component_set, HostBundleCliOperation::Repair, true, false)
                .unwrap();
        let mut registration = VerifyFailureRegistration {
            inner: CatalogHostComponentRegistrationAuthority::new_with_tracedecay_bin(
                "codex",
                home.path(),
                request.lifecycle.operation,
                tracedecay_bin,
            )
            .unwrap(),
            stale_export_path: stale_path.clone(),
            stale_export_present_at_verify: true,
            verify_failure_injected: false,
        };
        let mut writer =
            tracedecay_agent_hosts::agents::host_bundle::HostBundleWriterV1::open_with_lifecycle_root(
                home.path(),
                lifecycle.path(),
            )
            .unwrap();
        let mut transaction =
            tracedecay_agent_hosts::agents::host_bundle::HostComponentSetTransactionV1::new(
                &mut writer,
            );
        let preview = transaction
            .preview(
                &component_set.component_set,
                &request,
                &component_set,
                &mut registration,
            )
            .unwrap();
        let result = transaction.execute_confirmed(
            &component_set.component_set,
            &request,
            &preview,
            &component_set,
            &mut registration,
        );

        assert!(
            result.is_err(),
            "the injected post-apply verification failure must abort the transaction"
        );
        assert!(
            registration.verify_failure_injected,
            "verification must run so the failure lands after apply: {:?}",
            result.as_ref().err()
        );
        assert!(
            !registration.stale_export_present_at_verify,
            "apply must retire the stale managed export before verification: {:?}",
            result.as_ref().err()
        );

        assert_eq!(
            std::fs::read(&manifest_path).unwrap(),
            manifest_bytes.as_bytes(),
            "rollback must restore the ownership manifest: {:?}",
            result.as_ref().err()
        );
        assert_eq!(std::fs::read(&stale_path).unwrap(), stale_bytes);
        assert_eq!(std::fs::read(&current_path).unwrap(), current_bytes);
        assert_eq!(std::fs::read(&user_path).unwrap(), user_bytes);
        let mut remaining = std::fs::read_dir(&agents_dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        remaining.sort();
        assert_eq!(
            remaining,
            vec![
                ".tracedecay-managed-agents.json",
                "tracedecay-code-explorer.toml",
                "tracedecay-legacy.toml",
                "user-agent.toml",
            ]
        );
    }

    #[test]
    fn explicit_core_component_lifecycle_preserves_opencode_companions() {
        let _profile = pinned_host_profile();
        let home = tempfile::tempdir().unwrap();
        let lifecycle = tempfile::tempdir().unwrap();
        let tracedecay_bin = std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let config_path = home.path().join(".config/opencode/opencode.json");
        let context_set = canonical_host_component_set_with_tracedecay_bin(
            "opencode",
            Some(crate::cli::HostBundleComponentArg::ContextMcp),
            0,
            &tracedecay_bin,
        )
        .unwrap()
        .unwrap();
        let agent_set = canonical_host_component_set_with_tracedecay_bin(
            "opencode",
            Some(crate::cli::HostBundleComponentArg::Agent),
            0,
            &tracedecay_bin,
        )
        .unwrap()
        .unwrap();
        let context_path = home
            .path()
            .join(&context_set.component_set.components[0].manifest.artifacts[0].relative_path);
        let agent_path = home
            .path()
            .join(&agent_set.component_set.components[0].manifest.artifacts[0].relative_path);
        for path in [&config_path, &context_path, &agent_path] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        }
        std::fs::write(&config_path, OPENCODE_CONTEXT_CONFIG).unwrap();
        std::fs::write(&context_path, b"context-sentinel\n").unwrap();
        std::fs::write(&agent_path, b"agent-sentinel\n").unwrap();
        let core_set = canonical_host_component_set_with_tracedecay_bin(
            "opencode",
            Some(crate::cli::HostBundleComponentArg::Core),
            0,
            &tracedecay_bin,
        )
        .unwrap()
        .unwrap();
        let options = crate::cli::HostBundleCliOptions {
            component: Some(crate::cli::HostBundleComponentArg::Core),
            dry_run: false,
            yes: true,
            adopt: false,
        };

        for operation in [
            HostBundleCliOperation::Install,
            HostBundleCliOperation::Update,
            HostBundleCliOperation::Repair,
            HostBundleCliOperation::Uninstall,
        ] {
            apply_canonical_component_set(
                "opencode",
                operation,
                &core_set,
                &options,
                home.path(),
                lifecycle.path(),
                &ComponentSetApplyContext::with_tracedecay_bin(&tracedecay_bin),
            )
            .unwrap();
            let config: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
            assert_eq!(config["unrelated"]["keep"], true);
            assert_eq!(
                config["mcp"]["other"]["command"],
                serde_json::json!(["other"])
            );
            assert_eq!(
                config["mcp"]["tracedecay"]["command"],
                serde_json::json!(["tracedecay", "serve"])
            );
            if operation == HostBundleCliOperation::Uninstall {
                assert!(config["lsp"].get("tracedecay").is_none());
            } else {
                // The bridge binds its workspace roots from the host's own
                // `initialize` frame, so the registration deliberately carries
                // no `--project`: pinning it to OpenCode's process CWD would
                // override the folders the editor actually opened.
                assert_eq!(
                    config["lsp"]["tracedecay"]["command"],
                    serde_json::json!([tracedecay_bin.clone(), "lsp", "bridge", "--stdio"])
                );
            }
            assert_eq!(std::fs::read(&context_path).unwrap(), b"context-sentinel\n");
            assert_eq!(std::fs::read(&agent_path).unwrap(), b"agent-sentinel\n");
            assert!(!PathBuf::from(format!("{}.bak", config_path.display())).exists());
        }
    }

    /// Kiro's canonical component set drives the global registry through the
    /// native CLI and keeps its own descriptor under `.kiro/tracedecay`. The
    /// non-interactive apply must still converge while preserving a peer MCP
    /// server in Kiro's shared registry.
    #[cfg(unix)]
    #[tokio::test]
    async fn kiro_context_mcp_apply_converges_without_rollback() {
        let _profile = pinned_host_profile();
        #[cfg(unix)]
        let kiro_cli_dir = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        let kiro_cli_path = kiro_cli_dir.path().join("kiro-cli");
        #[cfg(unix)]
        write_fake_kiro_cli(&kiro_cli_path);
        #[cfg(unix)]
        let _kiro_path =
            tracedecay_runtime_core::config::HostProgramSearchPathGuard::set(kiro_cli_dir.path());
        let tracedecay_bin = std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let options = crate::cli::HostBundleCliOptions {
            component: None,
            dry_run: false,
            yes: true,
            adopt: false,
        };

        for existing in [
            None,
            Some(br#"{"mcpServers":{"other":{"command":"other","args":[]}}}"#.to_vec()),
        ] {
            let home = tempfile::tempdir().unwrap();
            let lifecycle = tempfile::tempdir().unwrap();
            let mcp_path = home.path().join(".kiro/settings/mcp.json");
            std::fs::create_dir_all(mcp_path.parent().unwrap()).unwrap();
            if let Some(bytes) = &existing {
                std::fs::write(&mcp_path, bytes).unwrap();
            }
            let component_set =
                canonical_host_component_set_with_tracedecay_bin("kiro", None, 0, &tracedecay_bin)
                    .unwrap()
                    .unwrap();

            for operation in [
                HostBundleCliOperation::Install,
                HostBundleCliOperation::Update,
                HostBundleCliOperation::Repair,
            ] {
                apply_canonical_component_set(
                    "kiro",
                    operation,
                    &component_set,
                    &options,
                    home.path(),
                    lifecycle.path(),
                    &ComponentSetApplyContext::with_tracedecay_bin(&tracedecay_bin),
                )
                .unwrap_or_else(|error| {
                    panic!(
                        "kiro {operation:?} apply must converge (existing: {existing:?}): {error}"
                    )
                });
                let config: serde_json::Value =
                    serde_json::from_slice(&std::fs::read(&mcp_path).unwrap()).unwrap();
                assert_eq!(
                    config["mcpServers"]["tracedecay"]["command"],
                    tracedecay_bin
                );
                // The shared MCP document is merged, never replaced: a
                // third-party server the operator registered survives.
                if existing.is_some() {
                    assert_eq!(config["mcpServers"]["other"]["command"], "other");
                }
            }

            apply_canonical_component_set(
                "kiro",
                HostBundleCliOperation::Uninstall,
                &component_set,
                &options,
                home.path(),
                lifecycle.path(),
                &ComponentSetApplyContext::with_tracedecay_bin(&tracedecay_bin),
            )
            .unwrap();
            match &existing {
                // Deregistration is a merge too: the operator's own server
                // outlives the uninstall.
                Some(_) => {
                    let config: serde_json::Value =
                        serde_json::from_slice(&std::fs::read(&mcp_path).unwrap()).unwrap();
                    assert!(config["mcpServers"].get("tracedecay").is_none());
                    assert_eq!(config["mcpServers"]["other"]["command"], "other");
                }
                // Nothing but TraceDecay was ever registered, so Kiro's editor
                // retires the document it created.
                None => assert!(!mcp_path.exists()),
            }
        }
    }

    /// A component set's managed artifacts and its native registration surface
    /// are two writers. The transaction writes its artifacts *after* the
    /// adapter confirms a registration revision and *before* it applies, so an
    /// artifact write that moves that revision makes the adapter's recheck read
    /// TraceDecay's own bytes as third-party drift. This invariant is checked
    /// for every host that ships a canonical set, including hosts whose native
    /// CLI owns the registration document separately from the managed
    /// descriptor.
    #[test]
    fn host_artifact_writes_never_invalidate_the_confirmed_revision() {
        use tracedecay_agent_hosts::agents::host_bundle::HostComponentSetRegistrationV1;

        let mut self_invalidating = Vec::new();
        for agent in [
            "claude", "codex", "cursor", "hermes", "kimi", "kiro", "opencode",
        ] {
            let _profile = pinned_host_profile();
            let home = tempfile::tempdir().unwrap();
            let component_set = canonical_host_component_set(agent, None, 0)
                .unwrap()
                .unwrap_or_else(|| panic!("{agent} must ship a canonical component set"));
            let request =
                component_set_request(&component_set, HostBundleCliOperation::Install, true, false)
                    .unwrap();
            let registration = CatalogHostComponentRegistrationAuthority::new(
                agent,
                home.path(),
                request.lifecycle.operation,
            )
            .unwrap();
            let confirmed = registration
                .current_revision(&component_set.component_set, &request)
                .unwrap();

            for component in &component_set.component_set.components {
                for asset in &component.contents {
                    let deployed = home.path().join(&asset.relative_path);
                    std::fs::create_dir_all(deployed.parent().unwrap()).unwrap();
                    std::fs::write(&deployed, &asset.bytes).unwrap();
                }
            }

            if registration
                .current_revision(&component_set.component_set, &request)
                .unwrap()
                != confirmed
            {
                self_invalidating.push(agent);
            }
        }

        assert!(
            self_invalidating.is_empty(),
            "deploying their own managed artifacts moved the registration revision these \
             hosts just confirmed: {self_invalidating:?}"
        );
    }

    #[test]
    fn opencode_core_refuses_a_competing_analyzer_without_mutation() {
        let _profile = pinned_host_profile();
        let home = tempfile::tempdir().unwrap();
        let lifecycle = tempfile::tempdir().unwrap();
        let config_path = home.path().join(".config/opencode/opencode.json");
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        std::fs::write(&config_path, OPENCODE_UNRELATED_CONFIG).unwrap();
        let component_set = canonical_host_component_set(
            "opencode",
            Some(crate::cli::HostBundleComponentArg::Core),
            0,
        )
        .unwrap()
        .unwrap();
        let options = crate::cli::HostBundleCliOptions {
            component: Some(crate::cli::HostBundleComponentArg::Core),
            dry_run: false,
            yes: true,
            adopt: false,
        };

        let error = apply_canonical_component_set(
            "opencode",
            HostBundleCliOperation::Install,
            &component_set,
            &options,
            home.path(),
            lifecycle.path(),
            &ComponentSetApplyContext::resolved(),
        )
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("a non-tracedecay LSP entry runs the tracedecay binary"),
            "unexpected error: {error}"
        );
        assert_eq!(
            std::fs::read(&config_path).unwrap(),
            OPENCODE_UNRELATED_CONFIG
        );
        for artifact in &component_set.component_set.components[0].manifest.artifacts {
            assert!(!home.path().join(&artifact.relative_path).exists());
        }
    }

    #[tokio::test]
    async fn kimi_tracked_reinstall_refuses_before_staging_without_native_activation() {
        let _profile = pinned_host_profile();
        let home = tempfile::tempdir().unwrap();
        let code_home = home.path().join(".kimi-code");
        let _kimi_home = EnvVarGuard::set(
            tracedecay_agent_hosts::agents::kimi::KIMI_CODE_HOME_ENV,
            &code_home,
        );
        let installed_path = code_home.join("plugins/installed.json");
        std::fs::create_dir_all(installed_path.parent().unwrap()).unwrap();
        let original = br#"{"version":1,"plugins":[{"id":"tracedecay","enabled":false}]}
"#;
        std::fs::write(&installed_path, original).unwrap();

        let results = reinstall_agent_integrations_with_persisted_dashboard_policies(
            &["kimi".to_string()],
            home.path(),
            "new-tracedecay",
        )
        .await;

        let [(id, Err(error))] = results.as_slice() else {
            panic!("tracked Kimi reinstall should return one typed refusal");
        };
        assert_eq!(id, "kimi");
        assert!(error.to_string().contains("/plugins install"));
        assert_eq!(std::fs::read(&installed_path).unwrap(), original);
        assert!(!code_home.join("plugins/managed/tracedecay").exists());
        assert!(
            home.path()
                .join(".tracedecay/host-bundle-stage/kimi/tracedecay/.kimi-plugin/plugin.json")
                .is_file()
        );
    }

    #[tokio::test]
    async fn kimi_native_activated_retry_tracks_staged_source() {
        use tracedecay_agent_hosts::agents::host_bundle::{
            HostComponentV1, HostKindV1, latest_host_component_receipt_at,
            resolved_host_bundle_lifecycle_root,
        };

        let _profile = pinned_host_profile();
        let home = tempfile::tempdir().unwrap();
        let code_home = home.path().join(".kimi-code");
        let _kimi_home = EnvVarGuard::set(
            tracedecay_agent_hosts::agents::kimi::KIMI_CODE_HOME_ENV,
            &code_home,
        );
        let tracedecay_bin = tracedecay_agent_hosts::agents::which_tracedecay()
            .unwrap_or_else(|| "tracedecay".to_string());
        let integration = tracedecay_agent_hosts::agents::get_integration("kimi").unwrap();
        let ctx = tracedecay_agent_hosts::agents::InstallContext {
            home: home.path().to_path_buf(),
            tracedecay_bin: tracedecay_bin.clone(),
            project_root: None,
            dashboard: true,
        };
        assert!(matches!(
            integration.prepare_non_interactive_install(&ctx).unwrap(),
            tracedecay_agent_hosts::agents::NonInteractiveInstallOutcome::DeferredUserAction(_)
        ));
        let staged = home
            .path()
            .join(".tracedecay/host-bundle-stage/kimi/tracedecay")
            .canonicalize()
            .unwrap();
        let managed = code_home.join("plugins/managed/tracedecay");
        copy_test_bundle(&staged, &managed);
        let installed_path = code_home.join("plugins/installed.json");
        std::fs::create_dir_all(installed_path.parent().unwrap()).unwrap();
        std::fs::write(
            &installed_path,
            serde_json::json!({
                "version": 1,
                "plugins": [{
                    "id": "tracedecay",
                    "enabled": true,
                    "source": "local-path",
                    "root": managed,
                    "originalSource": staged,
                }],
            })
            .to_string(),
        )
        .unwrap();

        let results = reinstall_agent_integrations_with_persisted_dashboard_policies(
            &["kimi".to_string()],
            home.path(),
            &tracedecay_bin,
        )
        .await;
        assert!(matches!(
            results.as_slice(),
            [(id, Ok(AgentReinstallOutcome::Installed))] if id == "kimi"
        ));
        let lifecycle_root = resolved_host_bundle_lifecycle_root().unwrap();
        assert!(
            latest_host_component_receipt_at(
                &lifecycle_root,
                HostKindV1::KimiCode,
                HostComponentV1::Core,
            )
            .unwrap()
            .is_some()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn codex_native_activated_retry_tracks_component_set() {
        use tracedecay_agent_hosts::agents::host_bundle::{
            HostComponentV1, HostKindV1, latest_host_component_receipt_at,
            resolved_host_bundle_lifecycle_root,
        };

        let _profile = pinned_host_profile();
        // The stale-cache leg below is exactly the leg that has to re-drive
        // `codex plugin add`, so the host CLI is a precondition of the
        // behaviour under test, not an ambient machine detail.
        let codex_cli_dir = tempfile::tempdir().unwrap();
        let _codex_path = install_fake_codex_cli(codex_cli_dir.path());
        let home = host_cli_tempdir();
        // Keep the lifecycle root on the same filesystem as `home`: receipt
        // transactions back up staged artifacts with an atomic rename.
        let data_dir = home.path().join(".tracedecay-data");
        let _data_dir_guard = EnvVarGuard::set(
            tracedecay_runtime_core::config::USER_DATA_DIR_ENV,
            &data_dir,
        );
        // The reinstall path renders the canonical Codex component set with
        // the PATH-resolved binary, and the host-native activation probe
        // compares the staged source byte-for-byte against that rendering.
        // Stage with the same identity or the probe reports a stale cache.
        let tracedecay_bin = tracedecay_agent_hosts::agents::which_tracedecay()
            .unwrap_or_else(|| "tracedecay".to_string());
        let integration = tracedecay_agent_hosts::agents::get_integration("codex").unwrap();
        let ctx = tracedecay_agent_hosts::agents::InstallContext {
            home: home.path().to_path_buf(),
            tracedecay_bin: tracedecay_bin.clone(),
            project_root: None,
            dashboard: true,
        };
        assert!(matches!(
            integration.prepare_non_interactive_install(&ctx).unwrap(),
            tracedecay_agent_hosts::agents::NonInteractiveInstallOutcome::Ready
        ));
        let config_path = home.path().join(".codex/config.toml");
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        std::fs::write(
            &config_path,
            "[plugins.\"tracedecay@personal\"]\nenabled = true\n",
        )
        .unwrap();
        let cache_root = home
            .path()
            .join(".codex/plugins/cache/personal/tracedecay")
            .join(tracedecay_agent_hosts::PRODUCT_VERSION);
        let cache_manifest = cache_root.join(".codex-plugin/plugin.json");
        std::fs::create_dir_all(&cache_root).unwrap();
        copy_test_bundle(&home.path().join(".codex/plugins/tracedecay"), &cache_root);

        let results = reinstall_agent_integrations_with_persisted_dashboard_policies(
            &["codex".to_string()],
            home.path(),
            &tracedecay_bin,
        )
        .await;
        assert!(
            matches!(
                results.as_slice(),
                [(id, Ok(AgentReinstallOutcome::Installed))] if id == "codex"
            ),
            "{results:?}"
        );
        let lifecycle_root = resolved_host_bundle_lifecycle_root().unwrap();
        assert!(
            latest_host_component_receipt_at(
                &lifecycle_root,
                HostKindV1::Codex,
                HostComponentV1::Core,
            )
            .unwrap()
            .is_some()
        );

        std::fs::write(
            &cache_manifest,
            br#"{"name":"tracedecay","version":"stale"}"#,
        )
        .unwrap();
        let stale = reinstall_agent_integrations_with_persisted_dashboard_policies(
            &["codex".to_string()],
            home.path(),
            &tracedecay_bin,
        )
        .await;
        assert!(
            matches!(
                stale.as_slice(),
                [(id, Ok(AgentReinstallOutcome::Installed))] if id == "codex"
            ),
            "{stale:?}"
        );
        std::fs::copy(
            home.path()
                .join(".codex/plugins/tracedecay/.codex-plugin/plugin.json"),
            &cache_manifest,
        )
        .unwrap();
        let recovered = reinstall_agent_integrations_with_persisted_dashboard_policies(
            &["codex".to_string()],
            home.path(),
            &tracedecay_bin,
        )
        .await;
        assert!(
            matches!(
                recovered.as_slice(),
                [(id, Ok(AgentReinstallOutcome::Installed))] if id == "codex"
            ),
            "{recovered:?}"
        );
    }

    #[tokio::test]
    async fn codex_native_removed_retry_cleans_receipt_owned_source() {
        let _profile = pinned_host_profile();
        let home = host_cli_tempdir();
        // Same filesystem as `home`: receipt rollback moves artifacts into
        // `lifecycle` and requires an atomic rename.
        let lifecycle = host_cli_tempdir();
        let tracedecay_bin = "new-tracedecay";
        let component_set =
            canonical_host_component_set_with_tracedecay_bin("codex", None, 0, tracedecay_bin)
                .unwrap()
                .unwrap();
        let source_manifest = home
            .path()
            .join(".codex/plugins/tracedecay/.codex-plugin/plugin.json");
        let marketplace = home.path().join(".agents/plugins/marketplace.json");
        std::fs::create_dir_all(marketplace.parent().unwrap()).unwrap();
        std::fs::write(
            &marketplace,
            serde_json::json!({
                "name": "personal",
                "plugins": [{
                    "name": "tracedecay",
                    "source": {
                        "source": "local",
                        "path": "./.codex/plugins/tracedecay",
                    }
                }]
            })
            .to_string(),
        )
        .unwrap();
        let config_path = home.path().join(".codex/config.toml");
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        std::fs::write(
            &config_path,
            "[plugins.\"tracedecay@personal\"]\nenabled = true\n",
        )
        .unwrap();
        let cache_root = home
            .path()
            .join(".codex/plugins/cache/personal/tracedecay")
            .join(tracedecay_agent_hosts::PRODUCT_VERSION);
        std::fs::create_dir_all(&cache_root).unwrap();
        std::fs::create_dir_all(source_manifest.parent().unwrap()).unwrap();
        for artifact in component_set
            .component_set
            .components
            .iter()
            .flat_map(|component| component.contents.iter())
            .filter_map(|artifact| {
                artifact
                    .relative_path
                    .strip_prefix(".codex/plugins/tracedecay/")
                    .map(|relative| (relative, &artifact.bytes))
            })
        {
            let cache_path = cache_root.join(artifact.0);
            std::fs::create_dir_all(cache_path.parent().unwrap()).unwrap();
            std::fs::write(cache_path, artifact.1).unwrap();
        }
        let options = crate::cli::HostBundleCliOptions {
            component: None,
            dry_run: false,
            yes: true,
            adopt: false,
        };
        apply_canonical_component_set(
            "codex",
            HostBundleCliOperation::Install,
            &component_set,
            &options,
            home.path(),
            lifecycle.path(),
            &ComponentSetApplyContext::with_tracedecay_bin(tracedecay_bin),
        )
        .unwrap();
        assert!(source_manifest.is_file());

        std::fs::remove_file(config_path).unwrap();
        std::fs::remove_dir_all(cache_root).unwrap();
        apply_canonical_component_set(
            "codex",
            HostBundleCliOperation::Uninstall,
            &component_set,
            &options,
            home.path(),
            lifecycle.path(),
            &ComponentSetApplyContext::with_tracedecay_bin(tracedecay_bin),
        )
        .unwrap();
        assert!(
            !source_manifest.exists(),
            "native removal must let the receipt transaction clean its staged source"
        );
    }

    #[tokio::test]
    async fn kimi_canonical_component_set_fails_before_direct_host_mutation() {
        let _profile = pinned_host_profile();
        let home = tempfile::tempdir().unwrap();
        let lifecycle = tempfile::tempdir().unwrap();
        let empty_path = tempfile::tempdir().unwrap();
        let code_home = home.path().join(".kimi-code");
        let _kimi_home = EnvVarGuard::set(
            tracedecay_agent_hosts::agents::kimi::KIMI_CODE_HOME_ENV,
            &code_home,
        );
        let _path =
            tracedecay_runtime_core::config::HostProgramSearchPathGuard::set(empty_path.path());
        let installed_path = code_home.join("plugins/installed.json");
        std::fs::create_dir_all(installed_path.parent().unwrap()).unwrap();
        std::fs::write(
            &installed_path,
            br#"{"version":1,"plugins":[{"id":"foreign","enabled":true}],"unrelated":"keep"}
"#,
        )
        .unwrap();
        let component_set = canonical_host_component_set("kimi", None, 0)
            .unwrap()
            .unwrap();
        let options = crate::cli::HostBundleCliOptions {
            component: None,
            dry_run: false,
            yes: true,
            adopt: false,
        };

        for operation in [
            HostBundleCliOperation::Install,
            HostBundleCliOperation::Update,
            HostBundleCliOperation::Repair,
        ] {
            let error = apply_canonical_component_set(
                "kimi",
                operation,
                &component_set,
                &options,
                home.path(),
                lifecycle.path(),
                &ComponentSetApplyContext::resolved(),
            )
            .unwrap_err()
            .to_string();
            let expected = match operation {
                HostBundleCliOperation::Install => "host capability is unsupported",
                HostBundleCliOperation::Update | HostBundleCliOperation::Repair => "cache is stale",
                HostBundleCliOperation::Uninstall => unreachable!("not exercised by this loop"),
            };
            assert!(error.contains(expected), "{operation:?}: {error}");
        }

        assert_eq!(
            std::fs::read(&installed_path).unwrap(),
            br#"{"version":1,"plugins":[{"id":"foreign","enabled":true}],"unrelated":"keep"}
"#
        );
        assert!(
            !code_home.join("plugins/managed/tracedecay").exists(),
            "preflight must fail before deploying managed plugin bytes"
        );
        for artifact in &component_set.component_set.components[0].manifest.artifacts {
            assert!(
                !home.path().join(&artifact.relative_path).exists(),
                "failed Kimi preflight must not create artifact {}",
                artifact.relative_path
            );
        }
    }

    #[tokio::test]
    async fn kimi_registration_preflight_creates_no_backup_for_unavailable_api() {
        use tracedecay_agent_hosts::agents::host_bundle::HostComponentSetRegistrationV1;

        let _profile = pinned_host_profile();
        let home = tempfile::tempdir().unwrap();
        let lifecycle = tempfile::tempdir().unwrap();
        let empty_path = tempfile::tempdir().unwrap();
        let code_home = home.path().join(".kimi-code");
        let _kimi_home = EnvVarGuard::set(
            tracedecay_agent_hosts::agents::kimi::KIMI_CODE_HOME_ENV,
            &code_home,
        );
        let _path =
            tracedecay_runtime_core::config::HostProgramSearchPathGuard::set(empty_path.path());
        let installed_path = code_home.join("plugins/installed.json");
        std::fs::create_dir_all(installed_path.parent().unwrap()).unwrap();
        let original =
            br#"{"version":1,"plugins":[{"id":"foreign","enabled":true}],"unrelated":"keep"}
"#;
        std::fs::write(&installed_path, original).unwrap();
        let component_set = canonical_host_component_set("kimi", None, 0)
            .unwrap()
            .unwrap();
        let request =
            component_set_request(&component_set, HostBundleCliOperation::Install, true, false)
                .unwrap();
        let mut registration = CatalogHostComponentRegistrationAuthority::new(
            "kimi",
            home.path(),
            request.lifecycle.operation,
        )
        .unwrap();
        assert_eq!(
            registration.preflight(&component_set.component_set, &request),
            Err(
                tracedecay_agent_hosts::agents::host_bundle::HostBundleError::UnsupportedCapability
            )
        );
        assert_eq!(std::fs::read(installed_path).unwrap(), original);
        assert!(
            !lifecycle
                .path()
                .join(".tracedecay-host-registration-v1")
                .exists()
        );
    }

    /// Kiro's supported route is its MCP registration alone. Core carries the
    /// degraded hook route and stays out of the canonical default set.
    #[test]
    fn kiro_canonical_component_set_refuses_degraded_hook_route() {
        let default_set = canonical_host_component_set("kiro", None, 0)
            .unwrap()
            .expect("Kiro's MCP registration is a supported first-party route");
        assert_eq!(
            default_set
                .component_set
                .components
                .iter()
                .map(|component| component.manifest.component)
                .collect::<Vec<_>>(),
            vec![tracedecay_agent_hosts::agents::host_bundle::HostComponentV1::ContextMcp]
        );
        assert!(
            canonical_host_component_set(
                "kiro",
                Some(crate::cli::HostBundleComponentArg::Core),
                0,
            )
            .is_err()
        );
    }

    #[test]
    fn stale_registration_stage_does_not_run_inverse_rollback_edit() {
        use tracedecay_agent_hosts::agents::host_bundle::{
            HostBundleError, HostComponentSetLifecyclePreviewV1, HostComponentSetRegistrationV1,
        };

        let _profile = pinned_host_profile();
        let home = tempfile::tempdir().unwrap();
        let component_set = canonical_host_component_set("opencode", None, 0)
            .unwrap()
            .unwrap();
        let request =
            component_set_request(&component_set, HostBundleCliOperation::Install, true, false)
                .unwrap();
        let mut registration = CatalogHostComponentRegistrationAuthority::new(
            "opencode",
            home.path(),
            request.lifecycle.operation,
        )
        .unwrap();
        registration
            .preflight(&component_set.component_set, &request)
            .unwrap();
        let revision = registration
            .current_revision(&component_set.component_set, &request)
            .unwrap();
        let preview = HostComponentSetLifecyclePreviewV1 {
            operation_id: request.operation_id,
            plan_digest: [7; 32],
            base_registration_revision: revision,
            current_registration_revision: revision,
            artifact_state_revision: [8; 32],
            component_plans: Vec::new(),
            competing_extension_claims: Vec::new(),
            confirmation_required: false,
        };
        registration
            .confirm_preview(&component_set.component_set, &request, &preview)
            .unwrap();

        let registration_path = home.path().join(".config/opencode/opencode.json");
        std::fs::create_dir_all(registration_path.parent().unwrap()).unwrap();
        std::fs::write(&registration_path, b"{\"external\":true}").unwrap();
        assert!(matches!(
            registration.stage(&component_set.component_set, &request),
            Err(HostBundleError::StalePreview(_))
        ));
        registration
            .rollback(&component_set.component_set, &request)
            .unwrap();
        assert_eq!(
            std::fs::read(registration_path).unwrap(),
            b"{\"external\":true}"
        );
    }

    #[test]
    fn hermes_receiptless_beta_plugin_is_adopted_and_refreshed() {
        let _profile = pinned_host_profile();
        let home = tempfile::tempdir().unwrap();
        let data_dir = home.path().join(".tracedecay-data");
        let _data_dir_guard = EnvVarGuard::set(
            tracedecay_runtime_core::config::USER_DATA_DIR_ENV,
            &data_dir,
        );
        std::fs::create_dir_all(home.path().join(".hermes/profiles/work")).unwrap();

        apply_default_canonical_component_set(
            "hermes",
            HostBundleCliOperation::Install,
            home.path(),
            true,
            false,
        )
        .unwrap();

        std::fs::remove_dir_all(data_dir.join("host-components")).unwrap();
        for manifest in [
            home.path().join(".hermes/plugins/tracedecay/plugin.yaml"),
            home.path()
                .join(".hermes/profiles/work/plugins/tracedecay/plugin.yaml"),
        ] {
            let contents = std::fs::read_to_string(&manifest).unwrap();
            std::fs::write(
                manifest,
                contents.replace(env!("CARGO_PKG_VERSION"), "0.1.0-beta.33"),
            )
            .unwrap();
        }

        apply_default_canonical_component_set(
            "hermes",
            HostBundleCliOperation::Install,
            home.path(),
            true,
            true,
        )
        .expect("a receiptless generated Hermes plugin must be adoptable");
    }
}
