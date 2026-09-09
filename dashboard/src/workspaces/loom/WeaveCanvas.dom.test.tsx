import { fireEvent, render, screen } from '@testing-library/react';
import { MemoryRouter } from 'react-router';
import { describe, expect, it, vi } from 'vitest';
import { WeaveCanvas, LoadedEventCanvas, eventPositions } from './WeaveCanvas.tsx';
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

it('shares stable source coordinates across viewport and reveal changes', () => {
  const frames = [10, 20, null].map((timestamp, index) => ({
    id: `event-${index}`, ordinal: index, timestamp, role: 'assistant', tool: null,
    content: null, excerpt: '', summaryNodeIds: [],
  }));
  const first = eventPositions(frames);
  expect(eventPositions([...frames])).toEqual(first);
  expect(first.points.map(({ x, y }) => [x, y])).toEqual([[0, 200], [1, 200], [1, 350]]);
  expect(frames[2]!.timestamp).toBeNull();
});


it('keeps dense event hit regions from covering adjacent event centers', () => {
  const frames = Array.from({ length: 200 }, (_, index) => ({
    id: `event-${index}`, ordinal: index, timestamp: BASE + index,
    role: 'assistant', tool: null, content: null, excerpt: '', summaryNodeIds: [],
  }));
  const onSelect = vi.fn();
  render(<MemoryRouter><LoadedEventCanvas frames={frames} visible={frames} activeId="event-199" onSelect={onSelect} onInspect={() => {}} toolbar={null} scrubber={null} /></MemoryRouter>);
  const target = screen.getByRole('button', { name: 'Select stored event event-100' });
  const next = screen.getByRole('button', { name: 'Select stored event event-101' });
  const targetRect = target.querySelector('rect')!;
  const targetCenter = Number(target.querySelector('circle')!.getAttribute('cx'));
  const nextRect = next.querySelector('rect')!;
  expect(Number(nextRect.getAttribute('x'))).toBeGreaterThan(targetCenter);
  expect(Number(targetRect.getAttribute('x')) + Number(targetRect.getAttribute('width'))).toBeLessThan(Number(next.querySelector('circle')!.getAttribute('cx')));
  fireEvent.click(targetRect);
  expect(onSelect).toHaveBeenCalledWith('event-100');
});


it('maps the visible vertical session window into the minimap and supports keyboard navigation', () => {
  renderCanvas();
  const viewport = screen.getByRole('region', { name: 'Session lane viewport' });
  Object.defineProperties(viewport, { scrollHeight: { value: 1000 }, clientHeight: { value: 200 }, scrollTop: { value: 100, writable: true } });
  const scrollBy = vi.fn();
  viewport.scrollBy = scrollBy;
  fireEvent.scroll(viewport);
  const minimap = screen.getByRole('group', { name: 'Session hierarchy minimap' });
  const window = minimap.querySelector('[data-session-viewport]')!;
  const earlier = Number(window.getAttribute('y'));
  expect(Number(window.getAttribute('height'))).toBeLessThan(64);
  viewport.scrollTop = 500;
  fireEvent.scroll(viewport);
  expect(Number(window.getAttribute('y'))).toBeGreaterThan(earlier);
  fireEvent.keyDown(minimap, { key: 'ArrowDown' });
  expect(scrollBy).toHaveBeenCalledWith({ top: 150 });
});
