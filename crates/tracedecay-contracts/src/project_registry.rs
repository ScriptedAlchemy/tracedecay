//! Portable project-registry presentation DTOs and the MCP/daemon read port.
//!
//! MCP owns selector parsing and rendering; the daemon owns the registry
//! database. Handlers therefore name a [`ProjectRegistryReadPort`] instead of a
//! concrete registry store, and receive presentation views plus the typed
//! unresolved state.
//!
//! Genuine read failures keep [`tracedecay_domain::errors::TraceDecayError`] so an
//! unreadable registry stays a failure instead of collapsing into a
//! successful empty listing.

use std::fmt::Write as _;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracedecay_domain::errors::Result;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProjectRegistrySummary {
    pub project_count: usize,
    pub repo_count: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProjectRepoGroup {
    pub label: String,
    pub git_common_dir: Option<String>,
    pub project_count: usize,
    pub branches: Vec<String>,
    pub projects: Vec<ProjectRegistryEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProjectRegistryEntry {
    pub project_id: String,
    pub label: String,
    pub project_root: String,
    pub canonical_root: String,
    pub kind: String,
    /// Repository default branch recorded at registration.
    pub default_branch: Option<String>,
    /// Branch the checkout's HEAD names now; `None` when HEAD is detached or
    /// no checkout of this project could be read.
    pub head_branch: Option<String>,
    pub branches: Vec<String>,
    pub store_count: usize,
    pub artifact_count: usize,
    pub alias_count: usize,
    pub last_seen_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_active: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PublicCodeProject {
    pub project_id: String,
    pub label: String,
    pub project_root: String,
    pub display_root: String,
    pub canonical_root: String,
    pub git_common_dir: Option<String>,
    /// Repository default branch recorded at registration.
    pub default_branch: Option<String>,
    /// Branch the checkout's HEAD names now; `None` when HEAD is detached or
    /// no checkout of this project could be read.
    pub head_branch: Option<String>,
    pub created_at: i64,
    pub last_seen_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_active: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProjectRegistryView {
    pub summary: ProjectRegistrySummary,
    pub project_tree: Vec<ProjectRepoGroup>,
}

/// Terminal/tool-output text for one registry view.
///
/// Shared by the MCP registry tools and `tracedecay project list`. Lives
/// next to the presentation DTO so neither `tracedecay-mcp` nor
/// `tracedecay-dashboard-api` owns a private copy.
pub fn render_project_registry_view(title: &str, view: &ProjectRegistryView) -> String {
    if view.summary.project_count == 0 {
        return format!("No {title} found.");
    }
    let mut out = String::new();
    let _ = writeln!(
        out,
        "Found {} {title} across {} repositories.\n\nRepositories:",
        view.summary.project_count, view.summary.repo_count
    );
    for group in &view.project_tree {
        let group_branches = if group.branches.is_empty() {
            "-".to_string()
        } else {
            group.branches.join(", ")
        };
        let _ = writeln!(out, "- {} (branches: {})", group.label, group_branches);
        for project in &group.projects {
            let marker = if project.is_active == Some(true) {
                " *"
            } else {
                ""
            };
            let branches = if project.branches.is_empty() {
                "-".to_string()
            } else {
                project.branches.join(", ")
            };
            let _ = writeln!(
                out,
                "  - `{}`{} [{}] branches: {}; stores: {}; path: {}",
                project.project_id,
                marker,
                project.kind,
                branches,
                project.store_count,
                project.project_root
            );
        }
    }
    if view.summary.truncated {
        out.push_str("\nResult truncated; increase limit for more projects.\n");
    }
    out
}

/// Which registered project a context read names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectRegistrySelector {
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
pub enum ProjectRegistryListingScope {
    /// Every registered project, newest registration order preserved.
    All,
    /// Registered projects matching a caller query.
    Matching { query: String },
}

/// A bounded listing read together with the project root the dispatched graph
/// serves, when one is mounted.
///
/// Routing stays with the caller: MCP names the served root, and the daemon
/// resolves that root's registry identity to mark the active project. A
/// projectless connection reads the same registry with no active root, so no
/// project is marked active.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectRegistryListingCommand {
    pub active_project_root: Option<PathBuf>,
    pub scope: ProjectRegistryListingScope,
    pub limit: usize,
}

/// A single-project context read, scoped the same way as a listing read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectRegistryContextCommand {
    pub active_project_root: Option<PathBuf>,
    pub selector: ProjectRegistrySelector,
}

/// A bounded page of registered projects with its presentation view.
#[derive(Clone, Debug)]
pub struct ProjectRegistryListingView {
    pub registry_path: PathBuf,
    pub truncated: bool,
    pub view: ProjectRegistryView,
    pub projects: Vec<PublicCodeProject>,
}

/// One resolved registered project with its aliases and store instances.
#[derive(Clone, Debug)]
pub struct ProjectRegistryContextView {
    pub registry_path: PathBuf,
    pub is_active: bool,
    pub project: PublicCodeProject,
    /// Alias and store rows serialized by their owning authority. MCP renders
    /// them verbatim and never interprets them, so the exact registry record
    /// shape crosses the boundary unchanged.
    pub aliases: Vec<Value>,
    pub stores: Vec<Value>,
}

/// Closed set of single-project context results.
#[derive(Clone, Debug)]
pub enum ProjectRegistryContextOutcome {
    Context(Box<ProjectRegistryContextView>),
    /// The registry answered, and no registered project matches the selector.
    NotFound {
        registry_path: PathBuf,
    },
}

pub type ProjectRegistryListingFuture<'a> =
    Pin<Box<dyn Future<Output = Result<ProjectRegistryListingView>> + Send + 'a>>;
pub type ProjectRegistryContextFuture<'a> =
    Pin<Box<dyn Future<Output = Result<ProjectRegistryContextOutcome>> + Send + 'a>>;

/// The one path the registry reads take through the daemon's profile registry.
pub trait ProjectRegistryReadPort: Send + Sync {
    fn list(&self, command: ProjectRegistryListingCommand) -> ProjectRegistryListingFuture<'_>;

    fn context(&self, command: ProjectRegistryContextCommand) -> ProjectRegistryContextFuture<'_>;
}
