//! Project-open source access and capability-grant authorization.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

use tracedecay_contracts::{ApplicationContractError, CapabilityGrantSnapshot, ResolvedScope};
use tracedecay_domain::configuration::{
    ACCESS_RULES_SETTING_KEY, AuthorityRef, CapabilityResolutionContextV1, ConfigurationValueV1,
    SettingKey, resolve_restrictive_capabilities,
};
use tracedecay_domain::{ActorId, CapabilityId as DomainCapabilityId, UtcMicros, canonical_sha256};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

use crate::source_authorization::{ProjectSourceAccessSnapshot, ProjectSourceAccessSnapshotPort};

const POLICY_REVISION_V1: u64 = 1;

#[derive(Clone)]
pub struct ProjectOpenSourceAccessAuthorityV1 {
    requester: ActorId,
    granted_capabilities: BTreeSet<CapabilityId>,
    grant_horizon: Duration,
}

impl ProjectOpenSourceAccessAuthorityV1 {
    pub fn new(
        requester: ActorId,
        granted_capabilities: BTreeSet<CapabilityId>,
        grant_horizon: Duration,
    ) -> Self {
        Self {
            requester,
            granted_capabilities,
            grant_horizon,
        }
    }
}

impl ProjectSourceAccessSnapshotPort for ProjectOpenSourceAccessAuthorityV1 {
    fn source_access_at(
        &self,
        scope: &ResolvedScope,
        project_root: &Path,
        configuration: &tracedecay_configuration::config::PinnedRuntimeConfiguration,
        observed_at: UtcMicros,
    ) -> Result<ProjectSourceAccessSnapshot, ApplicationContractError> {
        let grant_expires_at = UtcMicros(
            observed_at
                .0
                .saturating_add(i64::try_from(self.grant_horizon.as_micros()).unwrap_or(i64::MAX)),
        );
        project_open_source_access_at(
            scope,
            project_root,
            configuration,
            self.requester.clone(),
            self.granted_capabilities.clone(),
            observed_at,
            grant_expires_at,
        )
    }
}

fn project_open_source_access_at(
    scope: &ResolvedScope,
    project_root: &Path,
    configuration: &tracedecay_configuration::config::PinnedRuntimeConfiguration,
    requester: ActorId,
    granted_capabilities: BTreeSet<CapabilityId>,
    observed_at: UtcMicros,
    grant_expires_at: UtcMicros,
) -> Result<ProjectSourceAccessSnapshot, ApplicationContractError> {
    if configuration.target().project_id != scope.project_id {
        return Err(ApplicationContractError::Inconsistent {
            field: "project-open configuration project",
        });
    }
    if grant_expires_at <= observed_at {
        return Err(ApplicationContractError::Inconsistent {
            field: "project-open source access grant",
        });
    }
    configuration
        .snapshot()
        .validate()
        .map_err(|_| ApplicationContractError::Inconsistent {
            field: "project-open configuration snapshot",
        })?;
    let expected_binding =
        tracedecay_configuration::config::scope_control::daemon_owned_project_source_binding(
            &scope.project_id,
            project_root,
        )
        .map_err(|_| ApplicationContractError::Inconsistent {
            field: "project-open source binding",
        })?;
    let bindings_key = SettingKey::new(
        tracedecay_domain::configuration::SOURCE_BINDINGS_SETTING_KEY,
    )
    .map_err(|_| ApplicationContractError::Inconsistent {
        field: "project-open source bindings key",
    })?;
    let Some(ConfigurationValueV1::SourceBindings(bindings)) =
        configuration.snapshot().effective_values.get(&bindings_key)
    else {
        return Err(ApplicationContractError::Inconsistent {
            field: "project-open source bindings",
        });
    };
    let authority = AuthorityRef::Project(scope.project_id.clone());
    let configured_bindings = bindings
        .iter()
        .filter(|candidate| {
            candidate.source_kind == expected_binding.source_kind
                && candidate.authority == authority
        })
        .collect::<Vec<_>>();
    let [binding] = configured_bindings.as_slice() else {
        return Err(ApplicationContractError::Inconsistent {
            field: "project-open source binding authority",
        });
    };
    if binding.source_locator_digest != expected_binding.source_locator_digest {
        return Err(ApplicationContractError::Inconsistent {
            field: "project-open source binding authority",
        });
    }
    let access_rules_key = SettingKey::new(ACCESS_RULES_SETTING_KEY).map_err(|_| {
        ApplicationContractError::Inconsistent {
            field: "project-open access rules key",
        }
    })?;
    let Some(ConfigurationValueV1::AccessRules(access_rules)) = configuration
        .snapshot()
        .effective_values
        .get(&access_rules_key)
    else {
        return Err(ApplicationContractError::Inconsistent {
            field: "project-open access rules",
        });
    };
    let granted_capabilities = granted_capabilities
        .into_iter()
        .map(|capability| DomainCapabilityId::new(capability.as_str().to_owned()))
        .collect::<Result<BTreeSet<_>, _>>()
        .map_err(|_| ApplicationContractError::Inconsistent {
            field: "project-open granted capabilities",
        })?;
    let resolution = resolve_restrictive_capabilities(
        granted_capabilities,
        access_rules,
        &CapabilityResolutionContextV1 {
            actor: requester.clone(),
            operation: None,
            source_kind: binding.source_kind,
            authority,
            evaluated_at: observed_at,
        },
    )
    .map_err(|_| ApplicationContractError::Inconsistent {
        field: "project-open capability resolution",
    })?;
    let effective_capabilities = resolution
        .effective
        .into_iter()
        .map(|capability| CapabilityId::new(capability.as_str().to_owned()))
        .collect::<Result<BTreeSet<_>, _>>()
        .map_err(|_| ApplicationContractError::Inconsistent {
            field: "project-open effective capabilities",
        })?;
    Ok(ProjectSourceAccessSnapshot {
        scope: scope.clone(),
        requester,
        binding: (**binding).clone(),
        configuration_revision: configuration.revision_id().clone(),
        configuration_digest: configuration.snapshot().effective_behavior_digest.clone(),
        configuration_provenance_digest: configuration
            .snapshot()
            .resolution_provenance_digest
            .clone(),
        effective_capabilities,
        grant_expires_at,
    })
}

pub fn project_open_work_capabilities() -> Result<BTreeSet<CapabilityId>, ApplicationContractError>
{
    tracedecay_contracts::WORK_APPLICATION_OPERATION_IDS_V1
        .into_iter()
        .chain(tracedecay_contracts::WORKFLOW_APPLICATION_OPERATION_IDS)
        .chain(tracedecay_contracts::HANDOFF_APPLICATION_OPERATION_IDS_V1)
        .map(|(_, capability, _)| CapabilityId::new(capability))
        .collect::<Result<BTreeSet<_>, _>>()
        .map_err(|_| ApplicationContractError::Inconsistent {
            field: "project-open Work capability",
        })
}

pub fn project_open_work_grant(
    access: &ProjectSourceAccessSnapshot,
    observed_at: UtcMicros,
) -> Result<Option<CapabilityGrantSnapshot>, ApplicationContractError> {
    let capabilities = project_open_work_capabilities()?;
    if observed_at >= access.grant_expires_at {
        return Err(ApplicationContractError::Inconsistent {
            field: "project-open Work capability grant",
        });
    }
    if !capabilities
        .iter()
        .all(|capability| access.effective_capabilities.contains(capability))
    {
        return Ok(None);
    }
    let use_cases = tracedecay_contracts::WORK_APPLICATION_OPERATION_IDS_V1
        .iter()
        .chain(tracedecay_contracts::WORKFLOW_APPLICATION_OPERATION_IDS.iter())
        .chain(tracedecay_contracts::HANDOFF_APPLICATION_OPERATION_IDS_V1.iter())
        .map(|(_, _, use_case)| UseCaseId::new(*use_case))
        .collect::<Result<BTreeSet<_>, _>>()
        .map_err(|_| ApplicationContractError::Inconsistent {
            field: "project-open Work use cases",
        })?;
    let grant_digest = canonical_sha256(&(
        "tracedecay.project-open.work-grant.v1",
        &access.scope,
        &access.requester,
        &access.binding,
        &access.effective_capabilities,
        &capabilities,
        &use_cases,
    ))
    .map_err(|_| ApplicationContractError::Inconsistent {
        field: "project-open Work grant digest",
    })?;
    CapabilityGrantSnapshot::new(
        tracedecay_contracts::CapabilityGrantId::new(format!(
            "grant.tracedecay-daemon.project-open.work.{}",
            grant_digest.as_str().trim_start_matches("sha256:")
        ))?,
        POLICY_REVISION_V1,
        grant_digest,
        access.requester.clone(),
        observed_at,
        access.grant_expires_at,
        access.scope.clone(),
        capabilities,
        use_cases,
        tracedecay_contracts::DisclosureClass::Sensitive,
    )
    .map(Some)
}

#[cfg(test)]
mod work_grant_tests;
