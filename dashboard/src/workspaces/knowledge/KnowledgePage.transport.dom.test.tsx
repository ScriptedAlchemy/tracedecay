import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen, waitFor } from '@testing-library/react';
import { MemoryRouter } from 'react-router';
import { afterAll, afterEach, beforeAll, describe, expect, it } from 'vitest';
import {
  allRoutesFail,
  fixtureServer,
  type HttpFault,
} from '../../../stories/fixtures/handlers.ts';
import { KnowledgePage } from './KnowledgePage.tsx';

/**
 * HTTP fault injection for the Knowledge workspace (plan 11, "MSW covers
 * HTTP/SSE faults").
 *
 * Knowledge is the subject for headline quantities. Its rail leads with the
 * fact count at display size — the single largest number the dashboard prints —
 * and its list has "no facts recorded" written under it. A failed read that
 * produced either one would be the project's central failure: a memory store
 * nobody could reach, reported as a memory store holding nothing.
 */
const server = fixtureServer();
beforeAll(() => server.listen({ onUnhandledRequest: 'error' }));
afterEach(() => server.resetHandlers());
afterAll(() => server.close());

const FAULTS: ReadonlyArray<{ fault: HttpFault; kind: string; detail: string | null }> = [
  { fault: 'server_error', kind: 'error', detail: 'HTTP 500' },
  { fault: 'not_found', kind: 'error', detail: 'HTTP 404' },
  // A refused read is its own state, carrying no status code: the chip label
  // and its guidance already say the store was reachable and would not answer.
  { fault: 'unauthorized', kind: 'unauthorized', detail: null },
  { fault: 'forbidden', kind: 'denied', detail: null },
  { fault: 'network_error', kind: 'offline', detail: null },
  { fault: 'malformed_body', kind: 'unsupported_schema', detail: null },
  { fault: 'unsupported_shape', kind: 'unsupported_schema', detail: null },
];

describe('KnowledgePage under HTTP transport faults', () => {
  it.each(FAULTS)(
    'answers $fault with a state instead of a fact count',
    async ({ fault, kind, detail }) => {
      server.use(allRoutesFail(fault));
      const { container } = renderKnowledge();

      // The rail and the list both hang off the memory overview read, and the
      // curation panel's two reads (curator status, curation plan) fail with
      // them — all four report the failure rather than any rendering a hollow
      // shell.
      const chips = await settledChips(container);
      expect(chips).toHaveLength(4);
      for (const chip of chips) {
        expect(chip.getAttribute('data-state')).toBe(kind);
        if (detail) expect(chip.textContent).toContain(detail);
      }

      // No quantity of any size, under any of the rail's legends.
      expect(readout('facts')).toBeNull();
      expect(readout('entities')).toBeNull();
      expect(readout('banks')).toBeNull();
      // ...and specifically not a zero, which is the reading a reader would
      // act on as "this store is empty, go build it".
      expect(container.textContent).not.toMatch(/\b0 recorded\b/);
      expect(screen.queryByText(/no facts recorded/i)).toBeNull();
      // The daemon saying "my store is missing" is a different finding from the
      // daemon never answering, and only the second one happened here.
      expect(screen.queryByText(/memory store unavailable/i)).toBeNull();
      expect(screen.queryByText(/fact list read failed/i)).toBeNull();
    },
  );

  it('does not offer a search box over facts it could not read', async () => {
    // The composer sits inside the same boundary as the rail. Leaving it
    // operable would invite a reader to search a store the browser never
    // reached and read the empty result as "no matches".
    server.use(allRoutesFail('server_error'));
    const { container } = renderKnowledge();

    await settledChips(container);
    expect(screen.queryByLabelText('Search facts')).toBeNull();
    expect(screen.queryByRole('textbox')).toBeNull();
  });
});

function renderKnowledge() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: 0 } },
  });
  return render(
    <QueryClientProvider client={client}>
      {/* The workspace's view camera lives in the address (`?view=`), so the
        * page needs a router to read it. `MemoryRouter` rather than the real
        * one: these cases assert reads and states, not navigation. */}
      <MemoryRouter>
        <KnowledgePage />
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

/** Every state chip on the page once no read is still in flight. */
async function settledChips(container: HTMLElement): Promise<Element[]> {
  await waitFor(() => {
    expect(container.querySelectorAll('[data-state="loading"]')).toHaveLength(0);
    expect(container.querySelectorAll('[data-state]').length).toBeGreaterThan(0);
  });
  return [...container.querySelectorAll('[data-state]')];
}

/** The value printed under a readout's engraved legend, or null when that
 * readout is not on screen at all. */
function readout(label: string): string | null {
  const legend = [...document.querySelectorAll('.td-legend')].find(
    (node) => node.textContent === label,
  );
  return legend?.parentElement?.querySelector('[data-cell="numeric"]')?.textContent ?? null;
}
