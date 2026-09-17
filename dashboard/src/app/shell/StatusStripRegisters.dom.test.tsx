// @vitest-environment jsdom

/**
 * Workspace registers on the strip: published while the workspace is mounted,
 * numbered on from the shell's own cells, lit from the same state rule as the
 * chip, and gone the moment the workspace unmounts — so a route the reader has
 * left can never keep reporting an authority on the strip.
 */
import { render, screen, within } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { ProjectionSync } from '../../data/sse/useEvents.tsx';
import {
  usePublishStatusRegisters,
  useStatusRegistersStore,
  type StatusRegister,
} from '../../data/shell/statusRegisters.ts';
import { StatusStrip } from './StatusStrip.tsx';

vi.mock('../../data/sse/useEvents.tsx', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../../data/sse/useEvents.tsx')>();
  return {
    ...actual,
    useEventStreamState: () => ({ state: 'live' as const, lastEventAt: null }),
    useProjectionSync: () => ({ kind: 'synced' }) as ProjectionSync,
  };
});

function Workspace({ registers }: { registers: readonly StatusRegister[] }) {
  usePublishStatusRegisters('test-workspace', registers);
  return null;
}

const GRAPH: StatusRegister = {
  id: 'test:graph',
  label: 'Graph',
  value: 'ready',
  state: 'ready',
  detail: '12,873 symbols',
};
const SELECTION: StatusRegister = {
  id: 'test:selection',
  label: 'Selection',
  value: 'subgraph_payload',
  state: 'identity',
};

afterEach(() => {
  useStatusRegistersStore.setState({ owners: new Map() });
});

describe('workspace status registers', () => {
  it('renders published registers as numbered cells after the shell cells', () => {
    render(
      <>
        <Workspace registers={[GRAPH, SELECTION]} />
        <StatusStrip />
      </>,
    );

    const strip = screen.getByRole('contentinfo', { name: 'Status' });
    const graph = within(strip).getByText('ready').closest('[data-register]');
    expect(graph?.getAttribute('data-register')).toBe('test:graph');
    expect(graph?.getAttribute('data-state')).toBe('ready');
    expect(within(strip).getByText('12,873 symbols')).toBeTruthy();
    // Numbered on from the shell's own four: the workspace cells are the
    // strip's fifth and sixth registers, not a second status word.
    expect(within(strip).getByText('5 · Graph')).toBeTruthy();
    expect(within(strip).getByText('6 · Selection')).toBeTruthy();
    // An identifier keeps its case; the strip never uppercases a selection.
    expect(within(strip).getByText('subgraph_payload')).toBeTruthy();
  });

  it('withdraws every register when its workspace unmounts', () => {
    const view = render(
      <>
        <Workspace registers={[GRAPH]} />
        <StatusStrip />
      </>,
    );
    expect(screen.getByText('5 · Graph')).toBeTruthy();

    view.rerender(<StatusStrip />);

    expect(screen.queryByText('5 · Graph')).toBeNull();
    expect(document.querySelector('[data-register]')).toBeNull();
  });

  it('re-publishes when a register changes and not otherwise', () => {
    const publish = vi.spyOn(useStatusRegistersStore.getState(), 'publish');
    const view = render(<Workspace registers={[GRAPH]} />);
    const initial = publish.mock.calls.length;

    // Same readings, new array identity: nothing to publish.
    view.rerender(<Workspace registers={[{ ...GRAPH }]} />);
    expect(publish.mock.calls.length).toBe(initial);

    view.rerender(<Workspace registers={[{ ...GRAPH, value: 'stale', state: 'stale' }]} />);
    expect(publish.mock.calls.length).toBe(initial + 1);
    expect(useStatusRegistersStore.getState().owners.get('test-workspace')?.[0]?.value).toBe(
      'stale',
    );
  });
});
