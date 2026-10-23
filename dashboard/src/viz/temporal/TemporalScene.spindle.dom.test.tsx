import { fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { TemporalScene } from './TemporalScene.tsx';
import { fittedWindowFor, layoutTemporalScene } from './layout.ts';
import { layoutSpindle } from './spindle.ts';
import type { JourneyLane, JourneyProjection, LayoutOptions, TemporalSceneModel } from './types.ts';

const START = 1_784_700_000;
const ROOT = JSON.stringify(['cursor', 'shared-session-id']);
const CHILD = JSON.stringify(['cursor', 'child']);
const OTHER = JSON.stringify(['codex', 'shared-session-id']);

function fixture(overrides: Partial<LayoutOptions> = {}) {
  const lanes: JourneyLane[] = [
    { id: ROOT, sessionId: 'shared-session-id', provider: 'cursor', label: 'Cursor parent', start: START, end: START + 700, parentId: null, depth: 0, isSubagent: false },
    { id: CHILD, sessionId: 'child', provider: 'cursor', label: 'Cursor child', start: START + 80, end: START + 600, parentId: ROOT, depth: 1, isSubagent: true },
    { id: OTHER, sessionId: 'shared-session-id', provider: 'codex', label: 'Codex parent', start: START + 20, end: START + 800, parentId: null, depth: 0, isSubagent: false },
  ].map((item) => ({ ...item, endSource: 'session_end' as const, agent: null, messages: 10, editedFilesRecorded: false, editedFileCount: 0, models: [] }));
  const projection: JourneyProjection = {
    lanes,
    events: lanes.flatMap((lane) => [
      { id: `start:${lane.id}`, laneId: lane.id, kind: 'session_start' as const, time: lane.start, sequence: 0, grade: 'exact' as const, source: 'session' as const, label: lane.label, detail: null, ref: lane.sessionId },
      { id: `commit:${lane.id}`, laneId: lane.id, kind: 'commit' as const, time: START + 300, sequence: 1, grade: 'exact' as const, source: 'commit' as const, label: `commit ${lane.label}`, detail: 'recorded commit evidence', ref: `sha:${lane.id}` },
    ]),
    relations: [], gaps: [], intervals: [], extent: { start: START, end: START + 800 },
    stats: { lanes: 3, roots: 2, subagents: 1, messages: 30, openEnded: 0, hollow: 0, undated: 0, providers: [{ id: 'cursor', lanes: 2, messages: 20 }, { id: 'codex', lanes: 1, messages: 10 }] },
  };
  const options: LayoutOptions = {
    viewport: { width: 1000, left: 200, right: 24, window: { start: START, end: START + 800 } },
    zoom: 'agent', branches: { collapsed: new Set(), expanded: new Set() },
    selectedLaneId: null, selectedEventId: null, reveal: null,
    hiddenKinds: new Set(), denseLaneThreshold: 48, ...overrides,
  };
  const base = layoutTemporalScene(projection, options);
  return { projection, base, model: layoutSpindle(projection, base, options) };
}

function eventElement(container: HTMLElement, id: string): SVGGElement {
  const element = [...container.querySelectorAll<SVGGElement>('[data-event]')]
    .find((item) => item.dataset['event'] === id);
  expect(element, `selectable event ${id}`).toBeDefined();
  return element!;
}

function numbers(value: string): number[] {
  return (value.match(/-?\d*\.?\d+(?:e[-+]?\d+)?/gi) ?? []).map(Number);
}

describe('spindle scene interaction', () => {
  beforeEach(() => {
    vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue(null);
  });
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it('keeps transformed glyphs, pointer regions and keyboard selection together across view changes without Canvas2D', () => {
    const { base, model } = fixture();
    const onSelectEvent = vi.fn();
    const handlers = { onSelectEvent, onSelectLane: vi.fn(), onToggleBranch: vi.fn(), onWindowChange: vi.fn() };
    const scene = (display: TemporalSceneModel, variant: 'weave' | 'rail') => (
      <TemporalScene model={display} variant={variant} ariaLabel="Execution evidence" reducedMotion={true} {...handlers} />
    );
    const { container, rerender } = render(scene(model, 'weave'));
    const canvas = container.querySelector('canvas');
    const id = `commit:${CHILD}`;
    const element = eventElement(container, id);
    expect(screen.getByRole('status').textContent).toContain('scene layer unavailable');

    for (const [display, variant] of [[model, 'weave'], [base, 'rail'], [model, 'weave']] as const) {
      rerender(scene(display, variant));
      const node = display.nodes.find((item) => item.id === id)!;
      expect(container.querySelector('canvas')).toBe(canvas);
      expect(eventElement(container, id)).toBe(element);
      const hit = element.querySelector<SVGElement>('rect[fill="transparent"], circle[fill="transparent"]')!;
      const circle = hit.tagName.toLowerCase() === 'circle';
      const x = circle ? Number(hit.getAttribute('cx')) : Number(hit.getAttribute('x')) + Number(hit.getAttribute('width')) / 2;
      const y = circle ? Number(hit.getAttribute('cy')) : Number(hit.getAttribute('y')) + Number(hit.getAttribute('height')) / 2;
      expect(x).toBeCloseTo(node.x, 8);
      expect(y).toBeCloseTo(node.y, 8);
      const glyph = element.querySelector('g[transform]')!;
      expect(numbers(glyph.getAttribute('transform')!)).toEqual([node.x, node.y]);
      expect(element.dataset['lane']).toBe(CHILD);

      fireEvent.click(hit);
      expect(onSelectEvent).toHaveBeenLastCalledWith(id);
      fireEvent.keyDown(container.querySelector('svg[data-scene-layer="overlay"]')!, { key: 'Enter' });
      expect(onSelectEvent).toHaveBeenLastCalledWith(id);
    }
    expect(onSelectEvent).toHaveBeenCalledTimes(6);
  });

  it('draws the canonical strand shapes in the minimap and keeps its navigation on the recorded full time axis', () => {
    const { projection, model } = fixture();
    const onWindowChange = vi.fn();
    const fullWindow = fittedWindowFor(projection.extent);
    const { container } = render(
      <TemporalScene model={model} variant="weave" ariaLabel="Execution evidence" reducedMotion={true}
        fullWindow={fullWindow} onSelectEvent={vi.fn()} onSelectLane={vi.fn()}
        onToggleBranch={vi.fn()} onWindowChange={onWindowChange} />,
    );

    const marks = [...container.querySelectorAll('[data-minimap-lane]')];
    expect(marks).toHaveLength(model.minimap.lanes.length);
    for (const lane of model.minimap.lanes) {
      const mark = marks.find((item) => item.getAttribute('data-minimap-lane') === lane.id)!;
      const coordinates = numbers(mark.getAttribute('points') ?? mark.getAttribute('d') ?? '');
      const expected = lane.points!.flatMap((point) => [point.x, point.y]);
      expect(coordinates).toHaveLength(expected.length);
      coordinates.forEach((value, index) => expect(value).toBeCloseTo(expected[index]!, 2));
    }

    fireEvent.click(screen.getByRole('group', { name: 'Temporal minimap' }), { clientX: 350 });
    const center = fullWindow.start + 0.35 * (fullWindow.end - fullWindow.start);
    expect(onWindowChange).toHaveBeenCalledWith({ start: center - 400, end: center + 400 });
  });

  it('keeps collapsed member evidence selectable and walks every visible event by keyboard', () => {
    const { projection, base, model } = fixture({ zoom: 'workstream' });
    expect(base.lanes.some((lane) => lane.id === CHILD)).toBe(false);
    expect(model.nodes.length).toBeGreaterThan(base.nodes.length);
    const onSelectEvent = vi.fn();
    const { container } = render(
      <TemporalScene model={model} variant="weave" ariaLabel="Execution evidence" reducedMotion={true}
        onSelectEvent={onSelectEvent} onSelectLane={vi.fn()} onToggleBranch={vi.fn()} onWindowChange={vi.fn()} />,
    );
    const ids = new Set(projection.events.map((event) => event.id));
    expect(new Set([...container.querySelectorAll('[data-event]')].map((element) => element.getAttribute('data-event')))).toEqual(ids);
    const overlay = container.querySelector('svg[data-scene-layer="overlay"]')!;
    fireEvent.keyDown(overlay, { key: 'Home' });
    for (let index = 0; index < model.nodes.length; index += 1) {
      fireEvent.keyDown(overlay, { key: 'Enter' });
      fireEvent.keyDown(overlay, { key: 'ArrowRight' });
    }
    expect(new Set(onSelectEvent.mock.calls.map(([id]) => id))).toEqual(ids);
    fireEvent.click(eventElement(container, `commit:${CHILD}`));
    expect(onSelectEvent).toHaveBeenLastCalledWith(`commit:${CHILD}`);
  });
});
