import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { FIXTURES, resolveFixture } from '../../../stories/fixtures/data.ts';
import { fixtureEnvelope } from '../../test/fixtureEnvelope.ts';
import { CostsPage } from './CostsPage.tsx';

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('CostsPage truth claims', () => {
  it('separates provider, tokenizer, and estimate coverage without inventing cache causality', async () => {
    const payload = savingsOverviewPayload();
    const sessions = payload['sessions'] as Record<string, unknown>;
    sessions['usage_messages'] = 100;
    sessions['tokenized_messages'] = 300;
    sessions['estimated_messages'] = 600;
    sessions['messages'] = 1000;

    renderCosts(payload);

    expect(await screen.findByText('tokenized')).toBeTruthy();
    expect(screen.getByText(/provider-reported token breakdown/i)).toBeTruthy();
    expect(screen.getByText(/the wire does not report why/i)).toBeTruthy();
    expect(screen.queryByText(/they share one cache/i)).toBeNull();
  });

  it('reports an unreported message class as unreported, not as zero coverage', async () => {
    const payload = savingsOverviewPayload();
    const sessions = payload['sessions'] as Record<string, unknown>;
    // The block is available and holds messages, but the per-class counts and
    // the session count never came back. Coalescing them printed "0% of 41,204
    // messages carry token counts the provider reported" over four zeroes.
    sessions['messages'] = 41_204;
    sessions['usage_messages'] = null;
    sessions['tokenized_messages'] = null;
    sessions['estimated_messages'] = null;
    sessions['unknown_model_messages'] = null;
    sessions['session_count'] = null;

    renderCosts(payload);

    expect(await screen.findByText(/the measured share is unknown/i)).toBeTruthy();
    expect(screen.queryByText(/0% of/i)).toBeNull();
    expect(screen.getAllByText('not reported').length).toBe(4);
    // The wider denominator is stated only where it was served.
    expect(screen.queryByText(/0 messages in 0 sessions/i)).toBeNull();
    expect(
      screen.getByText(/messages and sessions the ledger did not report/i),
    ).toBeTruthy();
  });

  it('renders failed turn reads as unavailable instead of actual zero spend', async () => {
    const payload = savingsOverviewPayload();
    // `savings_api::read_failed_block` — the block reports the failure and
    // leaves every figure null rather than settling to zero.
    payload['turns'] = {
      available: false,
      status: 'read_failed',
      error: 'failed to read priced turn ledger',
      turn_count: null,
      total_cost_usd: null,
      total_tokens: null,
      cost_basis: null,
    };

    renderCosts(payload);

    expect(await screen.findByText(/priced turn ledger read failed/i)).toBeTruthy();
    expect(screen.queryByText('$0.00')).toBeNull();
    expect(screen.queryByText(/0 across those turns/i)).toBeNull();
  });

  it('renders a failed session aggregate separately from an empty ledger', async () => {
    const payload = savingsOverviewPayload();
    payload['sessions'] = {
      available: false,
      db: '/fast/projects/tracedecay/.tracedecay/sessions.db',
      status: 'read_failed',
      error: 'failed to aggregate session tokens',
      scope: null,
      messages: null,
      usage_messages: null,
      tokenized_messages: null,
      estimated_messages: null,
      cost_basis: null,
      actual: null,
      tokenized: null,
      estimated: null,
      session_count: null,
      model_count: null,
      unknown_model_messages: null,
      token_counting: null,
    };

    renderCosts(payload);

    expect(await screen.findAllByText(/session ledger read failed/i)).not.toHaveLength(0);
    expect(screen.queryByText(/reported no token breakdown/i)).toBeNull();
    expect(screen.queryByText(/reported no messages/i)).toBeNull();
  });

  it('keeps the canonical cost read alive when the savings ledger read fails', async () => {
    const payload = savingsOverviewPayload();
    // `savings_api::read_failed_block` shape: the block reports the failure and
    // leaves both summaries null rather than settling them to zero.
    payload['savings'] = {
      available: false,
      db: '/fast/projects/tracedecay/.tracedecay/savings.db',
      error: 'failed to read savings ledger',
      ledger: null,
      lifetime_counters: null,
      recording: null,
    };

    renderCosts(payload);

    // The failed legacy read reports itself...
    expect(await screen.findAllByText(/Savings ledger read failed/i)).not.toHaveLength(0);
    // ...and the independent canonical projection still renders its own
    // measurements rather than being blanked by its neighbour.
    expect(await screen.findByText('provider tokens')).toBeTruthy();
    expect(screen.getByText('latency breakdown')).toBeTruthy();
  });

  it('discloses that project savings are a capped top slice', async () => {
    const payload = savingsOverviewPayload();
    const savings = payload['savings'] as Record<string, unknown>;
    const lifetime = savings['lifetime_counters'] as Record<string, unknown>;
    lifetime['project_total'] = 57;
    lifetime['projects_limit'] = 25;
    lifetime['projects_truncated'] = true;

    renderCosts(payload);

    expect(await screen.findByText(/top 25 of 57 projects/i)).toBeTruthy();
  });
});

/**
 * The page issues two independent reads. The savings overview is the payload
 * under test; every other route — the canonical `/api/costs` projection among
 * them — is served its own fixture, because a stub that answers every URL with
 * the savings body would make the canonical panel report a schema error and
 * hide whichever failure the case is actually about.
 */
function renderCosts(savingsOverview: unknown) {
  vi.stubGlobal(
    'fetch',
    vi.fn(async (input: RequestInfo | URL) => {
      const pathname = new URL(String(input), 'http://localhost').pathname;
      const body =
        pathname === '/api/plugins/savings/overview'
          ? fixtureEnvelope(savingsOverview)
          : resolveFixture(pathname, '');
      return new Response(JSON.stringify(body), { status: 200 });
    }),
  );
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: 0 } },
  });
  render(
    <QueryClientProvider client={client}>
      <CostsPage />
    </QueryClientProvider>,
  );
}

function savingsOverviewPayload(): Record<string, unknown> {
  const fixture = structuredClone(FIXTURES['/api/plugins/savings/overview']) as {
    payload: Record<string, unknown>;
  };
  return fixture.payload;
}
