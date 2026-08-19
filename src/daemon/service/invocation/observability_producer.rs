use std::sync::atomic::{AtomicU64, Ordering};

use super::*;

const DAEMON_OBSERVABILITY_PRODUCER_REVISION: &str = "tracedecay-daemon-observability.v1";
const DAEMON_OBSERVABILITY_QUEUE_CAPACITY: usize = 1_024;
const DAEMON_DELIVERY_SETTLEMENT_QUEUE_CAPACITY: usize = 1_024;
static NEXT_DAEMON_OBSERVABILITY_PRODUCER_REGISTRATION: AtomicU64 = AtomicU64::new(1);

fn daemon_observability_producer_identity(
    project_id: &ProjectId,
    configuration_revision: &ManifestDigest,
    policy_revision: &ManifestDigest,
) -> Result<tracedecay_usecases::observability::ObservabilityProducerIdentityV1, TraceDecayError> {
    let registration = NEXT_DAEMON_OBSERVABILITY_PRODUCER_REGISTRATION
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
            current.checked_add(1)
        })
        .map_err(|_| TraceDecayError::Config {
            message: "daemon observability producer registrations are exhausted".to_owned(),
        })?;
    Ok(
        tracedecay_usecases::observability::ObservabilityProducerIdentityV1 {
            authorized_scope_ref: project_id.as_str().to_owned(),
            process_boot_id: format!(
                "daemon:{}:{registration}",
                crate::runtime_identity::process_run_id()
            ),
            producer_revision: DAEMON_OBSERVABILITY_PRODUCER_REVISION.to_owned(),
            configuration_revision: configuration_revision.as_str().to_owned(),
            policy_revision: policy_revision.as_str().to_owned(),
        },
    )
}

fn registered_observability_producer_matches_mount(
    registered: &RegisteredObservabilityProducerV1,
    database: &crate::global_db::RegisteredGlobalDbLeaseV1,
    project_id: &ProjectId,
    configuration_revision: &ManifestDigest,
    policy_revision: &ManifestDigest,
) -> bool {
    let incumbent = registered.producer();
    registered.matches(
        database,
        &tracedecay_usecases::observability::ObservabilityProducerIdentityV1 {
            authorized_scope_ref: project_id.as_str().to_owned(),
            process_boot_id: incumbent.identity().process_boot_id.clone(),
            producer_revision: DAEMON_OBSERVABILITY_PRODUCER_REVISION.to_owned(),
            configuration_revision: configuration_revision.as_str().to_owned(),
            policy_revision: policy_revision.as_str().to_owned(),
        },
    )
}

impl DaemonInvocationService {
    pub(crate) async fn mount_observability_producer(
        &self,
        project_root: PathBuf,
        database: crate::global_db::RegisteredGlobalDbLeaseV1,
        project_id: ProjectId,
        configuration_revision: ManifestDigest,
        policy_revision: ManifestDigest,
    ) -> Result<
        Arc<tracedecay_usecases::observability::BoundedObservabilityProducerV1>,
        TraceDecayError,
    > {
        self.project_runtimes
            .register_or_reconcile(
                project_root.clone(),
                |registered: &mut RegisteredObservabilityProducerV1| {
                    registered_observability_producer_matches_mount(
                        registered,
                        &database,
                        &project_id,
                        &configuration_revision,
                        &policy_revision,
                    )
                    .then_some(())
                    .ok_or_else(|| TraceDecayError::Config {
                        message:
                            "a different observability producer is already mounted for this project"
                                .to_owned(),
                    })
                },
                || {
                    let identity = daemon_observability_producer_identity(
                        &project_id,
                        &configuration_revision,
                        &policy_revision,
                    )?;
                    let producer =
                        tracedecay_usecases::observability::BoundedObservabilityProducerV1::start(
                            database.clone(),
                            identity,
                            DAEMON_OBSERVABILITY_QUEUE_CAPACITY,
                        )
                        .map_err(|error| TraceDecayError::Config {
                            message: format!(
                                "project observability producer mount failed: {error}"
                            ),
                        })?;
                    RegisteredObservabilityProducerV1::new(
                        database.clone(),
                        producer,
                        DAEMON_DELIVERY_SETTLEMENT_QUEUE_CAPACITY,
                    )
                    .map_err(|error| TraceDecayError::Config {
                        message: format!("project delivery settlement mount failed: {error}"),
                    })
                },
            )
            .await?;
        self.observability_producer(Some(&project_root))
            .await
            .ok_or_else(|| TraceDecayError::Config {
                message: "mounted observability producer is unavailable".to_owned(),
            })
    }

    pub(crate) async fn observability_producer(
        &self,
        project_root: Option<&Path>,
    ) -> Option<Arc<tracedecay_usecases::observability::BoundedObservabilityProducerV1>> {
        self.project_runtimes
            .read::<RegisteredObservabilityProducerV1, _, _>(project_root?, |registered| {
                registered.producer()
            })
            .await
    }

    /// The mounted producer together with the exact project session database
    /// it writes through, for owners that also record directly through the
    /// registered observation authority.
    pub(crate) async fn observability_producer_with_database(
        &self,
        project_root: Option<&Path>,
    ) -> Option<(
        crate::global_db::RegisteredGlobalDbLeaseV1,
        Arc<tracedecay_usecases::observability::BoundedObservabilityProducerV1>,
    )> {
        self.project_runtimes
            .read::<RegisteredObservabilityProducerV1, _, _>(project_root?, |registered| {
                (registered.database(), registered.producer())
            })
            .await
    }

    pub(crate) fn observability_producer_for_project_root(
        &self,
        project_root: &Path,
    ) -> Option<Arc<tracedecay_usecases::observability::BoundedObservabilityProducerV1>> {
        self.project_runtimes
            .read_now::<RegisteredObservabilityProducerV1, _, _>(project_root, |registered| {
                registered.producer()
            })
    }

    pub(crate) fn observability_producer_for_brain_profile_project(
        &self,
        brain_id: &tracedecay_domain::BrainId,
        profile_id: &tracedecay_domain::UserProfileId,
        project_id: &ProjectId,
    ) -> Option<Arc<tracedecay_usecases::observability::BoundedObservabilityProducerV1>> {
        self.project_runtimes
            .find_equivalent::<RegisteredObservabilityProducerV1, _, _, _>(|registered| {
                let database = registered.database();
                let binding = database.binding();
                let tracedecay_store::StoreShardScopeV1::ProjectSessions {
                    project_id: registered_project,
                } = &binding.shard_id.scope
                else {
                    return None;
                };
                if &binding.shard_id.brain_id != brain_id
                    || &binding.shard_id.profile_id != profile_id
                    || registered_project != project_id
                {
                    return None;
                }
                let producer = registered.producer();
                (producer.identity().authorized_scope_ref == project_id.as_str()).then(|| {
                    (
                        (binding.clone(), database.verified_locator().clone()),
                        producer,
                    )
                })
            })
    }

    pub(crate) async fn delivery_settlement_authority(
        &self,
        project_root: Option<&Path>,
    ) -> Result<
        Option<Arc<tracedecay_usecases::observability::DeliverySettlementAuthorityV1>>,
        &'static str,
    > {
        let Some(project_root) = project_root else {
            return Ok(None);
        };
        Ok(self
            .project_runtimes
            .read::<RegisteredObservabilityProducerV1, _, _>(project_root, |registered| {
                registered.delivery_settlement_authority()
            })
            .await)
    }

    pub(crate) async fn delivery_settlement_recorder(
        &self,
        project_root: Option<&Path>,
    ) -> Option<Arc<tracedecay_usecases::observability::BoundedDeliverySettlementRecorderV1>> {
        self.project_runtimes
            .read::<RegisteredObservabilityProducerV1, _, _>(project_root?, |registered| {
                registered.delivery_settlement_recorder()
            })
            .await
    }

    /// Resolve the durable workflow owner for an exact Work attempt without
    /// widening response-path reads to every run. The production `SQLite` port
    /// answers this through the run journal primary key.
    pub(crate) async fn work_fan_out_binding(
        &self,
        project_root: Option<&Path>,
        identity: &tracedecay_domain::WorkAttemptIdentityV1,
    ) -> Option<tracedecay_application::WorkflowFanOutAttemptBindingV1> {
        let runtime = self
            .project_runtimes
            .get::<RegisteredWorkRuntime>(project_root?)
            .await?;
        let workflow = runtime.database.workflow_application_services().ok()?;
        tracedecay_application::WorkflowRunStoragePort::fan_out_binding(
            workflow.effects(),
            identity,
        )
        .ok()
        .flatten()
    }
}
