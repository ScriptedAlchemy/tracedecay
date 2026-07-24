import { render, within } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { SignalPanel } from './BrainPage.tsx';
import type { LiveActivityPulse } from '../../data/sse/connect.ts';

/**
 * A dead stream and an idle system are different states, and the panel is the
 * only place the dashboard says which one it is in. These tests exist to stop
 * the two from ever converging on screen.
 */

const NOW = 1_700_000_000_000;

function pulses(): LiveActivityPulse[] {
  return [
    { projectId: 'p1', family: 'heartbeat', streamId: 'heartbeat', at: NOW - 400_000 },
    { projectId: 'p1', family: 'heartbeat', streamId: 'heartbeat', at: NOW - 380_000 },
    {
      projectId: 'p1',
      family: 'code_index_completed',
      streamId: 'code_index',
      at: NOW - 370_000,
    },
  ];
}

function renderPanel(state: 'connecting' | 'live' | 'offline', lastEventAt: number | null) {
  vi.spyOn(Date, 'now').mockReturnValue(NOW);
  return render(
    <SignalPanel pulses={pulses()} sseState={state} lastEventAt={lastEventAt} />,
  );
}

afterEach(() => {
  vi.restoreAllMocks();
});

describe('SignalPanel connection honesty', () => {
  it('says in words that a dead stream is not an idle one', () => {
    const { container, getByText } = renderPanel('offline', NOW - 370_000);
    expect(getByText(/frozen, not idle/i)).toBeTruthy();
    // State is carried by the taxonomy chip's own data attribute, icon and
    // label — never by colour alone.
    expect(container.querySelector('[data-state="offline"]')).toBeTruthy();
    expect(within(container).getByText(/^Offline$/i)).toBeTruthy();
  });

  it('withholds the rate while offline rather than decaying it to a healthy zero', () => {
    const { getByText, queryByText } = renderPanel('offline', NOW - 370_000);
    expect(getByText(/rate · not measured/i)).toBeTruthy();
    expect(getByText('—')).toBeTruthy();
    expect(queryByText(/per min/i)).toBeNull();
  });

  it('reports a connected but silent system as connected, with the age climbing', () => {
    const { container, getByText, queryByText } = renderPanel('live', NOW - 370_000);
    expect(container.querySelector('[data-state="ready"]')).toBeTruthy();
    expect(queryByText(/frozen, not idle/i)).toBeNull();
    // Six minutes of quiet on an open stream: the rate is a truthful zero and
    // the age says how long the quiet has lasted.
    expect(getByText(/per min · last 60s/i)).toBeTruthy();
    expect(getByText('6m')).toBeTruthy();
  });

  it('gives idle and offline different chips, wording and rate treatment', () => {
    const idle = renderPanel('live', NOW - 370_000).container.textContent ?? '';
    vi.restoreAllMocks();
    const dead = renderPanel('offline', NOW - 370_000).container.textContent ?? '';
    expect(idle).not.toEqual(dead);
    expect(idle).toContain('current');
    expect(dead).toContain('Disconnected');
  });

  it('distinguishes reconnecting from both', () => {
    const { container, getByText } = renderPanel('connecting', NOW - 4_000);
    expect(container.querySelector('[data-state="loading"]')).toBeTruthy();
    expect(getByText(/Reconnecting\./i)).toBeTruthy();
  });

  it('shows an em dash, not a zero, when no event has ever arrived', () => {
    vi.spyOn(Date, 'now').mockReturnValue(NOW);
    const { getByText } = render(
      <SignalPanel pulses={[]} sseState="live" lastEventAt={null} />,
    );
    expect(getByText('—')).toBeTruthy();
    expect(getByText(/no events observed yet/i)).toBeTruthy();
  });
});
