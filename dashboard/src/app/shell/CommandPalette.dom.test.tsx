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
import { render, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { MemoryRouter } from 'react-router';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { CommandPalette } from './CommandPalette.tsx';
import {
  ProjectsPayloadSchema,
  type ProjectRegistryEntry,
  type ProjectsPayload,
} from '../../contracts/wire.ts';
import { scopeWritable, useScope } from '../../data/scope/store.ts';

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
): ProjectRegistryEntry {
  return {
    alias_count: 0,
    artifact_count: 0,
    branches: ['master'],
    canonical_root: `/repos/${projectId}`,
    default_branch: 'master',
    graph_scope_count: 1,
    kind: 'repo',
    label,
    last_seen_at: 2,
    project_id: projectId,
    project_root: `/repos/${projectId}`,
    store_count: 1,
    ...(isActive === undefined ? {} : { is_active: isActive }),
  };
}

/** A listing shaped like the daemon's, parsed through the generated schema so
 * a body this dashboard could not read cannot pass as one it could. The
 * palette reads `project_tree`, so that is where the entries have to be. */
function listing(entries: readonly ProjectRegistryEntry[]): ProjectsPayload {
  return ProjectsPayloadSchema.parse({
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

function stubListing(payload: ProjectsPayload) {
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
  return render(
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <CommandPalette open onOpenChange={() => {}} />
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

/** The row the input currently points at, by its rendered label. */
function activeOptionLabel(): string {
  const input = document.querySelector('input[role="combobox"]');
  const id = input?.getAttribute('aria-activedescendant');
  if (!id) return '(nothing is announced as active)';
  const option = document.getElementById(id);
  return option?.querySelector('span')?.textContent ?? '(the active id names no row)';
}

beforeEach(() => {
  useScope.getState().selectAllProjects();
});

afterEach(() => {
  vi.unstubAllGlobals();
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
