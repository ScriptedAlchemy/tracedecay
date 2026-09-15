use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use sha2::{Digest, Sha256};
use tracedecay_domain::{BrainId, BrainNodeId, RepositoryId, UserProfileId, WorktreeId};
use tracedecay_store::{
    CodeShardScopeV1, LocatorDigest, ProjectId, StoreIncarnationV1, StoreRuntimeBindingV1,
    StoreShardIdV1, VerifiedStoreLocatorV1,
};

use super::{
    Database, DatabaseAccessMode, DatabaseAuthority, DatabaseOwnerV1, StoreRuntimeClientLease,
};
use crate::store_runtime::registry::{
    LifecycleShardRuntimePublisher, ProfileAuthorityPinResult, ResolvedStoreLocator,
    StoreRuntimeKey, StoreRuntimeOpenMode, StoreRuntimeOpenRequest, StoreRuntimeOpenResult,
    StoreRuntimeRegistry, StoreRuntimeRegistryFailure, StoreRuntimeRegistryFuture,
    StoreRuntimeResolver, StoreRuntimeRetirementTarget,
};
use tracedecay_domain::errors::{Result, TraceDecayError};

#[doc(hidden)]
#[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TestDatabaseRuntimeMode {
    Initialize,
    Existing,
    ReadOnly,
}

/// Exact production-minted profile identity supplied to a test runtime.
///
/// Production-shaped daemon fixtures must construct this value from the
/// persisted profile identity authority. The runtime-core fixture never mints,
/// derives, or replaces that authority on their behalf.
#[doc(hidden)]
#[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TestRuntimeProfileIdentityV1 {
    brain_id: BrainId,
    profile_id: UserProfileId,
}

#[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
impl TestRuntimeProfileIdentityV1 {
    #[must_use]
    pub fn new(brain_id: BrainId, profile_id: UserProfileId) -> Self {
        Self {
            brain_id,
            profile_id,
        }
    }

    #[must_use]
    pub fn brain_id(&self) -> &BrainId {
        &self.brain_id
    }

    #[must_use]
    pub fn profile_id(&self) -> &UserProfileId {
        &self.profile_id
    }

    fn into_parts(self) -> (BrainId, UserProfileId) {
        (self.brain_id, self.profile_id)
    }
}

/// Exact shard family a fixture publication constructs.
#[doc(hidden)]
#[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum TestRuntimeShardFamilyV1 {
    Code,
    ProfileMemory,
    Registered(TestDatabaseRuntimeScope),
}

#[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
impl TestRuntimeShardFamilyV1 {
    fn shard(self, identity: TestRuntimeProfileIdentityV1) -> Result<StoreShardIdV1> {
        let (brain_id, profile_id) = identity.into_parts();
        match self {
            Self::Code => test_code_shard(brain_id, profile_id),
            Self::ProfileMemory => Ok(StoreShardIdV1::profile_memory(brain_id, profile_id)),
            Self::Registered(scope) => test_registered_shard(brain_id, profile_id, scope),
        }
    }
}

/// Exact shard family published by an isolated registered-store fixture.
#[doc(hidden)]
#[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TestDatabaseRuntimeScope {
    Profile,
    ProfileMemory,
    ProfileSessions,
    Project { project_id: ProjectId },
    ProjectSessions { project_id: ProjectId },
    RemoteNode,
}

/// Test-only control for retiring and reopening one exact registered fixture
/// through the registry that originally published it.
#[doc(hidden)]
#[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
pub struct RegisteredTestRuntimeRetirementControlV1 {
    registry: StoreRuntimeRegistry,
    authority: DatabaseAuthority,
    binding: StoreRuntimeBindingV1,
    locator: VerifiedStoreLocatorV1,
}

impl RegisteredTestRuntimeRetirementControlV1 {
    #[must_use]
    pub fn registry(&self) -> &StoreRuntimeRegistry {
        &self.registry
    }

    #[must_use]
    pub fn retirement_target(&self) -> StoreRuntimeRetirementTarget {
        StoreRuntimeRetirementTarget::new(self.binding.clone(), self.authority.clone())
    }

    #[must_use]
    pub fn binding(&self) -> &StoreRuntimeBindingV1 {
        &self.binding
    }

    #[must_use]
    pub fn locator(&self) -> &VerifiedStoreLocatorV1 {
        &self.locator
    }

    /// Reopens the exact retired fixture binding through its original resolver.
    pub async fn reopen(&self) -> Result<StoreRuntimeClientLease> {
        let target_shard = self.binding.shard_id.clone();
        let profile_shard = StoreShardIdV1::profile(
            target_shard.brain_id.clone(),
            target_shard.profile_id.clone(),
        );
        let profile_pin = if target_shard == profile_shard {
            None
        } else {
            match self.registry.profile_authority_pin(&profile_shard) {
                ProfileAuthorityPinResult::Pinned(pin) => Some(pin),
                outcome => {
                    return Err(test_runtime_error(
                        "pin reopened registered test runtime profile",
                        format!("{outcome:?}"),
                    ));
                }
            }
        };
        let request = StoreRuntimeOpenRequest::new_authorized(
            target_shard,
            self.binding.incarnation,
            profile_pin,
            self.authority.clone(),
        );
        match self.registry.open(request).await {
            StoreRuntimeOpenResult::Published(runtime) => Ok(runtime),
            StoreRuntimeOpenResult::Failed(failure) => Err(test_runtime_open_failure(
                "reopen registered test runtime",
                failure,
            )),
        }
    }
}

/// Published registered fixture plus its exact retirement control.
#[doc(hidden)]
#[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
pub struct RegisteredTestRuntimeFixtureV1 {
    owner: DatabaseOwnerV1,
    runtime: StoreRuntimeClientLease,
    retirement: RegisteredTestRuntimeRetirementControlV1,
}

impl RegisteredTestRuntimeFixtureV1 {
    #[must_use]
    pub fn into_parts(
        self,
    ) -> (
        DatabaseOwnerV1,
        StoreRuntimeClientLease,
        RegisteredTestRuntimeRetirementControlV1,
    ) {
        (self.owner, self.runtime, self.retirement)
    }
}

pub(super) struct FixtureRuntimePublication {
    pub(super) owner: DatabaseOwnerV1,
    runtime: StoreRuntimeClientLease,
    registry: StoreRuntimeRegistry,
    authority: DatabaseAuthority,
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

    fn resolve_graph<'a>(
        &'a self,
        key: &'a StoreRuntimeKey,
    ) -> StoreRuntimeRegistryFuture<
        'a,
        std::result::Result<ResolvedStoreLocator, StoreRuntimeRegistryFailure>,
    > {
        Box::pin(async move {
            let locator = self.locators.get(key).ok_or_else(|| {
                StoreRuntimeRegistryFailure::ResolverFailed {
                    message: "test graph resolver received the wrong typed shard".to_owned(),
                }
            })?;
            Ok(ResolvedStoreLocator::new(
                locator.verified.clone(),
                locator.path.clone(),
            ))
        })
    }
}

impl Database {
    /// Publishes a standalone runtime-core fixture using a deterministic,
    /// path-scoped identity that is not production-persisted. Cross-profile
    /// daemon fixtures must use an explicit production-minted identity seam.
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
        Self::publish_fixture_runtime(
            db_path,
            authority,
            mode,
            TestRuntimeShardFamilyV1::Code,
            None,
        )
        .await
    }

    /// Publishes an isolated profile-memory fixture with the runtime-core-only
    /// path-scoped identity fallback.
    #[doc(hidden)]
    #[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
    pub async fn publish_profile_memory_test_runtime(
        db_path: &Path,
        authority: &DatabaseAuthority,
        mode: TestDatabaseRuntimeMode,
    ) -> Result<(Self, bool)> {
        if authority.role() != super::DatabaseAuthorityRole::Test {
            return Err(TraceDecayError::Database {
                message: "profile-memory test runtime requires explicit test authority".to_owned(),
                operation: "publish profile-memory test database runtime".to_owned(),
            });
        }
        let published = Self::publish_fixture_runtime(
            db_path,
            authority,
            mode,
            TestRuntimeShardFamilyV1::ProfileMemory,
            None,
        )
        .await?;
        match mode {
            TestDatabaseRuntimeMode::Initialize | TestDatabaseRuntimeMode::Existing => {
                crate::db::migrations::ensure_schema_current(&published.0).await?;
            }
            TestDatabaseRuntimeMode::ReadOnly => {}
        }
        Ok(published)
    }

    /// Publishes an isolated registered-store fixture with the runtime-core-only
    /// path-scoped identity fallback. Production-shaped fixtures must use
    /// [`Self::publish_registered_test_runtime_for_profile_identity`].
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
        Self::publish_fixture_runtime(
            db_path,
            authority,
            mode,
            TestRuntimeShardFamilyV1::Registered(scope),
            None,
        )
        .await
    }

    /// Publishes a registered fixture under an identity already minted and
    /// persisted by the production profile identity authority.
    #[doc(hidden)]
    #[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
    pub async fn publish_registered_test_runtime_for_profile_identity(
        db_path: &Path,
        authority: &DatabaseAuthority,
        mode: TestDatabaseRuntimeMode,
        identity: TestRuntimeProfileIdentityV1,
        scope: TestDatabaseRuntimeScope,
    ) -> Result<(Self, bool)> {
        if authority.role() != super::DatabaseAuthorityRole::Test {
            return Err(TraceDecayError::Database {
                message: "registered test runtime requires explicit test authority".to_owned(),
                operation: "publish registered test database runtime".to_owned(),
            });
        }
        Self::publish_fixture_runtime(
            db_path,
            authority,
            mode,
            TestRuntimeShardFamilyV1::Registered(scope),
            Some(identity),
        )
        .await
    }

    /// Publishes a fresh registered fixture with the runtime-core-only
    /// path-scoped identity fallback while retaining exact retirement control.
    #[doc(hidden)]
    #[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
    pub async fn publish_registered_test_runtime_with_retirement_control(
        db_path: &Path,
        authority: &DatabaseAuthority,
        mode: TestDatabaseRuntimeMode,
        scope: TestDatabaseRuntimeScope,
    ) -> Result<RegisteredTestRuntimeFixtureV1> {
        if authority.role() != super::DatabaseAuthorityRole::Test {
            return Err(TraceDecayError::Database {
                message: "registered test runtime requires explicit test authority".to_owned(),
                operation: "publish registered test runtime".to_owned(),
            });
        }
        Self::publish_registered_fixture_with_retirement_control(
            db_path, authority, mode, None, scope,
        )
        .await
    }

    /// Publishes a registered fixture under the active daemon authority whose
    /// scope the test controls. This stays separate from the unconditional
    /// Test-authority publisher so a fixture cannot silently bypass actor-time
    /// scope revocation.
    #[doc(hidden)]
    #[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
    pub async fn publish_registered_daemon_test_runtime_with_retirement_control(
        db_path: &Path,
        authority: &DatabaseAuthority,
        mode: TestDatabaseRuntimeMode,
        scope: TestDatabaseRuntimeScope,
    ) -> Result<RegisteredTestRuntimeFixtureV1> {
        Self::publish_registered_daemon_fixture_with_retirement_control(
            db_path, authority, mode, None, scope,
        )
        .await
    }

    /// Publishes the same daemon-scoped fixture under identity already minted
    /// by the production profile authority.
    #[doc(hidden)]
    #[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
    pub async fn publish_registered_daemon_test_runtime_with_retirement_control_for_profile_identity(
        db_path: &Path,
        authority: &DatabaseAuthority,
        mode: TestDatabaseRuntimeMode,
        identity: TestRuntimeProfileIdentityV1,
        scope: TestDatabaseRuntimeScope,
    ) -> Result<RegisteredTestRuntimeFixtureV1> {
        Self::publish_registered_daemon_fixture_with_retirement_control(
            db_path,
            authority,
            mode,
            Some(identity),
            scope,
        )
        .await
    }

    #[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
    async fn publish_registered_daemon_fixture_with_retirement_control(
        db_path: &Path,
        authority: &DatabaseAuthority,
        mode: TestDatabaseRuntimeMode,
        identity: Option<TestRuntimeProfileIdentityV1>,
        scope: TestDatabaseRuntimeScope,
    ) -> Result<RegisteredTestRuntimeFixtureV1> {
        if authority.role() != super::DatabaseAuthorityRole::Daemon {
            return Err(TraceDecayError::Database {
                message: "daemon-scoped registered test runtime requires explicit daemon authority"
                    .to_owned(),
                operation: "publish daemon-scoped registered test runtime".to_owned(),
            });
        }
        Self::publish_registered_fixture_with_retirement_control(
            db_path, authority, mode, identity, scope,
        )
        .await
    }

    /// Publishes a retirement-controlled registered fixture under an identity
    /// already minted and persisted by the production profile authority.
    #[doc(hidden)]
    #[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
    pub async fn publish_registered_test_runtime_with_retirement_control_for_profile_identity(
        db_path: &Path,
        authority: &DatabaseAuthority,
        mode: TestDatabaseRuntimeMode,
        identity: TestRuntimeProfileIdentityV1,
        scope: TestDatabaseRuntimeScope,
    ) -> Result<RegisteredTestRuntimeFixtureV1> {
        Self::publish_registered_test_runtime_with_retirement_control_inner(
            db_path,
            authority,
            mode,
            Some(identity),
            scope,
        )
        .await
    }

    #[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
    async fn publish_registered_test_runtime_with_retirement_control_inner(
        db_path: &Path,
        authority: &DatabaseAuthority,
        mode: TestDatabaseRuntimeMode,
        identity: Option<TestRuntimeProfileIdentityV1>,
        scope: TestDatabaseRuntimeScope,
    ) -> Result<RegisteredTestRuntimeFixtureV1> {
        if authority.role() != super::DatabaseAuthorityRole::Test {
            return Err(TraceDecayError::Database {
                message: "registered test runtime requires explicit test authority".to_owned(),
                operation: "publish registered test runtime".to_owned(),
            });
        }
        Self::publish_registered_fixture_with_retirement_control(
            db_path, authority, mode, identity, scope,
        )
        .await
    }

    #[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
    async fn publish_registered_fixture_with_retirement_control(
        db_path: &Path,
        authority: &DatabaseAuthority,
        mode: TestDatabaseRuntimeMode,
        identity: Option<TestRuntimeProfileIdentityV1>,
        scope: TestDatabaseRuntimeScope,
    ) -> Result<RegisteredTestRuntimeFixtureV1> {
        let publication = Self::publish_fixture_runtime_publication(
            db_path,
            authority,
            mode,
            TestRuntimeShardFamilyV1::Registered(scope),
            identity,
        )
        .await?;
        let FixtureRuntimePublication {
            owner,
            runtime,
            registry,
            authority,
        } = publication;
        let binding = runtime.binding().clone();
        let locator = runtime.locator().verified().clone();
        Ok(RegisteredTestRuntimeFixtureV1 {
            owner,
            runtime,
            retirement: RegisteredTestRuntimeRetirementControlV1 {
                registry,
                authority,
                binding,
                locator,
            },
        })
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
        Self::publish_fixture_runtime(
            db_path,
            authority,
            mode,
            TestRuntimeShardFamilyV1::Code,
            None,
        )
        .await
    }

    #[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
    pub(super) async fn publish_fixture_runtime(
        db_path: &Path,
        authority: &DatabaseAuthority,
        mode: TestDatabaseRuntimeMode,
        shard_family: TestRuntimeShardFamilyV1,
        identity: Option<TestRuntimeProfileIdentityV1>,
    ) -> Result<(Self, bool)> {
        let FixtureRuntimePublication { owner, .. } = Self::publish_fixture_runtime_publication(
            db_path,
            authority,
            mode,
            shard_family,
            identity,
        )
        .await?;
        let database = owner.issue_lease().map_err(|error| {
            test_runtime_error("issue test database client lease", format!("{error:?}"))
        })?;
        Ok((database, false))
    }

    #[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
    pub(super) async fn publish_fixture_runtime_publication(
        db_path: &Path,
        authority: &DatabaseAuthority,
        mode: TestDatabaseRuntimeMode,
        shard_family: TestRuntimeShardFamilyV1,
        identity: Option<TestRuntimeProfileIdentityV1>,
    ) -> Result<FixtureRuntimePublication> {
        let graph_shard_check = shard_family == TestRuntimeShardFamilyV1::Code;
        let authority = authority.hold_for(db_path, "publish test database runtime")?;
        authority.require_active_write_scope("publish test database runtime")?;
        let path = authority.canonical_database_path().to_path_buf();
        let identity = match identity {
            Some(identity) => identity,
            None => isolated_test_runtime_identity(&path)?,
        };
        let target_shard = shard_family.shard(identity)?;
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
                return Err(test_runtime_open_failure(
                    "publish test profile runtime",
                    failure,
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
                    return Err(test_runtime_open_failure(
                        "publish test database runtime",
                        failure,
                    ));
                }
            }
        };
        let access = if mode == TestDatabaseRuntimeMode::ReadOnly {
            DatabaseAccessMode::ReadOnly
        } else {
            DatabaseAccessMode::ReadWrite
        };
        let retained_runtime = runtime.clone();
        let owner = Self::publish_runtime(runtime, access).await?;
        // Writable GRAPH test runtimes assert the store already carries the
        // schema this binary creates; there is no ladder to step, so nothing
        // is ever reported as migrated. Registered (global/session) shards are
        // a different schema family: they carry their own installer and sit at
        // user_version 0 by design, so the graph identity check must not run
        // against them.
        let graph_shard = graph_shard_check;
        match mode {
            TestDatabaseRuntimeMode::Initialize | TestDatabaseRuntimeMode::Existing
                if graph_shard =>
            {
                let database = owner.issue_lease().map_err(|error| {
                    test_runtime_error(
                        "issue schema test database client lease",
                        format!("{error:?}"),
                    )
                })?;
                crate::db::migrations::ensure_schema_current(&database).await?;
            }
            _ => {}
        }
        Ok(FixtureRuntimePublication {
            owner,
            runtime: retained_runtime,
            registry,
            authority,
        })
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
fn isolated_test_runtime_identity(
    canonical_database_path: &Path,
) -> Result<TestRuntimeProfileIdentityV1> {
    let mut digest = Sha256::new();
    digest.update(b"tracedecay.isolated-test-runtime.identity.v1\0");
    digest.update(canonical_database_path.as_os_str().as_encoded_bytes());
    let digest = hex::encode(digest.finalize());
    let brain_id = BrainId::try_from(format!("brain.{}", &digest[..32]))
        .map_err(|error| test_runtime_error("construct test brain identity", error.to_string()))?;
    let profile_id =
        UserProfileId::try_from(format!("profile.{}", &digest[32..])).map_err(|error| {
            test_runtime_error("construct test profile identity", error.to_string())
        })?;
    Ok(TestRuntimeProfileIdentityV1::new(brain_id, profile_id))
}

#[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
fn test_code_shard(brain_id: BrainId, profile_id: UserProfileId) -> Result<StoreShardIdV1> {
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
fn test_registered_shard(
    brain_id: BrainId,
    profile_id: UserProfileId,
    scope: TestDatabaseRuntimeScope,
) -> Result<StoreShardIdV1> {
    let shard = match scope {
        TestDatabaseRuntimeScope::Profile => StoreShardIdV1::profile(brain_id, profile_id),
        TestDatabaseRuntimeScope::ProfileMemory => {
            StoreShardIdV1::profile_memory(brain_id, profile_id)
        }
        TestDatabaseRuntimeScope::ProfileSessions => {
            StoreShardIdV1::profile_sessions(brain_id, profile_id)
        }
        TestDatabaseRuntimeScope::Project { project_id } => {
            StoreShardIdV1::project(brain_id, profile_id, project_id)
        }
        TestDatabaseRuntimeScope::ProjectSessions { project_id } => {
            StoreShardIdV1::project_sessions(brain_id, profile_id, project_id)
        }
        TestDatabaseRuntimeScope::RemoteNode => StoreShardIdV1::remote_node(
            brain_id,
            profile_id,
            BrainNodeId::try_from("node.test-runtime".to_owned()).map_err(|error| {
                test_runtime_error("construct test remote-node identity", error.to_string())
            })?,
        ),
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

#[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
fn test_runtime_open_failure(
    operation: &'static str,
    failure: StoreRuntimeRegistryFailure,
) -> TraceDecayError {
    match failure {
        StoreRuntimeRegistryFailure::ResetRequired { authority, reason } => {
            TraceDecayError::reset_required(authority, reason)
        }
        failure => test_runtime_error(operation, format!("{failure:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn caller_supplied_profile_identity_controls_registered_fixture_binding() {
        let root = tempfile::tempdir().expect("registered fixture root");
        let path = root.path().join("sessions.db");
        let authority = DatabaseAuthority::acquire_test(&path, "explicit fixture identity")
            .expect("acquire fixture authority");
        let identity = TestRuntimeProfileIdentityV1::new(
            BrainId::new("brain.explicit-fixture").expect("fixture brain identity"),
            UserProfileId::new("profile.explicit-fixture").expect("fixture profile identity"),
        );

        let (database, _) = Database::publish_registered_test_runtime_for_profile_identity(
            &path,
            &authority,
            TestDatabaseRuntimeMode::Initialize,
            identity.clone(),
            TestDatabaseRuntimeScope::ProfileSessions,
        )
        .await
        .expect("publish explicitly identified fixture");

        assert_eq!(
            database.registered_binding().shard_id,
            StoreShardIdV1::profile_sessions(
                identity.brain_id().clone(),
                identity.profile_id().clone(),
            )
        );
    }

    #[tokio::test]
    async fn daemon_scoped_registered_fixture_requires_exact_daemon_authority() {
        let root = tempfile::tempdir().expect("daemon-scoped fixture root");
        let _scope = crate::db::enter_daemon_database_scope(
            root.path(),
            1,
            "daemon-scoped-registered-fixture",
        )
        .expect("enter daemon database scope");
        let path = root.path().join("sessions.db");
        let authority = DatabaseAuthority::for_owned_runtime(
            &path,
            "daemon-scoped registered fixture authority",
        )
        .expect("acquire daemon fixture authority");
        let identity = TestRuntimeProfileIdentityV1::new(
            BrainId::new("brain.daemon-scoped-fixture").expect("fixture brain identity"),
            UserProfileId::new("profile.daemon-scoped-fixture").expect("fixture profile identity"),
        );

        let fixture = Database::publish_registered_daemon_test_runtime_with_retirement_control_for_profile_identity(
                &path,
                &authority,
                TestDatabaseRuntimeMode::Initialize,
                identity.clone(),
                TestDatabaseRuntimeScope::ProfileSessions,
            )
            .await
            .expect("publish daemon-scoped registered fixture");
        let (owner, _runtime, _retirement) = fixture.into_parts();
        let database = owner.issue_lease().expect("issue daemon-scoped client");
        assert_eq!(
            database
                .write_authority()
                .expect("daemon-scoped write authority")
                .role(),
            crate::db::DatabaseAuthorityRole::Daemon
        );
        assert_eq!(
            database.registered_binding().shard_id,
            StoreShardIdV1::profile_sessions(
                identity.brain_id().clone(),
                identity.profile_id().clone(),
            )
        );

        let invalid_path = root.path().join("invalid.db");
        let invalid = DatabaseAuthority::acquire_test(
            &invalid_path,
            "invalid daemon-scoped registered fixture authority",
        )
        .expect("acquire explicit test authority");
        let error = match Database::publish_registered_daemon_test_runtime_with_retirement_control(
            &invalid_path,
            &invalid,
            TestDatabaseRuntimeMode::Initialize,
            TestDatabaseRuntimeScope::ProfileSessions,
        )
        .await
        {
            Ok(_) => panic!("Test authority published a daemon-scoped fixture"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("explicit daemon authority"));

        let foreign_path = root.path().join("foreign.db");
        let error = match Database::publish_registered_daemon_test_runtime_with_retirement_control(
            &foreign_path,
            &authority,
            TestDatabaseRuntimeMode::Initialize,
            TestDatabaseRuntimeScope::ProfileSessions,
        )
        .await
        {
            Ok(_) => panic!("foreign daemon authority published another fixture"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("different database"));
    }

    #[test]
    fn test_runtime_open_failure_preserves_typed_reset_required() {
        let error = test_runtime_open_failure(
            "publish test database runtime",
            StoreRuntimeRegistryFailure::ResetRequired {
                authority: "SQLite store".to_owned(),
                reason: "database schema is missing required index".to_owned(),
            },
        );

        assert_eq!(
            error.reset_required_context(),
            Some(("SQLite store", "database schema is missing required index"))
        );
    }

    #[test]
    fn test_runtime_open_failure_keeps_other_failures_generic() {
        let error = test_runtime_open_failure(
            "publish test database runtime",
            StoreRuntimeRegistryFailure::ResolverFailed {
                message: "missing fixture".to_owned(),
            },
        );

        assert!(matches!(
            error,
            TraceDecayError::Database { operation, message }
                if operation == "publish test database runtime"
                    && message.contains("ResolverFailed")
                    && message.contains("missing fixture")
        ));
    }
}
