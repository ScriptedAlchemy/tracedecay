use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use sha2::{Digest, Sha256};
use tracedecay_domain::{BrainId, RepositoryId, UserProfileId, WorktreeId};
use tracedecay_store::{
    CodeShardScopeV1, LocatorDigest, ProjectId, StoreIncarnationV1, StoreShardIdV1,
    VerifiedStoreLocatorV1,
};

use super::{Database, DatabaseAccessMode, DatabaseAuthority, database_slot};
use crate::errors::{Result, TraceDecayError};
use crate::store_runtime::registry::{
    LifecycleShardRuntimePublisher, ProfileAuthorityPinResult, ResolvedStoreLocator,
    StoreRuntimeKey, StoreRuntimeOpenMode, StoreRuntimeOpenRequest, StoreRuntimeOpenResult,
    StoreRuntimeRegistry, StoreRuntimeRegistryFailure, StoreRuntimeRegistryFuture,
    StoreRuntimeResolver,
};

#[doc(hidden)]
#[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TestDatabaseRuntimeMode {
    Initialize,
    Existing,
    ReadOnly,
}

/// Exact shard family published by an isolated registered-store fixture.
#[doc(hidden)]
#[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TestDatabaseRuntimeScope {
    Profile,
    ProfileSessions,
    ProjectSessions { project_id: ProjectId },
}

#[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
struct ExactTestRuntimeResolver {
    locators: BTreeMap<StoreRuntimeKey, ExactTestRuntimeLocator>,
}

#[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
struct ExactTestRuntimeLocator {
    verified: VerifiedStoreLocatorV1,
    path: PathBuf,
}

#[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
impl StoreRuntimeResolver for ExactTestRuntimeResolver {
    fn resolve<'a>(
        &'a self,
        key: &'a StoreRuntimeKey,
        mode: StoreRuntimeOpenMode,
        database_authority: Option<&'a DatabaseAuthority>,
    ) -> StoreRuntimeRegistryFuture<
        'a,
        std::result::Result<ResolvedStoreLocator, StoreRuntimeRegistryFailure>,
    > {
        Box::pin(async move {
            let locator = self.locators.get(key).ok_or_else(|| {
                StoreRuntimeRegistryFailure::ResolverFailed {
                    message: "test runtime resolver received the wrong typed shard".to_owned(),
                }
            })?;
            let authority =
                database_authority.ok_or_else(|| StoreRuntimeRegistryFailure::ResolverFailed {
                    message: "test runtime publication requires exact database authority"
                        .to_owned(),
                })?;
            authority
                .require_active_write_scope("resolve canonical test runtime")
                .map_err(|error| StoreRuntimeRegistryFailure::ResolverFailed {
                    message: error.to_string(),
                })?;
            if authority.canonical_database_path() != locator.path {
                return Err(StoreRuntimeRegistryFailure::ResolverFailed {
                    message: "test runtime authority does not match its exact locator".to_owned(),
                });
            }
            match (mode, locator.path.try_exists()) {
                (StoreRuntimeOpenMode::Initialize, Ok(false)) => {
                    Ok(ResolvedStoreLocator::prospective(
                        locator.verified.clone(),
                        locator.path.clone(),
                    ))
                }
                (StoreRuntimeOpenMode::Existing, Ok(true)) => Ok(ResolvedStoreLocator::new(
                    locator.verified.clone(),
                    locator.path.clone(),
                )),
                (StoreRuntimeOpenMode::Initialize, Ok(true)) => {
                    Err(StoreRuntimeRegistryFailure::ResolverFailed {
                        message: "test runtime initialization requires a missing database"
                            .to_owned(),
                    })
                }
                (StoreRuntimeOpenMode::Existing, Ok(false)) => {
                    Err(StoreRuntimeRegistryFailure::ResolverFailed {
                        message: "test runtime database does not exist".to_owned(),
                    })
                }
                (_, Err(error)) => Err(StoreRuntimeRegistryFailure::ResolverFailed {
                    message: error.to_string(),
                }),
            }
        })
    }
}

impl Database {
    #[doc(hidden)]
    #[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
    pub async fn publish_test_runtime(
        db_path: &Path,
        authority: &DatabaseAuthority,
        mode: TestDatabaseRuntimeMode,
    ) -> Result<(Self, bool)> {
        if authority.role() != super::DatabaseAuthorityRole::Test {
            return Err(TraceDecayError::Database {
                message: "canonical test runtime requires explicit test authority".to_owned(),
                operation: "publish test database runtime".to_owned(),
            });
        }
        Self::publish_fixture_runtime(db_path, authority, mode, test_code_shard()?).await
    }

    /// Publishes an isolated registered-store fixture with the exact typed
    /// shard family consumed by the test.
    #[doc(hidden)]
    #[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
    pub async fn publish_registered_test_runtime(
        db_path: &Path,
        authority: &DatabaseAuthority,
        mode: TestDatabaseRuntimeMode,
        scope: TestDatabaseRuntimeScope,
    ) -> Result<(Self, bool)> {
        if authority.role() != super::DatabaseAuthorityRole::Test {
            return Err(TraceDecayError::Database {
                message: "registered test runtime requires explicit test authority".to_owned(),
                operation: "publish registered test database runtime".to_owned(),
            });
        }
        Self::publish_fixture_runtime(db_path, authority, mode, test_registered_shard(scope)?).await
    }

    /// Publishes an isolated integration-test fixture with the retained
    /// exclusive-maintenance authority whose scope the test controls.
    ///
    /// This remains separate from [`Self::publish_test_runtime`]: it accepts
    /// only maintenance authority, rejects production paths, and therefore
    /// preserves actor-time scope revocation without weakening the Test-only
    /// fixture escape hatch.
    #[doc(hidden)]
    #[cfg(any(test, feature = "test-transport"))]
    pub async fn publish_maintenance_test_runtime(
        db_path: &Path,
        authority: &DatabaseAuthority,
        mode: TestDatabaseRuntimeMode,
    ) -> Result<(Self, bool)> {
        if authority.role() != super::DatabaseAuthorityRole::Maintenance {
            return Err(TraceDecayError::Database {
                message:
                    "maintenance test runtime requires explicit exclusive-maintenance authority"
                        .to_owned(),
                operation: "publish maintenance test database runtime".to_owned(),
            });
        }
        if !crate::db::access::is_isolated_test_path(db_path) {
            return Err(TraceDecayError::Database {
                message: format!(
                    "maintenance test database must be inside an isolated test root at '{}'",
                    db_path.display()
                ),
                operation: "publish maintenance test database runtime".to_owned(),
            });
        }
        Self::publish_fixture_runtime(db_path, authority, mode, test_code_shard()?).await
    }

    #[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
    pub(super) async fn publish_fixture_runtime(
        db_path: &Path,
        authority: &DatabaseAuthority,
        mode: TestDatabaseRuntimeMode,
        target_shard: StoreShardIdV1,
    ) -> Result<(Self, bool)> {
        let authority = authority.hold_for(db_path, "publish test database runtime")?;
        authority.require_active_write_scope("publish test database runtime")?;
        let path = authority.canonical_database_path().to_path_buf();
        let existing_slot = database_slot(authority.database_identity_key());
        if let Some(inner) = existing_slot.lock().await.upgrade() {
            if mode == TestDatabaseRuntimeMode::ReadOnly {
                let database =
                    Self::publish_runtime(inner._runtime.clone(), DatabaseAccessMode::ReadOnly)
                        .await?;
                return Ok((database, false));
            }
            return Ok((Self { inner }, false));
        }
        let profile_shard = StoreShardIdV1::profile(
            target_shard.brain_id.clone(),
            target_shard.profile_id.clone(),
        );
        let incarnation = StoreIncarnationV1::new(1)
            .map_err(|error| test_runtime_error("construct test incarnation", error.to_string()))?;
        let mut digest = Sha256::new();
        digest.update(b"tracedecay.test-runtime.profile.v1\0");
        digest.update(path.as_os_str().as_encoded_bytes());
        let profile_name = format!(
            ".tracedecay-test-profile-{}.db",
            &hex::encode(digest.finalize())[..16]
        );
        let target_is_profile = target_shard == profile_shard;
        let profile_path = if target_is_profile {
            path.clone()
        } else {
            path.with_file_name(profile_name)
        };
        let (profile_key, profile_locator) =
            exact_test_runtime_locator(profile_shard.clone(), incarnation, profile_path.clone())?;
        let (target_key, target_locator) =
            exact_test_runtime_locator(target_shard.clone(), incarnation, path)?;
        let mut locators = BTreeMap::new();
        locators.insert(profile_key, profile_locator);
        locators.insert(target_key, target_locator);
        let resolver = Arc::new(ExactTestRuntimeResolver { locators });
        let registry =
            StoreRuntimeRegistry::new(resolver, Arc::new(LifecycleShardRuntimePublisher));
        let profile_authority = if target_is_profile {
            authority.clone()
        } else {
            DatabaseAuthority::acquire_test(&profile_path, "publish test profile runtime")?
        };
        let profile_exists = profile_path.try_exists().map_err(|error| {
            test_runtime_error("inspect test profile runtime", error.to_string())
        })?;
        let profile_request = if profile_exists {
            StoreRuntimeOpenRequest::new_authorized(
                profile_shard.clone(),
                incarnation,
                None,
                profile_authority,
            )
        } else {
            StoreRuntimeOpenRequest::new_initialize_authorized(
                profile_shard.clone(),
                incarnation,
                None,
                profile_authority,
            )
        };
        let profile_runtime = match registry.open(profile_request).await {
            StoreRuntimeOpenResult::Published(runtime) => runtime,
            StoreRuntimeOpenResult::Failed(failure) => {
                return Err(test_runtime_error(
                    "publish test profile runtime",
                    format!("{failure:?}"),
                ));
            }
        };
        let runtime = if target_is_profile {
            profile_runtime
        } else {
            let profile_pin = match registry.profile_authority_pin(&profile_shard) {
                ProfileAuthorityPinResult::Pinned(pin) => pin,
                outcome => {
                    return Err(test_runtime_error(
                        "pin test profile runtime",
                        format!("{outcome:?}"),
                    ));
                }
            };
            let open_mode = match mode {
                TestDatabaseRuntimeMode::Initialize => StoreRuntimeOpenMode::Initialize,
                TestDatabaseRuntimeMode::Existing | TestDatabaseRuntimeMode::ReadOnly => {
                    StoreRuntimeOpenMode::Existing
                }
            };
            let request = match open_mode {
                StoreRuntimeOpenMode::Initialize => {
                    StoreRuntimeOpenRequest::new_initialize_authorized(
                        target_shard,
                        incarnation,
                        Some(profile_pin),
                        authority.clone(),
                    )
                }
                StoreRuntimeOpenMode::Existing => StoreRuntimeOpenRequest::new_authorized(
                    target_shard,
                    incarnation,
                    Some(profile_pin),
                    authority.clone(),
                ),
            };
            match registry.open(request).await {
                StoreRuntimeOpenResult::Published(runtime) => runtime,
                StoreRuntimeOpenResult::Failed(failure) => {
                    return Err(test_runtime_error(
                        "publish test database runtime",
                        format!("{failure:?}"),
                    ));
                }
            }
        };
        let _schema_initialized = runtime.schema_migrated();
        let access = if mode == TestDatabaseRuntimeMode::ReadOnly {
            DatabaseAccessMode::ReadOnly
        } else {
            DatabaseAccessMode::ReadWrite
        };
        let database = Self::publish_runtime(runtime, access).await?;
        // Writable test runtimes assert the store already carries the schema
        // this binary creates; there is no ladder to step, so nothing is ever
        // reported as migrated.
        match mode {
            TestDatabaseRuntimeMode::Initialize | TestDatabaseRuntimeMode::Existing => {
                crate::db::migrations::ensure_schema_current(&database).await?;
            }
            TestDatabaseRuntimeMode::ReadOnly => {}
        }
        Ok((database, false))
    }
}

#[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
fn exact_test_runtime_locator(
    shard_id: StoreShardIdV1,
    incarnation: StoreIncarnationV1,
    path: PathBuf,
) -> Result<(StoreRuntimeKey, ExactTestRuntimeLocator)> {
    let mut digest = Sha256::new();
    digest.update(b"tracedecay.test-runtime.locator.v1\0");
    digest.update(path.as_os_str().as_encoded_bytes());
    let verified = VerifiedStoreLocatorV1::new(
        shard_id.clone(),
        incarnation,
        LocatorDigest::new(format!("sha256:{}", hex::encode(digest.finalize()))).map_err(
            |error| test_runtime_error("construct test locator digest", error.to_string()),
        )?,
    );
    Ok((
        StoreRuntimeKey::new(shard_id, incarnation),
        ExactTestRuntimeLocator { verified, path },
    ))
}

#[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
fn test_runtime_identity() -> Result<(BrainId, UserProfileId)> {
    let brain_id = BrainId::try_from("brain.test-runtime".to_owned())
        .map_err(|error| test_runtime_error("construct test brain identity", error.to_string()))?;
    let profile_id =
        UserProfileId::try_from("profile.test-runtime".to_owned()).map_err(|error| {
            test_runtime_error("construct test profile identity", error.to_string())
        })?;
    Ok((brain_id, profile_id))
}

#[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
pub(super) fn test_code_shard() -> Result<StoreShardIdV1> {
    let (brain_id, profile_id) = test_runtime_identity()?;
    Ok(StoreShardIdV1::code(
        brain_id,
        profile_id,
        ProjectId::try_from("project.test-runtime".to_owned()).map_err(|error| {
            test_runtime_error("construct test project identity", error.to_string())
        })?,
        RepositoryId::try_from("repository.test-runtime".to_owned()).map_err(|error| {
            test_runtime_error("construct test repository identity", error.to_string())
        })?,
        CodeShardScopeV1::Worktree {
            worktree_id: WorktreeId::try_from("worktree.test-runtime".to_owned()).map_err(
                |error| test_runtime_error("construct test worktree identity", error.to_string()),
            )?,
        },
    ))
}

#[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
fn test_registered_shard(scope: TestDatabaseRuntimeScope) -> Result<StoreShardIdV1> {
    let (brain_id, profile_id) = test_runtime_identity()?;
    let shard = match scope {
        TestDatabaseRuntimeScope::Profile => StoreShardIdV1::profile(brain_id, profile_id),
        TestDatabaseRuntimeScope::ProfileSessions => {
            StoreShardIdV1::profile_sessions(brain_id, profile_id)
        }
        TestDatabaseRuntimeScope::ProjectSessions { project_id } => {
            StoreShardIdV1::project_sessions(brain_id, profile_id, project_id)
        }
    };
    Ok(shard)
}

#[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
fn test_runtime_error(operation: &'static str, message: String) -> TraceDecayError {
    TraceDecayError::Database {
        message,
        operation: operation.to_owned(),
    }
}
