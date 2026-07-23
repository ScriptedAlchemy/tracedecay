import { create } from 'zustand';

/** Dashboard-wide scope (plan 11a all-projects-first model): every workspace
 * renders the all-projects aggregate until a specific project is selected.
 * Project-scoped reads route through the `/api/projects/{id}/…` gateway. */
export type DashboardScope =
  | { kind: 'all' }
  | { kind: 'project'; projectId: string; label: string };

interface ScopeState {
  scope: DashboardScope;
  selectProject: (projectId: string, label: string) => void;
  selectAllProjects: () => void;
}

export const useScope = create<ScopeState>((set) => ({
  scope: { kind: 'all' },
  selectProject: (projectId, label) =>
    set({ scope: { kind: 'project', projectId, label } }),
  selectAllProjects: () => set({ scope: { kind: 'all' } }),
}));

/** Prefix for project-gateway API routes under the current scope. The active
 * project's own surfaces are served unprefixed; a selected project routes
 * through the registry gateway. */
export function scopeApiBase(scope: DashboardScope): string {
  return scope.kind === 'project'
    ? `/api/projects/${encodeURIComponent(scope.projectId)}`
    : '';
}
