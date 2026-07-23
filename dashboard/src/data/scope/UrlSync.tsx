import { useEffect, useRef } from 'react';
import { useSearchParams } from 'react-router';
import { useScope } from './store.ts';

/** Bidirectional URL <-> scope binding (plan 11: deep links capture opaque
 * scope; plan 11a: narrowing updates the URL). The URL carries only the
 * opaque project id and display label — never paths or payloads. A missing
 * param is the all-projects default; an unknown id still renders as a chip
 * whose reads resolve truthfully server-side. */
export function ScopeUrlSync() {
  const [searchParams, setSearchParams] = useSearchParams();
  const scope = useScope((s) => s.scope);
  const selectProject = useScope((s) => s.selectProject);
  const selectAllProjects = useScope((s) => s.selectAllProjects);
  const applyingFromUrl = useRef(false);

  // URL -> store (initial load and back/forward navigation).
  useEffect(() => {
    const id = searchParams.get('scope');
    applyingFromUrl.current = true;
    if (id) selectProject(id, searchParams.get('scopeLabel') ?? id);
    else if (scope.kind !== 'all') selectAllProjects();
    applyingFromUrl.current = false;
    // Only URL changes drive this effect; store echoes are guarded below.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [searchParams]);

  // Store -> URL (user selections), replace-not-push to keep history calm.
  useEffect(() => {
    if (applyingFromUrl.current) return;
    // Stale-render guard: on mount (and any lagging render) this closure can
    // hold a scope older than the store — echoing it would erase the deep
    // link the first effect just applied.
    if (scope !== useScope.getState().scope) return;
    const current = searchParams.get('scope');
    const next = scope.kind === 'project' ? scope.projectId : null;
    if (current === next) return;
    const params = new URLSearchParams(searchParams);
    if (scope.kind === 'project') {
      params.set('scope', scope.projectId);
      params.set('scopeLabel', scope.label);
    } else {
      params.delete('scope');
      params.delete('scopeLabel');
    }
    setSearchParams(params, { replace: true });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [scope]);

  return null;
}
