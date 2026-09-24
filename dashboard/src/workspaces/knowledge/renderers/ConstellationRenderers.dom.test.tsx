/**
 * The three candidate constellation renderers, drawn from one scene: text at
 * its printed pixel size, a withheld fact as violet crosshatch printed by
 * identity, and the same inspect/select verbs as the default drawing.
 */
import { fireEvent, render, screen, within } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';

import type { MemoryFactRowV1, MemoryGraphPayloadV1 } from '../../../contracts/generated.ts';
import { ConstellationVariant } from './ConstellationVariant.tsx';
import { CONSTELLATION_VARIANTS, type ConstellationVariant as Variant } from './variant.ts';

const T0 = Date.UTC(2026, 8, 1) * 1000;
const WITHHELD = 'fact.withheld.000042';

function factNode(id: string, trust: number, category: string) {
  return {
    id: `fact:${id}`,
    kind: 'fact' as const,
    label: `${id} content`,
    fact_id: id,
    payload_access: 'eligible' as const,
    projected_as_of: T0,
    content: `${id} content`,
    category,
    trust_score: trust,
    retrieval_count: 10,
    helpful_count: 1,
  };
}

const GRAPH: MemoryGraphPayloadV1 = {
  nodes: [
    factNode('fact-one', 0.9, 'decision'),
    factNode('fact-three', 0.7, 'tool'),
    {
      id: `fact:${WITHHELD}`,
      kind: 'fact',
      label: WITHHELD,
      fact_id: WITHHELD,
      payload_access: 'redacted',
      projected_as_of: T0,
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
  fact_candidates_examined: 3,
  unavailable_fact_candidates: 0,
  root_count: 3,
  relation_limit: 100,
  relation_count: 3,
};

const ROWS: MemoryFactRowV1[] = [];

function draw(variant: Variant, selectedFactId: string | null = null) {
  const onInspect = vi.fn();
  const onSelect = vi.fn();
  const view = render(
    <ConstellationVariant
      variant={variant}
      graph={GRAPH}
      rows={ROWS}
      inspectedFactId={null}
      selectedFactId={selectedFactId}
      onInspect={onInspect}
      onSelect={onSelect}
      graphRead={{ state: 'partial', code: 'graph_coverage_incomplete' }}
    />,
  );
  return { ...view, onInspect, onSelect };
}

describe.each(CONSTELLATION_VARIANTS)('%s renderer', (variant) => {
  it('sets every label at its printed pixel size on an unscaled drawing', () => {
    draw(variant);
    const svg = screen.getByTestId('fact-constellation-svg');
    expect(svg.getAttribute('viewBox')).toBe(`0 0 ${svg.getAttribute('width')} ${svg.getAttribute('height')}`);
    const sizes = new Set([...svg.querySelectorAll('text')].map((text) => text.getAttribute('font-size')));
    expect([...sizes].sort()).toEqual(variant === 'lattice' ? ['10'] : ['10', '11']);
  });

  it('draws a withheld fact as violet crosshatch and never as content', () => {
    const { container } = draw(variant);
    const withheld = container.querySelector(`[data-fact-id="${WITHHELD}"]`)!;
    expect(withheld.getAttribute('data-access')).toBe('redacted');
    expect(withheld.querySelector('[data-mark="restricted"]')?.getAttribute('stroke')).toBe('var(--raw-state-locked)');
    expect(withheld.textContent).toContain('redacted · …000042');
    expect(container.textContent).not.toContain(`${WITHHELD} content`);
  });

  it('inspects on pointer movement and selects on click', () => {
    const { container, onInspect, onSelect } = draw(variant);
    const body = container.querySelector('[data-fact-id="fact-three"]')!;
    fireEvent.pointerMove(body);
    expect(onInspect).toHaveBeenCalledWith('fact-three');
    expect(body.getAttribute('data-inspected')).toBe('true');
    expect(screen.getByTestId('constellation-focal').getAttribute('data-focal-role')).toBe('inspecting');
    fireEvent.click(body);
    expect(onSelect).toHaveBeenCalledWith('fact-three');
  });

  it('rings the selected fact and names it in the focal readout', () => {
    const { container } = draw(variant, 'fact-one');
    const body = container.querySelector('[data-fact-id="fact-one"]')!;
    expect(body.getAttribute('data-selected')).toBe('true');
    const focal = screen.getByTestId('constellation-focal');
    expect(focal.getAttribute('data-focal-role')).toBe('selected');
    expect(within(focal).getByText('trust 0.90')).toBeTruthy();
  });
});

describe('trust field', () => {
  it('enumerates contradictions and supersessions beneath the field, and a row selects its source', () => {
    const { onSelect } = draw('field');
    const disputes = screen.getByTestId('field-disputes');
    const buttons = within(disputes).getAllByRole('button');
    expect(buttons.map((button) => button.getAttribute('data-dispute'))).toEqual(['contradicts', 'supersedes']);
    expect(buttons[1]!.textContent).toContain('redacted · trust absent');
    fireEvent.click(buttons[0]!);
    expect(onSelect).toHaveBeenCalledWith('fact-one');
  });

  it('zooms into one entity from the keyboard-operable select', () => {
    draw('field');
    fireEvent.change(screen.getByLabelText("Zoom the field into one entity's facts"), { target: { value: 'entity:A' } });
    expect(screen.getByTestId('fact-constellation-svg').getAttribute('data-zoom')).toBe('entity:A');
  });
});

describe('provenance cameras', () => {
  it('prints an absent trust as the word, on a rail anchored at zero', () => {
    const { container } = draw('cameras');
    const withheld = container.querySelector(`[data-fact-id="${WITHHELD}"]`)!;
    expect(withheld.textContent).toContain('absent');
    const one = container.querySelector('[data-fact-id="fact-one"] [data-rail-width]')!;
    const three = container.querySelector('[data-fact-id="fact-three"] [data-rail-width]')!;
    expect(Number(one.getAttribute('data-rail-width')) / Number(three.getAttribute('data-rail-width'))).toBeCloseTo(0.9 / 0.7);
  });
});
