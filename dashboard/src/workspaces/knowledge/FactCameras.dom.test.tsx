/**
 * The provenance cameras as drawn: shell type tiers, a withheld fact as
 * violet crosshatch printed by identity, the inspect/select verbs, the
 * selected fact's relations lifted, the disputes as selectable rows, and a
 * field too full for its rows opening one frame at a time.
 */
import { fireEvent, render, screen, within } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';

import type { MemoryGraphPayloadV1 } from '../../contracts/generated.ts';
import { FactCameras } from './FactCameras.tsx';
import { composeFactScene } from './factScene.ts';

const WITHHELD = 'fact.withheld.000042';

function factNode(id: string, trust: number, category: string) {
  return {
    id: `fact:${id}`,
    kind: 'fact' as const,
    label: `${id} content`,
    fact_id: id,
    payload_access: 'eligible' as const,
    projected_as_of: 1,
    content: `${id} content`,
    category,
    trust_score: trust,
    retrieval_count: 10,
    helpful_count: 1,
  };
}

function graph(over: Partial<MemoryGraphPayloadV1> = {}): MemoryGraphPayloadV1 {
  return {
    nodes: [
      factNode('fact-one', 0.9, 'decision'),
      factNode('fact-two', 0.4, 'decision'),
      factNode('fact-three', 0.7, 'tool'),
      {
        id: `fact:${WITHHELD}`,
        kind: 'fact',
        label: WITHHELD,
        fact_id: WITHHELD,
        payload_access: 'redacted',
        projected_as_of: 1,
        content: null,
        category: null,
        trust_score: null,
        retrieval_count: null,
        helpful_count: null,
      },
      { id: 'entity:A', kind: 'entity', entity_id: 'A', label: 'A' },
    ],
    edges: [
      { kind: 'mentions', source: 'fact:fact-one', target: 'entity:A' },
      { kind: 'supports', source: 'fact:fact-one', target: 'fact:fact-two' },
      { kind: 'contradicts', source: 'fact:fact-one', target: 'fact:fact-three' },
      { kind: 'supersedes', source: 'fact:fact-three', target: `fact:${WITHHELD}` },
    ],
    coverage: {
      completeness: 'partial',
      eligible: null,
      examined: null,
      matched: null,
      excluded: null,
      omitted: null,
      unknown: null,
      denominator: null,
      unit: null,
      omission_reasons: [],
    },
    fact_universe_count: 30,
    fact_candidates_examined: 4,
    unavailable_fact_candidates: 0,
    root_count: 4,
    relation_limit: 100,
    relation_count: 4,
    ...over,
  };
}

function draw(payload: MemoryGraphPayloadV1 = graph(), selectedFactId: string | null = null) {
  const onInspect = vi.fn();
  const onSelect = vi.fn();
  const view = render(
    <FactCameras
      scene={composeFactScene(payload, [])}
      inspectedFactId={null}
      selectedFactId={selectedFactId}
      onInspect={onInspect}
      onSelect={onSelect}
      graphRead={{ state: 'partial', code: 'graph_coverage_incomplete' }}
    />,
  );
  return { ...view, onInspect, onSelect };
}

describe('provenance cameras', () => {
  it('sets text in the shell tiers on an unscaled drawing', () => {
    draw();
    const svg = screen.getByTestId('fact-constellation-svg');
    expect(svg.getAttribute('viewBox')).toBe(`0 0 ${svg.getAttribute('width')} ${svg.getAttribute('height')}`);
    const tiers = [...svg.querySelectorAll('text')].map((text) => [text.getAttribute('data-tier'), text.getAttribute('class')?.split(' ')[0]]);
    expect(new Set(tiers.map(([tier, cls]) => `${tier}:${cls}`))).toEqual(new Set(['body:text-body', 'legend:td-legend', 'value:td-value']));
    expect(svg.querySelectorAll('text[font-size]')).toHaveLength(0);
  });

  it('draws a withheld fact as violet crosshatch printed by identity, never content', () => {
    const { container } = draw();
    const withheld = container.querySelector(`[data-fact-id="${WITHHELD}"]`)!;
    expect(withheld.getAttribute('data-access')).toBe('redacted');
    expect(withheld.querySelector('[data-mark="restricted"]')?.getAttribute('stroke')).toBe('var(--raw-state-locked)');
    expect(withheld.textContent).toContain('redacted · …000042');
    expect(withheld.textContent).toContain('absent');
    expect(container.textContent).not.toContain(`${WITHHELD} content`);
  });

  it('draws trust as a zero-anchored rail whose length and brightness both follow trust', () => {
    const { container } = draw();
    const rail = (id: string) => container.querySelector(`[data-fact-id="${id}"] [data-rail-width]`)!;
    expect([rail('fact-one').getAttribute('data-rail-width'), rail('fact-one').getAttribute('fill-opacity')]).toEqual(['54', '0.93']);
    expect([rail('fact-two').getAttribute('data-rail-width'), rail('fact-two').getAttribute('fill-opacity')]).toEqual(['24', '0.58']);
  });

  it('inspects on pointer movement, dimming unwired rows, and selects on click', () => {
    const { container, onInspect, onSelect } = draw();
    const three = container.querySelector('[data-fact-id="fact-three"]')!;
    fireEvent.pointerMove(three);
    expect(onInspect).toHaveBeenCalledWith('fact-three');
    expect(three.getAttribute('data-inspected')).toBe('true');
    expect(screen.getByTestId('constellation-focal').getAttribute('data-focal-role')).toBe('inspecting');
    expect(container.querySelector('[data-fact-id="fact-two"]')!.getAttribute('opacity')).toBe('0.3');
    expect(container.querySelector('[data-fact-id="fact-one"]')!.getAttribute('opacity')).toBe('1');
    fireEvent.click(three);
    expect(onSelect).toHaveBeenCalledWith('fact-three');
  });

  it('stops dimming when the pointer moves off a row, leaving only the page inspection', () => {
    const { container, onInspect } = draw();
    const three = container.querySelector('[data-fact-id="fact-three"]')!;
    const two = container.querySelector('[data-fact-id="fact-two"]')!;
    fireEvent.pointerMove(three);
    expect(two.getAttribute('opacity')).toBe('0.3');
    // The page holds no inspection here (it was dismissed), so leaving the
    // row onto the empty field lifts the neighbourhood dimming.
    fireEvent.pointerLeave(three, { relatedTarget: screen.getByTestId('fact-constellation-svg') });
    expect(two.getAttribute('opacity')).toBe('1');
    expect(three.getAttribute('data-inspected')).toBeNull();
    // Returning to the same row inspects it again.
    fireEvent.pointerMove(three);
    expect(onInspect).toHaveBeenCalledTimes(2);
  });

  it("lifts the selected fact's relations with a halo and lets the others recede", () => {
    const { container } = draw(graph(), 'fact-three');
    const lifted = [...container.querySelectorAll('[data-lifted]')].map((node) => node.getAttribute('data-relation'));
    expect(lifted).toEqual(['contradicts', 'supersedes']);
    expect(container.querySelectorAll('[data-halo]')).toHaveLength(2);
    const supports = container.querySelector('[data-relation="supports"] path')!;
    expect(supports.getAttribute('stroke-opacity')).toBe('0.14');
    expect(container.querySelector('[data-fact-id="fact-three"] [data-selected-gutter]')).not.toBeNull();
    const focal = screen.getByTestId('constellation-focal');
    expect(focal.getAttribute('data-focal-role')).toBe('selected');
    expect(within(focal).getByText(/trust 0\.70 · 2 relations/)).toBeTruthy();
  });

  it('lists every contradiction and supersession as a row that selects its source', () => {
    const { onSelect } = draw();
    const buttons = within(screen.getByTestId('fact-disputes')).getAllByRole('button');
    expect(buttons.map((button) => button.getAttribute('data-dispute'))).toEqual(['contradicts', 'supersedes']);
    expect(buttons[0]!.textContent).toContain('trust 0.90 → trust 0.70');
    expect(buttons[1]!.textContent).toContain('trust 0.70 → redacted · trust absent');
    fireEvent.click(buttons[1]!);
    expect(onSelect).toHaveBeenCalledWith('fact-three');
  });

  it('states a partial read with no disputes rather than printing an empty list', () => {
    draw(graph({ edges: [{ kind: 'supports', source: 'fact:fact-one', target: 'fact:fact-two' }] }));
    expect(screen.getByTestId('fact-disputes').textContent).toBe(
      'no contradiction or supersession among the drawn facts; this partial read may not hold them all',
    );
  });

  it('aggregates a field too full for its rows and opens one frame at a time', () => {
    const categories = ['alpha', 'beta', 'gamma', 'delta'];
    const nodes = categories.flatMap((category) =>
      Array.from({ length: 8 }, (_, index) => factNode(`${category}-${index}`, (index + 1) / 10, category)),
    );
    const { container } = draw(graph({ nodes, edges: [] }));
    expect(screen.getByTestId('fact-constellation').getAttribute('data-camera-mode')).toBe('aggregate');
    expect(container.querySelectorAll('[data-tick]')).toHaveLength(32);
    expect(container.querySelector('[data-frame="alpha"]')!.textContent).toContain('0.10–0.80');
    fireEvent.click(screen.getByRole('button', { name: 'Open beta: 8 facts' }));
    expect(screen.getByTestId('fact-constellation').getAttribute('data-camera-mode')).toBe('open');
    expect(container.querySelectorAll('[data-fact-id^="beta-"]')).toHaveLength(8);
    expect(container.querySelectorAll('[data-fact-id^="alpha-"]')).toHaveLength(0);
    fireEvent.click(screen.getByRole('button', { name: 'All categories' }));
    expect(screen.getByTestId('fact-constellation').getAttribute('data-camera-mode')).toBe('aggregate');
  });
});
