//! Typed contract for daemon-owned project-registry reads.
//!
//! MCP owns selector parsing and rendering; the daemon owns the registry
//! database. Handlers in this tree therefore name a
//! [`ProjectRegistryReadPort`] instead of a `RegisteredGlobalDb`, and receive
//! presentation views plus the typed missing-registry and unresolved states.
//!
//! Genuine read failures keep [`crate::errors::TraceDecayError`] so an
//! unreadable registry stays a failure instead of collapsing into a
//! successful empty listing.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;

use serde_json::Value;

use crate::errors::Result;
use crate::project_registry::{ProjectRegistryView, PublicCodeProject};

/// Which registered project a context read names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ProjectRegistrySelector {
    /// An exact registered `project_id`.
    ProjectId(String),
    /// A filesystem path. `allow_git_identity` mirrors the caller-supplied
    /// selector shape: only an explicit absolute path may fall back to Git
    /// identity, so a bare relative path never adopts a sibling checkout.
    Path {
        path: PathBuf,
        allow_git_identity: bool,
    },
}

/// Which registered projects a listing read covers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ProjectRegistryListingScope {
    /// Every registered project, newest registration order preserved.
    All,
    /// Registered projects matching a caller query.
    Matching { query: String },
}

/// A bounded listing read together with the project root the dispatched graph
/// serves.
///
/// Routing stays with the caller: MCP names the served root, and the daemon
/// resolves that root's registry identity to mark the active project.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProjectRegistryListingCommand {
    pub(crate) active_project_root: PathBuf,
    pub(crate) scope: ProjectRegistryListingScope,
    pub(crate) limit: usize,
}

/// A single-project context read, scoped the same way as a listing read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProjectRegistryContextCommand {
    pub(crate) active_project_root: PathBuf,
    pub(crate) selector: ProjectRegistrySelector,
}

/// A bounded page of registered projects with its presentation view.
#[derive(Clone, Debug)]
pub(crate) struct ProjectRegistryListingView {
    pub(crate) registry_path: PathBuf,
    pub(crate) truncated: bool,
    pub(crate) view: ProjectRegistryView,
    pub(crate) projects: Vec<PublicCodeProject>,
}

/// One resolved registered project with its aliases and store instances.
#[derive(Clone, Debug)]
pub(crate) struct ProjectRegistryContextView {
    pub(crate) registry_path: PathBuf,
    pub(crate) is_active: bool,
    pub(crate) project: PublicCodeProject,
    /// Alias and store rows serialized by their owning authority. MCP renders
    /// them verbatim and never interprets them, so the exact registry record
    /// shape crosses the boundary unchanged.
    pub(crate) aliases: Vec<Value>,
    pub(crate) stores: Vec<Value>,
}

/// Closed set of listing results.
#[derive(Clone, Debug)]
pub(crate) enum ProjectRegistryListingOutcome {
    Listing(ProjectRegistryListingView),
    /// No registry authority is mounted for this profile. This is a state, not
    /// an empty listing: callers must report it as such.
    RegistryUnavailable,
}

/// Closed set of single-project context results.
#[derive(Clone, Debug)]
pub(crate) enum ProjectRegistryContextOutcome {
    Context(ProjectRegistryContextView),
    /// The registry answered, and no registered project matches the selector.
    NotFound {
        registry_path: PathBuf,
    },
    RegistryUnavailable,
}

pub(crate) type ProjectRegistryListingFuture<'a> =
    Pin<Box<dyn Future<Output = Result<ProjectRegistryListingOutcome>> + Send + 'a>>;
pub(crate) type ProjectRegistryContextFuture<'a> =
    Pin<Box<dyn Future<Output = Result<ProjectRegistryContextOutcome>> + Send + 'a>>;

/// The one path MCP handlers use to read the project registry.
pub(crate) trait ProjectRegistryReadPort: Send + Sync {
    fn list(&self, command: ProjectRegistryListingCommand) -> ProjectRegistryListingFuture<'_>;

    fn context(&self, command: ProjectRegistryContextCommand) -> ProjectRegistryContextFuture<'_>;
}

/// Reads the registry through `port`, reporting the typed missing-registry
/// state when no port is mounted.
pub(crate) async fn list_registered_projects(
    port: Option<&dyn ProjectRegistryReadPort>,
    command: ProjectRegistryListingCommand,
) -> Result<ProjectRegistryListingOutcome> {
    match port {
        Some(port) => port.list(command).await,
        None => Ok(ProjectRegistryListingOutcome::RegistryUnavailable),
    }
}

/// Resolves one registered project through `port`, reporting the typed
/// missing-registry state when no port is mounted.
pub(crate) async fn read_registered_project_context(
    port: Option<&dyn ProjectRegistryReadPort>,
    command: ProjectRegistryContextCommand,
) -> Result<ProjectRegistryContextOutcome> {
    match port {
        Some(port) => port.context(command).await,
        None => Ok(ProjectRegistryContextOutcome::RegistryUnavailable),
    }
}
