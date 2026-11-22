import { matchPath, type Location } from 'react-router';
import { scopedWorkspacePath, type DashboardScope } from '../data/scope/store.ts';

/** Reselecting the current workspace preserves its view, replay and selection.
 * A real workspace or project change carries only the admitted project scope. */
export function workspaceNavigationTarget(
  scope: DashboardScope,
  path: string,
  location: Pick<Location, 'pathname' | 'search' | 'hash'>,
): { to: string; replace: boolean } {
  const params = new URLSearchParams(location.search);
  const sameScope = scope.kind === 'all'
    ? !params.has('scope')
    : params.get('scope') === scope.projectId;
  const sameWorkspace = matchPath(`/${path}`, location.pathname) !== null ||
    (path === 'brain' && location.pathname === '/');
  if (!sameScope || !sameWorkspace) {
    return { to: scopedWorkspacePath(scope, path), replace: false };
  }

  // The URL label is a display claim. Correct it without changing entity,
  // renderer or window parameters belonging to the current admitted project.
  const staleLabel = scope.kind === 'all'
    ? params.has('scopeLabel')
    : params.get('scopeLabel') !== scope.label;
  let search = location.search;
  if (staleLabel) {
    if (scope.kind === 'project') params.set('scopeLabel', scope.label);
    else params.delete('scopeLabel');
    const encoded = params.toString();
    search = encoded ? `?${encoded}` : '';
  }
  return { to: `${location.pathname}${search}${location.hash}`, replace: true };
}
