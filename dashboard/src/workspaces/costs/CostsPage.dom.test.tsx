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
});
