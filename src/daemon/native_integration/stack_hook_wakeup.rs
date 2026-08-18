//! `CursorDesktop`'s opaque, authenticated availability wakeup for stack delivery.
//!
//! This is only an in-process lookup for a mounted project runtime. The
//! registered database remains the durable queue authority; Hook V2 learns no
//! signal, recipient, stack, or actor details and cannot settle a delivery.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use tracedecay_application::ResolvedScope;

use super::stack_runtime::DaemonGitHubStackRuntimeV1;

type HookStackRuntimeRegistry =
    Mutex<BTreeMap<([u8; 16], [u8; 16]), Weak<DaemonGitHubStackRuntimeV1>>>;

fn runtime_registry() -> &'static HookStackRuntimeRegistry {
    static REGISTRY: OnceLock<HookStackRuntimeRegistry> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// Associates a Hook V2 binding with its project-open runtime. The hook only
/// receives an opaque availability bit after this exact lookup.
pub(crate) fn register_github_stack_hook_runtime(
    scope: &ResolvedScope,
    runtime: &Arc<DaemonGitHubStackRuntimeV1>,
) {
    let (project_id, worktree_id) = crate::hooks::hook_scope_locators(scope);
    if let Ok(mut registry) = runtime_registry().lock() {
        registry.insert((project_id, worktree_id), Arc::downgrade(runtime));
    }
}

/// Returns only whether an authenticated `CursorDesktop` hook should wake its
/// user. Store failures suppress a wakeup; durable `host_pending` rows remain
/// available for a later hook admission and MCP expansion.
pub(crate) fn github_stack_hook_available(project_id: [u8; 16], worktree_id: [u8; 16]) -> bool {
    let runtime = runtime_registry().lock().ok().and_then(|mut registry| {
        let runtime = registry
            .get(&(project_id, worktree_id))
            .and_then(Weak::upgrade);
        if runtime.is_none() {
            registry.remove(&(project_id, worktree_id));
        }
        runtime
    });
    runtime
        .and_then(|runtime| runtime.pending_host_deliveries().ok())
        .is_some_and(|deliveries| !deliveries.is_empty())
}
