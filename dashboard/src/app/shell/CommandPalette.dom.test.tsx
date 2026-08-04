/**
 * The palette, operated the way it is meant to be operated: from the keyboard.
 *
 * The palette is the only surface that sets project scope without a URL, so
 * whatever it puts in the store is what the write controls on the next screen
 * will believe. Two things therefore have to hold, and only one of them is
 * about accessibility:
 *
 *  1. Arrow keys and Enter reach every row, including the project rows, and
 *     the active row is announced — the list is a `listbox` whose selection
 *     lives in `aria-activedescendant` on the input, so a mouse-only test
 *     would pass while a screen reader was told nothing.
 *
 *  2. The activation the pick lands in is the one the registry measured. The
 *     listing carries `is_active` computed against the same `active_project_id`
 *     the gateway accepts writes on, so a pick may start from that answer —
 *     but the field is optional on the wire, and a row that omits it must
 *     leave the scope unresolved rather than reading absence as "not active".
 */
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, render, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { MemoryRouter } from 'react-router';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { CommandPalette } from './CommandPalette.tsx';
import {
  ProjectsPayloadV1Schema,
  type ProjectRegistryEntry,
  type ProjectsPayloadV1,
} from '../../contracts/generated.ts';
import { projectRegistryListKey } from '../../data/query/projectRegistry.ts';
import { UNSCOPED_CACHE_KEY, scopeWritable, useScope } from '../../data/scope/store.ts';

/**
 * A registry entry as the daemon sends it.
 *
 * `is_active` is passed through as given, including `undefined`, because the
 * field is optional on the wire and the case where it is missing is one of the
 * cases under test — a helper that defaulted it would delete that case.
 */
function registryEntry(
  projectId: string,
  label: string,
  isActive: boolean | null | undefined,
  canonicalRoot = `/repos/${projectId}`,
): ProjectRegistryEntry {
  return entryAt(projectId, label, isActive, canonicalRoot);
}

function entryAt(
  projectId: string,
  label: string,
  isActive: boolean | null | undefined,
  root: string,
): ProjectRegistryEntry {
  return {
    alias_count: 0,
    artifact_count: 0,
    branches: ['master'],
    canonical_root: root,
    default_branch: 'master',
    graph_scope_count: 1,
    kind: 'repo',
    label,
    last_seen_at: 2,
    project_id: projectId,
    project_root: root,
    store_count: 1,
    ...(isActive === undefined ? {} : { is_active: isActive }),
  };
}

/** A listing shaped like the daemon's, parsed through the generated schema so
 * a body this dashboard could not read cannot pass as one it could. The
 * palette reads `project_tree`, so that is where the entries have to be. */
function listing(entries: readonly ProjectRegistryEntry[]): ProjectsPayloadV1 {
  return ProjectsPayloadV1Schema.parse({
    active_project_id: 'proj-active',
    active_project_root: '/repos/proj-active',
    error: null,
    limit: 100,
    projects: [],
    project_tree: [
      {
        branches: ['master'],
        git_common_dir: null,
        label: 'workspaces',
        project_count: entries.length,
        projects: [...entries],
      },
    ],
    status: 'ok',
    summary: null,
    truncated: false,
  });
}

function stubListing(payload: ProjectsPayloadV1) {
  vi.stubGlobal(
    'fetch',
    vi.fn(
      async () =>
        new Response(JSON.stringify(payload), {
          status: 200,
          headers: { 'content-type': 'application/json' },
        }),
    ),
  );
}

function renderPalette() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return {
    client,
    ...render(
      <QueryClientProvider client={client}>
        <MemoryRouter>
          <CommandPalette open onOpenChange={() => {}} />
        </MemoryRouter>
      </QueryClientProvider>,
    ),
  };
}

function combobox(): HTMLElement {
  const input = document.querySelector<HTMLElement>('input[role="combobox"]');
  if (!input) throw new Error('the palette rendered no combobox');
  return input;
}

/** The row the input currently points at, by its rendered label.
 *
 * Resolved with `getElementById`, exactly as an assistive technology resolves
 * an IDREF — so an `aria-activedescendant` that names no element reports "the
 * active id names no row" rather than quietly passing. */
function activeOptionLabel(): string {
  const id = combobox().getAttribute('aria-activedescendant');
  if (!id) return '(nothing is announced as active)';
  const option = document.getElementById(id);
  return option?.querySelector('span')?.textContent ?? '(the active id names no row)';
}

/**
 * Rows the palette scrolled to, in order.
 *
 * jsdom implements no layout and therefore no `scrollIntoView`, so this stands
 * in for it everywhere in the file rather than only where it is asserted —
 * without it the palette's keyboard effect throws on every arrow press, for a
 * method that exists in every browser it will ever run in.
 */
let scrolledInto: Element[] = [];

beforeEach(() => {
  useScope.getState().selectAllProjects();
  scrolledInto = [];
  Element.prototype.scrollIntoView = function scrollIntoView(this: Element) {
    scrolledInto.push(this);
  };
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe('CommandPalette keyboard operation', () => {
  it('focuses the search box on open, so the first keystroke reaches it', async () => {
    stubListing(listing([]));
    renderPalette();
    await waitFor(() =>
      expect(document.activeElement).toBe(document.querySelector('input[role="combobox"]')),
    );
  });

  it('announces the active row through aria-activedescendant as the arrows move', async () => {
    const user = userEvent.setup();
    stubListing(listing([]));
    const { findByRole } = renderPalette();
    await findByRole('listbox');

    const first = activeOptionLabel();
    await user.keyboard('{ArrowDown}');
    const second = activeOptionLabel();
    expect(second).not.toBe(first);
    // Announced, not merely styled: a highlight with no activedescendant is
    // invisible to a screen reader.
    expect(document.getElementById(
      document.querySelector('input[role="combobox"]')!.getAttribute('aria-activedescendant')!,
    )?.getAttribute('aria-selected')).toBe('true');

    await user.keyboard('{ArrowUp}');
    expect(activeOptionLabel()).toBe(first);
  });

  it('does not run off either end of the list', async () => {
    const user = userEvent.setup();
    stubListing(listing([]));
    const { findByRole } = renderPalette();
    await findByRole('listbox');

    await user.keyboard('{ArrowUp}{ArrowUp}');
    const top = activeOptionLabel();
    expect(top).not.toContain('nothing is announced');

    await user.keyboard('{ArrowDown}'.repeat(40));
    expect(activeOptionLabel()).not.toContain('nothing is announced');
  });

  it('selects a project by keyboard and starts it from the measured activation', async () => {
    const user = userEvent.setup();
    stubListing(listing([registryEntry('proj-active', 'Production', true)]));
    const { findByText } = renderPalette();
    await findByText('Production');

    // Filter to the project row, then take it with Enter alone.
    await user.keyboard('Production');
    await waitFor(() => expect(activeOptionLabel()).toBe('Production'));
    await user.keyboard('{Enter}');

    expect(useScope.getState().scope).toMatchObject({
      kind: 'project',
      projectId: 'proj-active',
      label: 'Production',
      activation: 'active',
    });
    // The point of carrying the measurement: the next screen's write controls
    // are enabled immediately, rather than disabled behind a second read.
    expect(scopeWritable(useScope.getState().scope)).toEqual({
      state: 'writable',
      target: 'Production',
    });
  });

  it('marks a keyboard-selected non-active project read-only, naming the remedy', async () => {
    const user = userEvent.setup();
    stubListing(listing([registryEntry('proj-other', 'Scratch', false)]));
    const { findByText } = renderPalette();
    await findByText('Scratch');

    await user.keyboard('Scratch');
    await waitFor(() => expect(activeOptionLabel()).toBe('Scratch'));
    await user.keyboard('{Enter}');

    expect(useScope.getState().scope).toMatchObject({ activation: 'selected' });
    const writability = scopeWritable(useScope.getState().scope);
    expect(writability.state).toBe('read_only');
    if (writability.state !== 'read_only') throw new Error('unreachable');
    expect(writability.reason).toContain('Scratch is not the active project');
  });

  /**
   * A checkout path with a space in it, which is ordinary and which used to
   * break the announcement outright.
   *
   * The row's DOM id was built from the palette entry id, and a project entry's
   * id carries its `canonical_root`. HTML forbids ASCII whitespace in `id`, and
   * `aria-activedescendant` is an IDREF — so for anyone whose repositories live
   * under `~/My Projects`, the input pointed at an id that could not be
   * resolved and a screen reader was told nothing about which row was active.
   * The highlight still moved, so the defect was invisible on screen.
   */
  it('announces the active row for a project checked out under a path with spaces', async () => {
    const user = userEvent.setup();
    stubListing(
      listing([
        entryAt('proj-spaced', 'Spaced Out', true, '/home/dev/My Projects/client work'),
      ]),
    );
    const { findByText } = renderPalette();
    await findByText('Spaced Out');

    await user.keyboard('Spaced');
    await waitFor(() => expect(activeOptionLabel()).toBe('Spaced Out'));

    // Resolvable, and a legal id: no whitespace, and `getElementById` finds it.
    const announced = document
      .querySelector('input[role="combobox"]')!
      .getAttribute('aria-activedescendant')!;
    expect(announced).not.toMatch(/\s/);
    expect(document.getElementById(announced)).not.toBeNull();
    // No row carries the path in its id, whatever the entry key is made of.
    for (const option of document.querySelectorAll('[role="option"]')) {
      expect(option.id).not.toMatch(/\s/);
    }
  });

  it('scrolls the active row into view as the arrows leave the visible window', async () => {
    // The list is a fixed-height scroller. Arrowing past the last visible row
    // moved the highlight somewhere the reader could not see, so the palette
    // silently stopped showing what Enter would do.
    const user = userEvent.setup();
    const many = Array.from({ length: 30 }, (_, i) =>
      registryEntry(`proj-${i}`, `Project ${i}`, false),
    );
    stubListing(listing(many));
    const { findByText } = renderPalette();
    await findByText('Project 29');

    await user.keyboard('{ArrowDown}'.repeat(20));

    const lastScrolled = scrolledInto[scrolledInto.length - 1];
    expect(lastScrolled).toBeDefined();
    // It is the row that is now active that was scrolled to, not just any row.
    expect(lastScrolled?.getAttribute('aria-selected')).toBe('true');
    expect(lastScrolled?.textContent).toContain(activeOptionLabel());
  });

  /**
   * The list shrinking under a stationary cursor, which nothing resets.
   *
   * `active` is reset when the query or the open state changes — neither
   * happens when the registry read lands, or when a `project_registry_changed`
   * invalidation returns fewer rows. An index left past the end announces
   * nothing and activates nothing, so the palette reads as focused and does
   * not respond to Enter.
   */
  it('pulls the cursor back into range when the registry returns fewer rows', async () => {
    // Deliberately not driven by typing: a query change already resets the
    // index, so filtering cannot reach this. The list shrinking underneath a
    // stationary cursor is the case with nothing watching it — the registry
    // read landing, or a `project_registry_changed` invalidation coming back
    // with projects removed.
    const user = userEvent.setup();
    const many = Array.from({ length: 25 }, (_, i) =>
      registryEntry(`proj-${i}`, `Project ${i}`, false),
    );
    const bodies = [listing(many), listing(many.slice(0, 2))];
    let call = 0;
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => {
        const body = bodies[Math.min(call, bodies.length - 1)]!;
        call += 1;
        return new Response(JSON.stringify(body), {
          status: 200,
          headers: { 'content-type': 'application/json' },
        });
      }),
    );

    const { client, findByText } = renderPalette();
    await findByText('Project 24');

    await user.keyboard('{ArrowDown}'.repeat(24));
    expect(activeOptionLabel()).not.toContain('nothing is announced');

    await client.invalidateQueries();
    await waitFor(() =>
      expect(document.querySelectorAll('[role="option"]').length).toBeLessThan(25),
    );

    expect(activeOptionLabel()).not.toContain('nothing is announced');
    expect(activeOptionLabel()).not.toContain('names no row');
    // And Enter still does something: the row it names is really there.
    const announced = document
      .querySelector('input[role="combobox"]')!
      .getAttribute('aria-activedescendant')!;
    expect(document.getElementById(announced)?.getAttribute('aria-selected')).toBe('true');
  });

  it('leaves a row that never said whether it is active unresolved, not inactive', async () => {
    // `is_active` is optional on the wire. Reading its absence as `false`
    // would disable writes on the active project and tell the reader, in a
    // full sentence, that it is not the active one — a confident wrong answer
    // built out of a field the daemon simply did not send.
    const user = userEvent.setup();
    stubListing(listing([registryEntry('proj-quiet', 'Unstated', undefined)]));
    const { findByText } = renderPalette();
    await findByText('Unstated');

    await user.keyboard('Unstated');
    await waitFor(() => expect(activeOptionLabel()).toBe('Unstated'));
    await user.keyboard('{Enter}');

    expect(useScope.getState().scope).toMatchObject({ activation: 'unresolved' });
    expect(scopeWritable(useScope.getState().scope).state).toBe('unknown');
  });
});

/**
 * The option ids themselves, which are what the announcement is made of.
 *
 * `aria-activedescendant` holds an IDREF, and IDREFs are whitespace-delimited.
 * Option ids were built from `entry.id` — `scope:{project_id}:{canonical_root}`
 * — so a project checked out under a path containing a space produced a value
 * naming two ids, neither of which existed, and the row a keyboard user had just
 * moved to was announced as nothing. Every fixture above uses `/repos/{id}`,
 * which is why the suite never saw it.
 */
describe('CommandPalette option identity', () => {
  it('announces a row whose project path contains spaces', async () => {
    const user = userEvent.setup();
    stubListing(
      listing([
        registryEntry('proj-spaced', 'My Documents Repo', true, '/Users/me/My Documents/repo'),
      ]),
    );
    const { findByText } = renderPalette();
    await findByText('My Documents Repo');

    await user.keyboard('My Documents');
    await waitFor(() => expect(activeOptionLabel()).toBe('My Documents Repo'));

    // The stronger statement: the reference resolves to exactly one element, the
    // way an assistive technology resolves it.
    const id = combobox().getAttribute('aria-activedescendant') ?? '';
    expect(id).not.toBe('');
    expect(id).not.toMatch(/\s/);
    expect(document.getElementById(id)).not.toBeNull();
    expect(document.querySelectorAll(`[id="${id}"]`).length).toBe(1);
  });

  /**
   * Quotes and backslashes are legal in a POSIX path, and unlike a space they do
   * not break `getElementById` — so asserting only that the reference resolves
   * would pass with the ids built from the path. What has to hold is that no path
   * character reaches the id at all: the announcement must be a bare token,
   * whatever the project is checked out under.
   */
  it('keeps the announced id free of path characters entirely', async () => {
    const user = userEvent.setup();
    stubListing(
      listing([registryEntry('proj-odd', 'Odd Path', true, '/repos/a"b\\c d/repo')]),
    );
    const { findByText } = renderPalette();
    await findByText('Odd Path');

    await user.keyboard('Odd');
    await waitFor(() => expect(activeOptionLabel()).toBe('Odd Path'));

    const id = combobox().getAttribute('aria-activedescendant') ?? '';
    expect(document.getElementById(id)).not.toBeNull();
    // No whitespace (an IDREF is whitespace-delimited, so a space names two ids
    // and resolves to neither) and nothing that would need escaping in a
    // selector either.
    expect(id).not.toMatch(/[\s"'\\/]/);
  });
});

/**
 * The two ways the active index can stop describing a row.
 *
 * The list scrolls past about eight rows and the project rows come after every
 * workspace, so arrowing into the registry moved a selection off screen. And
 * `active` was reset only by a query change or a reopen, while the list has a
 * second input — the registry read — so a listing that shrank left the index
 * past the end: `aria-activedescendant` referring to nothing, and Enter firing
 * nothing, on a palette that still looked navigable.
 */
describe('CommandPalette long and shrinking lists', () => {
  const MANY = Array.from({ length: 24 }, (_, i) =>
    registryEntry(`proj-${i}`, `Project ${String(i).padStart(2, '0')}`, false),
  );

  it('scrolls the active row into view as the keyboard moves past the fold', async () => {
    const user = userEvent.setup();
    // jsdom implements no scrolling, so the call is the observable: what is
    // asserted is that the row the reader moved to was asked to become visible.
    const scrollIntoView = vi.fn();
    vi.spyOn(Element.prototype, 'scrollIntoView').mockImplementation(scrollIntoView);
    stubListing(listing(MANY));
    const { findByText } = renderPalette();
    await findByText('Project 23');

    scrollIntoView.mockClear();
    await user.keyboard('{ArrowDown}'.repeat(12));

    expect(scrollIntoView).toHaveBeenCalled();
    // `nearest` rather than a jump: a row already on screen must not move the
    // list under the reader.
    expect(scrollIntoView.mock.calls.at(-1)?.[0]).toEqual({ block: 'nearest' });
    // And the row it scrolled to is the announced one.
    const active = document.getElementById(
      combobox().getAttribute('aria-activedescendant') ?? '',
    );
    expect(active).not.toBeNull();
    expect(active?.getAttribute('aria-selected')).toBe('true');
  });

  it('clamps the active row when the listing shrinks, and Enter still fires', async () => {
    const user = userEvent.setup();
    stubListing(listing(MANY));
    const { findByText, client } = renderPalette();
    await findByText('Project 23');

    // Arrow deep into the project rows, well past anything a short list holds.
    await user.keyboard('{ArrowDown}'.repeat(26));
    const deep = activeOptionLabel();
    expect(deep).not.toContain('names no row');

    // The registry answers with fewer projects than before — a refetch after a
    // project was removed. Every row past the new end disappears at once.
    act(() => {
      client.setQueryData([...projectRegistryListKey, UNSCOPED_CACHE_KEY], {
        outcome: 'ok',
        data: listing([registryEntry('proj-0', 'Project 00', false)]),
      });
    });

    await waitFor(() => expect(activeOptionLabel()).not.toContain('names no row'));
    expect(activeOptionLabel()).not.toContain('nothing is announced');
    // Enter reaches the clamped row rather than falling into the gap the stale
    // index left.
    await user.keyboard('{Enter}');
    expect(useScope.getState().scope.kind).toBe('project');
  });

  /** The same clamp from the other direction: a registry read that stops being
   * `ok` drops every project row in one step. */
  it('clamps when a failed registry read removes every project row', async () => {
    const user = userEvent.setup();
    stubListing(listing(MANY));
    const { findByText, client } = renderPalette();
    await findByText('Project 23');

    await user.keyboard('{ArrowDown}'.repeat(26));
    act(() => {
      client.setQueryData([...projectRegistryListKey, UNSCOPED_CACHE_KEY], { outcome: 'offline' });
    });

    await waitFor(() => expect(activeOptionLabel()).not.toContain('names no row'));
    const id = combobox().getAttribute('aria-activedescendant') ?? '';
    expect(document.getElementById(id)).not.toBeNull();
  });
});
