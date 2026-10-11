//! Work, workflow and Remote Brain stores, which write through the writer's
//! exact-SQL lane rather than typed payloads.

use std::collections::BTreeSet;
use std::fmt::Debug;

use tracedecay_contracts::remote::capture::RemoteCapturePortV1;
use tracedecay_contracts::{
    WorkPlacementStoragePort, WorkflowEffectAuthorityPortV1, WorkflowEffectOperationV1,
    WorkflowEffectPreparedV1, WorkflowLifecycleOperation,
};
use tracedecay_domain::{UtcMicros, WorkPlacementBlockerV1};
use tracedecay_runtime_core::db::TestDatabaseRuntimeScope;
use tracedecay_store::StoreShardScopeV1;

use super::super::fixtures::{project_id, stores};
use super::registered::Registered;

fn debug<T: Debug, E: Debug>(result: Result<T, E>) -> Result<String, String> {
    result
        .map(|value| format!("{value:?}"))
        .map_err(|error| format!("{error:?}"))
}

pub async fn work_and_workflow() -> String {
    let project_id = project_id();
    let mut store = Registered::publish(
        "work.db",
        TestDatabaseRuntimeScope::ProjectSessions {
            project_id: project_id.clone(),
        },
    )
    .await;

    match store.database.work_storage() {
        Ok(work) => {
            let authority = stores::work_authority();
            let admitted = stores::admitted_placement();
            store.trace.note(
                "work_placement_admit",
                debug(work.publish_placement(&authority, None, &admitted)),
            );
            let quarantined = admitted
                .release(
                    BTreeSet::from([WorkPlacementBlockerV1::UniqueCommits]),
                    UtcMicros(400),
                )
                .unwrap();
            store.trace.note(
                "work_placement_quarantine",
                debug(work.publish_placement(
                    &authority,
                    Some(admitted.authority_version()),
                    &quarantined,
                )),
            );
            store.trace.note(
                "work_placement_stale_version",
                debug(work.publish_placement(
                    &authority,
                    Some(admitted.authority_version()),
                    &quarantined,
                )),
            );
        }
        Err(error) => store.trace.note("work_storage", debug::<(), _>(Err(error))),
    }

    match store.database.workflow_storage() {
        Ok(workflow) => {
            let definition = stores::workflow_definition(&project_id);
            let register = stores::workflow_effect(
                &project_id,
                WorkflowEffectOperationV1::RegisterDefinition,
                'p',
            );
            let prepared = WorkflowEffectPreparedV1::register_definition(
                register.input_digest().clone(),
                definition.clone(),
            );
            store.trace.note(
                "workflow_register_definition",
                debug(workflow.execute_effect(&register, &prepared, UtcMicros(20))),
            );
            store.trace.note(
                "workflow_register_definition_replay",
                debug(workflow.execute_effect(&register, &prepared, UtcMicros(21))),
            );
            let activate = stores::workflow_effect(
                &project_id,
                WorkflowEffectOperationV1::ActivateDefinition,
                'q',
            );
            let prepared = WorkflowEffectPreparedV1::activate_definition(
                activate.input_digest().clone(),
                stores::workflow_lifecycle(
                    &definition,
                    WorkflowLifecycleOperation::Activate,
                    1,
                    50,
                ),
            );
            store.trace.note(
                "workflow_activate_definition",
                debug(workflow.execute_effect(&activate, &prepared, UtcMicros(40))),
            );
            let retire = stores::workflow_effect(
                &project_id,
                WorkflowEffectOperationV1::RetireDefinition,
                'r',
            );
            let prepared = WorkflowEffectPreparedV1::retire_definition(
                retire.input_digest().clone(),
                stores::workflow_lifecycle(&definition, WorkflowLifecycleOperation::Retire, 3, 60),
            );
            store.trace.note(
                "workflow_retire_definition",
                debug(workflow.execute_effect(&retire, &prepared, UtcMicros(40))),
            );
        }
        Err(error) => store
            .trace
            .note("workflow_storage", debug::<(), _>(Err(error))),
    }
    store.finish()
}

pub async fn remote() -> String {
    let mut store =
        Registered::publish("remote-node.db", TestDatabaseRuntimeScope::RemoteNode).await;
    let StoreShardScopeV1::RemoteNode { node_id } = store.binding.shard_id.scope.clone() else {
        panic!("remote-node fixture publishes a remote-node shard");
    };
    let epoch = store.binding.authority_epoch.get();
    match store.database.provision_remote_storage(stores::keyring()) {
        Ok(remote) => {
            let grant = stores::enrollment_grant(node_id.as_str(), epoch);
            store.trace.note(
                "remote_store_enrollment_grant",
                debug(remote.store_enrollment_grant(&grant, &stores::enrollment_admission(&grant))),
            );
            store.trace.note(
                "remote_publish_writer_authority",
                debug(remote.publish_authority(
                    &stores::remote_writer(epoch),
                    &stores::replay_policy(epoch),
                    UtcMicros(10),
                )),
            );
            let capture = stores::admitted_capture(node_id.as_str(), epoch);
            store.trace.note(
                "remote_capture_pending",
                debug(
                    remote
                        .capture_pending(&capture)
                        .map(|receipt| receipt.disposition),
                ),
            );
            store.trace.note(
                "remote_capture_pending_again",
                debug(
                    remote
                        .capture_pending(&capture)
                        .map(|receipt| receipt.disposition),
                ),
            );
        }
        Err(error) => store
            .trace
            .note("remote_storage", debug::<(), _>(Err(error))),
    }
    store.finish()
}
