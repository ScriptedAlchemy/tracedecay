import { fireEvent, render, screen } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import { WeaveCanvas } from './WeaveCanvas.tsx';
import { composeWeave, type WeaveSession } from './weave.ts';

/**
 * The zoom/pan toolbar over the weave's time window. Under test: the window
 * helpers in `tracks.ts` are actually wired to the canvas — zooming narrows
 * the printed window and culls threads outside it, fit restores the whole
 * extent, and pan is only offered once there is somewhere to pan back to.
 */

const DAY = 86_400;
const BASE = 1_784_700_000;

function session(overrides: Partial<WeaveSession>): WeaveSession {
  return {
    session_id: 'sess',
    provider: 'cursor',
    title: null,
    started_at: BASE,
    last_message_at: BASE + 1_800,
    messages: 10,
    is_subagent: false,
    models: [],
    ...overrides,
  };
}

function renderCanvas() {
  // Two sessions a week apart: zooming to the centre of the extent must drop
  // both marks out of the field once the window is narrow enough.
  const weave = composeWeave([
    session({ session_id: 'early' }),
    session({ session_id: 'late', started_at: BASE + 7 * DAY, last_message_at: BASE + 7 * DAY + 1_800 }),
  ]);
  render(
    <WeaveCanvas weave={weave} selectedId={null} onSelect={() => {}} ariaLabel="weave" />,
  );
}

describe('WeaveCanvas time window', () => {
  it('starts fitted, with pan and fit disabled', () => {
    renderCanvas();
    expect(screen.getByRole('toolbar', { name: 'Time window' })).toBeTruthy();
    expect(screen.getByText('whole extent')).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Fit the whole extent' })).toHaveProperty(
      'disabled',
      true,
    );
    expect(
      screen.getByRole('button', { name: 'Pan to earlier sessions' }),
    ).toHaveProperty('disabled', true);
  });

  it('zooming narrows the printed window and enables pan and fit', () => {
    renderCanvas();
    fireEvent.click(screen.getByRole('button', { name: 'Zoom in' }));
    expect(screen.queryByText('whole extent')).toBeNull();
    expect(screen.getByRole('button', { name: 'Fit the whole extent' })).toHaveProperty(
      'disabled',
      false,
    );
    expect(
      screen.getByRole('button', { name: 'Pan to later sessions' }),
    ).toHaveProperty('disabled', false);
  });

  it('a deep zoom culls threads outside the window; fit restores them', () => {
    renderCanvas();
    const marks = () => document.querySelectorAll('svg [data-thread]').length;
    const before = marks();
    expect(before).toBeGreaterThan(0);
    // Zoom to well under half the extent, centred between the two clusters.
    for (let i = 0; i < 6; i += 1) {
      fireEvent.click(screen.getByRole('button', { name: 'Zoom in' }));
    }
    expect(marks()).toBeLessThan(before);
    fireEvent.click(screen.getByRole('button', { name: 'Fit the whole extent' }));
    expect(marks()).toBe(before);
    expect(screen.getByText('whole extent')).toBeTruthy();
  });
});
