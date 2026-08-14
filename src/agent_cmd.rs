use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use tracedecay::agents::host_component_registration::CatalogHostComponentRegistrationAuthority;
use tracedecay::user_config::UserConfig;

mod automation;
pub(crate) use automation::CodexAutomationInstall;
#[cfg(test)]
use automation::broker_codex_daemon_automation_project;
use automation::{
    install_codex_daemon_automation, validate_codex_automation_flags,
    validate_codex_automation_project_path,
};
mod feedback_component;
use feedback_component::{
    aggregate_with_feedback_component, companion_owned_live_paths, live_feedback_receipt,
    selected_feedback_component,
};

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

/// Stage only the host-native source required for an operator activation.
///
/// A ready host skips this path entirely and enters the catalog component
/// transaction without an out-of-band artifact write. A deferred host receives
/// its verified source and a truthful error, but no lifecycle receipt.
fn prepare_native_activation_if_needed(
    integration: &dyn tracedecay::agents::AgentIntegration,
    context: &tracedecay::agents::InstallContext,
) -> tracedecay::errors::Result<()> {
    if matches!(
        integration.preflight_non_interactive_install(context)?,
        tracedecay::agents::NonInteractiveInstallOutcome::Ready
    ) {
        return Ok(());
    }
    match integration.prepare_non_interactive_install(context)? {
        tracedecay::agents::NonInteractiveInstallOutcome::Ready => Ok(()),
        tracedecay::agents::NonInteractiveInstallOutcome::DeferredUserAction(deferred) => {
            Err(tracedecay::errors::TraceDecayError::Config {
                message: deferred.remediation,
            })
        }
    }
}

pub(crate) async fn handle_host_bundle_component_command(
    agent: Option<String>,
    operation: HostBundleCliOperation,
    options: crate::cli::HostBundleCliOptions,
) -> tracedecay::errors::Result<()> {
    if options.component.is_some() && !options.dry_run && !options.yes {
        return Err(tracedecay::errors::TraceDecayError::Config {
            message: "host component mutation requires --yes; use --dry-run first".to_string(),
        });
    }
    let home = tracedecay::agents::home_dir().ok_or_else(|| {
        tracedecay::errors::TraceDecayError::Config {
            message: "could not determine home directory".to_string(),
        }
    })?;
    let lifecycle_root = tracedecay::agents::host_bundle_v2::resolved_host_bundle_lifecycle_root()
        .map_err(|error| tracedecay::errors::TraceDecayError::Config {
            message: format!("could not resolve host lifecycle root: {error}"),
        })?;
    let mut user_config = tracedecay::user_config::UserConfig::load();
    let explicitly_scoped = agent.is_some();
    let agent_ids = match agent {
        Some(agent) => vec![agent],
        None if operation == HostBundleCliOperation::Install => {
            return Err(tracedecay::errors::TraceDecayError::Config {
                message: "component install requires --agent".to_string(),
            });
        }
        None => user_config.installed_agents.clone(),
    };
    if agent_ids.is_empty() {
        return Err(tracedecay::errors::TraceDecayError::Config {
            message: "no installed agents are tracked for component lifecycle".to_string(),
        });
    }
    let now_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| tracedecay::errors::TraceDecayError::Config {
            message: "system clock is before the Unix epoch".to_string(),
        })?
        .as_secs();
    for agent_id in &agent_ids {
        let component_set = canonical_host_component_set(agent_id, options.component, now_unix)?;
        let Some(component_set) = component_set else {
            if explicitly_scoped {
                return Err(tracedecay::errors::TraceDecayError::Config {
                    message: unsupported_host_component_set_message(agent_id),
                });
            }
            // A skipped host is a reported unavailable result, never a silent
            // success for the sweep that contained it.
            eprintln!(
                "skipped: {}",
                unsupported_host_component_set_message(agent_id)
            );
            continue;
        };
        if options.dry_run {
            dry_run_canonical_component_set(
                agent_id,
                operation,
                &component_set,
                &options,
                &home,
                &lifecycle_root,
            )?;
        } else {
            require_adoption_confirmation(
                agent_id,
                operation,
                &component_set,
                &options,
                &home,
                &lifecycle_root,
            )?;
            apply_canonical_component_set(
                agent_id,
                operation,
                &component_set,
                &options,
                &home,
                &lifecycle_root,
                &ComponentSetApplyContext::resolved(),
            )?;
        }
    }
    if operation == HostBundleCliOperation::Install && !options.dry_run {
        for agent_id in agent_ids {
            if !user_config.installed_agents.contains(&agent_id) {
                user_config.installed_agents.push(agent_id);
            }
        }
        user_config
            .save()
            .map_err(|error| tracedecay::errors::TraceDecayError::Config {
                message: format!("failed to save user config: {error}"),
            })?;
    }
    Ok(())
}

/// Refuse an explicit component mutation that would claim a file no receipt
/// records unless the operator confirmed adoption specifically.
///
/// `--yes` confirms the plan the preview showed; taking ownership of somebody
/// else's bytes is a separate decision, so it needs its own `--adopt`. This is
/// a confirmation gate only: the planner's adoption boundary is unchanged, a
/// file another owner claims is still refused with a typed ownership conflict,
/// and the automatic install/update/repair paths that migrate pre-receipt
/// deployments do not route through this command.
fn require_adoption_confirmation(
    agent_id: &str,
    operation: HostBundleCliOperation,
    component_set: &tracedecay::agents::host_bundle_registry::VerifiedEmbeddedHostComponentSetV1,
    options: &crate::cli::HostBundleCliOptions,
    home: &Path,
    lifecycle_root: &Path,
) -> tracedecay::errors::Result<()> {
    if options.adopt {
        return Ok(());
    }
    let preview = preview_canonical_component_set(
        agent_id,
        operation,
        component_set,
        options,
        home,
        lifecycle_root,
        None,
    )?;
    let adopted =
        adopted_relative_paths(&preview, lifecycle_root, component_set.component_set.host);
    if adopted.is_empty() {
        return Ok(());
    }
    Err(tracedecay::errors::TraceDecayError::Config {
        message: format!(
            "agent {agent_id:?} would take ownership of {} file(s) that no TraceDecay receipt \
             records ({}); the previous bytes are backed up under {}/<operation-id>, but claiming \
             them is a separate decision — review `--dry-run` and re-run with `--yes --adopt`",
            adopted.len(),
            adopted.join(", "),
            tracedecay::agents::host_bundle_v2::host_bundle_backup_root(lifecycle_root).display()
        ),
    })
}

/// Truthful reason a host component set is unavailable, so a skipped or
/// refused agent never reads as an empty success.
fn unsupported_host_component_set_message(agent: &str) -> String {
    match host_kind_for_agent(agent)
        .ok()
        .and_then(tracedecay::agents::host_bundle_registry::unsupported_host_component_set_reason)
    {
        Some(reason) => {
            format!("agent {agent:?} has no installable first-party host component set: {reason:?}")
        }
        None => format!("agent {agent:?} has no canonical first-party host component set"),
    }
}

fn canonical_host_component_set(
    agent: &str,
    component: Option<crate::cli::HostBundleComponentArg>,
    now_unix: u64,
) -> tracedecay::errors::Result<
    Option<tracedecay::agents::host_bundle_registry::VerifiedEmbeddedHostComponentSetV1>,
> {
    let tracedecay_bin =
        tracedecay::agents::which_tracedecay().unwrap_or_else(|| "tracedecay".to_string());
    canonical_host_component_set_with_tracedecay_bin(agent, component, now_unix, &tracedecay_bin)
}

fn canonical_host_component_set_with_tracedecay_bin(
    agent: &str,
    component: Option<crate::cli::HostBundleComponentArg>,
    now_unix: u64,
    tracedecay_bin: &str,
) -> tracedecay::errors::Result<
    Option<tracedecay::agents::host_bundle_registry::VerifiedEmbeddedHostComponentSetV1>,
> {
    let host = match host_kind_for_agent(agent) {
        Ok(host) => host,
        Err(_) => return Ok(None),
    };
    let requested = component.map(host_bundle_component).map_or_else(
        || tracedecay::agents::host_bundle_registry::default_components(host),
        |component| vec![component],
    );
    if requested.is_empty() {
        return Err(tracedecay::errors::TraceDecayError::Config {
            message: unsupported_host_component_set_message(agent),
        });
    }
    tracedecay::agents::host_bundle_registry::verified_embedded_host_component_set_with_tracedecay_bin(
        host,
        &requested,
        now_unix,
        tracedecay_bin,
    )
    .map(Some)
    .map_err(|error| tracedecay::errors::TraceDecayError::Config {
        message: format!("first-party {agent:?} component set is unavailable: {error}"),
    })
}

fn ensure_artifact_only_restore_boundary(
    agent_id: &str,
    component_set: &tracedecay::agents::host_bundle_registry::VerifiedEmbeddedHostComponentSetV1,
    home: &Path,
    lifecycle_root: &Path,
) -> tracedecay::errors::Result<()> {
    let registration = CatalogHostComponentRegistrationAuthority::new(
        agent_id,
        home,
        lifecycle_root,
        tracedecay::agents::host_bundle_v2::HostBundleLifecycleOpV1::Repair,
    )?;
    if registration.supports_artifact_only_backup_restore(&component_set.component_set) {
        return Ok(());
    }
    Err(tracedecay::errors::TraceDecayError::Config {
        message: format!(
            "artifact backup/restore for agent {agent_id:?} is unavailable because this component \
             uses host registration state; this command does not manage registration state"
        ),
    })
}

fn apply_host_bundle_artifact_action_at(
    action: crate::cli::HostBundleAction,
    options: crate::cli::HostBundleCliOptions,
    home: &Path,
    lifecycle_root: &Path,
    now_unix: u64,
) -> tracedecay::errors::Result<[u8; 16]> {
    if options.dry_run {
        return Err(tracedecay::errors::TraceDecayError::Config {
            message: "artifact backup/restore has no dry-run mode".to_string(),
        });
    }
    // A backup only ever writes a new snapshot; nothing deployed changes, so
    // it needs no confirmation. A restore overwrites deployed bytes and keeps
    // requiring `--yes`.
    if matches!(action, crate::cli::HostBundleAction::ArtifactRestore { .. }) && !options.yes {
        return Err(tracedecay::errors::TraceDecayError::Config {
            message: "artifact restore requires --yes".to_string(),
        });
    }
    let component =
        options
            .component
            .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
                message: "artifact backup/restore requires --component".to_string(),
            })?;
    let (agent_id, backup_operation_id) = match &action {
        crate::cli::HostBundleAction::ArtifactBackup { agent } => (agent.as_str(), None),
        crate::cli::HostBundleAction::ArtifactRestore { agent, backup_id } => {
            let mut decoded = [0_u8; 16];
            hex::decode_to_slice(backup_id, &mut decoded).map_err(|_| {
                tracedecay::errors::TraceDecayError::Config {
                    message:
                        "artifact restore --backup-id must be 32 lowercase hexadecimal characters"
                            .to_string(),
                }
            })?;
            if hex::encode(decoded) != *backup_id {
                return Err(tracedecay::errors::TraceDecayError::Config {
                    message:
                        "artifact restore --backup-id must be 32 lowercase hexadecimal characters"
                            .to_string(),
                });
            }
            (agent.as_str(), Some(decoded))
        }
        crate::cli::HostBundleAction::Status | crate::cli::HostBundleAction::Recover { .. } => {
            return Err(tracedecay::errors::TraceDecayError::Config {
                message: "status and recovery are not artifact backup/restore operations"
                    .to_string(),
            });
        }
    };
    let component_set = canonical_host_component_set(agent_id, Some(component), now_unix)?
        .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
            message: unsupported_host_component_set_message(agent_id),
        })?;
    ensure_artifact_only_restore_boundary(agent_id, &component_set, home, lifecycle_root)?;
    let [entry] = component_set.component_set.components.as_slice() else {
        return Err(tracedecay::errors::TraceDecayError::Config {
            message: "artifact backup/restore requires exactly one canonical component".to_string(),
        });
    };
    let operation_id = tracedecay::request_identity::mint_global_operation_id(
        tracedecay::request_identity::GlobalOperationIdentityKind::HostArtifact,
    )
    .map_err(|error| tracedecay::errors::TraceDecayError::Config {
        message: format!("could not generate host artifact operation id: {error}"),
    })?;
    let mut writer =
        tracedecay::agents::host_bundle_v2::HostBundleWriterV1::open_with_lifecycle_root(
            home,
            lifecycle_root,
        )
        .map_err(host_bundle_error)?;
    match backup_operation_id {
        None => {
            writer
                .backup_component(&entry.manifest, operation_id, options.yes, &component_set)
                .map_err(host_bundle_error)?;
        }
        Some(backup_operation_id) => {
            writer
                .restore_component_backup(
                    backup_operation_id,
                    operation_id,
                    options.yes,
                    &component_set,
                )
                .map_err(host_bundle_error)?;
        }
    }
    Ok(operation_id)
}

pub(crate) async fn handle_host_bundle_artifact_command(
    action: crate::cli::HostBundleAction,
    options: crate::cli::HostBundleCliOptions,
) -> tracedecay::errors::Result<()> {
    let home = tracedecay::agents::home_dir().ok_or_else(|| {
        tracedecay::errors::TraceDecayError::Config {
            message: "could not determine home directory".to_string(),
        }
    })?;
    let lifecycle_root = tracedecay::agents::host_bundle_v2::resolved_host_bundle_lifecycle_root()
        .map_err(|error| tracedecay::errors::TraceDecayError::Config {
            message: format!("could not resolve host lifecycle root: {error}"),
        })?;
    let now_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| tracedecay::errors::TraceDecayError::Config {
            message: "system clock is before the Unix epoch".to_string(),
        })?
        .as_secs();
    let is_restore = matches!(
        &action,
        crate::cli::HostBundleAction::ArtifactRestore { .. }
    );
    let operation_id =
        apply_host_bundle_artifact_action_at(action, options, &home, &lifecycle_root, now_unix)?;
    if is_restore {
        eprintln!(
            "\x1b[32m✔\x1b[0m managed artifact files restored; host registration was not changed; receipt {}",
            hex::encode(operation_id)
        );
    } else {
        eprintln!(
            "\x1b[32m✔\x1b[0m managed artifact files backed up; host registration was not captured; backup id {}",
            hex::encode(operation_id)
        );
    }
    Ok(())
}

fn lifecycle_operation(
    operation: HostBundleCliOperation,
) -> tracedecay::agents::host_bundle_v2::HostBundleLifecycleOpV1 {
    match operation {
        HostBundleCliOperation::Install => {
            tracedecay::agents::host_bundle_v2::HostBundleLifecycleOpV1::Install
        }
        HostBundleCliOperation::Update => {
            tracedecay::agents::host_bundle_v2::HostBundleLifecycleOpV1::Update
        }
        HostBundleCliOperation::Repair => {
            tracedecay::agents::host_bundle_v2::HostBundleLifecycleOpV1::Repair
        }
        HostBundleCliOperation::Uninstall => {
            tracedecay::agents::host_bundle_v2::HostBundleLifecycleOpV1::Uninstall
        }
    }
}

fn component_set_request(
    component_set: &tracedecay::agents::host_bundle_registry::VerifiedEmbeddedHostComponentSetV1,
    operation: HostBundleCliOperation,
    explicit_confirmation: bool,
) -> tracedecay::errors::Result<
    tracedecay::agents::host_bundle_v2::HostComponentSetExecutionRequestV1,
> {
    let operation_id = tracedecay::request_identity::mint_global_operation_id(
        tracedecay::request_identity::GlobalOperationIdentityKind::HostComponentSet,
    )
    .map_err(|error| tracedecay::errors::TraceDecayError::Config {
        message: format!("could not generate host lifecycle operation id: {error}"),
    })?;
    let host = component_set.component_set.host;
    Ok(
        tracedecay::agents::host_bundle_v2::HostComponentSetExecutionRequestV1 {
            lifecycle: tracedecay::agents::host_bundle_v2::HostComponentSetLifecycleRequestV1 {
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
                    host == tracedecay::agents::host_bundle_v2::HostKindV1::Hermes,
                ),
            },
            operation_id,
        },
    )
}

fn dry_run_canonical_component_set(
    agent_id: &str,
    operation: HostBundleCliOperation,
    component_set: &tracedecay::agents::host_bundle_registry::VerifiedEmbeddedHostComponentSetV1,
    options: &crate::cli::HostBundleCliOptions,
    home: &Path,
    lifecycle_root: &Path,
) -> tracedecay::errors::Result<()> {
    let preview = preview_canonical_component_set(
        agent_id,
        operation,
        component_set,
        options,
        home,
        lifecycle_root,
        None,
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
    let backup_root = tracedecay::agents::host_bundle_v2::host_bundle_backup_root(lifecycle_root);
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
        if plan.rollback_required {
            eprintln!("    backups: {}/<operation-id>", backup_root.display());
        }
    }
    Ok(())
}

/// Deploy paths the durable receipt for this component already claims. A path
/// missing from this set is one no receipt records, so replacing an existing
/// file there is an adoption rather than an ordinary refresh.
fn receipt_owned_paths(
    lifecycle_root: &Path,
    host: tracedecay::agents::host_bundle_v2::HostKindV1,
    component: tracedecay::agents::host_bundle_v2::HostBundleComponentV1,
) -> std::collections::BTreeSet<String> {
    tracedecay::agents::host_bundle_v2::latest_host_component_receipt_at(
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
    action: &tracedecay::agents::host_bundle_v2::HostArtifactActionV1,
    receipt_owned: &std::collections::BTreeSet<String>,
    relative_path: &str,
) -> &'static str {
    use tracedecay::agents::host_bundle_v2::HostArtifactActionV1 as Action;

    match action {
        Action::Noop => "unchanged",
        Action::WriteNew => "write-new",
        Action::BackupThenRemove => "backup-then-remove",
        Action::BackupThenReplace if receipt_owned.contains(relative_path) => "backup-then-replace",
        Action::BackupThenReplace => "adopt",
    }
}

/// Mutations that would claim an existing file no receipt records.
fn adopted_relative_paths(
    preview: &tracedecay::agents::host_bundle_v2::HostComponentSetLifecyclePreviewV1,
    lifecycle_root: &Path,
    host: tracedecay::agents::host_bundle_v2::HostKindV1,
) -> Vec<String> {
    use tracedecay::agents::host_bundle_v2::HostArtifactActionV1 as Action;

    let mut adopted = Vec::new();
    for plan in &preview.component_plans {
        let owned = receipt_owned_paths(lifecycle_root, host, plan.component);
        for mutation in &plan.mutations {
            if mutation.action == Action::BackupThenReplace
                && !owned.contains(&mutation.relative_path)
            {
                adopted.push(mutation.relative_path.clone());
            }
        }
    }
    adopted.sort();
    adopted.dedup();
    adopted
}

fn preview_canonical_component_set(
    agent_id: &str,
    operation: HostBundleCliOperation,
    component_set: &tracedecay::agents::host_bundle_registry::VerifiedEmbeddedHostComponentSetV1,
    options: &crate::cli::HostBundleCliOptions,
    home: &Path,
    lifecycle_root: &Path,
    install_context: Option<&tracedecay::agents::InstallContext>,
) -> tracedecay::errors::Result<
    tracedecay::agents::host_bundle_v2::HostComponentSetLifecyclePreviewV1,
> {
    let request = component_set_request(component_set, operation, options.yes)?;
    let mut registration = match install_context {
        Some(install) => {
            CatalogHostComponentRegistrationAuthority::new_with_tracedecay_bin_and_dashboard(
                agent_id,
                home,
                lifecycle_root,
                request.lifecycle.operation,
                install.tracedecay_bin.clone(),
                install.dashboard,
            )?
        }
        None => CatalogHostComponentRegistrationAuthority::new(
            agent_id,
            home,
            lifecycle_root,
            request.lifecycle.operation,
        )?,
    };
    tracedecay::agents::host_bundle_v2::dry_run_host_component_set_lifecycle_with_lifecycle_root_at(
        home,
        lifecycle_root,
        &component_set.component_set,
        &request,
        component_set,
        &mut registration,
    )
    .map_err(|error| host_bundle_error_for_agent(agent_id, error))
}

/// Recover this host's outstanding component-set journal before a confirmed
/// apply mutates anything.
///
/// `HostComponentSetTransactionV1::execute` already recovers first, but the
/// preview/`execute_confirmed` pair used by every non-interactive refresh did
/// not: a completed rollback intentionally leaves its journal behind as an
/// explicit reconciliation boundary, and the very next preview then refuses
/// with `RecoveryRequired`. Without this, a single failed apply wedges the
/// refresh loop until an operator runs `tracedecay host-bundle recover` by
/// hand. Recovery is bound to the *journal's* own operation, exactly like the
/// recover command, because the registration backup only validates against the
/// operation that wrote it.
fn recover_pending_component_set_journal(
    agent_id: &str,
    host: tracedecay::agents::host_bundle_v2::HostKindV1,
    writer: &mut tracedecay::agents::host_bundle_v2::HostBundleWriterV1,
    build_registration: impl FnOnce(
        tracedecay::agents::host_bundle_v2::HostBundleLifecycleOpV1,
    ) -> tracedecay::errors::Result<
        CatalogHostComponentRegistrationAuthority,
    >,
) -> tracedecay::errors::Result<()> {
    let Some(operation) = writer
        .pending_component_set_journal_operation(host)
        .map_err(host_bundle_error)?
    else {
        return Ok(());
    };
    let mut registration = build_registration(operation)?;
    tracedecay::agents::host_bundle_v2::HostComponentSetTransactionV1::new(writer)
        .recover_host(host, &mut registration)
        .map_err(|error| host_bundle_error_for_agent(agent_id, error))
}

/// How one component-set apply reaches the outside world: which `tracedecay`
/// binary the written registrations invoke, and whether the dashboard component
/// is registered alongside them.
///
/// Both knobs were previously spelled as a telescoping chain of forwarding
/// overloads (`_with_dashboard`, `_with_tracedecay_bin`,
/// `_with_tracedecay_bin_and_dashboard`), so every caller had to know which rung
/// defaulted which knob. Naming them once keeps
/// [`apply_canonical_component_set`] a single entry point whose defaults are
/// chosen by the constructor the caller names.
#[derive(Clone, Debug)]
struct ComponentSetApplyContext {
    tracedecay_bin: String,
    dashboard: bool,
}

impl ComponentSetApplyContext {
    /// The production context: the resolved installed binary, dashboard on.
    fn resolved() -> Self {
        Self::resolved_with_dashboard(true)
    }

    /// The production binary with the dashboard registration decided by the
    /// caller, which is what the lifecycle commands pass through from
    /// `--no-dashboard` and the per-agent dashboard policy.
    fn resolved_with_dashboard(dashboard: bool) -> Self {
        Self {
            tracedecay_bin: tracedecay::agents::which_tracedecay()
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

fn apply_canonical_component_set(
    agent_id: &str,
    operation: HostBundleCliOperation,
    component_set: &tracedecay::agents::host_bundle_registry::VerifiedEmbeddedHostComponentSetV1,
    options: &crate::cli::HostBundleCliOptions,
    home: &Path,
    lifecycle_root: &Path,
    context: &ComponentSetApplyContext,
) -> tracedecay::errors::Result<()> {
    let ComponentSetApplyContext {
        tracedecay_bin,
        dashboard,
    } = context;
    let dashboard = *dashboard;
    let request = component_set_request(
        component_set,
        operation,
        options.component.is_none() || options.yes,
    )?;
    let mut writer =
        tracedecay::agents::host_bundle_v2::HostBundleWriterV1::open_with_lifecycle_root(
            home,
            lifecycle_root,
        )
        .map_err(|error| host_bundle_error_for_agent(agent_id, error))?;
    recover_pending_component_set_journal(
        agent_id,
        component_set.component_set.host,
        &mut writer,
        |operation| {
            CatalogHostComponentRegistrationAuthority::new_with_tracedecay_bin_and_dashboard(
                agent_id,
                home,
                lifecycle_root,
                operation,
                tracedecay_bin.to_string(),
                dashboard,
            )
        },
    )?;
    let mut transaction =
        tracedecay::agents::host_bundle_v2::HostComponentSetTransactionV1::new(&mut writer);
    let mut registration =
        CatalogHostComponentRegistrationAuthority::new_with_tracedecay_bin_and_dashboard(
            agent_id,
            home,
            lifecycle_root,
            request.lifecycle.operation,
            tracedecay_bin.to_string(),
            dashboard,
        )?;
    // Recover this host's own outstanding journal before previewing, exactly as
    // `HostComponentSetTransactionV1::execute` does. Without this the residue of
    // any earlier failure — including one that has since been fixed — makes
    // every later run refuse with `RecoveryRequired` until somebody runs
    // `host-bundle recover` by hand, so a transient fault becomes permanent.
    transaction
        .recover_host(component_set.component_set.host, &mut registration)
        .map_err(|error| host_bundle_error_for_agent(agent_id, error))?;
    let preview = transaction
        .preview(
            &component_set.component_set,
            &request,
            component_set,
            &mut registration,
        )
        .map_err(|error| host_bundle_error_for_agent(agent_id, error))?;
    // A full default install is otherwise treated as confirmed. A competing
    // third-party claim is exactly the ambiguity that must not be resolved on
    // the operator's behalf, so it demands an explicit `--yes`.
    if !preview.competing_extension_claims.is_empty() && !options.yes {
        return Err(tracedecay::errors::TraceDecayError::Config {
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
    Ok(())
}

/// Apply the agent's default component set. `dashboard` decides whether the
/// dashboard component is registered with it; uninstall paths pass `true`
/// because removal must cover everything an install could have written.
fn apply_default_canonical_component_set(
    agent_id: &str,
    operation: HostBundleCliOperation,
    home: &Path,
    dashboard: bool,
) -> tracedecay::errors::Result<()> {
    let now_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| tracedecay::errors::TraceDecayError::Config {
            message: "system clock is before the Unix epoch".to_string(),
        })?
        .as_secs();
    let component_set =
        canonical_host_component_set(agent_id, None, now_unix)?.ok_or_else(|| {
            tracedecay::errors::TraceDecayError::Config {
                message: unsupported_host_component_set_message(agent_id),
            }
        })?;
    let lifecycle_root = tracedecay::agents::host_bundle_v2::resolved_host_bundle_lifecycle_root()
        .map_err(|error| tracedecay::errors::TraceDecayError::Config {
            message: format!("could not resolve host lifecycle root: {error}"),
        })?;
    apply_canonical_component_set(
        agent_id,
        operation,
        &component_set,
        &crate::cli::HostBundleCliOptions {
            component: None,
            dry_run: false,
            yes: true,
            adopt: false,
        },
        home,
        &lifecycle_root,
        &ComponentSetApplyContext::resolved_with_dashboard(dashboard),
    )?;
    Ok(())
}

fn load_host_lifecycle_user_config() -> tracedecay::errors::Result<UserConfig> {
    UserConfig::load_strict().map_err(|error| tracedecay::errors::TraceDecayError::Config {
        message: format!("failed to load host lifecycle policy: {error}"),
    })
}

pub(crate) async fn handle_project_local_lifecycle_command(
    _agent_id: String,
    _operation: HostBundleCliOperation,
) -> tracedecay::errors::Result<()> {
    Err(project_local_host_lifecycle_unavailable())
}

fn project_local_host_lifecycle_unavailable() -> tracedecay::errors::TraceDecayError {
    tracedecay::errors::TraceDecayError::Config {
        message: "project-local host lifecycle is unavailable; install the canonical user-level host component set instead"
            .to_string(),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum FeedbackRollbackCliStatus {
    Prepared,
    Applied,
    Restored,
}

const FEEDBACK_ROLLBACK_STATE_SCHEMA_VERSION: u16 = 6;
const MIN_FEEDBACK_ROLLBACK_STATE_SCHEMA_VERSION: u16 = 6;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct FeedbackRollbackCliState {
    schema_version: u16,
    agent_id: String,
    host: tracedecay::agents::host_bundle_v2::HostKindV1,
    status: FeedbackRollbackCliStatus,
    previous_aggregate: tracedecay::agents::host_bundle_v2::HostComponentSetReceiptV1,
    previous_manifest: tracedecay::agents::host_bundle_v2::HostBundleManifestV1,
    previous_contents: Vec<tracedecay::agents::host_bundle_v2::HostBundleArtifactContentV1>,
    target_manifest: tracedecay::agents::host_bundle_v2::HostBundleManifestV1,
    dashboard_enabled: bool,
    switch_operation_id: [u8; 16],
    effect_started: bool,
    registration_effect_started: bool,
    registration_intent_root: PathBuf,
    compensation_preserves_registration: bool,
    switch_receipt: Option<tracedecay::agents::host_bundle_v2::FeedbackPathRollbackReceiptV1>,
    restore_operation_id: Option<[u8; 16]>,
    restore_effect_started: bool,
    restore_receipt: Option<tracedecay::agents::host_bundle_v2::FeedbackPathRestoreReceiptV1>,
    identity: FeedbackRollbackIdentityV2,
    registration_files: Vec<FeedbackRegistrationFileState>,
    artifact_permissions: Vec<FeedbackArtifactPermissionStateV4>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct FeedbackRollbackIdentityV2 {
    canonical_home: PathBuf,
    canonical_lifecycle_root: PathBuf,
    canonical_project: PathBuf,
    integration_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct FeedbackRegistrationFileState {
    path_index: usize,
    path_digest: [u8; 32],
    contents: Option<Vec<u8>>,
    permissions: Option<FeedbackFilePermissionsV2>,
    #[serde(default)]
    metadata: Option<tracedecay::agents::HostFileMetadataIdentityV1>,
    applied_state: Option<FeedbackFileObservedStateV2>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct FeedbackFileObservedStateV2 {
    present: bool,
    digest: [u8; 32],
    #[serde(default)]
    metadata: Option<tracedecay::agents::HostFileMetadataIdentityV1>,
}

#[derive(Deserialize)]
struct FeedbackHostConfigWriteIntentV2 {
    schema_version: u16,
    digest: [u8; 32],
    metadata: Option<tracedecay::agents::HostFileMetadataIdentityV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct FeedbackFilePermissionsV2 {
    readonly: bool,
    unix_mode: Option<u32>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct FeedbackArtifactPermissionStateV4 {
    relative_path: String,
    permissions: FeedbackFilePermissionsV2,
}

impl FeedbackRollbackIdentityV2 {
    fn current(
        integration_id: &str,
        home: &Path,
        lifecycle_root: &Path,
    ) -> tracedecay::errors::Result<Self> {
        let project = std::env::current_dir().map_err(|error| {
            tracedecay::errors::TraceDecayError::Config {
                message: format!("could not determine feedback rollback project: {error}"),
            }
        })?;
        Ok(Self {
            canonical_home: canonical_feedback_path("home", home)?,
            canonical_lifecycle_root: canonical_feedback_path("lifecycle profile", lifecycle_root)?,
            canonical_project: canonical_feedback_path("project", &project)?,
            integration_id: integration_id.to_string(),
        })
    }

    fn validate(
        &self,
        integration_id: &str,
        home: &Path,
        lifecycle_root: &Path,
    ) -> tracedecay::errors::Result<()> {
        let current = Self::current(integration_id, home, lifecycle_root)?;
        (self == &current)
            .then_some(())
            .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
                message:
                    "feedback rollback state belongs to a different home, profile, project, or integration"
                        .to_string(),
            })
    }
}

fn canonical_feedback_path(label: &str, path: &Path) -> tracedecay::errors::Result<PathBuf> {
    fs::canonicalize(path).map_err(|error| tracedecay::errors::TraceDecayError::Config {
        message: format!(
            "could not canonicalize feedback rollback {label} {}: {error}",
            path.display()
        ),
    })
}

#[derive(Clone)]
struct FeedbackPairVerifier {
    digests: [[u8; 32]; 2],
}

impl tracedecay::agents::host_bundle_v2::HostBundleVerificationAdapterV1 for FeedbackPairVerifier {
    fn verify_manifest(
        &self,
        manifest: &tracedecay::agents::host_bundle_v2::HostBundleManifestV1,
    ) -> Result<(), tracedecay::agents::host_bundle_v2::HostBundleError> {
        manifest.validate_structure()?;
        self.digests
            .contains(&manifest.canonical_digest()?)
            .then_some(())
            .ok_or(tracedecay::agents::host_bundle_v2::HostBundleError::CatalogMismatch)
    }
}

struct FeedbackPreviewStorage;

impl tracedecay::agents::host_bundle_v2::HostBundleLifecycleStorageV1 for FeedbackPreviewStorage {
    fn recover_lifecycle(
        &mut self,
    ) -> Result<(), tracedecay::agents::host_bundle_v2::HostBundleError> {
        Ok(())
    }

    fn execute_lifecycle<V: tracedecay::agents::host_bundle_v2::HostBundleVerificationAdapterV1>(
        &mut self,
        _manifest: &tracedecay::agents::host_bundle_v2::HostBundleManifestV1,
        _request: &tracedecay::agents::host_bundle_v2::HostBundleExecutionRequestV1,
        _contents: &[tracedecay::agents::host_bundle_v2::HostBundleArtifactContentV1],
        _verifier: &V,
    ) -> Result<
        tracedecay::agents::host_bundle_v2::HostBundleInstallReceiptV1,
        tracedecay::agents::host_bundle_v2::HostBundleError,
    > {
        Err(tracedecay_host_integration::host_bundle_storage_failure!())
    }
}

pub(crate) async fn handle_feedback_rollback_command(
    action: crate::cli::FeedbackRollbackAction,
) -> tracedecay::errors::Result<()> {
    match action {
        crate::cli::FeedbackRollbackAction::DryRun { agent } => feedback_rollback_dry_run(&agent),
        crate::cli::FeedbackRollbackAction::Apply { agent, state, yes } => {
            if !yes {
                return Err(tracedecay::errors::TraceDecayError::Config {
                    message: "feedback rollback apply requires --yes".to_string(),
                });
            }
            feedback_rollback_apply(&agent, Path::new(&state))
        }
        crate::cli::FeedbackRollbackAction::Restore { state, yes } => {
            if !yes {
                return Err(tracedecay::errors::TraceDecayError::Config {
                    message: "feedback rollback restore requires --yes".to_string(),
                });
            }
            feedback_rollback_restore(Path::new(&state))
        }
    }
}

fn feedback_rollback_inputs(
    agent_id: &str,
) -> tracedecay::errors::Result<(
    PathBuf,
    PathBuf,
    tracedecay::agents::host_bundle_v2::HostComponentSetReceiptV1,
    tracedecay::agents::host_bundle_registry::VerifiedEmbeddedHostBundleV1,
)> {
    let home = tracedecay::agents::home_dir().ok_or_else(|| {
        tracedecay::errors::TraceDecayError::Config {
            message: "could not determine home directory".to_string(),
        }
    })?;
    let lifecycle_root = tracedecay::agents::host_bundle_v2::resolved_host_bundle_lifecycle_root()
        .map_err(|error| tracedecay::errors::TraceDecayError::Config {
            message: format!("could not resolve host lifecycle root: {error}"),
        })?;
    let host = host_kind_for_agent(agent_id)?;
    let previous = tracedecay::agents::host_bundle_v2::latest_host_component_set_receipt_at(
        &lifecycle_root,
        host,
    )
    .map_err(host_bundle_error)?
    .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
        message: format!("no aggregate host receipt exists for {agent_id}"),
    })?;
    let component = selected_feedback_component(&previous)?;
    let mut target =
        tracedecay::agents::host_bundle_registry::verified_embedded_host_bundle(host, component, 0)
            .map_err(|error| tracedecay::errors::TraceDecayError::Config {
                message: format!("compiled feedback route is unavailable for {agent_id}: {error}"),
            })?;
    let companion_owned_paths = companion_owned_live_paths(&home, &previous)?;
    target
        .manifest
        .artifacts
        .retain(|artifact| !companion_owned_paths.contains(&artifact.relative_path));
    target.contents.retain(|content| {
        target
            .manifest
            .artifacts
            .iter()
            .any(|artifact| artifact.relative_path == content.relative_path)
    });
    #[cfg(feature = "test-transport")]
    if let Some(revision) = std::env::var_os("TRACEDECAY_TEST_FEEDBACK_ROUTE_REVISION") {
        let revision = revision.to_string_lossy();
        target.manifest.configuration_snapshot_id = revision.clone().into_owned();
        let content = target.contents.first_mut().ok_or_else(|| {
            tracedecay::errors::TraceDecayError::Config {
                message: "compiled feedback route has no artifact bytes".to_string(),
            }
        })?;
        content
            .bytes
            .extend_from_slice(format!("\nfeedback-route:{revision}\n").as_bytes());
        let artifact = target
            .manifest
            .artifacts
            .iter_mut()
            .find(|artifact| artifact.relative_path == content.relative_path)
            .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
                message: "compiled feedback route content has no manifest artifact".to_string(),
            })?;
        artifact.artifact_digest = Sha256::digest(&content.bytes).into();
    }
    Ok((home, lifecycle_root, previous, target))
}

fn feedback_pair_verifier(
    previous: &tracedecay::agents::host_bundle_v2::HostBundleManifestV1,
    target: &tracedecay::agents::host_bundle_v2::HostBundleManifestV1,
) -> tracedecay::errors::Result<FeedbackPairVerifier> {
    Ok(FeedbackPairVerifier {
        digests: [
            previous.canonical_digest().map_err(host_bundle_error)?,
            target.canonical_digest().map_err(host_bundle_error)?,
        ],
    })
}

fn feedback_request(
    manifest: &tracedecay::agents::host_bundle_v2::HostBundleManifestV1,
    operation: tracedecay::agents::host_bundle_v2::HostBundleLifecycleOpV1,
    confirmed: bool,
) -> tracedecay::errors::Result<tracedecay::agents::host_bundle_v2::HostBundleExecutionRequestV1> {
    let operation_id = tracedecay::request_identity::mint_global_operation_id(
        tracedecay::request_identity::GlobalOperationIdentityKind::HostFeedbackRollback,
    )
    .map_err(|error| tracedecay::errors::TraceDecayError::Config {
        message: format!("could not generate feedback rollback operation id: {error}"),
    })?;
    Ok(
        tracedecay::agents::host_bundle_v2::HostBundleExecutionRequestV1 {
            lifecycle: tracedecay::agents::host_bundle_v2::HostBundleLifecycleRequestV1 {
                operation,
                expected_host: manifest.host,
                expected_component: manifest.component,
                explicit_confirmation: confirmed,
                hermes_profile_bindings: u8::from(
                    manifest.host == tracedecay::agents::host_bundle_v2::HostKindV1::Hermes,
                ),
            },
            operation_id,
        },
    )
}

fn feedback_observed(
    home: &Path,
    target: &tracedecay::agents::host_bundle_v2::HostBundleManifestV1,
    previous_receipt: &tracedecay::agents::host_bundle_v2::HostBundleInstallReceiptV1,
) -> tracedecay::errors::Result<(
    Vec<tracedecay::agents::host_bundle_v2::ObservedHostArtifactV1>,
    Vec<tracedecay::agents::host_bundle_v2::ObservedHostArtifactV1>,
)> {
    use tracedecay::agents::host_bundle_v2::{ObservedArtifactKindV1, ObservedHostArtifactV1};

    let observe =
        |relative_path: &str,
         owned: Option<&tracedecay::agents::host_bundle_v2::HostBundleReceiptArtifactV1>,
         cataloged_ownership_marker: Option<String>|
         -> tracedecay::errors::Result<ObservedHostArtifactV1> {
            let path = tracedecay::agents::host_bundle_v2::inspect_install_target(
                home,
                Path::new(relative_path),
            )
            .map_err(host_bundle_error)?;
            let (kind, artifact_digest) = match fs::read(&path) {
                Ok(bytes) => (
                    ObservedArtifactKindV1::RegularFile,
                    Some(Sha256::digest(bytes).into()),
                ),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    (ObservedArtifactKindV1::Missing, None)
                }
                Err(_) => {
                    return Err(tracedecay::errors::TraceDecayError::Config {
                        message: format!("could not inspect feedback artifact {}", path.display()),
                    });
                }
            };
            Ok(ObservedHostArtifactV1 {
                relative_path: relative_path.to_string(),
                kind,
                artifact_digest,
                // The exact prior receipt and digest carry ownership across a
                // catalog revision. Present the target marker to the planner
                // only for that receipt-bound path; foreign bytes still fail
                // the owned-digest comparison below.
                ownership_marker: owned.and_then(|_| cataloged_ownership_marker.clone()),
                owned_artifact_digest: owned.map(|owned| owned.artifact_digest),
                cataloged_ownership_marker,
            })
        };

    let manifest_observed = target
        .artifacts
        .iter()
        .map(|artifact| {
            let owned = previous_receipt
                .artifacts
                .iter()
                .find(|owned| owned.relative_path == artifact.relative_path);
            observe(
                &artifact.relative_path,
                owned,
                Some(artifact.ownership_marker.clone()),
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    let orphan_observed = previous_receipt
        .artifacts
        .iter()
        .filter(|owned| {
            !target
                .artifacts
                .iter()
                .any(|artifact| artifact.relative_path == owned.relative_path)
        })
        .map(|owned| observe(&owned.relative_path, Some(owned), None))
        .collect::<Result<Vec<_>, _>>()?;
    Ok((manifest_observed, orphan_observed))
}

fn read_feedback_contents(
    home: &Path,
    manifest: &tracedecay::agents::host_bundle_v2::HostBundleManifestV1,
) -> tracedecay::errors::Result<Vec<tracedecay::agents::host_bundle_v2::HostBundleArtifactContentV1>>
{
    manifest
        .artifacts
        .iter()
        .map(|artifact| {
            let path = tracedecay::agents::host_bundle_v2::inspect_install_target(
                home,
                Path::new(&artifact.relative_path),
            )
            .map_err(host_bundle_error)?;
            let bytes =
                fs::read(&path).map_err(|error| tracedecay::errors::TraceDecayError::Config {
                    message: format!(
                        "could not snapshot feedback artifact {}: {error}",
                        path.display()
                    ),
                })?;
            let digest: [u8; 32] = Sha256::digest(&bytes).into();
            if digest != artifact.artifact_digest {
                return Err(tracedecay::errors::TraceDecayError::Config {
                    message: format!(
                        "feedback artifact {} no longer matches its ownership receipt",
                        path.display()
                    ),
                });
            }
            Ok(
                tracedecay::agents::host_bundle_v2::HostBundleArtifactContentV1 {
                    relative_path: artifact.relative_path.clone(),
                    bytes,
                },
            )
        })
        .collect()
}

fn read_feedback_repair_contents(
    home: &Path,
    manifest: &tracedecay::agents::host_bundle_v2::HostBundleManifestV1,
) -> tracedecay::errors::Result<Vec<tracedecay::agents::host_bundle_v2::HostBundleArtifactContentV1>>
{
    manifest
        .artifacts
        .iter()
        .map(|artifact| {
            let path = tracedecay::agents::host_bundle_v2::inspect_install_target(
                home,
                Path::new(&artifact.relative_path),
            )
            .map_err(host_bundle_error)?;
            let bytes =
                fs::read(&path).map_err(|error| tracedecay::errors::TraceDecayError::Config {
                    message: format!(
                        "could not snapshot feedback repair artifact {}: {error}",
                        path.display()
                    ),
                })?;
            Ok(
                tracedecay::agents::host_bundle_v2::HostBundleArtifactContentV1 {
                    relative_path: artifact.relative_path.clone(),
                    bytes,
                },
            )
        })
        .collect()
}

fn snapshot_feedback_registration(
    home: &Path,
    integration: &dyn tracedecay::agents::AgentIntegration,
    component: tracedecay::agents::host_bundle_v2::HostBundleComponentV1,
) -> tracedecay::errors::Result<Vec<FeedbackRegistrationFileState>> {
    let paths = feedback_registration_paths(home, integration, component)?;
    paths
        .into_iter()
        .enumerate()
        .map(|(path_index, path)| {
            let contents = match fs::read(&path) {
                Ok(contents) => Some(contents),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => {
                    return Err(tracedecay::errors::TraceDecayError::Config {
                        message: format!(
                            "could not snapshot feedback registration {}: {error}",
                            path.display()
                        ),
                    });
                }
            };
            let permissions = fs::metadata(&path)
                .ok()
                .map(|metadata| feedback_file_permissions(&metadata.permissions()));
            let metadata = match &contents {
                Some(_) => Some(
                    tracedecay::agents::capture_host_file_metadata(&path).map_err(|error| {
                        tracedecay::errors::TraceDecayError::Config {
                            message: format!(
                                "could not snapshot feedback registration metadata {}: {error}",
                                path.display()
                            ),
                        }
                    })?,
                ),
                None => None,
            };
            Ok(FeedbackRegistrationFileState {
                path_index,
                path_digest: feedback_path_digest(&path)?,
                contents,
                permissions,
                metadata,
                applied_state: None,
            })
        })
        .collect()
}

/// Re-resolves the registration inventory and pins it to a recorded snapshot.
/// `inventory_changed` carries the caller's own wording for a stale inventory.
fn feedback_registration_paths_for_state(
    home: &Path,
    integration: &dyn tracedecay::agents::AgentIntegration,
    component: tracedecay::agents::host_bundle_v2::HostBundleComponentV1,
    registration_files: &[FeedbackRegistrationFileState],
    inventory_changed: &str,
) -> tracedecay::errors::Result<Vec<PathBuf>> {
    let paths = feedback_registration_paths(home, integration, component)?;
    if paths.len() == registration_files.len() {
        Ok(paths)
    } else {
        Err(tracedecay::errors::TraceDecayError::Config {
            message: inventory_changed.to_string(),
        })
    }
}

fn feedback_registration_path<'a>(
    paths: &'a [PathBuf],
    file: &FeedbackRegistrationFileState,
) -> tracedecay::errors::Result<&'a Path> {
    paths
        .get(file.path_index)
        .map(PathBuf::as_path)
        .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
            message: "feedback registration path index is invalid".to_string(),
        })
}

fn capture_feedback_applied_registration(
    home: &Path,
    integration: &dyn tracedecay::agents::AgentIntegration,
    component: tracedecay::agents::host_bundle_v2::HostBundleComponentV1,
    registration_files: &mut [FeedbackRegistrationFileState],
) -> tracedecay::errors::Result<()> {
    let paths = feedback_registration_paths_for_state(
        home,
        integration,
        component,
        registration_files,
        "feedback registration inventory changed during apply",
    )?;
    for file in registration_files {
        let path = feedback_registration_path(&paths, file)?;
        if feedback_path_digest(path)? != file.path_digest {
            return Err(tracedecay::errors::TraceDecayError::Config {
                message: "feedback registration path identity changed during apply".to_string(),
            });
        }
        file.applied_state = Some(feedback_file_observed_state(path)?);
    }
    Ok(())
}

fn validate_feedback_registration_snapshot(
    home: &Path,
    integration: &dyn tracedecay::agents::AgentIntegration,
    component: tracedecay::agents::host_bundle_v2::HostBundleComponentV1,
    registration_files: &[FeedbackRegistrationFileState],
) -> tracedecay::errors::Result<()> {
    let paths = feedback_registration_paths_for_state(
        home,
        integration,
        component,
        registration_files,
        "feedback registration inventory changed before apply",
    )?;
    for file in registration_files {
        let path = feedback_registration_path(&paths, file)?;
        if feedback_path_digest(path)? != file.path_digest
            || feedback_file_observed_state(path)?
                != feedback_observed_state_for_contents(
                    file.contents.as_deref(),
                    file.metadata.clone(),
                )
        {
            return Err(tracedecay::errors::TraceDecayError::Config {
                message: format!(
                    "feedback registration {} changed before apply; refusing stale activation",
                    path.display()
                ),
            });
        }
    }
    Ok(())
}

fn validate_feedback_registration_restore(
    home: &Path,
    integration: &dyn tracedecay::agents::AgentIntegration,
    component: tracedecay::agents::host_bundle_v2::HostBundleComponentV1,
    registration_files: &[FeedbackRegistrationFileState],
    effect_started: bool,
    intent_root: Option<&Path>,
) -> tracedecay::errors::Result<Vec<PathBuf>> {
    let paths = feedback_registration_paths_for_state(
        home,
        integration,
        component,
        registration_files,
        "feedback registration inventory no longer matches rollback state",
    )?;
    for file in registration_files {
        let path = feedback_registration_path(&paths, file)?;
        if feedback_path_digest(path)? != file.path_digest {
            return Err(tracedecay::errors::TraceDecayError::Config {
                message: "feedback registration path identity no longer matches rollback state"
                    .to_string(),
            });
        }
        let original =
            feedback_observed_state_for_contents(file.contents.as_deref(), file.metadata.clone());
        let expected = if let Some(applied) = file.applied_state.clone() {
            applied
        } else if effect_started {
            let intent_path = tracedecay::agents::host_config_write_intent_path(
                intent_root.ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
                    message: "feedback registration effect has no write-intent root".to_string(),
                })?,
                path,
            )?;
            match fs::read(intent_path) {
                Ok(intent) if intent.len() == 33 && intent[0] == 1 => {
                    let mut digest = [0_u8; 32];
                    digest.copy_from_slice(&intent[1..]);
                    FeedbackFileObservedStateV2 {
                        present: true,
                        digest,
                        metadata: original.metadata.clone(),
                    }
                }
                Ok(intent) => {
                    let intent: FeedbackHostConfigWriteIntentV2 = serde_json::from_slice(&intent)
                        .map_err(|_| {
                        tracedecay::errors::TraceDecayError::Config {
                            message: "invalid feedback registration write intent".to_string(),
                        }
                    })?;
                    if intent.schema_version != 2 {
                        return Err(tracedecay::errors::TraceDecayError::Config {
                            message: "unsupported feedback registration write-intent version"
                                .to_string(),
                        });
                    }
                    FeedbackFileObservedStateV2 {
                        present: true,
                        digest: intent.digest,
                        metadata: intent.metadata,
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => original.clone(),
                _ => {
                    return Err(tracedecay::errors::TraceDecayError::Config {
                        message: "invalid feedback registration write intent".to_string(),
                    });
                }
            }
        } else {
            original.clone()
        };
        let observed = feedback_file_observed_state(path)?;
        if observed != expected && observed != original {
            return Err(tracedecay::errors::TraceDecayError::Config {
                message: format!(
                    "feedback registration {} changed after apply; refusing stale restore",
                    path.display()
                ),
            });
        }
    }
    Ok(paths)
}

fn restore_feedback_registration(
    home: &Path,
    integration: &dyn tracedecay::agents::AgentIntegration,
    component: tracedecay::agents::host_bundle_v2::HostBundleComponentV1,
    registration_files: &[FeedbackRegistrationFileState],
    effect_started: bool,
    intent_root: Option<&Path>,
) -> tracedecay::errors::Result<()> {
    let paths = validate_feedback_registration_restore(
        home,
        integration,
        component,
        registration_files,
        effect_started,
        intent_root,
    )?;
    for file in registration_files {
        let path = &paths[file.path_index];
        let mut removed = false;
        match &file.contents {
            Some(contents) => {
                tracedecay::agents::safe_write_bytes_file_with_metadata(
                    path,
                    contents,
                    None,
                    file.metadata.as_ref(),
                )?;
                if file.metadata.is_none()
                    && let Some(permissions) = &file.permissions
                {
                    restore_feedback_file_permissions(path, permissions)?;
                }
            }
            None => match fs::remove_file(path) {
                Ok(()) => removed = true,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(tracedecay::errors::TraceDecayError::Config {
                        message: format!(
                            "could not remove restored feedback registration {}: {error}",
                            path.display()
                        ),
                    });
                }
            },
        }
        if removed {
            tracedecay_application::sync_parent_directory(
                path,
                tracedecay_application::DirectorySyncPolicy::TolerateUnsupported,
            )
            .map_err(|error| tracedecay::errors::TraceDecayError::Config {
                message: format!(
                    "could not durably remove feedback registration {}: {error}",
                    path.display()
                ),
            })?;
        }
    }
    Ok(())
}

fn validate_feedback_applied_artifacts(
    home: &Path,
    target_manifest: &tracedecay::agents::host_bundle_v2::HostBundleManifestV1,
    applied_receipt: &tracedecay::agents::host_bundle_v2::HostBundleInstallReceiptV1,
) -> tracedecay::errors::Result<()> {
    if target_manifest
        .canonical_digest()
        .map_err(host_bundle_error)?
        != applied_receipt.manifest_digest
    {
        return Err(tracedecay::errors::TraceDecayError::Config {
            message: "feedback target manifest no longer matches its applied receipt".to_string(),
        });
    }
    let contents = read_feedback_contents(home, target_manifest)?;
    if contents.len() != applied_receipt.artifacts.len() {
        return Err(tracedecay::errors::TraceDecayError::Config {
            message: "feedback artifact inventory no longer matches its applied receipt"
                .to_string(),
        });
    }
    for artifact in &applied_receipt.artifacts {
        let Some(content) = contents
            .iter()
            .find(|content| content.relative_path == artifact.relative_path)
        else {
            return Err(tracedecay::errors::TraceDecayError::Config {
                message: format!(
                    "feedback artifact {} is missing before restore",
                    artifact.relative_path
                ),
            });
        };
        if <[u8; 32]>::from(Sha256::digest(&content.bytes)) != artifact.artifact_digest {
            return Err(tracedecay::errors::TraceDecayError::Config {
                message: format!(
                    "feedback artifact {} changed before restore; refusing stale mutation",
                    artifact.relative_path
                ),
            });
        }
    }
    Ok(())
}

fn validate_feedback_active_receipts(
    lifecycle_root: &Path,
    switch_receipt: &tracedecay::agents::host_bundle_v2::FeedbackPathRollbackReceiptV1,
    selected_component: tracedecay::agents::host_bundle_v2::HostBundleComponentV1,
    expected_aggregates: &[tracedecay::agents::host_bundle_v2::HostComponentSetReceiptV1],
    previous_aggregate: Option<&tracedecay::agents::host_bundle_v2::HostComponentSetReceiptV1>,
) -> tracedecay::errors::Result<()> {
    use tracedecay::agents::host_bundle_v2::{
        latest_host_component_receipt_at, latest_host_component_set_receipt_at,
    };

    let component =
        latest_host_component_receipt_at(lifecycle_root, switch_receipt.host, selected_component)
            .map_err(host_bundle_error)?;
    let aggregate = latest_host_component_set_receipt_at(lifecycle_root, switch_receipt.host)
        .map_err(host_bundle_error)?;
    let aggregate_component = aggregate.as_ref().and_then(|receipt| {
        receipt
            .component_receipts
            .iter()
            .find(|receipt| receipt.component == selected_component)
    });
    let self_authored_partial_transition = previous_aggregate.is_some_and(|previous| {
        aggregate.as_ref() == Some(previous)
            && component.as_ref() == Some(&switch_receipt.apply_receipt)
    });
    if component.as_ref() != Some(&switch_receipt.apply_receipt)
        || aggregate
            .as_ref()
            .is_none_or(|current| !expected_aggregates.contains(current))
        || (aggregate_component != Some(&switch_receipt.apply_receipt)
            && !self_authored_partial_transition)
    {
        return Err(tracedecay::errors::TraceDecayError::Config {
            message: "feedback ownership receipt changed before restore; refusing stale mutation"
                .to_string(),
        });
    }
    Ok(())
}

fn snapshot_feedback_artifact_permissions(
    home: &Path,
    manifest: &tracedecay::agents::host_bundle_v2::HostBundleManifestV1,
) -> tracedecay::errors::Result<Vec<FeedbackArtifactPermissionStateV4>> {
    manifest
        .artifacts
        .iter()
        .map(|artifact| {
            let path = home.join(&artifact.relative_path);
            let metadata = fs::symlink_metadata(&path).map_err(|error| {
                tracedecay::errors::TraceDecayError::Config {
                    message: format!(
                        "could not capture feedback artifact permissions for {}: {error}",
                        path.display()
                    ),
                }
            })?;
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(tracedecay::errors::TraceDecayError::Config {
                    message: format!("unsafe feedback artifact {}", path.display()),
                });
            }
            Ok(FeedbackArtifactPermissionStateV4 {
                relative_path: artifact.relative_path.clone(),
                permissions: feedback_file_permissions(&metadata.permissions()),
            })
        })
        .collect()
}

fn restore_feedback_artifact_permissions(
    home: &Path,
    permissions: &[FeedbackArtifactPermissionStateV4],
) -> tracedecay::errors::Result<()> {
    for artifact in permissions {
        restore_feedback_file_permissions(
            &home.join(&artifact.relative_path),
            &artifact.permissions,
        )?;
    }
    Ok(())
}

fn feedback_registration_paths(
    home: &Path,
    integration: &dyn tracedecay::agents::AgentIntegration,
    component: tracedecay::agents::host_bundle_v2::HostBundleComponentV1,
) -> tracedecay::errors::Result<Vec<PathBuf>> {
    let mut paths = integration.host_component_registration_paths_checked(&[component], home)?;
    if integration.id() == "claude" {
        let artifact_owned_manifest =
            home.join(".claude/plugins/marketplaces/tracedecay/.claude-plugin/marketplace.json");
        paths.retain(|path| path != &artifact_owned_manifest);
    } else if integration.id() == "cursor" {
        let artifact_owned_manifest =
            home.join(".cursor/plugins/local/tracedecay/.cursor-plugin/plugin.json");
        paths.retain(|path| path != &artifact_owned_manifest);
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn feedback_path_digest(path: &Path) -> tracedecay::errors::Result<[u8; 32]> {
    let bytes =
        serde_json::to_vec(path).map_err(|error| tracedecay::errors::TraceDecayError::Config {
            message: format!("could not bind feedback registration path: {error}"),
        })?;
    Ok(Sha256::digest(bytes).into())
}

fn feedback_file_observed_state(
    path: &Path,
) -> tracedecay::errors::Result<FeedbackFileObservedStateV2> {
    match fs::read(path) {
        Ok(bytes) => Ok(FeedbackFileObservedStateV2 {
            present: true,
            digest: Sha256::digest(bytes).into(),
            metadata: Some(
                tracedecay::agents::capture_host_file_metadata(path).map_err(|error| {
                    tracedecay::errors::TraceDecayError::Config {
                        message: format!(
                            "could not inspect feedback registration metadata {}: {error}",
                            path.display()
                        ),
                    }
                })?,
            ),
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(FeedbackFileObservedStateV2 {
                present: false,
                digest: [0; 32],
                metadata: None,
            })
        }
        Err(error) => Err(tracedecay::errors::TraceDecayError::Config {
            message: format!(
                "could not inspect feedback registration {}: {error}",
                path.display()
            ),
        }),
    }
}

fn feedback_observed_state_for_contents(
    contents: Option<&[u8]>,
    metadata: Option<tracedecay::agents::HostFileMetadataIdentityV1>,
) -> FeedbackFileObservedStateV2 {
    contents.map_or(
        FeedbackFileObservedStateV2 {
            present: false,
            digest: [0; 32],
            metadata: None,
        },
        |bytes| FeedbackFileObservedStateV2 {
            present: true,
            digest: Sha256::digest(bytes).into(),
            metadata,
        },
    )
}

fn feedback_file_permissions(permissions: &fs::Permissions) -> FeedbackFilePermissionsV2 {
    #[cfg(unix)]
    let unix_mode = Some(permissions.mode());
    #[cfg(not(unix))]
    let unix_mode = None;
    FeedbackFilePermissionsV2 {
        readonly: permissions.readonly(),
        unix_mode,
    }
}

fn restore_feedback_file_permissions(
    path: &Path,
    state: &FeedbackFilePermissionsV2,
) -> tracedecay::errors::Result<()> {
    let mut permissions = fs::metadata(path)
        .map_err(|error| tracedecay::errors::TraceDecayError::Config {
            message: format!(
                "could not inspect feedback registration permissions {}: {error}",
                path.display()
            ),
        })?
        .permissions();
    #[cfg(unix)]
    if let Some(mode) = state.unix_mode {
        permissions.set_mode(mode);
    }
    #[cfg(not(unix))]
    permissions.set_readonly(state.readonly);
    fs::set_permissions(path, permissions).map_err(|error| {
        tracedecay::errors::TraceDecayError::Config {
            message: format!(
                "could not restore feedback registration permissions {}: {error}",
                path.display()
            ),
        }
    })?;
    fs::File::open(path)
        .and_then(|file| file.sync_all())
        .and_then(|()| {
            tracedecay_application::sync_parent_directory(
                path,
                tracedecay_application::DirectorySyncPolicy::TolerateUnsupported,
            )
        })
        .map_err(|error| tracedecay::errors::TraceDecayError::Config {
            message: format!(
                "could not durably restore feedback registration {}: {error}",
                path.display()
            ),
        })
}

fn write_feedback_state(
    path: &Path,
    state: &FeedbackRollbackCliState,
) -> tracedecay::errors::Result<()> {
    let bytes = serde_json::to_vec_pretty(state).map_err(|error| {
        tracedecay::errors::TraceDecayError::Config {
            message: format!("could not serialize feedback rollback state: {error}"),
        }
    })?;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|error| tracedecay::errors::TraceDecayError::Config {
        message: format!("could not create feedback state directory: {error}"),
    })?;
    tracedecay_application::atomic_write(
        path,
        "feedback-rollback-state",
        &bytes,
        tracedecay_application::DirectorySyncPolicy::TolerateUnsupported,
    )
    .map_err(|error| tracedecay::errors::TraceDecayError::Config {
        message: format!("could not durably publish feedback rollback state: {error}"),
    })
}

fn feedback_doctor_state_path(lifecycle_root: &Path, agent_id: &str) -> PathBuf {
    lifecycle_root
        .join(".tracedecay-host-bundle-v1")
        .join(format!("feedback-rollback.{agent_id}.v1.json"))
}

fn persist_feedback_state(
    state_path: &Path,
    lifecycle_root: &Path,
    state: &FeedbackRollbackCliState,
) -> tracedecay::errors::Result<()> {
    write_feedback_state(state_path, state)?;
    let doctor_path = feedback_doctor_state_path(lifecycle_root, &state.agent_id);
    let bytes = serde_json::to_vec_pretty(&serde_json::json!({
        "schema_version": FEEDBACK_ROLLBACK_STATE_SCHEMA_VERSION,
        "agent_id": state.agent_id,
        "host": state.host,
        "status": state.status,
        "state_path": state_path,
    }))
    .map_err(|error| tracedecay::errors::TraceDecayError::Config {
        message: format!("could not serialize feedback Doctor state: {error}"),
    })?;
    let parent = doctor_path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|error| tracedecay::errors::TraceDecayError::Config {
        message: format!("could not create feedback Doctor state directory: {error}"),
    })?;
    let temporary = doctor_path.with_extension("json.new");
    fs::write(&temporary, bytes).map_err(|error| tracedecay::errors::TraceDecayError::Config {
        message: format!("could not stage feedback Doctor state: {error}"),
    })?;
    fs::rename(&temporary, doctor_path).map_err(|error| {
        tracedecay::errors::TraceDecayError::Config {
            message: format!("could not publish feedback Doctor state: {error}"),
        }
    })
}

fn feedback_rollback_dry_run(agent_id: &str) -> tracedecay::errors::Result<()> {
    let (home, _lifecycle_root, aggregate, target) = feedback_rollback_inputs(agent_id)?;
    let (previous, previous_receipt) = live_feedback_receipt(&home, &aggregate)?;
    let verifier = feedback_pair_verifier(&previous, &target.manifest)?;
    let request = feedback_request(
        &target.manifest,
        tracedecay::agents::host_bundle_v2::HostBundleLifecycleOpV1::Repair,
        false,
    )?;
    let (manifest_observed, orphan_observed) =
        feedback_observed(&home, &target.manifest, &previous_receipt)?;
    let lifecycle = tracedecay::agents::host_bundle_v2::HostBundleLifecycleRuntimeV1::new(
        verifier,
        FeedbackPreviewStorage,
    );
    let rollback = tracedecay::agents::host_bundle_v2::FeedbackPathRollbackSwitchV1::new(lifecycle);
    let preview = rollback
        .feedback_rollback_switch_dry_run(
            &previous,
            &target.manifest,
            &request,
            &manifest_observed,
            Some(&previous_receipt),
            &orphan_observed,
            &[],
        )
        .map_err(host_bundle_error)?;
    println!(
        "{agent_id} feedback rollback: {} mutation(s), rollback={}, confirmation={}",
        preview.plan.mutations.len(),
        preview.plan.rollback_required,
        preview.confirmation_required
    );
    for mutation in preview.plan.mutations {
        println!("  {:?} {}", mutation.action, mutation.relative_path);
    }
    Ok(())
}

fn feedback_rollback_apply(agent_id: &str, state_path: &Path) -> tracedecay::errors::Result<()> {
    let dashboard_enabled =
        load_host_lifecycle_user_config()?.dashboard_enabled_for_agent(agent_id);
    let (home, lifecycle_root, aggregate, target) = feedback_rollback_inputs(agent_id)?;
    let (previous, _previous_receipt) = live_feedback_receipt(&home, &aggregate)?;
    let previous_contents = read_feedback_repair_contents(&home, &previous)?;
    let artifact_permissions = snapshot_feedback_artifact_permissions(&home, &previous)?;
    let integration = tracedecay::agents::get_integration(agent_id)?;
    let registration_files =
        snapshot_feedback_registration(&home, integration.as_ref(), target.manifest.component)?;
    let verifier = feedback_pair_verifier(&previous, &target.manifest)?;
    let request = feedback_request(
        &target.manifest,
        tracedecay::agents::host_bundle_v2::HostBundleLifecycleOpV1::Repair,
        true,
    )?;
    let identity = FeedbackRollbackIdentityV2::current(agent_id, &home, &lifecycle_root)?;
    let mut state = FeedbackRollbackCliState {
        schema_version: FEEDBACK_ROLLBACK_STATE_SCHEMA_VERSION,
        agent_id: agent_id.to_string(),
        host: target.manifest.host,
        status: FeedbackRollbackCliStatus::Prepared,
        previous_aggregate: aggregate,
        previous_manifest: previous.clone(),
        previous_contents,
        target_manifest: target.manifest.clone(),
        dashboard_enabled,
        switch_operation_id: request.operation_id,
        effect_started: false,
        registration_effect_started: false,
        registration_intent_root: lifecycle_root
            .join("feedback-registration-intents")
            .join(hex::encode(request.operation_id)),
        compensation_preserves_registration: false,
        switch_receipt: None,
        restore_operation_id: None,
        restore_effect_started: false,
        restore_receipt: None,
        identity,
        registration_files,
        artifact_permissions,
    };
    persist_feedback_state(state_path, &lifecycle_root, &state)?;
    validate_feedback_registration_snapshot(
        &home,
        integration.as_ref(),
        target.manifest.component,
        &state.registration_files,
    )?;

    let writer = tracedecay::agents::host_bundle_v2::HostBundleWriterV1::open_with_lifecycle_root(
        &home,
        &lifecycle_root,
    )
    .map_err(host_bundle_error)?;
    let lifecycle =
        tracedecay::agents::host_bundle_v2::HostBundleLifecycleRuntimeV1::new(verifier, writer);
    let mut rollback =
        tracedecay::agents::host_bundle_v2::FeedbackPathRollbackSwitchV1::new(lifecycle);
    state.effect_started = true;
    persist_feedback_state(state_path, &lifecycle_root, &state)?;
    let switch_receipt = rollback
        .feedback_rollback_switch_apply(
            &previous,
            &target.manifest,
            &request,
            &target.contents,
            &[],
        )
        .map_err(host_bundle_error)?;
    #[cfg(feature = "test-transport")]
    if std::env::var_os("TRACEDECAY_TEST_ABORT_AFTER_FEEDBACK_SWITCH").is_some() {
        std::process::abort();
    }
    state.switch_receipt = Some(switch_receipt.clone());
    persist_feedback_state(state_path, &lifecycle_root, &state)?;
    let lifecycle = rollback.into_lifecycle();
    let writer = lifecycle.into_storage();

    let context = tracedecay::agents::InstallContext {
        home: home.clone(),
        tracedecay_bin: tracedecay::agents::which_tracedecay()
            .unwrap_or_else(|| "tracedecay".to_string()),
        tool_permissions: tracedecay::agents::expected_tool_perms(),
        project_root: None,
        dashboard: state.dashboard_enabled,
    };
    let registration_snapshot = validate_feedback_registration_snapshot(
        &home,
        integration.as_ref(),
        target.manifest.component,
        &state.registration_files,
    );
    let registration_effect_attempted = registration_snapshot.is_ok();
    let registration_result = match registration_snapshot {
        Err(error) => Err(error),
        Ok(()) => {
            state.registration_effect_started = true;
            persist_feedback_state(state_path, &lifecycle_root, &state)?;
            let activation_result = tracedecay::agents::with_host_config_write_intents(
                state.registration_intent_root.clone(),
                || {
                    integration.activate_deployed_host_component_registration(
                        &[target.manifest.component],
                        &context,
                    )
                },
            )
            .and_then(|()| {
                if integration.id() == "cursor" {
                    return Ok(());
                }
                let health = tracedecay::agents::HealthcheckContext {
                    home: home.clone(),
                    project_path: std::env::current_dir().unwrap_or_else(|_| home.clone()),
                };
                (integration.host_component_registration(target.manifest.component, &health)
                    == tracedecay::agents::host_bundle_v2::HostBundleRegistrationStateV1::Current)
                    .then_some(())
                    .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
                        message: format!(
                            "{agent_id} did not verify its activated feedback registration"
                        ),
                    })
            });
            let capture_result = capture_feedback_applied_registration(
                &home,
                integration.as_ref(),
                target.manifest.component,
                &mut state.registration_files,
            );
            if capture_result.is_ok() {
                persist_feedback_state(state_path, &lifecycle_root, &state)?;
            }
            activation_result.and(capture_result)
        }
    };
    if let Err(registration_error) = registration_result {
        state.compensation_preserves_registration = !registration_effect_attempted;
        persist_feedback_state(state_path, &lifecycle_root, &state)?;
        drop(writer);
        feedback_rollback_restore(state_path)?;
        return Err(registration_error);
    }
    writer
        .publish_feedback_component_set_receipt(&target.manifest, &switch_receipt.apply_receipt)
        .map_err(host_bundle_error)?;

    state.status = FeedbackRollbackCliStatus::Applied;
    persist_feedback_state(state_path, &lifecycle_root, &state)?;
    println!(
        "\x1b[32m✔\x1b[0m {agent_id} feedback rollback applied; state {}",
        state_path.display()
    );
    Ok(())
}

fn feedback_rollback_restore(state_path: &Path) -> tracedecay::errors::Result<()> {
    let bytes =
        fs::read(state_path).map_err(|error| tracedecay::errors::TraceDecayError::Config {
            message: format!(
                "could not read feedback rollback state {}: {error}",
                state_path.display()
            ),
        })?;
    let mut state: FeedbackRollbackCliState = serde_json::from_slice(&bytes).map_err(|error| {
        tracedecay::errors::TraceDecayError::Config {
            message: format!("invalid feedback rollback state: {error}"),
        }
    })?;
    if !(MIN_FEEDBACK_ROLLBACK_STATE_SCHEMA_VERSION..=FEEDBACK_ROLLBACK_STATE_SCHEMA_VERSION)
        .contains(&state.schema_version)
    {
        return Err(tracedecay::errors::TraceDecayError::Config {
            message: "unsupported feedback rollback state version".to_string(),
        });
    }
    let home = tracedecay::agents::home_dir().ok_or_else(|| {
        tracedecay::errors::TraceDecayError::Config {
            message: "could not determine home directory".to_string(),
        }
    })?;
    let lifecycle_root = tracedecay::agents::host_bundle_v2::resolved_host_bundle_lifecycle_root()
        .map_err(|error| tracedecay::errors::TraceDecayError::Config {
            message: format!("could not resolve host lifecycle root: {error}"),
        })?;
    if host_kind_for_agent(&state.agent_id)? != state.host {
        return Err(tracedecay::errors::TraceDecayError::Config {
            message: "feedback rollback state host does not match its integration".to_string(),
        });
    }
    state
        .identity
        .validate(&state.agent_id, &home, &lifecycle_root)?;
    let feedback_component = selected_feedback_component(&state.previous_aggregate)?;
    if state.previous_manifest.component != feedback_component
        || state.target_manifest.component != feedback_component
    {
        return Err(tracedecay::errors::TraceDecayError::Config {
            message: "feedback rollback state component does not match its aggregate".to_string(),
        });
    }
    let integration = tracedecay::agents::get_integration(&state.agent_id)?;
    let dashboard_enabled =
        load_host_lifecycle_user_config()?.dashboard_enabled_for_agent(&state.agent_id);
    if dashboard_enabled != state.dashboard_enabled {
        return Err(tracedecay::errors::TraceDecayError::Config {
            message: format!(
                "feedback rollback dashboard policy changed for {:?}; restore the exact policy before resuming",
                state.agent_id
            ),
        });
    }
    if state.switch_receipt.is_none() && !state.effect_started {
        validate_feedback_registration_restore(
            &home,
            integration.as_ref(),
            feedback_component,
            &state.registration_files,
            false,
            Some(&state.registration_intent_root),
        )?;
        read_feedback_contents(&home, &state.previous_manifest)?;
        state.status = FeedbackRollbackCliStatus::Restored;
        persist_feedback_state(state_path, &lifecycle_root, &state)?;
        return Ok(());
    }
    if !state.compensation_preserves_registration {
        validate_feedback_registration_restore(
            &home,
            integration.as_ref(),
            feedback_component,
            &state.registration_files,
            state.registration_effect_started,
            Some(&state.registration_intent_root),
        )?;
    }
    if !state.restore_effect_started
        && let Some(switch_receipt) = state.switch_receipt.as_ref()
    {
        validate_feedback_applied_artifacts(
            &home,
            &state.target_manifest,
            &switch_receipt.apply_receipt,
        )?;
    }
    let writer = tracedecay::agents::host_bundle_v2::HostBundleWriterV1::open_with_lifecycle_root(
        &home,
        &lifecycle_root,
    )
    .map_err(host_bundle_error)?;
    let previous_manifest_digest = state
        .previous_manifest
        .canonical_digest()
        .map_err(host_bundle_error)?;
    let committed_restore_receipt = if state.restore_effect_started {
        use tracedecay::agents::host_bundle_v2::latest_host_component_receipt_at;
        latest_host_component_receipt_at(&lifecycle_root, state.host, feedback_component)
            .map_err(host_bundle_error)?
            .filter(|receipt| {
                Some(receipt.operation_id) == state.restore_operation_id
                    && receipt.manifest_digest == previous_manifest_digest
            })
    } else {
        None
    };
    if let (Some(restore_receipt), Some(switch_receipt)) = (
        committed_restore_receipt.as_ref(),
        state.switch_receipt.as_ref(),
    ) {
        use tracedecay::agents::host_bundle_v2::latest_host_component_set_receipt_at;

        let target_aggregate = aggregate_with_feedback_component(
            &state.previous_aggregate,
            &state.target_manifest,
            &switch_receipt.apply_receipt,
        );
        let restored_aggregate = aggregate_with_feedback_component(
            &state.previous_aggregate,
            &state.previous_manifest,
            restore_receipt,
        );
        let aggregate = latest_host_component_set_receipt_at(&lifecycle_root, state.host)
            .map_err(host_bundle_error)?;
        if aggregate.as_ref() != Some(&target_aggregate)
            && aggregate.as_ref() != Some(&restored_aggregate)
        {
            return Err(tracedecay::errors::TraceDecayError::Config {
                message: "feedback aggregate changed during interrupted restore".to_string(),
            });
        }
    }
    let switch_receipt = if committed_restore_receipt.is_some() {
        state
            .switch_receipt
            .clone()
            .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
                message: "feedback restore effect has no switch receipt identity".to_string(),
            })?
    } else if let Some(switch_receipt) = state.switch_receipt.clone() {
        let target_aggregate = aggregate_with_feedback_component(
            &state.previous_aggregate,
            &state.target_manifest,
            &switch_receipt.apply_receipt,
        );
        let expected_aggregates = if state.status == FeedbackRollbackCliStatus::Applied {
            vec![target_aggregate]
        } else {
            vec![state.previous_aggregate.clone(), target_aggregate]
        };
        validate_feedback_active_receipts(
            &lifecycle_root,
            &switch_receipt,
            feedback_component,
            &expected_aggregates,
            (state.status != FeedbackRollbackCliStatus::Applied)
                .then_some(&state.previous_aggregate),
        )?;
        switch_receipt
    } else {
        use tracedecay::agents::host_bundle_v2::{
            FeedbackPathRollbackReceiptV1, latest_host_component_receipt_at,
            latest_host_component_set_receipt_at,
        };

        if latest_host_component_set_receipt_at(&lifecycle_root, state.host)
            .map_err(host_bundle_error)?
            .as_ref()
            != Some(&state.previous_aggregate)
        {
            return Err(tracedecay::errors::TraceDecayError::Config {
                message: "feedback aggregate changed during interrupted apply".to_string(),
            });
        }
        let component =
            latest_host_component_receipt_at(&lifecycle_root, state.host, feedback_component)
                .map_err(host_bundle_error)?;
        let target_manifest_digest = state
            .target_manifest
            .canonical_digest()
            .map_err(host_bundle_error)?;
        if let Some(apply_receipt) = component.filter(|receipt| {
            receipt.operation_id == state.switch_operation_id
                && receipt.manifest_digest == target_manifest_digest
        }) {
            FeedbackPathRollbackReceiptV1 {
                host: state.host,
                previous_manifest_digest: state
                    .previous_manifest
                    .canonical_digest()
                    .map_err(host_bundle_error)?,
                applied_manifest_digest: target_manifest_digest,
                apply_receipt,
            }
        } else {
            // The writer already resolved the lower-level artifact journal.
            // No feedback registration or aggregate receipt effect occurred.
            validate_feedback_registration_restore(
                &home,
                integration.as_ref(),
                feedback_component,
                &state.registration_files,
                state.registration_effect_started,
                Some(&state.registration_intent_root),
            )?;
            read_feedback_contents(&home, &state.previous_manifest)?;
            restore_feedback_artifact_permissions(&home, &state.artifact_permissions)?;
            state.status = FeedbackRollbackCliStatus::Restored;
            persist_feedback_state(state_path, &lifecycle_root, &state)?;
            return Ok(());
        }
    };
    let verifier = feedback_pair_verifier(&state.previous_manifest, &state.target_manifest)?;
    if committed_restore_receipt.is_none() && !state.compensation_preserves_registration {
        validate_feedback_registration_restore(
            &home,
            integration.as_ref(),
            feedback_component,
            &state.registration_files,
            state.registration_effect_started,
            Some(&state.registration_intent_root),
        )?;
        validate_feedback_applied_artifacts(
            &home,
            &state.target_manifest,
            &switch_receipt.apply_receipt,
        )?;
    }
    let mut request = feedback_request(
        &state.previous_manifest,
        tracedecay::agents::host_bundle_v2::HostBundleLifecycleOpV1::Repair,
        true,
    )?;
    if let Some(operation_id) = state.restore_operation_id {
        request.operation_id = operation_id;
    } else {
        state.restore_operation_id = Some(request.operation_id);
    }
    if !state.restore_effect_started {
        state.restore_effect_started = true;
        persist_feedback_state(state_path, &lifecycle_root, &state)?;
    }
    let lifecycle =
        tracedecay::agents::host_bundle_v2::HostBundleLifecycleRuntimeV1::new(verifier, writer);
    let mut rollback =
        tracedecay::agents::host_bundle_v2::FeedbackPathRollbackSwitchV1::new(lifecycle);
    let restore = if let Some(restore_receipt) = committed_restore_receipt {
        tracedecay::agents::host_bundle_v2::FeedbackPathRestoreReceiptV1 {
            switch_operation_id: switch_receipt.apply_receipt.operation_id,
            restore_receipt,
        }
    } else {
        rollback
            .feedback_rollback_switch_restore(
                &switch_receipt,
                &state.previous_manifest,
                &request,
                &state.previous_contents,
                &[],
            )
            .map_err(host_bundle_error)?
    };
    #[cfg(feature = "test-transport")]
    if std::env::var_os("TRACEDECAY_TEST_ABORT_AFTER_FEEDBACK_RESTORE").is_some() {
        std::process::abort();
    }
    state.restore_receipt = Some(restore.clone());
    persist_feedback_state(state_path, &lifecycle_root, &state)?;
    let lifecycle = rollback.into_lifecycle();
    let writer = lifecycle.into_storage();
    let context = tracedecay::agents::InstallContext {
        home,
        tracedecay_bin: tracedecay::agents::which_tracedecay()
            .unwrap_or_else(|| "tracedecay".to_string()),
        tool_permissions: tracedecay::agents::expected_tool_perms(),
        project_root: None,
        dashboard: state.dashboard_enabled,
    };
    if !state.compensation_preserves_registration {
        restore_feedback_registration(
            &context.home,
            integration.as_ref(),
            feedback_component,
            &state.registration_files,
            state.registration_effect_started,
            Some(&state.registration_intent_root),
        )?;
    }
    restore_feedback_artifact_permissions(&context.home, &state.artifact_permissions)?;
    if !state.compensation_preserves_registration {
        validate_feedback_registration_snapshot(
            &context.home,
            integration.as_ref(),
            feedback_component,
            &state.registration_files,
        )?;
    }
    writer
        .publish_feedback_component_set_receipt(&state.previous_manifest, &restore.restore_receipt)
        .map_err(host_bundle_error)?;
    state.status = FeedbackRollbackCliStatus::Restored;
    persist_feedback_state(state_path, &lifecycle_root, &state)?;
    println!(
        "\x1b[32m✔\x1b[0m {} feedback route restored; state {}",
        state.agent_id,
        state_path.display()
    );
    Ok(())
}

fn host_bundle_component(
    component: crate::cli::HostBundleComponentArg,
) -> tracedecay::agents::host_bundle_v2::HostBundleComponentV1 {
    match component {
        crate::cli::HostBundleComponentArg::Core => {
            tracedecay::agents::host_bundle_v2::HostBundleComponentV1::Core
        }
        crate::cli::HostBundleComponentArg::Agent => {
            tracedecay::agents::host_bundle_v2::HostBundleComponentV1::Agent
        }
        crate::cli::HostBundleComponentArg::ContextMcp => {
            tracedecay::agents::host_bundle_v2::HostBundleComponentV1::ContextMcp
        }
        crate::cli::HostBundleComponentArg::OperatorMcp => {
            tracedecay::agents::host_bundle_v2::HostBundleComponentV1::OperatorMcp
        }
    }
}

/// Inspect or recover an interrupted first-party host component transaction.
///
/// This is the supported replacement for hand-deleting
/// `~/.tracedecay/host-components/.tracedecay-host-bundle-v1/component-set-journal.*.json`,
/// which used to be the only way out of a wedged host lifecycle.
pub(crate) async fn handle_host_bundle_recovery_command(
    action: crate::cli::HostBundleAction,
    dry_run: bool,
    yes: bool,
) -> tracedecay::errors::Result<()> {
    let home = tracedecay::agents::home_dir().ok_or_else(|| {
        tracedecay::errors::TraceDecayError::Config {
            message: "could not determine home directory".to_string(),
        }
    })?;
    let lifecycle_root = tracedecay::agents::host_bundle_v2::resolved_host_bundle_lifecycle_root()
        .map_err(|error| tracedecay::errors::TraceDecayError::Config {
            message: format!("could not resolve host lifecycle root: {error}"),
        })?;
    let mut writer =
        tracedecay::agents::host_bundle_v2::HostBundleWriterV1::open_with_lifecycle_root(
            &home,
            &lifecycle_root,
        )
        .map_err(host_bundle_error)?;

    let (selected_agent, quarantine, status_only) = match action {
        crate::cli::HostBundleAction::Status => (None, false, true),
        crate::cli::HostBundleAction::Recover { agent, quarantine } => (agent, quarantine, dry_run),
        crate::cli::HostBundleAction::ArtifactBackup { .. }
        | crate::cli::HostBundleAction::ArtifactRestore { .. } => {
            return Err(tracedecay::errors::TraceDecayError::Config {
                message: "artifact backup and restore are not recovery operations".to_string(),
            });
        }
    };

    let mut pending = writer
        .pending_component_set_journal_hosts()
        .map_err(host_bundle_error)?;
    if let Some(agent) = selected_agent.as_deref() {
        let host = host_kind_for_agent(agent)?;
        pending.retain(|pending_host| *pending_host == host);
    }
    if pending.is_empty() {
        eprintln!("\x1b[32m✔\x1b[0m no host component lifecycle journal is awaiting recovery");
        return Ok(());
    }
    for host in &pending {
        eprintln!(
            "  pending: {} ({:?})",
            tracedecay::agents::integration_id_for_host(*host),
            host
        );
    }
    if status_only {
        return Ok(());
    }
    if !yes {
        return Err(tracedecay::errors::TraceDecayError::Config {
            message: "host component recovery mutates deployed files; re-run with --yes"
                .to_string(),
        });
    }

    let now_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| tracedecay::errors::TraceDecayError::Config {
            message: "system clock is before the Unix epoch".to_string(),
        })?
        .as_secs();
    for host in pending {
        let agent_id = tracedecay::agents::integration_id_for_host(host);
        let operation = writer
            .pending_component_set_journal_operation(host)
            .map_err(host_bundle_error)?
            .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
                message: format!("{agent_id}: pending lifecycle journal disappeared"),
            })?;
        let mut registration = CatalogHostComponentRegistrationAuthority::new(
            agent_id,
            &home,
            &lifecycle_root,
            operation,
        )?;
        let outcome =
            tracedecay::agents::host_bundle_v2::HostComponentSetTransactionV1::new(&mut writer)
                .recover_host(host, &mut registration);
        match outcome {
            Ok(()) => eprintln!("\x1b[32m✔\x1b[0m {agent_id}: lifecycle journal recovered"),
            Err(error) if quarantine => {
                let quarantined = writer
                    .quarantine_component_set_journal(host, now_unix)
                    .map_err(host_bundle_error)?;
                match quarantined {
                    Some(path) => eprintln!(
                        "\x1b[33m!\x1b[0m {agent_id}: {error}; journal quarantined at {} (rollback backups preserved)",
                        path.display()
                    ),
                    None => eprintln!("\x1b[32m✔\x1b[0m {agent_id}: lifecycle journal cleared"),
                }
            }
            Err(error) => return Err(host_bundle_error(error)),
        }
    }
    Ok(())
}

/// Inverse of `integration_id_for_host`, derived from the stock host list so
/// the two cannot drift apart.
///
/// That mapping is many-to-one, so the alias hosts that share an id with a
/// canonical one are skipped: `cursor` resolves to the desktop host and `cline`
/// to the single Cline host rather than the family.
fn host_kind_for_agent(
    agent: &str,
) -> tracedecay::errors::Result<tracedecay::agents::host_bundle_v2::HostKindV1> {
    use tracedecay::agents::host_bundle_v2::HostKindV1;

    const ALIASED_HOSTS: [HostKindV1; 2] = [HostKindV1::CursorCloud, HostKindV1::ClineFamily];

    tracedecay::agents::host_bundle_v2::stock_host_kinds()
        .into_iter()
        .filter(|host| !ALIASED_HOSTS.contains(host))
        .find(|host| tracedecay::agents::integration_id_for_host(*host) == agent)
        .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
            message: format!("agent {agent:?} has no embedded first-party host component"),
        })
}

fn host_bundle_error(
    error: tracedecay::agents::host_bundle_v2::HostBundleError,
) -> tracedecay::errors::TraceDecayError {
    tracedecay::errors::TraceDecayError::Config {
        message: format!("host bundle lifecycle failed: {error}"),
    }
}

fn host_bundle_error_for_agent(
    agent_id: &str,
    error: tracedecay::agents::host_bundle_v2::HostBundleError,
) -> tracedecay::errors::TraceDecayError {
    if error == tracedecay::agents::host_bundle_v2::HostBundleError::NativeUpdateRequired {
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
        return tracedecay::errors::TraceDecayError::Config {
            message: message.to_string(),
        };
    }
    if agent_id == "codex"
        && error == tracedecay::agents::host_bundle_v2::HostBundleError::UnsupportedCapability
    {
        return tracedecay::errors::TraceDecayError::Config {
            message: "Codex plugin activation could not be completed through `codex plugin add`. \
                      Confirm the `codex` CLI is on PATH and retry; hook trust still requires \
                      `/hooks` inside Codex after a successful add."
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
) -> tracedecay::errors::Result<()> {
    validate_codex_automation_flags(agent.as_deref(), automation)?;
    if local {
        return Err(project_local_host_lifecycle_unavailable());
    }
    let home = tracedecay::agents::home_dir().ok_or_else(|| {
        tracedecay::errors::TraceDecayError::Config {
            message: "could not determine home directory".to_string(),
        }
    })?;
    let tracedecay_bin = tracedecay::agents::which_tracedecay().ok_or_else(|| {
        tracedecay::errors::TraceDecayError::Config {
            message: "tracedecay not found on PATH. Install the checksummed GitHub release:\n  \
                          https://github.com/ScriptedAlchemy/tracedecay/releases/latest"
                .to_string(),
        }
    })?;
    let mut user_cfg = load_host_lifecycle_user_config()?;

    let mut installed_names: Vec<String> = Vec::new();
    let mut removed_names: Vec<String> = Vec::new();
    // Ids this pass actually (re)installed at the current binary version. The
    // tail uses this to decide whether the pass covered every tracked agent
    // and may disarm the startup silent reinstall.
    let mut refreshed_ids: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    if let Some(id) = agent {
        let ag = tracedecay::agents::get_integration(&id)?;
        let name = ag.name().to_string();
        let context = tracedecay::agents::InstallContext {
            home: home.clone(),
            tracedecay_bin: tracedecay_bin.clone(),
            tool_permissions: tracedecay::agents::expected_tool_perms(),
            project_root: None,
            dashboard: !no_dashboard,
        };
        prepare_native_activation_if_needed(ag.as_ref(), &context)?;
        apply_default_canonical_component_set(
            &id,
            HostBundleCliOperation::Install,
            &home,
            !no_dashboard,
        )?;
        refreshed_ids.insert(id.clone());
        if let Some(options) = automation.filter(|_| id == "codex") {
            let scoped_project_path = validate_codex_automation_project_path()?;
            install_codex_daemon_automation(&scoped_project_path, &home, options).await?;
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
            .map_err(|err| tracedecay::errors::TraceDecayError::Config {
                message: format!("failed to save user config: {err}"),
            })?;
    } else {
        let (to_install, to_uninstall) =
            tracedecay::agents::pick_integrations_interactive(&home, &user_cfg.installed_agents)?;

        for id in &to_uninstall {
            let ag = tracedecay::agents::get_integration(id)?;
            apply_default_canonical_component_set(
                id,
                HostBundleCliOperation::Uninstall,
                &home,
                true,
            )?;
            removed_names.push(ag.name().to_string());
            user_cfg.installed_agents.retain(|a| a != id);
            user_cfg.agent_dashboard_enabled.remove(id);
        }
        for id in &to_install {
            let ag = tracedecay::agents::get_integration(id)?;
            let context = tracedecay::agents::InstallContext {
                home: home.clone(),
                tracedecay_bin: tracedecay_bin.clone(),
                tool_permissions: tracedecay::agents::expected_tool_perms(),
                project_root: None,
                dashboard: !no_dashboard,
            };
            prepare_native_activation_if_needed(ag.as_ref(), &context)?;
            apply_default_canonical_component_set(
                id,
                HostBundleCliOperation::Install,
                &home,
                !no_dashboard,
            )?;
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
            .map_err(|err| tracedecay::errors::TraceDecayError::Config {
                message: format!("failed to save user config: {err}"),
            })?;
    }

    eprintln!();
    if installed_names.is_empty() && removed_names.is_empty() {
        eprintln!("No changes.");
    } else {
        for name in &installed_names {
            eprintln!("\x1b[32m+\x1b[0m {name}");
        }
        for name in &removed_names {
            eprintln!("\x1b[31m-\x1b[0m {name}");
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
            .map_err(|err| tracedecay::errors::TraceDecayError::Config {
                message: format!("failed to save user config: {err}"),
            })?;
    }

    tracedecay::agents::offer_git_post_commit_hook(&tracedecay_bin);
    Ok(())
}

pub(crate) async fn handle_reinstall_command() -> tracedecay::errors::Result<()> {
    let home = tracedecay::agents::home_dir().ok_or_else(|| {
        tracedecay::errors::TraceDecayError::Config {
            message: "could not determine home directory".to_string(),
        }
    })?;
    let tracedecay_bin = tracedecay::agents::which_tracedecay().ok_or_else(|| {
        tracedecay::errors::TraceDecayError::Config {
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
            .retain(|id| tracedecay::agents::get_integration(id).is_ok());
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
        let results = reinstall_agent_integrations(&agents, &home, &tracedecay_bin).await;
        // Reporting lives in `partition_reinstall_results`, which every
        // reinstall pass shares. Keep the reason with the name — a bare id list
        // left "failed for: claude, cursor, hermes, kimi" undiagnosable.
        match crate::update_cmd::partition_reinstall_results(results) {
            crate::update_cmd::ReinstallOutcome::AllOk => {
                eprintln!("\x1b[32m✔\x1b[0m All agents reinstalled");
            }
            crate::update_cmd::ReinstallOutcome::PartialFailure { failed } => {
                return Err(tracedecay::errors::TraceDecayError::Config {
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

pub(crate) async fn handle_update_plugin_command() -> tracedecay::errors::Result<()> {
    let home = tracedecay::agents::home_dir().ok_or_else(|| {
        tracedecay::errors::TraceDecayError::Config {
            message: "could not determine home directory".to_string(),
        }
    })?;
    let tracedecay_bin = tracedecay::agents::which_tracedecay().ok_or_else(|| {
        tracedecay::errors::TraceDecayError::Config {
            message: "tracedecay not found on PATH".to_string(),
        }
    })?;
    let user_cfg = load_host_lifecycle_user_config()?;

    for id in &user_cfg.installed_agents {
        let integration = tracedecay::agents::get_integration(id)?;
        let dashboard = user_cfg.dashboard_enabled_for_agent(id);
        let context = tracedecay::agents::InstallContext {
            home: home.clone(),
            tracedecay_bin: tracedecay_bin.clone(),
            tool_permissions: tracedecay::agents::expected_tool_perms(),
            project_root: None,
            dashboard,
        };
        prepare_native_activation_if_needed(integration.as_ref(), &context)?;
        apply_default_canonical_component_set(
            id,
            HostBundleCliOperation::Update,
            &home,
            dashboard,
        )?;
    }
    Ok(())
}

pub(crate) fn handle_reinstall_preflight_command() -> tracedecay::errors::Result<()> {
    let home = tracedecay::agents::home_dir().ok_or_else(|| {
        tracedecay::errors::TraceDecayError::Config {
            message: "could not determine home directory".to_string(),
        }
    })?;
    let tracedecay_bin = tracedecay::agents::which_tracedecay().ok_or_else(|| {
        tracedecay::errors::TraceDecayError::Config {
            message: "could not resolve the canonical preflight binary".to_string(),
        }
    })?;
    let user_config = load_host_lifecycle_user_config()?;
    let agent_ids = user_config.installed_agents.clone();
    let project_path = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let health_context = tracedecay::agents::HealthcheckContext {
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
        let integration = match tracedecay::agents::get_integration(id) {
            Ok(integration) => integration,
            Err(_) => {
                eprintln!("  \x1b[33m!\x1b[0m {id}: unknown tracked id; refresh will skip it");
                continue;
            }
        };
        checked += 1;
        let dashboard = user_config.dashboard_enabled_for_agent(id);
        let install_context = tracedecay::agents::InstallContext {
            home: home.clone(),
            tracedecay_bin: tracedecay_bin.clone(),
            tool_permissions: tracedecay::agents::expected_tool_perms(),
            project_root: None,
            dashboard,
        };
        match integration.preflight_non_interactive_install(&install_context) {
            Ok(tracedecay::agents::NonInteractiveInstallOutcome::Ready) => {}
            Ok(tracedecay::agents::NonInteractiveInstallOutcome::DeferredUserAction(action)) => {
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
    Err(tracedecay::errors::TraceDecayError::Config {
        message: format!(
            "integration refresh preflight failed for: {}",
            failures.join(", ")
        ),
    })
}

fn preflight_agent_integration(
    agent_id: &str,
    integration: &dyn tracedecay::agents::AgentIntegration,
    home: &Path,
    health_context: &tracedecay::agents::HealthcheckContext,
    install_context: &tracedecay::agents::InstallContext,
) -> tracedecay::errors::Result<String> {
    use tracedecay::agents::host_bundle_v2::HostBundleRegistrationStateV1;

    let now_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| tracedecay::errors::TraceDecayError::Config {
            message: "system clock is before the Unix epoch".to_string(),
        })?
        .as_secs();
    let Some(component_set) = canonical_host_component_set(agent_id, None, now_unix)? else {
        return Err(tracedecay::errors::TraceDecayError::Config {
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
            return Err(tracedecay::errors::TraceDecayError::Config {
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
    let lifecycle_root = tracedecay::agents::host_bundle_v2::resolved_host_bundle_lifecycle_root()
        .map_err(|error| tracedecay::errors::TraceDecayError::Config {
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
    state: tracedecay::agents::host_bundle_v2::HostBundleRegistrationStateV1,
) -> &'static str {
    use tracedecay::agents::host_bundle_v2::HostBundleRegistrationStateV1;

    match state {
        HostBundleRegistrationStateV1::Current => "current",
        HostBundleRegistrationStateV1::Repairable => "repairable",
        HostBundleRegistrationStateV1::Missing => "missing",
        HostBundleRegistrationStateV1::Corrupt => "corrupt",
    }
}

/// Reconciles the canonical component transaction for each tracked agent id.
///
/// An id that does NOT resolve to an integration (a later release renamed or
/// removed it, or a typo landed in `installed_agents`) is SKIPPED, not failed:
/// it is logged as a warning and left out of the returned results entirely.
/// Gating version-marker advancement on such an id would wedge the reinstall
/// loop forever. Only genuine component-transaction failures are reported as
/// `Err` so they still gate markers.
pub(crate) async fn reinstall_agent_integrations(
    agent_ids: &[String],
    home: &Path,
    tracedecay_bin: &str,
) -> Vec<(String, tracedecay::errors::Result<AgentReinstallOutcome>)> {
    reinstall_agent_integrations_with_persisted_dashboard_policies(agent_ids, home, tracedecay_bin)
        .await
}

/// Reinstalls tracked integrations while reusing lifecycle authority already
/// held by post-update maintenance.
pub(crate) async fn reinstall_agent_integrations_under_lease(
    agent_ids: &[String],
    home: &Path,
    tracedecay_bin: &str,
    lifecycle: &tracedecay::lifecycle_lease::LifecycleLease,
) -> Vec<(String, tracedecay::errors::Result<AgentReinstallOutcome>)> {
    let _ = lifecycle;
    reinstall_agent_integrations_with_persisted_dashboard_policies(agent_ids, home, tracedecay_bin)
        .await
}

async fn reinstall_agent_integrations_with_persisted_dashboard_policies(
    agent_ids: &[String],
    home: &Path,
    tracedecay_bin: &str,
) -> Vec<(String, tracedecay::errors::Result<AgentReinstallOutcome>)> {
    let user_config = match load_host_lifecycle_user_config() {
        Ok(config) => config,
        Err(error) => {
            let message = error.to_string();
            return agent_ids
                .iter()
                .map(|id| {
                    (
                        id.clone(),
                        Err(tracedecay::errors::TraceDecayError::Config {
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
    )
    .await
}

async fn reinstall_agent_integrations_with_dashboard_policies(
    agent_ids: &[String],
    home: &Path,
    tracedecay_bin: &str,
    dashboard_policies: &std::collections::BTreeMap<String, bool>,
) -> Vec<(String, tracedecay::errors::Result<AgentReinstallOutcome>)> {
    let mut results = Vec::new();
    for id in agent_ids {
        let ag = match tracedecay::agents::get_integration(id) {
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
        let context = tracedecay::agents::InstallContext {
            home: home.to_path_buf(),
            tracedecay_bin: tracedecay_bin.to_string(),
            tool_permissions: tracedecay::agents::expected_tool_perms(),
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
) -> tracedecay::errors::Result<()> {
    let home = tracedecay::agents::home_dir().ok_or_else(|| {
        tracedecay::errors::TraceDecayError::Config {
            message: "could not determine home directory".to_string(),
        }
    })?;
    let mut user_cfg = load_host_lifecycle_user_config()?;

    if let Some(id) = agent {
        apply_default_canonical_component_set(&id, HostBundleCliOperation::Uninstall, &home, true)?;
        user_cfg.installed_agents.retain(|a| a != &id);
        user_cfg.agent_dashboard_enabled.remove(&id);
        user_cfg
            .save()
            .map_err(|err| tracedecay::errors::TraceDecayError::Config {
                message: format!("failed to save user config: {err}"),
            })?;
    } else {
        for id in user_cfg.installed_agents.clone() {
            apply_default_canonical_component_set(
                &id,
                HostBundleCliOperation::Uninstall,
                &home,
                true,
            )?;
        }
        user_cfg.installed_agents.clear();
        user_cfg.agent_dashboard_enabled.clear();
        user_cfg
            .save()
            .map_err(|err| tracedecay::errors::TraceDecayError::Config {
                message: format!("failed to save user config: {err}"),
            })?;
        eprintln!("All agent integrations removed.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::ffi::{OsStr, OsString};
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
        component_set_request, reinstall_agent_integrations,
        reinstall_agent_integrations_with_dashboard_policies,
    };
    use tracedecay::agents::host_bundle_v2::{
        CompetingHostExtensionClaimV1, HostBundleError, HostComponentSetExecutionRequestV1,
        HostComponentSetLifecyclePreviewV1, HostComponentSetRegistrationV1, HostComponentSetV1,
    };

    const OPENCODE_UNRELATED_CONFIG: &[u8] = br#"{"lsp":{"other":{"command":["tracedecay","lsp","bridge","--stdio"]}},"unrelated":{"keep":true}}
"#;
    const OPENCODE_CONTEXT_CONFIG: &[u8] = br#"{"mcp":{"tracedecay":{"type":"local","command":["tracedecay","serve"]},"other":{"type":"local","command":["other"]}},"unrelated":{"keep":true}}
"#;
    /// [`PinnedUserDataDir`] gives each test its own profile root (and its own
    /// `HOME`) for the duration of the guard, and holds the crate-wide
    /// user-data-dir lock while the override is installed — the same lock every
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
        use tracedecay::agents::host_bundle_v2::HostArtifactActionV1 as Action;

        let owned = ["plugins/tracedecay.json".to_string()]
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>();

        assert_eq!(
            super::artifact_disposition(
                &Action::BackupThenReplace,
                &owned,
                "plugins/tracedecay.json"
            ),
            "backup-then-replace"
        );
        assert_eq!(
            super::artifact_disposition(&Action::BackupThenReplace, &owned, "plugins/unowned.json"),
            "adopt"
        );
        assert_eq!(
            super::artifact_disposition(&Action::WriteNew, &owned, "plugins/unowned.json"),
            "write-new"
        );
        assert_eq!(
            super::artifact_disposition(
                &Action::BackupThenRemove,
                &owned,
                "plugins/tracedecay.json"
            ),
            "backup-then-remove"
        );
        assert_eq!(
            super::artifact_disposition(&Action::Noop, &owned, "plugins/tracedecay.json"),
            "unchanged"
        );
    }

    /// An explicit component mutation that would claim an unrecorded file is
    /// refused without `--adopt`, and the refusal names every such path plus
    /// where the replaced bytes are preserved.
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
        // A pre-receipt deployment: the cataloged path exists on disk with
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

        let error = super::require_adoption_confirmation(
            "cursor",
            HostBundleCliOperation::Repair,
            &component_set,
            &options,
            home.path(),
            lifecycle.path(),
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains(adopted.as_str()), "{error}");
        assert!(error.contains("--yes --adopt"), "{error}");
        assert!(error.contains("backups"), "{error}");
        assert_eq!(std::fs::read(&deployed).unwrap(), b"pre-receipt");

        let confirmed = crate::cli::HostBundleCliOptions {
            adopt: true,
            ..options
        };
        super::require_adoption_confirmation(
            "cursor",
            HostBundleCliOperation::Repair,
            &component_set,
            &confirmed,
            home.path(),
            lifecycle.path(),
        )
        .unwrap();
        assert_eq!(
            std::fs::read(&deployed).unwrap(),
            b"pre-receipt",
            "the confirmation gate is read-only"
        );
    }

    #[test]
    fn feedback_registration_snapshot_rejects_pre_activation_edit() {
        let _profile = pinned_host_profile();
        let home = tempfile::tempdir().unwrap();
        let integration = tracedecay::agents::get_integration("opencode").unwrap();
        let registration_paths = super::feedback_registration_paths(
            home.path(),
            integration.as_ref(),
            tracedecay::agents::host_bundle_v2::HostBundleComponentV1::Core,
        )
        .unwrap();
        let config = home.path().join(".config/opencode/opencode.json");
        assert!(
            registration_paths.contains(&config),
            "the mutated file must be part of the host registration inventory: {registration_paths:?}"
        );
        let snapshot = super::snapshot_feedback_registration(
            home.path(),
            integration.as_ref(),
            tracedecay::agents::host_bundle_v2::HostBundleComponentV1::Core,
        )
        .unwrap();
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(
            &config,
            br#"{"mcpServers":{"operator":{"command":"foreign"}}}"#,
        )
        .unwrap();

        let error = super::validate_feedback_registration_snapshot(
            home.path(),
            integration.as_ref(),
            tracedecay::agents::host_bundle_v2::HostBundleComponentV1::Core,
            &snapshot,
        )
        .unwrap_err();

        assert!(error.to_string().contains("changed before apply"));
        assert_eq!(
            std::fs::read(&config).unwrap(),
            br#"{"mcpServers":{"operator":{"command":"foreign"}}}"#
        );
    }

    #[cfg(unix)]
    #[test]
    fn feedback_registration_snapshot_rejects_metadata_only_drift() {
        use std::os::unix::fs::PermissionsExt;

        let _profile = pinned_host_profile();
        let home = tempfile::tempdir().unwrap();
        let integration = tracedecay::agents::get_integration("opencode").unwrap();
        let config = home.path().join(".config/opencode/opencode.json");
        assert!(
            super::feedback_registration_paths(
                home.path(),
                integration.as_ref(),
                tracedecay::agents::host_bundle_v2::HostBundleComponentV1::Core,
            )
            .unwrap()
            .contains(&config),
            "the mutated file must be part of the host registration inventory"
        );
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(
            &config,
            br#"{"mcpServers":{"operator":{"command":"keep"}}}"#,
        )
        .unwrap();
        std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o640)).unwrap();
        let snapshot = super::snapshot_feedback_registration(
            home.path(),
            integration.as_ref(),
            tracedecay::agents::host_bundle_v2::HostBundleComponentV1::Core,
        )
        .unwrap();
        std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o600)).unwrap();

        let error = super::validate_feedback_registration_snapshot(
            home.path(),
            integration.as_ref(),
            tracedecay::agents::host_bundle_v2::HostBundleComponentV1::Core,
            &snapshot,
        )
        .unwrap_err();

        assert!(error.to_string().contains("changed before apply"));
        assert_eq!(
            std::fs::metadata(&config).unwrap().permissions().mode() & 0o777,
            0o600
        );
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

    /// Forwards the whole lifecycle to the real authority but always fails
    /// `verify`, which interrupts the transaction after its artifacts are on
    /// disk — the state that leaves a recovery journal behind.
    /// Pinned binary path for the Kiro fixtures. Resolving it from `PATH`
    /// would let a sibling test that swaps `PATH` change these artifacts
    /// mid-test.
    const KIRO_FIXTURE_BIN: &str = "/usr/local/bin/tracedecay";

    struct AlwaysFailVerifyRegistration {
        inner: CatalogHostComponentRegistrationAuthority,
    }

    impl HostComponentSetRegistrationV1 for AlwaysFailVerifyRegistration {
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
            _component_set: &HostComponentSetV1,
            _request: &HostComponentSetExecutionRequestV1,
        ) -> Result<(), HostBundleError> {
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

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: impl AsRef<OsStr>) -> Self {
            let previous = std::env::var_os(key);
            unsafe { std::env::set_var(key, value) };
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(previous) => unsafe { std::env::set_var(self.key, previous) },
                None => unsafe { std::env::remove_var(self.key) },
            }
        }
    }

    /// Keep Kiro lifecycle tests on the native `kiro-cli` route.  The child
    /// receives a cleared environment, so the script only uses absolute
    /// utility paths and derives its profile from the admitted `HOME`.
    #[cfg(unix)]
    fn write_fake_kiro_cli(path: &Path) {
        use std::os::unix::fs::PermissionsExt;

        let body = r#"#!/bin/sh
set -eu
config="$HOME/.kiro/settings/mcp.json"
/bin/mkdir -p "$HOME/.kiro/settings"
case "${1-}:${2-}" in
  mcp:add)
    if [ "${7-}" != "--args" ] || [ "${8-}" != "serve" ] || [ "${9-}" != "--scope" ] || [ "${10-}" != "global" ] || [ "${11-}" != "--force" ]; then
      echo "unexpected kiro-cli mcp add arguments: $*" >&2
      exit 64
    fi
    command="$6"
    if [ -f "$config" ] && /bin/grep -q '"other"' "$config"; then
      printf '{"mcpServers":{"other":{"command":"other","args":[]},"tracedecay":{"command":"%s","args":["serve"]}}}\n' "$command" > "$config"
    else
      printf '{"mcpServers":{"tracedecay":{"command":"%s","args":["serve"]}}}\n' "$command" > "$config"
    fi
    ;;
  mcp:remove)
    if [ "${3-}" != "--name" ] || [ "${4-}" != "tracedecay" ] || [ "${5-}" != "--scope" ] || [ "${6-}" != "global" ]; then
      echo "unexpected kiro-cli mcp remove arguments: $*" >&2
      exit 64
    fi
    if [ -f "$config" ] && /bin/grep -q '"other"' "$config"; then
      printf '{"mcpServers":{"other":{"command":"other","args":[]}}}\n' > "$config"
    else
      /bin/rm -f "$config"
    fi
    ;;
  *)
    echo "unexpected kiro-cli invocation: $*" >&2
    exit 64
    ;;
esac
"#;
        std::fs::write(path, body).expect("write fake kiro-cli");
        let mut permissions = std::fs::metadata(path)
            .expect("fake kiro-cli metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions).expect("chmod fake kiro-cli");
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
        let project = tempfile::tempdir().unwrap();
        let resolved = Arc::new(AtomicBool::new(false));
        let resolver_called = Arc::clone(&resolved);
        let error = broker_codex_daemon_automation_project(
            project.path(),
            |_| async {
                Err(tracedecay::errors::TraceDecayError::Config {
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
            tracedecay::agents::host_bundle_v2::HostBundleComponentV1::ContextMcp
        );
        let hermes = canonical_host_component_set("hermes", None, 0)
            .unwrap()
            .expect("Hermes has a first-party default Core set");
        assert_eq!(hermes.component_set.components.len(), 1);
        assert_eq!(
            hermes.component_set.components[0].manifest.component,
            tracedecay::agents::host_bundle_v2::HostBundleComponentV1::Core
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
            vec![tracedecay::agents::host_bundle_v2::HostBundleComponentV1::ContextMcp]
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
            vec![tracedecay::agents::host_bundle_v2::HostBundleComponentV1::ContextMcp]
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
    fn artifact_restore_refuses_components_with_registration_state() {
        let _profile = pinned_host_profile();
        let home = tempfile::tempdir().unwrap();
        let lifecycle = tempfile::tempdir().unwrap();
        let core = canonical_host_component_set(
            "codex",
            Some(crate::cli::HostBundleComponentArg::Core),
            0,
        )
        .unwrap()
        .unwrap();
        let error = super::ensure_artifact_only_restore_boundary(
            "codex",
            &core,
            home.path(),
            lifecycle.path(),
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("does not manage registration state")
        );

        let artifact_only = canonical_host_component_set(
            "opencode",
            Some(crate::cli::HostBundleComponentArg::Agent),
            0,
        )
        .unwrap()
        .unwrap();
        assert!(
            super::ensure_artifact_only_restore_boundary(
                "opencode",
                &artifact_only,
                home.path(),
                lifecycle.path(),
            )
            .is_ok()
        );
    }

    #[test]
    fn artifact_command_route_backs_up_and_restores_managed_files() {
        let _profile = pinned_host_profile();
        let home = tempfile::tempdir().unwrap();
        let lifecycle = tempfile::tempdir().unwrap();
        let component_set = canonical_host_component_set(
            "opencode",
            Some(crate::cli::HostBundleComponentArg::Agent),
            0,
        )
        .unwrap()
        .unwrap();
        let options = crate::cli::HostBundleCliOptions {
            component: Some(crate::cli::HostBundleComponentArg::Agent),
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

        let backup_id = super::apply_host_bundle_artifact_action_at(
            crate::cli::HostBundleAction::ArtifactBackup {
                agent: "opencode".to_string(),
            },
            options,
            home.path(),
            lifecycle.path(),
            0,
        )
        .unwrap();
        let content = &component_set.component_set.components[0].contents[0];
        std::fs::write(home.path().join(&content.relative_path), b"diverged").unwrap();

        let restore_id = super::apply_host_bundle_artifact_action_at(
            crate::cli::HostBundleAction::ArtifactRestore {
                agent: "opencode".to_string(),
                backup_id: hex::encode(backup_id),
            },
            options,
            home.path(),
            lifecycle.path(),
            0,
        )
        .unwrap();
        assert_ne!(restore_id, backup_id);
        assert_eq!(
            std::fs::read(home.path().join(&content.relative_path)).unwrap(),
            content.bytes
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
        let _kiro_path = EnvVarGuard::set("PATH", kiro_cli_dir.path());
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

    /// A transaction interrupted after it staged registration leaves a journal
    /// behind. The non-interactive path must recover that journal itself, or a
    /// single transient fault wedges every later run behind a manual
    /// `host-bundle recover`.
    #[cfg(unix)]
    #[test]
    fn interrupted_component_set_journal_recovers_on_next_non_interactive_apply() {
        let _profile = pinned_host_profile();
        #[cfg(unix)]
        let kiro_cli_dir = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        let kiro_cli_path = kiro_cli_dir.path().join("kiro-cli");
        #[cfg(unix)]
        write_fake_kiro_cli(&kiro_cli_path);
        #[cfg(unix)]
        let _kiro_path = EnvVarGuard::set("PATH", kiro_cli_dir.path());
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

        // Interrupt a real transaction the way a crash would: run it through a
        // registration authority that fails `verify` after the artifacts are
        // already on disk, which is exactly the state that leaves a journal.
        let request =
            component_set_request(&component_set, HostBundleCliOperation::Install, true).unwrap();
        let mut writer =
            tracedecay::agents::host_bundle_v2::HostBundleWriterV1::open_with_lifecycle_root(
                home.path(),
                lifecycle.path(),
            )
            .unwrap();
        let mut transaction =
            tracedecay::agents::host_bundle_v2::HostComponentSetTransactionV1::new(&mut writer);
        let mut interrupted_registration = AlwaysFailVerifyRegistration {
            inner: CatalogHostComponentRegistrationAuthority::new(
                "kiro",
                home.path(),
                lifecycle.path(),
                request.lifecycle.operation,
            )
            .unwrap(),
        };
        transaction
            .execute(
                &component_set.component_set,
                &request,
                &component_set,
                &mut interrupted_registration,
            )
            .expect_err("the injected verify failure must interrupt this transaction");
        drop(writer);
        let journal_path = lifecycle
            .path()
            .join(".tracedecay-host-bundle-v1/component-set-journal.kiro.v1.json");
        assert!(
            journal_path.is_file(),
            "the interrupted transaction must leave a recovery journal behind"
        );

        // The next non-interactive run must clear the residue by itself.
        apply_canonical_component_set(
            "kiro",
            HostBundleCliOperation::Repair,
            &component_set,
            &options,
            home.path(),
            lifecycle.path(),
            &ComponentSetApplyContext::with_tracedecay_bin(KIRO_FIXTURE_BIN),
        )
        .expect("a leftover journal must be recovered, not turned into a permanent refusal");

        let registered: serde_json::Value = serde_json::from_slice(
            &std::fs::read(home.path().join(".kiro/settings/mcp.json")).unwrap(),
        )
        .unwrap();
        assert!(registered["mcpServers"]["tracedecay"].is_object());
    }

    /// A standing refusal such as an ownership conflict must reach the operator
    /// under its own name. Reporting it as `StalePreview` tells them to retry
    /// something that can never succeed and hides the only actionable
    /// diagnostic they have.
    #[test]
    fn confirmed_apply_reports_an_ownership_conflict_as_itself() {
        use tracedecay::agents::host_bundle_v2::HostBundleError;

        let _profile = pinned_host_profile();
        let home = tempfile::tempdir().unwrap();
        let lifecycle = tempfile::tempdir().unwrap();
        let component_set =
            canonical_host_component_set_with_tracedecay_bin("kiro", None, 0, KIRO_FIXTURE_BIN)
                .unwrap()
                .unwrap();
        std::fs::create_dir_all(home.path().join(".kiro")).unwrap();
        let request =
            component_set_request(&component_set, HostBundleCliOperation::Install, true).unwrap();
        let mut writer =
            tracedecay::agents::host_bundle_v2::HostBundleWriterV1::open_with_lifecycle_root(
                home.path(),
                lifecycle.path(),
            )
            .unwrap();
        let mut transaction =
            tracedecay::agents::host_bundle_v2::HostComponentSetTransactionV1::new(&mut writer);
        let mut registration = CatalogHostComponentRegistrationAuthority::new(
            "kiro",
            home.path(),
            lifecycle.path(),
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

        // Somebody else takes the artifact path between preview and apply.
        let artifact_path = home
            .path()
            .join(&component_set.component_set.components[0].manifest.artifacts[0].relative_path);
        std::fs::create_dir_all(artifact_path.parent().unwrap()).unwrap();
        std::fs::write(&artifact_path, b"{\"owner\":\"somebody else\"}").unwrap();

        let error = transaction
            .execute_confirmed(
                &component_set.component_set,
                &request,
                &preview,
                &component_set,
                &mut registration,
            )
            .expect_err("an unowned file on the artifact path must refuse the apply");
        assert!(
            !matches!(error, HostBundleError::StalePreview(_)),
            "a standing refusal must not be laundered into a retryable staleness report"
        );
        assert_eq!(error, HostBundleError::OwnershipConflict);
    }

    /// A foreign edit landing between `stage` and `apply` must still abort the
    /// transaction. Scoping the apply-time recheck to the paths this
    /// transaction did not declare must not weaken that.
    #[test]
    fn foreign_registration_edit_between_stage_and_apply_still_aborts() {
        use tracedecay::agents::host_bundle_v2::{
            HostBundleError, HostComponentSetLifecyclePreviewV1, HostComponentSetRegistrationV1,
        };

        let _profile = pinned_host_profile();
        let home = tempfile::tempdir().unwrap();
        let lifecycle = tempfile::tempdir().unwrap();
        let component_set = canonical_host_component_set("opencode", None, 0)
            .unwrap()
            .unwrap();
        let request =
            component_set_request(&component_set, HostBundleCliOperation::Install, true).unwrap();
        let mut registration = CatalogHostComponentRegistrationAuthority::new(
            "opencode",
            home.path(),
            lifecycle.path(),
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
        use tracedecay::agents::host_bundle_v2::{
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
            let lifecycle = tempfile::tempdir().unwrap();
            let registration_path = home.path().join(".config/opencode/opencode.json");
            // Create the directory up front: a registration directory that is
            // absent at stage and present at apply is its own (unrelated)
            // staleness signal, and this test is about the file's content.
            std::fs::create_dir_all(registration_path.parent().unwrap()).unwrap();
            let request =
                component_set_request(&component_set, HostBundleCliOperation::Install, true)
                    .unwrap();
            let mut registration = CatalogHostComponentRegistrationAuthority::new(
                "opencode",
                home.path(),
                lifecycle.path(),
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
        use tracedecay::agents::host_bundle_v2::HostComponentSetRegistrationV1;

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
        let request =
            component_set_request(&component_set, HostBundleCliOperation::Install, true).unwrap();
        let mut registration = CatalogHostComponentRegistrationAuthority::new(
            "opencode",
            home.path(),
            lifecycle.path(),
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
        let integration = tracedecay::agents::get_integration("opencode").unwrap();
        let context = tracedecay::agents::InstallContext {
            home: home.path().to_path_buf(),
            tracedecay_bin: "tracedecay".to_string(),
            tool_permissions: Vec::new(),
            project_root: None,
            dashboard: true,
        };

        integration
            .activate_deployed_host_component_registration(
                &[tracedecay::agents::host_bundle_v2::HostBundleComponentV1::OperatorMcp],
                &context,
            )
            .unwrap();
        integration
            .deactivate_deployed_host_component_registration(
                &[tracedecay::agents::host_bundle_v2::HostBundleComponentV1::OperatorMcp],
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
        use tracedecay::agents::host_bundle_v2::HostComponentSetRegistrationV1;

        let _profile = pinned_host_profile();
        let home = tempfile::tempdir().unwrap();
        let lifecycle = tempfile::tempdir().unwrap();
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
            component_set_request(&component_set, HostBundleCliOperation::Install, true).unwrap();
        let mut registration = CatalogHostComponentRegistrationAuthority::new(
            "opencode",
            home.path(),
            lifecycle.path(),
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
        use tracedecay::agents::host_bundle_v2::HostComponentSetRegistrationV1;

        let _profile = pinned_host_profile();
        let home = tempfile::tempdir().unwrap();
        let lifecycle = tempfile::tempdir().unwrap();
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
            component_set_request(&component_set, HostBundleCliOperation::Repair, true).unwrap();
        let mut registration = CatalogHostComponentRegistrationAuthority::new(
            "opencode",
            home.path(),
            lifecycle.path(),
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

    #[test]
    fn codex_core_rollback_restores_generated_agent_exports_byte_for_byte() {
        let _profile = pinned_host_profile();
        let home = tempfile::tempdir().unwrap();
        let lifecycle = tempfile::tempdir().unwrap();
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

        let component_set = canonical_host_component_set(
            "codex",
            Some(crate::cli::HostBundleComponentArg::Core),
            0,
        )
        .unwrap()
        .unwrap();
        let request =
            component_set_request(&component_set, HostBundleCliOperation::Repair, true).unwrap();
        let mut registration = VerifyFailureRegistration {
            inner: CatalogHostComponentRegistrationAuthority::new(
                "codex",
                home.path(),
                lifecycle.path(),
                request.lifecycle.operation,
            )
            .unwrap(),
            stale_export_path: stale_path.clone(),
            stale_export_present_at_verify: true,
            verify_failure_injected: false,
        };
        let mut writer =
            tracedecay::agents::host_bundle_v2::HostBundleWriterV1::open_with_lifecycle_root(
                home.path(),
                lifecycle.path(),
            )
            .unwrap();
        let mut transaction =
            tracedecay::agents::host_bundle_v2::HostComponentSetTransactionV1::new(&mut writer);
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
        let _kiro_path = EnvVarGuard::set("PATH", kiro_cli_dir.path());
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
                // No lifecycle journal may be left behind by a converged apply.
                assert!(
                    tracedecay::agents::host_bundle_v2::HostBundleWriterV1::open_with_lifecycle_root(
                        home.path(),
                        lifecycle.path(),
                    )
                    .unwrap()
                    .pending_component_set_journal_operation(
                        component_set.component_set.host
                    )
                    .unwrap()
                    .is_none()
                );
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
        use tracedecay::agents::host_bundle_v2::HostComponentSetRegistrationV1;

        let mut self_invalidating = Vec::new();
        for agent in [
            "claude", "codex", "cursor", "hermes", "kimi", "kiro", "opencode",
        ] {
            let _profile = pinned_host_profile();
            let home = tempfile::tempdir().unwrap();
            let lifecycle = tempfile::tempdir().unwrap();
            let component_set = canonical_host_component_set(agent, None, 0)
                .unwrap()
                .unwrap_or_else(|| panic!("{agent} must ship a canonical component set"));
            let request =
                component_set_request(&component_set, HostBundleCliOperation::Install, true)
                    .unwrap();
            let registration = CatalogHostComponentRegistrationAuthority::new(
                agent,
                home.path(),
                lifecycle.path(),
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

    /// A rolled-back apply intentionally leaves its journal behind as an
    /// explicit reconciliation boundary. The non-interactive refresh must
    /// recover it before its next attempt instead of wedging until an operator
    /// runs `tracedecay host-bundle recover` by hand.
    #[cfg(all(unix, feature = "test-transport"))]
    #[tokio::test]
    async fn a_wedged_kiro_journal_is_recovered_by_the_next_apply() {
        let _profile = pinned_host_profile();
        #[cfg(unix)]
        let kiro_cli_dir = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        let kiro_cli_path = kiro_cli_dir.path().join("kiro-cli");
        #[cfg(unix)]
        write_fake_kiro_cli(&kiro_cli_path);
        #[cfg(unix)]
        let _kiro_path = EnvVarGuard::set("PATH", kiro_cli_dir.path());
        let tracedecay_bin = std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let home = tempfile::tempdir().unwrap();
        let lifecycle = tempfile::tempdir().unwrap();
        let mcp_path = home.path().join(".kiro/settings/mcp.json");
        std::fs::create_dir_all(mcp_path.parent().unwrap()).unwrap();
        std::fs::write(
            &mcp_path,
            br#"{"mcpServers":{"other":{"command":"other"}}}"#,
        )
        .unwrap();
        let component_set =
            canonical_host_component_set_with_tracedecay_bin("kiro", None, 0, &tracedecay_bin)
                .unwrap()
                .unwrap();
        let options = crate::cli::HostBundleCliOptions {
            component: None,
            dry_run: false,
            yes: true,
            adopt: false,
        };
        let pending_journal = || {
            tracedecay::agents::host_bundle_v2::HostBundleWriterV1::open_with_lifecycle_root(
                home.path(),
                lifecycle.path(),
            )
            .unwrap()
            .pending_component_set_journal_operation(component_set.component_set.host)
            .unwrap()
        };

        // Fail the transaction after registration has already been applied, so
        // it rolls back and leaves the journal exactly as the live defect did.
        let failure = EnvVarGuard::set("TRACEDECAY_TEST_FAIL_HOST_REGISTRATION_VERIFY", "1");
        apply_canonical_component_set(
            "kiro",
            HostBundleCliOperation::Install,
            &component_set,
            &options,
            home.path(),
            lifecycle.path(),
            &ComponentSetApplyContext::with_tracedecay_bin(&tracedecay_bin),
        )
        .unwrap_err();
        drop(failure);
        assert!(
            pending_journal().is_some(),
            "the failed apply must leave its reconciliation boundary behind"
        );

        apply_canonical_component_set(
            "kiro",
            HostBundleCliOperation::Install,
            &component_set,
            &options,
            home.path(),
            lifecycle.path(),
            &ComponentSetApplyContext::with_tracedecay_bin(&tracedecay_bin),
        )
        .expect("the next apply must recover the wedged journal before mutating");
        assert!(pending_journal().is_none());
        let config: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&mcp_path).unwrap()).unwrap();
        assert_eq!(
            config["mcpServers"]["tracedecay"]["command"],
            tracedecay_bin
        );
        assert_eq!(config["mcpServers"]["other"]["command"], "other");
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

        assert!(error.to_string().contains("ownership marker conflicts"));
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
        let _kimi_home = EnvVarGuard::set(tracedecay::agents::kimi::KIMI_CODE_HOME_ENV, &code_home);
        let installed_path = code_home.join("plugins/installed.json");
        std::fs::create_dir_all(installed_path.parent().unwrap()).unwrap();
        let original = br#"{"version":1,"plugins":[{"id":"tracedecay","enabled":false}]}
"#;
        std::fs::write(&installed_path, original).unwrap();

        let results =
            reinstall_agent_integrations(&["kimi".to_string()], home.path(), "new-tracedecay")
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
        use tracedecay::agents::host_bundle_v2::{
            HostBundleComponentV1, HostKindV1, latest_host_component_receipt_at,
            resolved_host_bundle_lifecycle_root,
        };

        let _profile = pinned_host_profile();
        let home = tempfile::tempdir().unwrap();
        let code_home = home.path().join(".kimi-code");
        let _kimi_home = EnvVarGuard::set(tracedecay::agents::kimi::KIMI_CODE_HOME_ENV, &code_home);
        let integration = tracedecay::agents::get_integration("kimi").unwrap();
        let ctx = tracedecay::agents::InstallContext {
            home: home.path().to_path_buf(),
            tracedecay_bin: "new-tracedecay".to_string(),
            tool_permissions: tracedecay::agents::expected_tool_perms(),
            project_root: None,
            dashboard: true,
        };
        assert!(matches!(
            integration.prepare_non_interactive_install(&ctx).unwrap(),
            tracedecay::agents::NonInteractiveInstallOutcome::DeferredUserAction(_)
        ));
        let staged = home
            .path()
            .join(".tracedecay/host-bundle-stage/kimi/tracedecay")
            .canonicalize()
            .unwrap();
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
                    "root": staged,
                }],
            })
            .to_string(),
        )
        .unwrap();

        let results =
            reinstall_agent_integrations(&["kimi".to_string()], home.path(), "new-tracedecay")
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
                HostBundleComponentV1::Core,
            )
            .unwrap()
            .is_some()
        );
    }

    #[tokio::test]
    async fn codex_native_activated_retry_tracks_component_set() {
        use tracedecay::agents::host_bundle_v2::{
            HostBundleComponentV1, HostKindV1, latest_host_component_receipt_at,
            resolved_host_bundle_lifecycle_root,
        };

        let _profile = pinned_host_profile();
        let home = tempfile::tempdir().unwrap();
        // The reinstall path renders the canonical Codex component set with
        // the PATH-resolved binary, and the host-native activation probe
        // compares the staged source byte-for-byte against that rendering.
        // Stage with the same identity or the probe reports a stale cache.
        let tracedecay_bin =
            tracedecay::agents::which_tracedecay().unwrap_or_else(|| "tracedecay".to_string());
        let integration = tracedecay::agents::get_integration("codex").unwrap();
        let ctx = tracedecay::agents::InstallContext {
            home: home.path().to_path_buf(),
            tracedecay_bin: tracedecay_bin.clone(),
            tool_permissions: tracedecay::agents::expected_tool_perms(),
            project_root: None,
            dashboard: true,
        };
        assert!(matches!(
            integration.prepare_non_interactive_install(&ctx).unwrap(),
            tracedecay::agents::NonInteractiveInstallOutcome::DeferredUserAction(_)
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

        let results =
            reinstall_agent_integrations(&["codex".to_string()], home.path(), &tracedecay_bin)
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
                HostBundleComponentV1::Core,
            )
            .unwrap()
            .is_some()
        );

        std::fs::write(
            &cache_manifest,
            br#"{"name":"tracedecay","version":"stale"}"#,
        )
        .unwrap();
        let stale =
            reinstall_agent_integrations(&["codex".to_string()], home.path(), &tracedecay_bin)
                .await;
        assert!(
            matches!(stale.as_slice(), [(id, Err(error))] if id == "codex" && error.to_string().contains("loaded TraceDecay cache is stale")),
            "{stale:?}"
        );
        std::fs::copy(
            home.path()
                .join(".codex/plugins/tracedecay/.codex-plugin/plugin.json"),
            &cache_manifest,
        )
        .unwrap();
        let recovered =
            reinstall_agent_integrations(&["codex".to_string()], home.path(), &tracedecay_bin)
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
        let home = tempfile::tempdir().unwrap();
        let lifecycle = tempfile::tempdir().unwrap();
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
        let _kimi_home = EnvVarGuard::set(tracedecay::agents::kimi::KIMI_CODE_HOME_ENV, &code_home);
        let _path = EnvVarGuard::set("PATH", empty_path.path());
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
            assert!(error.contains("host capability is unsupported"));
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
        use tracedecay::agents::host_bundle_v2::HostComponentSetRegistrationV1;

        let _profile = pinned_host_profile();
        let home = tempfile::tempdir().unwrap();
        let lifecycle = tempfile::tempdir().unwrap();
        let empty_path = tempfile::tempdir().unwrap();
        let code_home = home.path().join(".kimi-code");
        let _kimi_home = EnvVarGuard::set(tracedecay::agents::kimi::KIMI_CODE_HOME_ENV, &code_home);
        let _path = EnvVarGuard::set("PATH", empty_path.path());
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
            component_set_request(&component_set, HostBundleCliOperation::Install, true).unwrap();
        let mut registration = CatalogHostComponentRegistrationAuthority::new(
            "kimi",
            home.path(),
            lifecycle.path(),
            request.lifecycle.operation,
        )
        .unwrap();
        assert_eq!(
            registration.preflight(&component_set.component_set, &request),
            Err(tracedecay::agents::host_bundle_v2::HostBundleError::UnsupportedCapability)
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
            vec![tracedecay::agents::host_bundle_v2::HostBundleComponentV1::ContextMcp]
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
        use tracedecay::agents::host_bundle_v2::{
            HostBundleError, HostComponentSetLifecyclePreviewV1, HostComponentSetRegistrationV1,
        };

        let _profile = pinned_host_profile();
        let home = tempfile::tempdir().unwrap();
        let lifecycle = tempfile::tempdir().unwrap();
        let component_set = canonical_host_component_set("opencode", None, 0)
            .unwrap()
            .unwrap();
        let request =
            component_set_request(&component_set, HostBundleCliOperation::Install, true).unwrap();
        let mut registration = CatalogHostComponentRegistrationAuthority::new(
            "opencode",
            home.path(),
            lifecycle.path(),
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

    #[tokio::test]
    async fn hermes_dashboard_opt_out_survives_update_and_reinstall() {
        let _profile = pinned_host_profile();
        let home = tempfile::tempdir().unwrap();
        let tracedecay_bin = std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let plugin = home.path().join(".hermes/plugins/tracedecay");

        apply_default_canonical_component_set(
            "hermes",
            HostBundleCliOperation::Install,
            home.path(),
            false,
        )
        .unwrap();
        apply_default_canonical_component_set(
            "hermes",
            HostBundleCliOperation::Update,
            home.path(),
            false,
        )
        .unwrap();
        assert!(!plugin.join("dashboard/manifest.json").exists());
        assert!(!plugin.join("dashboard/plugin_api.py").exists());
        assert!(!plugin.join("dashboard/dist/index.js").exists());

        let policies = std::collections::BTreeMap::from([("hermes".to_string(), false)]);
        let results = reinstall_agent_integrations_with_dashboard_policies(
            &["hermes".to_string()],
            home.path(),
            &tracedecay_bin,
            &policies,
        )
        .await;
        assert!(matches!(
            results.as_slice(),
            [(id, Ok(AgentReinstallOutcome::Installed))] if id == "hermes"
        ));
        assert!(!plugin.join("dashboard/manifest.json").exists());
        assert!(!plugin.join("dashboard/plugin_api.py").exists());
        assert!(!plugin.join("dashboard/dist/index.js").exists());
    }
}
