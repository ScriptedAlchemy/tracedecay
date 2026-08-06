//! Scope-specific MCP servers routed through one canonical physical DB owner.
//!
//! `Database` performs the actual same-process handle sharing; this registry
//! keeps daemon cache aliases and branch-drift rekeys consistent with it, and
//! settles the post-open FTS health a deferred open handed to publication.
//!
//! Relocated verbatim from `daemon.rs` as a pure structural split; no logic
//! or signatures changed. `use super::*` re-exposes every name the parent
//! `daemon` module had in scope so the moved code resolves unchanged.

use super::*;

/// Scope-specific MCP servers routed through one canonical physical DB owner.
/// `Database` performs the actual same-process handle sharing; this registry
/// keeps daemon cache aliases and branch-drift rekeys consistent with it.
// Fields are `pub(super)`: the entry was private inside the flat `daemon.rs`,
// which made it visible to every `crate::daemon` descendant — `branch_admin`
// reads `server` directly, so the split must preserve that reach.
pub(super) struct DatabaseOwnerEntry<Server> {
    pub(super) server: Server,
    pub(super) last_used: Instant,
    pub(super) publication: ProjectServerPublication,
}

pub(super) struct DatabaseOwnerRegistry<Server = Arc<crate::mcp::McpServer>> {
    pub(super) servers: HashMap<ProjectServerKey, DatabaseOwnerEntry<Server>>,
    pub(super) aliases: HashMap<ProjectRouteKey, ProjectServerKey>,
    synchronous_health: HashSet<StoreOwnerKey>,
}

impl<Server> Default for DatabaseOwnerRegistry<Server> {
    fn default() -> Self {
        Self {
            servers: HashMap::new(),
            aliases: HashMap::new(),
            synchronous_health: HashSet::new(),
        }
    }
}

impl<Server> DatabaseOwnerRegistry<Server> {
    #[cfg(any(unix, test))]
    pub(super) fn get(&self, key: &ProjectServerKey) -> Option<&Server> {
        self.servers.get(key).map(|entry| &entry.server)
    }

    pub(super) fn get_ready(&self, key: &ProjectServerKey) -> Option<&Server> {
        self.servers
            .get(key)
            .filter(|entry| entry.publication.satisfies(ProjectServerRequirement::Core))
            .map(|entry| &entry.server)
    }

    #[cfg(test)]
    pub(super) fn insert(&mut self, key: ProjectServerKey, server: Server) {
        self.insert_at(key, server, Instant::now());
    }

    #[cfg(test)]
    pub(super) fn insert_at(&mut self, key: ProjectServerKey, server: Server, last_used: Instant) {
        self.servers.insert(
            key,
            DatabaseOwnerEntry {
                server,
                last_used,
                publication: ProjectServerPublication::RegisteredHostIngest,
            },
        );
    }

    pub(super) fn get_route(
        &self,
        route: &ProjectRouteKey,
    ) -> Option<(&ProjectServerKey, &Server)> {
        let key = self.aliases.get(route)?;
        let (key, entry) = self.servers.get_key_value(key)?;
        entry
            .publication
            .satisfies(ProjectServerRequirement::Core)
            .then_some((key, &entry.server))
    }

    pub(super) fn get_route_and_touch(
        &mut self,
        route: &ProjectRouteKey,
    ) -> Option<(&ProjectServerKey, &Server)> {
        self.get_route_and_touch_for(route, ProjectServerRequirement::Core)
    }

    pub(super) fn get_route_and_touch_for(
        &mut self,
        route: &ProjectRouteKey,
        requirement: ProjectServerRequirement,
    ) -> Option<(&ProjectServerKey, &Server)> {
        let key = self.aliases.get(route)?.clone();
        let entry = self.servers.get_mut(&key)?;
        if !entry.publication.satisfies(requirement) {
            return None;
        }
        entry.last_used = Instant::now();
        Some((self.aliases.get(route)?, &entry.server))
    }

    pub(super) fn mark_ready(&mut self, key: &ProjectServerKey) -> bool {
        let Some(entry) = self.servers.get_mut(key) else {
            return false;
        };
        entry.publication = ProjectServerPublication::Core;
        entry.last_used = Instant::now();
        true
    }

    pub(super) fn replace_ready_if<F>(
        &mut self,
        key: &ProjectServerKey,
        replacement: Server,
        matches: F,
    ) -> bool
    where
        F: FnOnce(&Server) -> bool,
    {
        let Some(entry) = self.servers.get_mut(key) else {
            return false;
        };
        if !entry.publication.satisfies(ProjectServerRequirement::Core) || !matches(&entry.server) {
            return false;
        }
        entry.server = replacement;
        entry.publication = ProjectServerPublication::RegisteredHostIngest;
        entry.last_used = Instant::now();
        true
    }

    pub(super) fn swap_ready_if<F>(
        &mut self,
        key: &ProjectServerKey,
        replacement: Server,
        matches: F,
    ) -> Option<Server>
    where
        F: FnOnce(&Server) -> bool,
    {
        let entry = self.servers.get_mut(key)?;
        if !entry.publication.satisfies(ProjectServerRequirement::Core) || !matches(&entry.server) {
            return None;
        }
        let displaced = std::mem::replace(&mut entry.server, replacement);
        entry.publication = ProjectServerPublication::Core;
        entry.last_used = Instant::now();
        Some(displaced)
    }

    #[cfg(test)]
    pub(super) fn remove(&mut self, key: &ProjectServerKey) -> Option<Server> {
        let entry = self.servers.remove(key)?;
        self.aliases.retain(|_, alias| alias != key);
        Some(entry.server)
    }

    pub(super) fn remove_owner(&mut self, owner: &StoreOwnerKey) -> Vec<Server> {
        let keys = self
            .servers
            .keys()
            .filter(|key| &key.owner == owner)
            .cloned()
            .collect::<Vec<_>>();
        let mut removed = Vec::with_capacity(keys.len());
        for key in keys {
            if let Some(entry) = self.servers.remove(&key) {
                removed.push(entry.server);
            }
        }
        self.aliases.retain(|_, key| &key.owner != owner);
        removed
    }

    pub(super) fn quarantine_and_remove_owner(&mut self, owner: &StoreOwnerKey) -> Vec<Server> {
        self.synchronous_health.insert(owner.clone());
        self.remove_owner(owner)
    }

    pub(super) fn requires_synchronous_health(&self, owner: &StoreOwnerKey) -> bool {
        self.synchronous_health.contains(owner)
    }

    pub(super) fn clear_synchronous_health(&mut self, owner: &StoreOwnerKey) {
        self.synchronous_health.remove(owner);
    }

    pub(super) fn bind_route(&mut self, route: ProjectRouteKey, key: ProjectServerKey) {
        debug_assert!(self.servers.contains_key(&key));
        if let Some(entry) = self.servers.get_mut(&key) {
            entry.last_used = Instant::now();
        }
        self.aliases.insert(route, key);
    }

    #[cfg(test)]
    pub(super) fn insert_route(
        &mut self,
        route: ProjectRouteKey,
        key: ProjectServerKey,
        server: Server,
    ) {
        self.insert(key.clone(), server);
        self.bind_route(route, key);
    }

    pub(super) fn insert_pending_route(
        &mut self,
        route: ProjectRouteKey,
        key: ProjectServerKey,
        server: Server,
    ) {
        self.servers.insert(
            key.clone(),
            DatabaseOwnerEntry {
                server,
                last_used: Instant::now(),
                publication: ProjectServerPublication::Pending,
            },
        );
        self.aliases.insert(route, key);
    }

    #[cfg(test)]
    pub(super) fn bind_or_insert_route(
        &mut self,
        route: ProjectRouteKey,
        key: ProjectServerKey,
        candidate: Server,
    ) -> (Server, bool)
    where
        Server: Clone,
    {
        if let Some(existing) = self.get(&key).cloned() {
            self.bind_route(route, key);
            return (existing, false);
        }
        self.insert_route(route, key, candidate.clone());
        (candidate, true)
    }

    pub(super) fn bind_or_insert_route_bounded<F>(
        &mut self,
        route: ProjectRouteKey,
        key: ProjectServerKey,
        candidate: Server,
        capacity: usize,
        mut is_leased: F,
    ) -> Option<(Server, bool)>
    where
        Server: Clone,
        F: FnMut(&Server) -> bool,
    {
        if let Some(existing) = self.servers.get_mut(&key) {
            if existing
                .publication
                .satisfies(ProjectServerRequirement::Core)
            {
                let server = existing.server.clone();
                self.bind_route(route, key);
                return Some((server, false));
            }
            existing.server = candidate.clone();
            existing.last_used = Instant::now();
            self.aliases.insert(route, key);
            return Some((candidate, true));
        }
        while self.servers.len() >= capacity {
            let evict = self
                .servers
                .iter()
                .filter(|(_, entry)| !is_leased(&entry.server))
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(key, _)| key.clone())?;
            self.servers.remove(&evict);
            self.aliases.retain(|_, key| key != &evict);
        }
        self.insert_pending_route(route, key, candidate.clone());
        Some((candidate, true))
    }

    pub(super) fn rekey(&mut self, old: &ProjectServerKey, new: &ProjectServerKey) -> bool {
        if old == new {
            return true;
        }
        let Some(server) = self.servers.remove(old) else {
            return false;
        };
        if self.servers.contains_key(new) {
            self.aliases.retain(|_, key| key != old);
            return false;
        }
        self.servers.insert(new.clone(), server);
        for key in self.aliases.values_mut() {
            if key == old {
                *key = new.clone();
            }
        }
        true
    }

    pub(super) fn values(&self) -> impl Iterator<Item = &Server> {
        self.servers.values().map(|entry| &entry.server)
    }
}

/// Post-open FTS health for one project publication.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum PostOpenHealthOutcome {
    /// The open owed publication nothing. It either applied its own post-open
    /// policy — including the crash preflight's skip, which proved the store
    /// healthy under both recovery locks and must not be re-spent on a second
    /// full `quick_check` — or opened read-only, where FTS damage cannot be
    /// repaired in place.
    SettledDuringOpen,
    /// The deferred check ran and found the retained handle intact.
    Intact,
    /// The deferred check rebuilt proven FTS-only damage. The report is the
    /// evidence publication has to make daemon-visible.
    Repaired(String),
}

fn repaired_post_open_health_log_fields(
    project_path: &Path,
    problem: &str,
) -> Vec<(&'static str, String)> {
    vec![
        ("project", project_path.display().to_string()),
        ("outcome", "fts_repaired".to_owned()),
        ("problem", problem.to_owned()),
    ]
}

/// Settles the post-open FTS health a deferred open handed to publication.
///
/// A deferred open publishes the core before its retained-handle check runs, so
/// publication owns what is left. `health` is absent exactly when the open owed
/// nothing. Repaired corruption is real evidence — an operator has to be able
/// to see that a store was rebuilt — so it is logged rather than discarded.
pub(super) async fn settle_deferred_post_open_health(
    project_path: &Path,
    health: Option<impl std::future::Future<Output = Result<Option<String>>>>,
) -> Result<PostOpenHealthOutcome> {
    let Some(health) = health else {
        return Ok(PostOpenHealthOutcome::SettledDuringOpen);
    };
    let Some(problem) = health.await? else {
        return Ok(PostOpenHealthOutcome::Intact);
    };
    log_daemon_event(
        "project_open_health",
        &repaired_post_open_health_log_fields(project_path, &problem),
    );
    Ok(PostOpenHealthOutcome::Repaired(problem))
}
