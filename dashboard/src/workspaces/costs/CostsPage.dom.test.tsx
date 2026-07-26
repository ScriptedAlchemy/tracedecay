import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { FIXTURES } from '../../../stories/fixtures/data.ts';
import { CostsPage } from './CostsPage.tsx';

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('CostsPage truth claims', () => {
  it('separates provider, tokenizer, and estimate coverage without inventing cache causality', async () => {
    const payload = structuredClone(
      FIXTURES['/api/plugins/savings/overview'],
    ) as Record<string, unknown>;
    const sessions = payload['sessions'] as Record<string, unknown>;
    sessions['usage_messages'] = 100;
    sessions['tokenized_messages'] = 300;
    sessions['estimated_messages'] = 600;
    sessions['messages'] = 1000;

    vi.stubGlobal(
      'fetch',
      vi.fn(async () => new Response(JSON.stringify(payload), { status: 200 })),
    );
    const client = new QueryClient({
      defaultOptions: { queries: { retry: false, gcTime: 0 } },
    });
    render(
      <QueryClientProvider client={client}>
        <CostsPage />
      </QueryClientProvider>,
    );

    expect(await screen.findByText('tokenized')).toBeTruthy();
    expect(screen.getByText(/provider-reported token breakdown/i)).toBeTruthy();
    expect(screen.getByText(/the wire does not report why/i)).toBeTruthy();
    expect(screen.queryByText(/they share one cache/i)).toBeNull();
  });

  it('renders failed turn reads as unavailable instead of actual zero spend', async () => {
    const payload = structuredClone(
      FIXTURES['/api/plugins/savings/overview'],
    ) as Record<string, unknown>;
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
    const payload = structuredClone(
      FIXTURES['/api/plugins/savings/overview'],
    ) as Record<string, unknown>;
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

  it('discloses that project savings are a capped top slice', async () => {
    const payload = structuredClone(
      FIXTURES['/api/plugins/savings/overview'],
    ) as Record<string, unknown>;
    const savings = payload['savings'] as Record<string, unknown>;
    const lifetime = savings['lifetime_counters'] as Record<string, unknown>;
    lifetime['project_total'] = 57;
    lifetime['projects_limit'] = 25;
    lifetime['projects_truncated'] = true;

    renderCosts(payload);

    expect(await screen.findByText(/top 25 of 57 projects/i)).toBeTruthy();
  });
});

function renderCosts(payload: unknown) {
  vi.stubGlobal(
    'fetch',
    vi.fn(async () => new Response(JSON.stringify(payload), { status: 200 })),
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
