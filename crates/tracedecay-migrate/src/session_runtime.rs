use std::sync::Arc;

use tokio::sync::Mutex;
use tracedecay_runtime_core::db::DatabaseAuthority;
use tracedecay_runtime_core::errors::{Result, TraceDecayError};
use tracedecay_runtime_core::store_runtime::registry::{
    LifecycleShardRuntimePublisher, ProfileAuthorityPin, ProfileAuthorityPinResult,
    StoreRuntimeHandle, StoreRuntimeKey, StoreRuntimeOpenRequest, StoreRuntimeOpenResult,
    StoreRuntimeRegistry, StoreRuntimeRegistryFailure,
};
use tracedecay_runtime_core::store_runtime::resolver::{
    LocalProfileStoreAuthorityV1, LocalStoreLocatorResolutionV1, LocalStoreRuntimeResolverV1,
};
use tracedecay_store::{StoreIncarnationV1, StoreRuntimeBindingV1, StoreShardIdV1};

use crate::profile_identity::LocalProfileIdentityAuthorityV1;
use crate::root_seam::global_db::RegisteredGlobalDb;

pub(crate) struct DaemonSessionRuntimeRegistryV1 {
    _profile_runtime: StoreRuntimeHandle,
    profile_database: Mutex<Option<Arc<RegisteredGlobalDb>>>,
}

impl DaemonSessionRuntimeRegistryV1 {
    pub(crate) async fn open(identity: LocalProfileIdentityAuthorityV1) -> Result<Self> {
        register_registered_schema_installer();
        let incarnation = runtime_incarnation()?;
        let resolver = Arc::new(LocalStoreRuntimeResolverV1::new(
            LocalProfileStoreAuthorityV1::new(
                identity.brain_id().clone(),
                identity.profile_id().clone(),
                identity.profile_root().to_path_buf(),
            ),
        ));
        let registry_resolver: Arc<
            dyn tracedecay_runtime_core::store_runtime::registry::StoreRuntimeResolver,
        > = resolver.clone();
        let registry =
            StoreRuntimeRegistry::new(registry_resolver, Arc::new(LifecycleShardRuntimePublisher));
        let profile_shard =
            StoreShardIdV1::profile(identity.brain_id().clone(), identity.profile_id().clone());
        let profile_runtime = open_runtime(
            &registry,
            resolver.as_ref(),
            profile_shard.clone(),
            incarnation,
            None,
            true,
            "mount migration profile authority",
        )
        .await?;
        let _profile_pin = match registry.profile_authority_pin(&profile_shard) {
            ProfileAuthorityPinResult::Pinned(pin) => pin,
            outcome => {
                return Err(runtime_error(
                    "pin migration profile authority",
                    format!("{outcome:?}"),
                ));
            }
        };
        Ok(Self {
            _profile_runtime: profile_runtime,
            profile_database: Mutex::new(None),
        })
    }

    pub(crate) async fn profile_database(&self) -> Result<Arc<RegisteredGlobalDb>> {
        let mut mounted = self.profile_database.lock().await;
        if let Some(database) = mounted.as_ref() {
            return Ok(Arc::clone(database));
        }
        let database = Arc::new(
            attach_registered(
                self._profile_runtime.clone(),
                "attach migration profile database",
            )
            .await?,
        );
        *mounted = Some(Arc::clone(&database));
        Ok(database)
    }
}

fn register_registered_schema_installer() {
    tracedecay_runtime_core::ports::registered_schema::register(|connection| {
        Box::pin(crate::root_seam::global_db::ensure_registered_schema(
            connection,
        ))
    });
}

fn runtime_incarnation() -> Result<StoreIncarnationV1> {
    let process_run_id = tracedecay_runtime_core::runtime_identity::process_run_id();
    let raw = process_run_id
        .get(..16)
        .and_then(|prefix| u64::from_str_radix(prefix, 16).ok())
        .or_else(|| {
            process_run_id
                .strip_prefix("mcp-")
                .and_then(|value| value.parse::<u64>().ok())
                .map(|timestamp| timestamp ^ u64::from(std::process::id()))
        })
        .ok_or_else(|| {
            runtime_error(
                "create migration store incarnation",
                "process runtime generation has an unsupported format",
            )
        })?;
    StoreIncarnationV1::new((raw & i64::MAX as u64).max(1))
        .map_err(|error| runtime_error("create migration store incarnation", error.to_string()))
}

async fn open_runtime(
    registry: &StoreRuntimeRegistry,
    resolver: &LocalStoreRuntimeResolverV1,
    shard_id: StoreShardIdV1,
    incarnation: StoreIncarnationV1,
    profile_pin: Option<ProfileAuthorityPin>,
    initialize_if_missing: bool,
    operation: &'static str,
) -> Result<StoreRuntimeHandle> {
    let key = StoreRuntimeKey::new(shard_id.clone(), incarnation);
    let locator = match resolver.resolve_key(&key) {
        LocalStoreLocatorResolutionV1::Resolved(locator) => locator,
        LocalStoreLocatorResolutionV1::Unavailable(unavailable) => {
            return Err(runtime_error(
                operation,
                format!(
                    "registered store locator unavailable: {:?}",
                    unavailable.reason
                ),
            ));
        }
    };
    let authority = DatabaseAuthority::for_runtime(locator.locator().path(), operation)?;
    let exists = locator
        .locator()
        .path()
        .try_exists()
        .map_err(|error| runtime_error(operation, error.to_string()))?;
    let request = if initialize_if_missing && !exists {
        StoreRuntimeOpenRequest::new_initialize_authorized(
            shard_id,
            incarnation,
            profile_pin,
            authority,
        )
    } else {
        StoreRuntimeOpenRequest::new_authorized(shard_id, incarnation, profile_pin, authority)
    };
    match registry.open(request).await {
        StoreRuntimeOpenResult::Published(runtime) => Ok(runtime),
        StoreRuntimeOpenResult::Failed(failure) => Err(registry_error(operation, failure)),
    }
}

async fn attach_registered(
    runtime: StoreRuntimeHandle,
    operation: &'static str,
) -> Result<RegisteredGlobalDb> {
    let expected_binding: StoreRuntimeBindingV1 = runtime.binding().clone();
    let expected_locator = runtime.locator().verified().clone();
    let authority = runtime
        .database_authority(operation)
        .map_err(|failure| registry_error(operation, failure))?;
    RegisteredGlobalDb::migrate_and_attach(runtime, expected_binding, expected_locator, authority)
        .await
}

fn registry_error(
    operation: &'static str,
    failure: StoreRuntimeRegistryFailure,
) -> TraceDecayError {
    runtime_error(operation, format!("{failure:?}"))
}

fn runtime_error(operation: &'static str, message: impl Into<String>) -> TraceDecayError {
    TraceDecayError::Database {
        operation: operation.to_owned(),
        message: message.into(),
    }
}
