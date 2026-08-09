import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen } from '@testing-library/react';
import { afterAll, afterEach, beforeAll, describe, expect, it } from 'vitest';
import {
  allRoutesFail,
  faultHandler,
  fixtureServer,
  type HttpFault,
} from '../../../stories/fixtures/handlers.ts';
import type { DomainStateKind } from '../../ui/StateChip.tsx';
import { AgentsPage } from './AgentsPage.tsx';

/**
 * HTTP fault injection for the Agents workspace (plan 11, "MSW covers HTTP/SSE
 * faults"). Every other DOM test on this page hands the component a `fetch`
 * that always succeeds and varies the body; these vary the transport instead,
 * through the same MSW handlers the visual audit resolves its fixtures from.
 *
 * Agents is the subject for the whole matrix because its outer boundary wraps
 * the entire page: a failed read has nowhere to hide, so the single rendered
 * state is the page's whole answer, and any invented number would have to
 * appear beside it.
 */
const server = fixtureServer();
beforeAll(() => server.listen({ onUnhandledRequest: 'error' }));
afterEach(() => server.resetHandlers());
afterAll(() => server.close());

/** The sentence `LegacyStates` prints for each state it can reach. Distinct
 * wording per state is the point: a reader has to be able to tell "the daemon
 * is down" from "the daemon answered wrong" without opening a console. */
const GUIDANCE = {
  error: /the read failed and nothing is being invented in its place/i,
  offline: /the daemon is not reachable from this browser/i,
  unauthorized: /the daemon accepted no identity for this read/i,
  denied: /does not permit it to read this scope/i,
  unsupported_schema: /a shape this build does not understand/i,
} as const;

type FailKind = keyof typeof GUIDANCE;

/**
 * What each wire fault must become on screen. `detail` is what keeps two faults
 * that share a state kind from being the same reading.
 *
 * The two authorization refusals carry no detail because they need none: each
 * is its own state with its own icon, label and guidance, so a 401 and a 403
 * are already two different readings without a status code to tell them apart.
 * They were both `error · HTTP 4xx` until the taxonomy's `unauthorized` and
 * `denied` states were actually wired to them.
 *
 * The last two rows deliberately declare the same reading. `PayloadResult`
 * carries a `detail` only on `error`, so a body that is not JSON and a body the
 * decoder rejects both arrive as a bare `unsupported_schema` — one state,
 * because both mean "this build cannot read what came back". Every other pair
 * here is required to be distinguishable, and the test below derives that
 * requirement from this table rather than from a hand-counted total.
 */
const MATRIX: ReadonlyArray<{ fault: HttpFault; kind: FailKind; detail: string | null }> = [
  { fault: 'server_error', kind: 'error', detail: 'HTTP 500' },
  { fault: 'not_found', kind: 'error', detail: 'HTTP 404' },
  { fault: 'unauthorized', kind: 'unauthorized', detail: null },
  { fault: 'forbidden', kind: 'denied', detail: null },
  { fault: 'network_error', kind: 'offline', detail: null },
  { fault: 'malformed_body', kind: 'unsupported_schema', detail: null },
  { fault: 'unsupported_shape', kind: 'unsupported_schema', detail: null },
];

describe('AgentsPage under HTTP transport faults', () => {
  it.each(MATRIX)(
    'renders $fault as the $kind state and invents no analytics counts',
    async ({ fault, kind, detail }) => {
      server.use(allRoutesFail(fault));
      const { container } = renderAgents();

      const chip = await findChip(container);
      expect(chip.getAttribute('data-state')).toBe(kind satisfies DomainStateKind);
      if (detail) expect(chip.textContent).toContain(detail);

      // The state's own sentence, and neither of the other two failure
      // sentences: an unreachable daemon must not read as a bad payload.
      expect(screen.getByText(GUIDANCE[kind])).toBeTruthy();
      for (const other of Object.keys(GUIDANCE) as FailKind[]) {
        if (other !== kind) expect(screen.queryByText(GUIDANCE[other])).toBeNull();
      }

      // Nothing was read, so nothing may be reported. The page's success
      // surfaces are absent rather than present-and-empty: no content region,
      // no event-window readouts, and none of the counts the fixtures serve.
      expect(screen.queryByRole('region', { name: 'Agents content' })).toBeNull();
      expect(screen.queryByText('Event window')).toBeNull();
      expect(screen.queryByText('mcp tool calls')).toBeNull();
      expect(screen.queryByText('hook calls')).toBeNull();
      // "the store answered and had nothing" is a different claim from "the
      // read never landed", and only the second one happened here.
      expect(screen.queryByText(/analytics store unavailable/i)).toBeNull();
      expect(readout('events')).toBeNull();
      expect(readout('events (capped)')).toBeNull();
    },
  );

  it('never lets two faults with different meanings share one reading', async () => {
    const rendered = new Map<HttpFault, string>();
    for (const { fault } of MATRIX) {
      server.resetHandlers();
      server.use(allRoutesFail(fault));
      const view = renderAgents();
      const chip = await findChip(view.container);
      rendered.set(fault, `${chip.getAttribute('data-state')}${chip.textContent}`);
      view.unmount();
    }

    // Two faults may render alike only where MATRIX says they mean the same
    // thing. Anything else collapsing — a 403 reading as a 500, an unreachable
    // daemon reading as a bad payload — shows up as a shortfall here.
    const declared = new Set(MATRIX.map((row) => `${row.kind}|${row.detail ?? ''}`));
    expect(new Set(rendered.values()).size).toBe(declared.size);

    // Spelled out for the rows that differ only by status code, since those
    // are the ones most likely to be flattened into a generic "request failed".
    const statuses = MATRIX.filter((row) => row.detail).map((row) => rendered.get(row.fault));
    expect(new Set(statuses).size).toBe(statuses.length);
  });

  it('keeps a failed diagnostics read from becoming zero tool calls', async () => {
    // Only the slow diagnostics fold fails. Usage and hints answer from the
    // fixtures, so the page renders — which is exactly when a fabricated zero
    // would be invisible, sitting in a readout among real numbers.
    server.use(faultHandler('*/api/plugins/analytics/diagnostics', 'server_error'));
    renderAgents();

    await screen.findByRole('region', { name: 'Agents content' });
    // The neighbouring read is untouched and still reports its own measurement.
    expect(readout('events (capped)')).toBe('10,000');
    // The failed one reports nothing, and an em dash is not a quantity.
    expect(readout('mcp tool calls')).toBe('—');
    expect(readout('hook calls')).toBe('—');
    expect(readout('mcp tool calls')).not.toBe('0');
    expect(readout('hook calls')).not.toBe('0');

    // Each card fed by diagnostics says so on its own face.
    const failed = [...document.querySelectorAll('[data-state="error"]')];
    expect(failed.length).toBeGreaterThan(0);
    for (const chip of failed) expect(chip.textContent).toContain('HTTP 500');

    // And the dashes are accounted for as a FAILED read. "unavailable" is the
    // source's own declaration, which never arrived here — the strip used to
    // put that word in the daemon's mouth for every pending or failed read.
    expect(screen.getAllByText(/analytics diagnostics could not be read/i).length)
      .toBeGreaterThan(0);
    expect(screen.queryByText(/analytics diagnostics unavailable/i)).toBeNull();
  });
});

function renderAgents() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: 0 } },
  });
  return render(
    <QueryClientProvider client={client}>
      <AgentsPage />
    </QueryClientProvider>,
  );
}

/** The one state chip a fully failed page renders, once it has settled. */
async function findChip(container: HTMLElement): Promise<Element> {
  await screen.findByText(/^(Error|Offline|Unauthorized|Denied|Unsupported schema)$/);
  const chips = [...container.querySelectorAll('[data-state]')];
  expect(chips).toHaveLength(1);
  return chips[0]!;
}

/** The value printed under a readout's engraved legend, or null when that
 * readout is not on screen at all. Asserting on the page's whole text would
 * pass just as happily with a number sitting under the wrong label. */
function readout(label: string): string | null {
  const legend = [...document.querySelectorAll('.td-legend')].find(
    (node) => node.textContent === label,
  );
  return legend?.parentElement?.querySelector('[data-cell="numeric"]')?.textContent ?? null;
}
