// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { resolveFixture } from '../../../stories/fixtures/data.ts';
import { useScope } from '../../data/scope/store.ts';
import { SystemsHeader } from './SystemsHeader.tsx';

/**
 * The segmented subsystem header must stamp real readings and reserve amber
 * for an actually-observed problem finding — never for a source that merely
 * failed to answer, and never OK for one that did not.
 */

/** The wire-true fixture envelopes, with one served problem finding added:
 * hand-rolled wrappers here would drift from the generated contract and turn
 * this into a test of the fixture author's memory. */
function findingsEnvelope(): unknown {
  const envelope = structuredClone(resolveFixture('/api/storage/findings')) as {
    payload: {
      entries: unknown[];
      kind_statuses: { kind: string; state: string }[];
    };
  };
  envelope.payload.entries = [
    {
      finding: {
        family: 'storage',
        state: 'degraded',
        coverage: {
          completeness: 'complete',
          statement: 'store size observed against soft budget',
        },
        evidence: [{ family: 'storage', reference: 'storage.over_budget_store.lcm.overage' }],
      },
      storage_kind: 'over_budget_store',
    },
  ];
  return envelope;
}

describe('SystemsHeader', () => {
  beforeEach(() => {
    useScope.getState().selectAllProjects();
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const url = new URL(String(input), 'http://localhost');
        if (url.pathname === '/api/storage/findings') {
          return json(findingsEnvelope());
        }
        if (url.pathname === '/api/storage/telemetry') {
          return json(resolveFixture('/api/storage/telemetry'));
        }
        // The freshness read fails on the wire: its segment must say so
        // rather than stamping OK or inventing amber.
        return new Response('down', { status: 500 });
      }),
    );
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it('stamps each subsystem from its own real reading', async () => {
    render(
      <QueryClientProvider
        client={new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } })}
      >
        <SystemsHeader />
      </QueryClientProvider>,
    );

    const header = await screen.findByRole('list', { name: 'Subsystem status' });

    // An observed degraded budget finding is the one thing that earns amber.
    // findBy*, because the header renders its READING stamps synchronously and
    // settles as each read lands.
    const budget = header.querySelector('[data-subsystem="budget"]') as HTMLElement;
    expect(await within(budget).findByText('alert · 1')).toBeTruthy();
    expect(budget.getAttribute('data-subsystem-tone')).toBe('alert');

    // A producer that looked and found nothing is OK; an unwired one says so.
    const orphans = header.querySelector('[data-subsystem="orphans"]') as HTMLElement;
    expect(await within(orphans).findByText('ok')).toBeTruthy();
    expect(orphans.getAttribute('data-subsystem-tone')).toBe('ok');
    expect(
      await within(header.querySelector('[data-subsystem="debris"]') as HTMLElement).findByText(
        'unsupported',
      ),
    ).toBeTruthy();

    // Envelope-backed segments stamp the envelope's own domain state.
    expect(
      await within(header.querySelector('[data-subsystem="stores"]') as HTMLElement).findByText(
        'ok',
      ),
    ).toBeTruthy();
    // A transport failure is an alert-toned error word, not OK.
    const index = header.querySelector('[data-subsystem="index"]') as HTMLElement;
    expect(await within(index).findByText('error')).toBeTruthy();
    expect(index.getAttribute('data-subsystem-tone')).toBe('alert');
  });
});

function json(body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { 'content-type': 'application/json' },
  });
}
