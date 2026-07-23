import { Outlet } from 'react-router';
import { NavRail } from './NavRail';
import { ScopeBar } from './ScopeBar';
import { StatusStrip } from './StatusStrip';

/** Shell layout per plan 11a: nav rail | (scope bar / content / status strip).
 * The inspector panel mounts inside workspace content (archetype-owned) so
 * its width interacts with the content grid, not the shell. */
export function Shell() {
  return (
    <div className="flex h-dvh w-full overflow-hidden bg-surface-0 text-text-primary">
      <a
        href="#td-main"
        className="sr-only focus:not-sr-only focus:absolute focus:left-2 focus:top-2 focus:z-50 focus:rounded-[var(--radius-standard)] focus:bg-surface-3 focus:px-3 focus:py-2"
      >
        Skip to content
      </a>
      <NavRail />
      <div className="flex min-w-0 flex-1 flex-col">
        <ScopeBar />
        <main id="td-main" className="min-h-0 flex-1 overflow-auto">
          <Outlet />
        </main>
        <StatusStrip />
      </div>
    </div>
  );
}
