import * as Dialog from '@radix-ui/react-dialog';
import { Command as CommandIcon, CornerDownLeft, Search } from 'lucide-react';
import { useEffect, useMemo, useRef, useState } from 'react';
import { useNavigate } from 'react-router';
import { WORKSPACES } from '../routes';
import { cn } from '../../ui/cn';
import { useProjectRegistry } from '../../data/query/projectRegistry.ts';
import { activationFor, useScope } from '../../data/scope/store.ts';

interface PaletteEntry {
  id: string;
  label: string;
  hint: string;
  action: () => void;
}

/**
 * The DOM id of one option row, by position.
 *
 * Positional rather than derived from the entry, because a project entry's id
 * carries its `canonical_root` — a filesystem path, which routinely contains
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

/** Command palette (plan 11a): scope-aware search across workspaces (and,
 * as slices land, entities, saved deep links, and legal actions only).
 * Results carry the same truth metadata as lists.
 * Lazy-loaded from Shell after first open so @radix-ui/react-dialog stays out
 * of the initial shell chunk. */
export function CommandPalette({
  open,
  onOpenChange,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const navigate = useNavigate();
  const selectProject = useScope((s) => s.selectProject);
  const [query, setQuery] = useState('');
  const [active, setActive] = useState(0);
  // Entities: registered projects become scope-setting results. Fetched only
  // while the palette is open; a failed registry read simply yields no
  // project rows (workspace navigation never depends on it).
  // The shared registry read, so opening the palette on a surface that already
  // listed the registry costs nothing, and so a registry change invalidates this
  // too — it used to have a private key that no event named.
  const projects = useProjectRegistry({ enabled: open });

  const entries = useMemo<PaletteEntry[]>(() => {
    const workspaceEntries = WORKSPACES.map((w) => ({
      id: `nav:${w.path}`,
      label: w.label,
      hint: 'workspace',
      action: () => {
        navigate(`/${w.path}`);
        onOpenChange(false);
      },
    }));
    const projectEntries =
      projects.data?.outcome === 'ok'
        ? (projects.data.data.project_tree ?? []).flatMap((group) =>
            group.projects.map((project) => ({
              id: `scope:${project.project_id}:${project.canonical_root}`,
              label: project.label,
              hint: `project · ${group.label}`,
              action: () => {
                // The listing already measured `is_active`, against the same
                // `active_project_id` the gateway accepts writes on, so the
                // pick starts from that answer instead of `unresolved`. It is
                // read through the shared authority rather than compared here,
                // so a row that omits the field stays unresolved — the daemon
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
                navigate('/brain');
                onOpenChange(false);
              },
            })),
          )
        : [];
    return [...workspaceEntries, ...projectEntries];
  }, [navigate, onOpenChange, projects.data, selectProject]);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return entries;
    return entries.filter((e) => e.label.toLowerCase().includes(q));
  }, [entries, query]);

  useEffect(() => setActive(0), [query, open]);
  useEffect(() => {
    if (open) setQuery('');
  }, [open]);

  /**
   * The row actually being pointed at, which is not always the one `active`
   * holds.
   *
   * `active` is only reset by a query change or a reopen, and the list has a
   * second input: the registry read behind the project rows. A listing that
   * shrinks — a refetch returning fewer projects, or an `ok` result becoming a
   * failure, which drops every project row at once — leaves the stored index
   * past the end. That row does not exist, so `aria-activedescendant` referred
   * to nothing and Enter fired nothing: a palette that looked navigable and was
   * inert. Clamped on read rather than corrected in an effect, so there is no
   * render in which the index is out of range.
   */
  const activeIndex = filtered.length === 0 ? 0 : Math.min(active, filtered.length - 1);
  const activeEntry = filtered[activeIndex];

  const listRef = useRef<HTMLUListElement>(null);
  useEffect(() => {
    // The list scrolls at `max-h-72` — about eight rows — and the project rows
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
      setActive(Math.min(activeIndex + 1, filtered.length - 1));
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      setActive(Math.max(activeIndex - 1, 0));
    } else if (e.key === 'Enter') {
      e.preventDefault();
      activeEntry?.action();
    }
  };

  return (
    <Dialog.Root open={open} onOpenChange={onOpenChange}>
      <Dialog.Portal>
        <Dialog.Overlay className="fixed inset-0 bg-black/40" />
        <Dialog.Content
          className={cn(
            'fixed left-1/2 top-24 w-[min(560px,90vw)] -translate-x-1/2',
            'overflow-hidden rounded-[var(--radius-standard)] border border-edge-strong bg-surface-1 shadow-2xl',
          )}
          onKeyDown={onKeyDown}
          aria-label="Command palette"
        >
          <Dialog.Title className="sr-only">Command palette</Dialog.Title>
          <div className="flex items-center gap-2 border-b border-edge-subtle px-3">
            <Search aria-hidden size={14} className="text-text-muted" />
            <input
              autoFocus
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder="Go to workspace or project…"
              className="h-10 w-full bg-transparent text-sm outline-none placeholder:text-text-muted"
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
            className="max-h-72 overflow-auto p-1"
          >
            {filtered.length === 0 ? (
              <li className="px-3 py-6 text-center text-sm text-text-muted">No matches</li>
            ) : (
              filtered.map((entry, i) => (
                <li
                  key={entry.id}
                  id={optionId(i)}
                  role="option"
                  aria-selected={i === activeIndex}
                  onMouseEnter={() => setActive(i)}
                  onClick={entry.action}
                  className={cn(
                    'flex h-9 cursor-pointer items-center justify-between rounded-[var(--radius-chip)] px-2.5 text-sm',
                    i === activeIndex ? 'bg-surface-2 text-text-primary' : 'text-text-secondary',
                  )}
                >
                  <span>{entry.label}</span>
                  <span className="flex items-center gap-2 text-2xs text-text-muted">
                    {entry.hint}
                    {i === activeIndex ? <CornerDownLeft aria-hidden size={11} /> : null}
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
