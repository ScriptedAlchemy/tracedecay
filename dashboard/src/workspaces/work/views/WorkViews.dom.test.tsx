/**
 * The Work cameras over one exact product graph: the board and the DAG.
 *
 * The invariant that carries this file is the product mandate: one canonical
 * selection, many synchronized projections. The switcher moves the camera and
 * must never move the selection, so a task selected on the board is still
 * selected after a projection change and a reload of the same address. A
 * refusal from the daemon is asserted never to render as a projection of
 * nothing, and the tabs never point at a region the page did not draw.
 */
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { MemoryRouter } from 'react-router';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { useScope } from '../../../data/scope/store.ts';
import { applicationProblem, applicationSuccess } from '../../../test/applicationEnvelope.ts';
import { workGraphRead, type WorkGraphVersionSpec } from '../../../test/workGraphFixture.ts';
import { WorkPage } from '../WorkPage.tsx';

const VIEWS_GRAPH: WorkGraphVersionSpec = {
  tasks: [
    { taskId: 'root', title: 'Root task', effort: 2 },
    { taskId: 'middle', title: 'Middle task', effort: 3, dependencies: ['root'] },
    { taskId: 'leaf', title: 'Leaf task', effort: 5, dependencies: ['middle'] },
    { taskId: 'loop-a', title: 'Loop A' },
    { taskId: 'loop-b', title: 'Loop B', dependencies: ['loop-a'] },
    { taskId: 'lonely', title: 'Lonely task' },
  ],
  criticalPath: ['root', 'middle', 'leaf'],
};

/** Serve every Work read the page issues. Routed by path rather than answered
 * with one body, so a projection cannot pass by reading the wrong contract. */
function serveWork() {
  serve((url) =>
    url.includes('/work/views')
      ? { status: 200, body: applicationSuccess('operation.work.views', workGraphRead(VIEWS_GRAPH)) }
      : { status: 503, body: applicationProblem('unavailable') },
  );
}

function serve(handler: (url: string) => { status: number; body: unknown }) {
  vi.stubGlobal(
    'fetch',
    vi.fn(async (url: string) => {
      const { status, body } = handler(String(url));
      return new Response(JSON.stringify(body), {
        status,
        headers: { 'content-type': 'application/json' },
      });
    }),
  );
}

function renderPage(entry = '/work') {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  });
  return render(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={[entry]}>
        <WorkPage />
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

async function open(name: string) {
  const user = userEvent.setup();
  await user.click(await screen.findByRole('tab', { name }));
  return user;
}

const ID_REFERENCES = ['aria-controls', 'aria-labelledby', 'aria-describedby'] as const;

/**
 * Every id an ARIA reference on the page names but the page did not draw.
 *
 * The accessibility gate reads references, not intentions: an `aria-controls`
 * naming an absent element is a critical `aria-valid-attr-value` failure, not
 * a control that merely happens to point at nothing. This returns the offences
 * rather than a boolean so a failure names the attribute that broke.
 */
function danglingReferences(container: HTMLElement): string[] {
  const offences: string[] = [];
  const selector = ID_REFERENCES.map((attribute) => `[${attribute}]`).join(',');
  for (const element of Array.from(container.querySelectorAll(selector))) {
    for (const attribute of ID_REFERENCES) {
      const value = element.getAttribute(attribute);
      if (value === null) continue;
      for (const id of value.split(/\s+/).filter((token) => token !== '')) {
        // Resolved against the document, the way an assistive technology
        // resolves an IDREF, not against this subtree.
        if (element.ownerDocument.getElementById(id) === null) {
          offences.push(`${element.tagName.toLowerCase()} ${attribute}="${id}"`);
        }
      }
    }
  }
  return offences;
}

beforeEach(() => {
  serveWork();
});

afterEach(() => {
  useScope.setState({ scope: { kind: 'all' } });
  vi.unstubAllGlobals();
});

describe('the projection switcher', () => {
  it('offers the board and the DAG as tabs and opens on the board', async () => {
    renderPage();
    const tablist = await screen.findByRole('tablist', { name: 'Work projection' });
    const tabs = within(tablist).getAllByRole('tab');

    expect(tabs.map((tab) => tab.textContent)).toEqual(['Board', 'DAG']);
    expect(within(tablist).getByRole('tab', { selected: true }).textContent).toBe('Board');
  });

  it('moves the camera between projections with the arrow keys', async () => {
    const user = userEvent.setup();
    renderPage();
    const board = await screen.findByRole('tab', { name: 'Board' });
    board.focus();

    await user.keyboard('{ArrowRight}');
    await waitFor(() =>
      expect(screen.getByRole('tab', { name: 'DAG' }).getAttribute('aria-selected')).toBe('true'),
    );
    await user.keyboard('{Home}');
    await waitFor(() =>
      expect(screen.getByRole('tab', { name: 'Board' }).getAttribute('aria-selected')).toBe('true'),
    );
  });

  /** A projection is a camera position; it does not own the selection and
   * must not clear one. */
  it('keeps the selected task across every projection change', async () => {
    const user = userEvent.setup();
    const { container } = renderPage();
    await user.click(await screen.findByRole('button', { name: 'Root task' }));
    await waitFor(() =>
      expect(container.querySelector('[data-work-task="root"][data-selected]')).not.toBeNull(),
    );

    for (const name of ['DAG', 'Board']) {
      await open(name);
      await waitFor(() =>
        expect(
          container.querySelector('[data-work-task="root"][aria-pressed="true"], [data-work-task="root"][data-selected]'),
        ).not.toBeNull(),
      );
    }
  });

  it('reopens the projection its address names', async () => {
    const { container } = renderPage('/work?view=dag');
    await waitFor(() => expect(container.querySelector('[data-work-view="dag"]')).not.toBeNull());
    expect(screen.getByRole('tab', { selected: true }).textContent).toBe('DAG');
  });

  /** An unreadable camera position opens the board rather than an empty frame:
   * the board is the one projection whose every channel this build measures.
   * The retired positions of issue #4220 take this path too. */
  it('opens the board when the address names a projection this build has not got', async () => {
    const { container } = renderPage('/work?view=cortex-9');
    await waitFor(() => expect(container.querySelector('[data-work-board]')).not.toBeNull());
    expect(screen.getByRole('tab', { selected: true }).textContent).toBe('Board');
  });

  it('resolves every ARIA reference it makes while a snapshot is drawn', async () => {
    const { container } = renderPage('/work?view=dag');
    await waitFor(() => expect(container.querySelector('[data-work-view="dag"]')).not.toBeNull());

    expect(danglingReferences(container)).toEqual([]);
  });

  /**
   * Losing the switcher on a refusal would strand a reader in a projection
   * they cannot leave, so the tabs stay. Which means the region they name has
   * to stay with them: tabs that keep `aria-controls` pointed at a panel the
   * refusal branch never drew are a dangling reference, and the accessibility
   * gate reads that as a critical invalid attribute value rather than as a
   * projection that is merely absent.
   */
  it('keeps the camera and the region it controls when the read refuses', async () => {
    serve(() => ({ status: 503, body: applicationProblem('unavailable') }));
    const { container } = renderPage('/work?view=dag');

    await waitFor(() => expect(screen.getByText(/fixture unavailable problem/)).toBeTruthy());
    expect(screen.getByRole('tablist', { name: 'Work projection' })).toBeTruthy();
    expect(container.querySelector('[data-work-view]')).toBeNull();

    // The refusal is what the camera is now pointed at, so it belongs inside
    // the region the tabs control rather than beside it.
    const panel = screen.getByRole('tabpanel');
    expect(within(panel).getByText(/fixture unavailable problem/)).toBeTruthy();
    expect(danglingReferences(container)).toEqual([]);
  });
});
