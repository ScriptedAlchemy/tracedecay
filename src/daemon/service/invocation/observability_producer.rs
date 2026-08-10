use super::*;

const DAEMON_OBSERVABILITY_PRODUCER_REVISION: &str = "tracedecay-daemon-observability.v1";
const DAEMON_OBSERVABILITY_QUEUE_CAPACITY: usize = 1_024;
const DAEMON_DELIVERY_SETTLEMENT_QUEUE_CAPACITY: usize = 1_024;

impl DaemonInvocationService {
    pub(crate) async fn mount_observability_producer(
        &self,
        project_root: PathBuf,
        database: Arc<crate::global_db::RegisteredGlobalDb>,
        project_id: ProjectId,
        configuration_revision: ManifestDigest,
        policy_revision: ManifestDigest,
    ) -> Result<
        Arc<tracedecay_usecases::observability::BoundedObservabilityProducerV1>,
        TraceDecayError,
    > {
        let identity = tracedecay_usecases::observability::ObservabilityProducerIdentityV1 {
            authorized_scope_ref: project_id.as_str().to_owned(),
            process_boot_id: format!("daemon:{}", crate::runtime_identity::process_run_id()),
            producer_revision: DAEMON_OBSERVABILITY_PRODUCER_REVISION.to_owned(),
            configuration_revision: configuration_revision.as_str().to_owned(),
            policy_revision: policy_revision.as_str().to_owned(),
        };
        self.project_runtimes
            .register_or_reconcile(
                project_root.clone(),
                |registered: &mut RegisteredObservabilityProducerV1| {
                    registered.matches(&database, &identity).then_some(()).ok_or_else(|| {
                        TraceDecayError::Config {
                            message: "a different observability producer is already mounted for this project"
                                .to_owned(),
                        }
                    })
                },
                || {
                    let producer = tracedecay_usecases::observability::BoundedObservabilityProducerV1::start(
                        Arc::clone(&database),
                        identity.clone(),
                        DAEMON_OBSERVABILITY_QUEUE_CAPACITY,
                    )
                    .map_err(|error| TraceDecayError::Config {
                        message: format!("project observability producer mount failed: {error}"),
                    })?;
                    RegisteredObservabilityProducerV1::new(
                        Arc::clone(&database),
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

    pub(crate) fn observability_producer_for_project_id(
        &self,
        project_id: &ProjectId,
    ) -> Option<Arc<tracedecay_usecases::observability::BoundedObservabilityProducerV1>> {
        self.project_runtimes
            .find_current::<RegisteredObservabilityProducerV1, _, _>(|registered| {
                let producer = registered.producer();
                (producer.identity().authorized_scope_ref == project_id.as_str())
                    .then_some(producer)
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
    /// widening response-path reads to every run. The production SQLite port
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
