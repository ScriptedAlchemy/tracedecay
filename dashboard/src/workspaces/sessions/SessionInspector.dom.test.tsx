import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { fixtureEnvelope } from '../../test/fixtureEnvelope.ts';
import { SessionInspector } from './SessionInspector.tsx';

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('Session transcript drill-down', () => {
  it('reports canonical temporal retrieval as unavailable without rendering raw turns', async () => {
    const response = fixtureEnvelope(null, 'unknown');
    response.coverage = {
      completeness: 'unknown',
      eligible: null,
      examined: null,
      matched: null,
      excluded: null,
      omitted: null,
      unknown: null,
      denominator: null,
      unit: 'records',
      omission_reasons: ['lcm_temporal_retrieval_not_mounted'],
    };
    renderInspector(response);

    expect(await screen.findByText('Unknown')).toBeTruthy();
    expect(await screen.findByText(/lcm_temporal_retrieval_not_mounted/)).toBeTruthy();
    expect(screen.queryByText('assistant')).toBeNull();
    expect(screen.queryByText(/raw messages/)).toBeNull();
  });
});

function renderInspector(payload: unknown) {
  vi.stubGlobal(
    'fetch',
    vi.fn(async () => new Response(JSON.stringify(payload), { status: 200 })),
  );
  renderWith();
}

function renderWith() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  render(
    <QueryClientProvider client={client}>
      <SessionInspector sessionId="claude:035c8f3c" onClose={() => {}} />
    </QueryClientProvider>,
  );
}
