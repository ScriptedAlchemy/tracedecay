import * as Dialog from '@radix-ui/react-dialog';
import { Command as CommandIcon, CornerDownLeft, Search } from 'lucide-react';
import { useEffect, useMemo, useRef, useState } from 'react';
import { useLocation, useNavigate } from 'react-router';
import {
  CHANNELS,
  channelNumber,
  paletteMatchesQuery,
  workspaceSearchText,
} from '../channels.ts';
import { cn } from '../../ui/cn';
import { useProjectRegistry, projectRegistryPayload } from '../../data/query/projectRegistry.ts';
import {
  activationFor,
  scopedWorkspacePath,
  useScope,
} from '../../data/scope/store.ts';

interface CommandPaletteRow {
  id: string;
  label: string;
  hint: string;
  searchText: string;
  /** The channel number for a workspace row; project rows have none. */
  channel: string | null;
  action: () => void;
}

/**
 * The DOM id of one option row, by position.
 *
 * Positional rather than derived from the entry, because a project entry's id
 * carries its `canonical_root`, a filesystem path, which routinely contains
 * spaces and may contain quotes or `#`. Interpolated into an `id`, that
 * produced an attribute HTML forbids whitespace in, and
 * `aria-activedescendant` is an IDREF: a screen reader could not resolve the
 * active option for any project checked out under a path with a space in it,
 * which is the population most likely to be using the palette to switch
 * between them. The position is the only part of a row that is guaranteed to
 * be a valid identifier, and it is exactly what the reference needs to name.
 */
function optionId(index: number): string {
  return `td-palette-option-${index}`;
}

/** Command palette: scope-aware search across workspaces, registered
 * projects. Lazy-loaded from Shell after first open so @radix-ui/react-dialog
 * stays out of the initial shell chunk. */
export function CommandPalette({
  open,
  onOpenChange,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const navigate = useNavigate();
  const location = useLocation();
  const scope = useScope((state) => state.scope);
  const selectProject = useScope((s) => s.selectProject);
  const [query, setQuery] = useState('');
  const [selection, setSelection] = useState<{
    query: string;
    index: number;
    rowId: string | null;
  }>({ query: '', index: 0, rowId: null });
  // Entities: registered projects become scope-setting results. Fetched only
  // while the palette is open; a failed registry read simply yields no
  // project rows (workspace navigation never depends on it).
  // The shared registry read, so opening the palette on a surface that already
  // listed the registry costs nothing, and so a registry change invalidates this
  // too, it used to have a private key that no event named.
  const projects = useProjectRegistry({ enabled: open });

  const entries = useMemo<CommandPaletteRow[]>(() => {
    const workspaceEntries = CHANNELS.map((channel) => ({
      id: `nav:${channel.path}`,
      label: channel.label,
      hint: channel.taskHint,
      searchText: workspaceSearchText(channel),
      channel: channelNumber(channel.path),
      action: () => {
        // Reselecting this workspace is not a renderer or selection reset.
        // Only an actual workspace/scope transition replaces view-owned state.
        const params = new URLSearchParams(location.search);
        const sameScope = scope.kind === 'all'
          ? !params.has('scope') && !params.has('scopeLabel')
          : params.get('scope') === scope.projectId && params.get('scopeLabel') === scope.label;
        // The index route mounts Brain too. Reopening it through the palette
        // must preserve field=sigma and entity selection just like /brain.
        const sameWorkspace = location.pathname === `/${channel.path}` ||
          (channel.path === 'brain' && location.pathname === '/');
        if (!sameWorkspace || !sameScope) {
          navigate(scopedWorkspacePath(scope, channel.path));
        }
        onOpenChange(false);
      },
    }));
    const listing = projectRegistryPayload(projects.data);
    const projectEntries =
      listing?.status === 'ok'
        ? (listing.project_tree ?? []).flatMap((group) =>
            group.projects.map((project) => ({
              id: `scope:${project.project_id}:${project.canonical_root}`,
              label: project.label,
              hint: `project · ${group.label}`,
              searchText: project.label.toLowerCase(),
              channel: null,
              action: () => {
                // The listing already measured `is_active`, against the same
                // `active_project_id` the gateway accepts writes on, so the
                // pick starts from that answer instead of `unresolved`. It is
                // read through the shared authority rather than compared here,
                // so a row that omits the field stays unresolved, the daemon
                // marks it optional, and absent is not "no".
                selectProject(
                  project.project_id,
                  project.label,
                  activationFor({
                    state: 'measured',
                    label: project.label,
                    isActive: project.is_active ?? null,
                  }),
                );
                navigate(scopedWorkspacePath(useScope.getState().scope, 'brain'));
                onOpenChange(false);
              },
            })),
          )
        : [];
    return [...workspaceEntries, ...projectEntries];
  }, [navigate, location.pathname, location.search, onOpenChange, projects.data, scope, selectProject]);

  const filtered = useMemo(() => {
    const needle = query.trim().toLowerCase();
    const matchRank = (entry: CommandPaletteRow) => {
      const label = entry.label.toLowerCase();
      if (label === needle) return 0;
      if (label.includes(needle)) return 1;
      return 2;
    };
    // Preserve the rail/project order inside each rank while ensuring that
    // typing a workspace's exact name selects that workspace on Enter.
    return entries
      .filter((entry) => paletteMatchesQuery(entry.searchText, query))
      .sort((left, right) => matchRank(left) - matchRank(right));
  }, [entries, query]);

  useEffect(() => {
    if (open) {
      setQuery('');
      setSelection({ query: '', index: 0, rowId: null });
    }
  }, [open]);

  /**
   * Preserve the selected row's identity when a registry refresh reorders
   * entries. A numeric index alone can silently activate a different project
   * after the registry changes. When that identity disappears, clamp its last
   * position to an extant row. Query changes reset selection synchronously.
   */
  const currentSelection =
    selection.query === query ? selection : { query, index: 0, rowId: null };
  const matchingIndex = currentSelection.rowId === null
    ? -1
    : filtered.findIndex((entry) => entry.id === currentSelection.rowId);
  const activeIndex = filtered.length === 0
    ? 0
    : matchingIndex >= 0
      ? matchingIndex
      : Math.max(0, Math.min(currentSelection.index, filtered.length - 1));
  const activeEntry = filtered[activeIndex];
  const selectIndex = (index: number) => {
    setSelection({ query, index, rowId: filtered[index]?.id ?? null });
  };

  const listRef = useRef<HTMLUListElement>(null);
  useEffect(() => {
    // The list scrolls at `max-h-72`, about eight rows, and the project rows
    // sit after every workspace, so on a real registry the keyboard selection
    // left the viewport almost immediately and moved something the reader could
    // not see. `nearest` keeps the list still when the row is already visible.
    //
    // Reached by position rather than by a selector built from the id: one row
    // per filtered entry, in order, so the index that names the option names the
    // child. Nothing here has to be escaped, which is the same reason the ids
    // themselves are positional.
    if (!open || filtered.length === 0) return;
    const row = listRef.current?.children.item(activeIndex);
    if (row instanceof HTMLElement) row.scrollIntoView({ block: 'nearest' });
  }, [activeIndex, open, filtered.length]);

  const onKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === 'ArrowDown') {
      e.preventDefault();
      // From the clamped index, so a shrunk list steps from its last real row
      // rather than from a stale index further down.
      // An empty result set has no final row. Setting -1 here would leave
      // a newly fetched matching project invisible to Enter and screen readers.
      if (filtered.length > 0) selectIndex(Math.min(activeIndex + 1, filtered.length - 1));
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      if (filtered.length > 0) selectIndex(Math.max(activeIndex - 1, 0));
    } else if (e.key === 'Enter') {
      e.preventDefault();
      activeEntry?.action();
    }
  };

  return (
    <Dialog.Root open={open} onOpenChange={onOpenChange}>
      <Dialog.Portal>
        <Dialog.Overlay className="fixed inset-0 bg-black/55" />
        {/* An overlay, not channel 15 (NAVIGATION.md): the same framed
          * night-glass panel as the shell, set over it. */}
        <Dialog.Content
          className={cn(
            'fixed left-1/2 top-24 w-[min(560px,90vw)] -translate-x-1/2',
            'overflow-hidden rounded-[var(--radius-panel)] border border-edge-frame bg-surface-1 shadow-2xl',
          )}
          onKeyDown={onKeyDown}
          aria-label="Command palette"
        >
          <span aria-hidden className="td-corners pointer-events-none absolute inset-[4px] z-10" />
          <Dialog.Title className="sr-only">Command palette</Dialog.Title>
          <div className="flex items-center gap-2 border-b border-edge-subtle px-3">
            <Search aria-hidden size={14} className="text-text-muted" />
            <input
              autoFocus
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder="Go to workspace or project…"
              className="min-h-[var(--touch-target-min)] w-full bg-transparent text-body outline-none placeholder:text-text-muted"
              role="combobox"
              aria-expanded="true"
              aria-controls="td-palette-list"
              aria-activedescendant={activeEntry ? optionId(activeIndex) : undefined}
            />
            <kbd className="flex items-center gap-0.5 text-2xs text-text-muted">
              <CommandIcon aria-hidden size={10} />K
            </kbd>
          </div>
          <ul
            ref={listRef}
            id="td-palette-list"
            role="listbox"
            className="td-well max-h-72 overflow-auto"
          >
            {filtered.length === 0 ? (
              <li className="px-3 py-6 text-center text-body text-text-muted">No matches</li>
            ) : (
              filtered.map((entry, i) => (
                <li
                  key={entry.id}
                  id={optionId(i)}
                  role="option"
                  aria-selected={i === activeIndex}
                  onMouseEnter={() => selectIndex(i)}
                  onClick={entry.action}
                  className={cn(
                    // The same selection grammar as the rail: a cyan position
                    // bar in the gutter and the raised face, never a glow.
                    'relative flex min-h-[var(--touch-target-min)] cursor-pointer items-center gap-2.5 border-b border-edge-subtle pl-3.5 pr-2.5 text-body',
                    i === activeIndex
                      ? 'td-raised text-text-primary before:absolute before:inset-y-0 before:left-0 before:w-[3px] before:bg-accent'
                      : 'text-text-secondary',
                  )}
                >
                  {/* Hidden from the option's accessible name: the row is
                    * announced as "Brain", not "01 Brain workspace". */}
                  <code
                    aria-hidden
                    className={cn(
                      'td-value w-5 shrink-0 text-2xs',
                      i === activeIndex ? 'text-accent' : 'text-text-muted',
                    )}
                  >
                    {entry.channel ?? ''}
                  </code>
                  <span className="min-w-[5rem] flex-1 truncate">{entry.label}</span>
                  <span className="flex min-w-0 max-w-[45%] items-center gap-2 text-sm text-text-muted">
                    <span className="min-w-0 truncate">{entry.hint}</span>
                    {i === activeIndex ? (
                      <CornerDownLeft aria-hidden size={11} className="shrink-0" />
                    ) : null}
                  </span>
                </li>
              ))
            )}
          </ul>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
