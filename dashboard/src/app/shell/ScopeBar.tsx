import { useEffect } from 'react';
import { Command, Moon, Sun, X } from 'lucide-react';
import {
  registryAnnotation,
  registryReading,
  useProjectEntry,
} from '../../data/query/projectRegistry.ts';
import { cn } from '../../ui/cn';
import { useScope } from '../../data/scope/store.ts';
import { channelNumber, type Channel } from '../channels.ts';

function toggleTheme() {
  const root = document.documentElement;
  const next = root.dataset['theme'] === 'light' ? 'dark' : 'light';
  root.dataset['theme'] = next;
  localStorage.setItem('td-theme', next);
}

/**
 * The scope/workspace register (NAVIGATION.md "Persistent regions" 3): a
 * 52px register carrying `Project: all` or the reconciled project label with
 * its canonical ID, the active channel and title, and the shell's own
 * controls. Every view preserves and displays scope; transitions are
 * explicit — the only scope control here is the one that clears it.
 *
 * `channel` is the route's channel, resolved by the shell (which sits inside
 * the router) rather than read here, so the register can be rendered and
 * tested on its own. Absent, the channel cell is simply not drawn; nothing is
 * invented for it.
 */
export function ScopeBar({
  channel,
  onOpenPalette,
}: {
  channel?: Channel | null;
  onOpenPalette?: () => void;
}) {
  const scope = useScope((s) => s.scope);
  const selectAllProjects = useScope((s) => s.selectAllProjects);
  const reconcileScope = useScope((s) => s.reconcileScope);
  // The bounded registry read: one project by id, rather than a search through
  // a truncated listing that cannot distinguish "not registered" from "past the
  // end of the page". Its key is rooted at the registry prefix the daemon's
  // `project_registry_changed` invalidation names, so a rename or an
  // active-project switch re-runs the reconciliation below instead of leaving
  // this — the read every write control depends on — stale until a reload.
  const entry = useProjectEntry(scope.kind === 'project' ? scope.projectId : null);

  // The one place a selected project is reconciled against the registry, for
  // both its activation and its label. Every entry into a project scope — deep
  // link, command palette, Remote Brain, this bar — arrives `unresolved` with
  // an unverified label, and this read is what settles both. Controls consult
  // `scopeWritable`, so until this lands they report writability as unknown
  // rather than offering a write the gateway would refuse.
  useEffect(() => {
    reconcileScope(registryReading(entry.data));
  }, [entry.data, reconcileScope]);

  // The label is the reconciled one from the store, not a second lookup: the
  // bar has to call the project what the write-target prose calls it, and two
  // lookups over the same payload are two things to keep in agreement. What
  // the bar adds is why the name may not be canonical yet.
  const annotation = scope.kind === 'project' ? registryAnnotation(entry.data) : null;
  return (
    // A minimum rather than a height: the scope cell stacks a name over its
    // canonical ID, so at 200% text zoom the pair is taller than the register.
    // Pinned to exactly 52 the bar could not take them, and a clip would cut
    // off the project name and its `unverified`/`not in registry` caveat at
    // precisely the zoom level someone would be using in order to read them.
    <header className="flex min-h-[var(--shell-register)] shrink-0 items-stretch border-b border-edge-frame bg-surface-1">
      {/* `min-w-0` without `overflow-hidden`: the horizontal containment comes
        * from `truncate` on the label itself, which shortens the name and
        * leaves the caveat beside it readable. */}
      <div className="flex min-w-0 flex-1 items-stretch" aria-label="Active scope">
        {scope.kind === 'project' ? (
          <button
            type="button"
            onClick={selectAllProjects}
            aria-label={
              annotation
                ? `Clear project scope ${scope.label} · ${annotation}`
                : `Clear project scope ${scope.label}`
            }
            className={cn(
              'group flex min-w-0 flex-col justify-center gap-1 border-r border-edge-subtle px-3 text-left',
              'bg-alert/10 hover:bg-alert/20',
            )}
          >
            <span className="flex min-w-0 items-baseline gap-1.5">
              <span className="text-base text-text-secondary">Project:</span>
              <span className="truncate text-base text-alert" data-scope-label>
                {scope.label}
              </span>
              {/* Kept out of the truncating value so a long label cannot clip
                * the caveat away and leave the name looking confirmed. */}
              {annotation ? (
                <span
                  data-scope-label-annotation={annotation}
                  className="shrink-0 text-3xs text-text-secondary"
                >
                  · {annotation}
                </span>
              ) : null}
              <X aria-hidden size={10} className="shrink-0 self-center text-text-muted" />
            </span>
            <span className="flex min-w-0 items-center gap-1.5">
              <span className="td-legend">ID</span>
              <span className="td-value truncate text-3xs text-text-secondary" data-scope-id>
                {scope.projectId}
              </span>
            </span>
          </button>
        ) : (
          <span className="flex min-w-0 shrink-0 items-baseline gap-1.5 border-r border-edge-subtle px-3 py-2">
            <span className="text-base text-text-secondary">Project:</span>
            <span className="text-base text-alert">all</span>
          </span>
        )}
        {channel ? (
          <span
            className="flex min-w-0 shrink-0 items-center gap-2 border-r border-edge-subtle px-3"
            aria-label="Active channel"
            data-active-channel={channel.path}
          >
            <span className="td-value text-2xs text-accent" data-cell="numeric">
              {channelNumber(channel.path)}
            </span>
            <span className="td-title text-text-primary">{channel.label}</span>
          </span>
        ) : null}
        <span aria-hidden className="flex-1 border-r border-edge-subtle" />
      </div>
      <button
        type="button"
        onClick={onOpenPalette}
        className={cn(
          'flex shrink-0 flex-col items-start justify-center gap-1 border-r border-edge-subtle px-3',
          'hover:bg-surface-2',
        )}
        aria-label="Open command palette"
      >
        <span className="td-legend">Command</span>
        <span className="flex items-center gap-1 text-text-secondary">
          <Command aria-hidden size={11} />
          <span className="td-value text-xs">K</span>
        </span>
      </button>
      <button
        type="button"
        onClick={toggleTheme}
        aria-label="Toggle theme"
        className="flex w-[var(--touch-target-min)] shrink-0 items-center justify-center text-text-muted hover:bg-surface-2 hover:text-text-primary"
      >
        <Sun aria-hidden size={14} className="hidden [[data-theme=light]_&]:block" />
        <Moon aria-hidden size={14} className="[[data-theme=light]_&]:hidden" />
      </button>
    </header>
  );
}
