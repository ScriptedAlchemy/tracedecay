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

/** Never-scoped surfaces: the registry itself and the dashboard chrome. */
const UNSCOPED_PREFIXES = ['/api/projects', '/api/dashboard'];

/** Rewrites an `/api/...` URL for the current scope. A selected project
 * routes through the read-only project gateway, which rewrites
 * `/api/projects/{id}/{tail}` back to `/api/{tail}` against that project's
 * state; the all-projects default and the active project stay unprefixed. */
export function scopedUrl(scope: DashboardScope, url: string): string {
  if (scope.kind !== 'project') return url;
  if (!url.startsWith('/api/')) return url;
  if (UNSCOPED_PREFIXES.some((prefix) => url.startsWith(prefix))) return url;
  return `/api/projects/${encodeURIComponent(scope.projectId)}/${url.slice('/api/'.length)}`;
}

/** Cache-key token for the current scope (splits query caches per scope). */
export function scopeKey(scope: DashboardScope): string {
  return scope.kind === 'project' ? `project:${scope.projectId}` : 'all';
}
