//! Exact registered-root locators retained by authorized scope sets.

use std::path::{Component, Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracedecay_domain::{ProjectId, UserProfileId};

use super::{AuthorizedScopeSetError, MultiRootQueryError};
use crate::{RequestContext, ResolvedScope};

/// Shared physical profile-store locator supplied by the profile authority.
///
/// The typed profile and store IDs select this locator. It never derives an
/// identity from a path, CWD, active graph, or mutable project alias.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct SharedProfileStoreLocatorV1 {
    pub profile_id: UserProfileId,
    pub store_id: String,
}

impl SharedProfileStoreLocatorV1 {
    pub fn new(
        profile_id: UserProfileId,
        store_id: impl Into<String>,
    ) -> Result<Self, MultiRootQueryError> {
        let locator = Self {
            profile_id,
            store_id: store_id.into(),
        };
        locator.validate()?;
        Ok(locator)
    }

    pub fn validate(&self) -> Result<(), MultiRootQueryError> {
        self.profile_id
            .validate()
            .map_err(|error| MultiRootQueryError::Invalid(error.to_string()))?;
        validate_locator_text(&self.store_id, "profile store id")
    }
}

/// Exact registered root locator retained with an authorized scope.
///
/// `canonical_root` is routing evidence for the already-resolved
/// [`ResolvedScope`]. It cannot replace or manufacture that scope identity.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RegisteredRootLocatorV1 {
    pub project_id: ProjectId,
    pub profile: SharedProfileStoreLocatorV1,
    pub canonical_root: PathBuf,
}

impl RegisteredRootLocatorV1 {
    pub fn new(
        project_id: ProjectId,
        profile_id: UserProfileId,
        store_id: impl Into<String>,
        canonical_root: impl Into<PathBuf>,
    ) -> Result<Self, MultiRootQueryError> {
        let locator = Self {
            project_id,
            profile: SharedProfileStoreLocatorV1::new(profile_id, store_id)?,
            canonical_root: canonical_root.into(),
        };
        locator.validate()?;
        Ok(locator)
    }

    pub fn validate(&self) -> Result<(), MultiRootQueryError> {
        self.project_id
            .validate()
            .map_err(|error| MultiRootQueryError::Invalid(error.to_string()))?;
        self.profile.validate()?;
        validate_absolute_root(&self.canonical_root)
    }
}

/// Exact registered-root selector accepted by scope-set CAS.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RegisteredRootSelectorV1 {
    pub project_id: ProjectId,
    pub root: PathBuf,
}

impl RegisteredRootSelectorV1 {
    pub fn new(
        project_id: ProjectId,
        root: impl Into<PathBuf>,
    ) -> Result<Self, MultiRootQueryError> {
        let selector = Self {
            project_id,
            root: root.into(),
        };
        selector.validate()?;
        Ok(selector)
    }

    pub fn validate(&self) -> Result<(), MultiRootQueryError> {
        self.project_id
            .validate()
            .map_err(|error| MultiRootQueryError::Invalid(error.to_string()))?;
        validate_absolute_root(&self.root)
    }
}

/// One exact application scope paired with its registered physical locator.
///
/// The scope remains the identity authority. The locator is retained only so
/// a later read or restart can reopen the exact registered root without an
/// active-graph or CWD fallback.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AuthorizedRoot {
    pub(super) scope: ResolvedScope,
    pub(super) locator: Option<RegisteredRootLocatorV1>,
}

impl AuthorizedRoot {
    pub(super) fn resolved(scope: ResolvedScope) -> Result<Self, AuthorizedScopeSetError> {
        scope
            .validate()
            .map_err(|error| AuthorizedScopeSetError::Invalid(error.to_string()))?;
        Ok(Self {
            scope,
            locator: None,
        })
    }

    pub(super) fn registered(
        scope: ResolvedScope,
        locator: RegisteredRootLocatorV1,
    ) -> Result<Self, AuthorizedScopeSetError> {
        scope
            .validate()
            .map_err(|error| AuthorizedScopeSetError::Invalid(error.to_string()))?;
        locator
            .validate()
            .map_err(|error| AuthorizedScopeSetError::Invalid(error.to_string()))?;
        if scope.project_id != locator.project_id {
            return Err(AuthorizedScopeSetError::Invalid(
                "registered locator project does not match its resolved scope".to_owned(),
            ));
        }
        Ok(Self {
            scope,
            locator: Some(locator),
        })
    }

    pub fn scope(&self) -> &ResolvedScope {
        &self.scope
    }

    pub fn locator(&self) -> Option<&RegisteredRootLocatorV1> {
        self.locator.as_ref()
    }
}

/// Ephemeral authorization input narrowed into one [`AuthorizedRoot`].
///
/// There is no parallel request-context model: admission consumes the
/// canonical application [`RequestContext`] and retains only its exact scope.
#[derive(Clone, Debug)]
pub struct AuthorizedRootAdmission {
    pub(super) context: RequestContext,
    pub(super) locator: RegisteredRootLocatorV1,
}

impl AuthorizedRootAdmission {
    pub fn new(
        context: RequestContext,
        locator: RegisteredRootLocatorV1,
    ) -> Result<Self, AuthorizedScopeSetError> {
        AuthorizedRoot::registered(context.scope().clone(), locator.clone())?;
        Ok(Self { context, locator })
    }
}

fn validate_locator_text(value: &str, field: &'static str) -> Result<(), MultiRootQueryError> {
    if value.is_empty()
        || value.trim() != value
        || value.len() > 512
        || value.chars().any(char::is_control)
    {
        return Err(MultiRootQueryError::Invalid(format!(
            "{field} is not canonical"
        )));
    }
    Ok(())
}

fn validate_absolute_root(root: &Path) -> Result<(), MultiRootQueryError> {
    if !root.is_absolute()
        || root
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return Err(MultiRootQueryError::Invalid(
            "registered root must be absolute and lexically normalized".to_owned(),
        ));
    }
    Ok(())
}
