import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { z } from 'zod';

import { fetchEnvelope } from '../data/query/envelope.ts';
import { fixtureEnvelope } from '../test/fixtureEnvelope.ts';
import { envelopeReadState, ReadSection } from './ReadSection.tsx';

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

it.each(['lcm_temporal_budget_execution_work_exhausted', undefined])(
  'renders a received unknown response with reason %s without claiming lost connectivity', async (reason) => {
    const envelope = fixtureEnvelope(null, 'unknown');
    envelope['coverage'] = {
      ...(envelope['coverage'] as Record<string, unknown>),
      omission_reasons: reason ? [reason] : [],
    };
    vi.stubGlobal('fetch', vi.fn(async () => new Response(JSON.stringify(envelope))));
    const result = await fetchEnvelope('/api/sessions/timeline', z.unknown());
    const child = vi.fn(() => <div>Timeline records</div>);
    render(
      <ReadSection
        title="Session timeline"
        chrome="centered"
        state={envelopeReadState(false, result, { loading: 'Reading timeline.' })}
      >
        {child}
      </ReadSection>,
    );

    expect(screen.getByText('Unknown')).toBeTruthy();
    expect(screen.getByText(`· ${reason ?? 'no usable result was provided'}`)).toBeTruthy();
    expect(screen.getByText(/The daemon responded without a usable result/)).toBeTruthy();
    expect(screen.queryByText(/No response has been recorded/)).toBeNull();
    expect(screen.queryByText(/Refresh once the daemon is serving/)).toBeNull();
    expect(screen.queryByText(/daemon unreachable/)).toBeNull();
    expect(child).not.toHaveBeenCalled();
  },
);

it('keeps an unobserved read distinct from a received unknown response', () => {
  render(
    <ReadSection
      title="Session timeline"
      chrome="centered"
      state={envelopeReadState(false, undefined, { loading: 'Reading timeline.' })}
    >
      {() => <div>Timeline records</div>}
    </ReadSection>,
  );

  expect(screen.getByText(/No response has been recorded/)).toBeTruthy();
  expect(screen.queryByText(/The daemon responded without a usable result/)).toBeNull();
});
