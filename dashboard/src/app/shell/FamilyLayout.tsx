import { NavLink, Outlet } from 'react-router';
import { scopedWorkspacePath, useScope } from '../../data/scope/store.ts';
import { cn } from '../../ui/cn';
import type { Channel } from '../channels.ts';

/**
 * A task family's page: a visible tab strip over its views, then the view.
 *
 * The strip is navigation, not a tab panel: each view is its own route with its
 * own address, so the links are real links, the current view is marked with
 * `aria-current`, and a view reloads, bookmarks and deep-links like any page.
 * Every sibling is always on screen, there is no overflow menu to open and no
 * view is hidden behind another. Project scope rides along on each link, the
 * same as on the rail.
 */
export function FamilyLayout({ channel }: { channel: Channel }) {
  const scope = useScope((state) => state.scope);
  return (
    <>
      <nav
        aria-label={`${channel.label} views`}
        data-family-views={channel.path}
        className="flex min-w-0 flex-wrap items-stretch gap-1 border-b border-edge-subtle bg-surface-1 px-2 py-1"
      >
        {channel.views.map((view) => (
          <NavLink
            key={view.path}
            to={scopedWorkspacePath(scope, view.path)}
            // The family's first view answers at the family path, which every
            // other view's path extends, so only it needs an exact match.
            end={view.path === channel.path}
            className={({ isActive }) =>
              cn(
                'flex min-h-[var(--touch-target-min)] items-center border px-3 text-2xs uppercase tracking-[0.14em]',
                'focus-visible:outline focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-accent',
                isActive
                  ? 'border-edge-strong bg-surface-3 text-text-primary'
                  : 'border-transparent text-text-secondary hover:bg-surface-2 hover:text-text-primary',
              )
            }
          >
            {view.label}
          </NavLink>
        ))}
      </nav>
      <Outlet />
    </>
  );
}
