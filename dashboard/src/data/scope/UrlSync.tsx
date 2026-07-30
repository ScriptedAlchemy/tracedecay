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
    // Read the store rather than the closure: this effect does not depend on
    // `scope`, so the value captured here can be a render behind.
    const current = useScope.getState().scope;
    applyingFromUrl.current = true;
    if (id) {
      const label = searchParams.get('scopeLabel') ?? id;
      // Only reselect when the link names a different *project*. This effect
      // runs on every search-param change, including params that belong to a
      // workspace and have nothing to do with scope, and reselecting would
      // reset a project whose activation the registry had already resolved
      // back to `unresolved` — withdrawing a legitimately enabled write
      // control on an unrelated filter change.
      //
      // The id alone decides that, deliberately. Comparing the label too used
      // to look like extra care and was in fact the undoing of reconciliation:
      // once the registry replaced a stale `scopeLabel` with the canonical
      // name, the store's label no longer matched the URL's, so the next
      // unrelated param change re-applied the URL's claim and dropped the
      // scope back to `unresolved`. A label is not identity — it is the thing
      // being corrected — so it cannot be a reason to reselect.
      if (current.kind !== 'project' || current.projectId !== id) {
        // A deep link carries an opaque id and a display label, and neither
        // says whether this is the active project. The scope therefore arrives
        // unresolved, and stays that way until something reads the registry —
        // it must not present as writable on the strength of a URL.
        selectProject(id, label, 'unresolved');
      }
    } else if (current.kind !== 'all') {
      selectAllProjects();
    }
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
    // The label is compared as well as the id, so a label the registry
    // corrected reaches the address bar. Without this the URL would keep
    // asserting the stale name it was opened with, and a reload would put that
    // name back on screen until the registry answered again. Reselection no
    // longer keys on the label, so writing it here cannot loop.
    const labelSettled =
      scope.kind !== 'project' || searchParams.get('scopeLabel') === scope.label;
    if (current === next && labelSettled) return;
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
