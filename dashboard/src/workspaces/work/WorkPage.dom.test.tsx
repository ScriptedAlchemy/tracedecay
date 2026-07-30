import { render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { WITHHELD_WORK, withheldPresentation } from './authority.ts';
import { WorkPage } from './WorkPage.tsx';

const GATE_SENTENCE =
  'No generated Work read model is available in this build. Kanban, DAG, timeline, causal, workload, runtime, and control state are withheld rather than inferred.';

const SURFACES = WITHHELD_WORK.flatMap((group) => group.surfaces);

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('WorkPage contract gate', () => {
  it('names the workspace and its unavailable contract boundary', () => {
    const { container } = render(<WorkPage />);

    expect(screen.getByRole('heading', { level: 1, name: 'Work' })).toBeTruthy();
    expect(container.querySelector('[data-work-authority="uncontracted"]')).not.toBeNull();
    expect(
      container.querySelector('[data-state="unsupported_schema"], [data-state="unsupported"]'),
    ).not.toBeNull();
    expect(screen.queryByText('Ready')).toBeNull();
    expect(screen.queryByText('Complete · zero findings')).toBeNull();

    // One row is partial, and exactly one: the task-activity subscription this
    // build really holds. Pinned to that row rather than asserted away, because
    // a partial appearing anywhere else would mean a projection had started
    // claiming half-read data it cannot have.
    const partials = container.querySelectorAll('[data-state="partial"]');
    expect(partials).toHaveLength(1);

    // Resolved in two steps on purpose. Optional-chaining the row and asserting
    // the chip `not.toBeNull()` passes on `undefined`, so deleting the row
    // entirely would have satisfied it.
    const activityRow = container.querySelector('[data-work-surface="task-activity"]');
    expect(activityRow, 'the subscribed stream row is missing').not.toBeNull();
    expect(activityRow?.querySelector('[data-state="partial"]')).not.toBeNull();
  });

  it('scrolls inside a named region and leaves the shell its own main landmark', () => {
    const { container } = render(<WorkPage />);

    // The shell renders `main#td-main`; a workspace that adds a second `main`
    // nests a landmark that may not nest.
    expect(container.querySelector('main')).toBeNull();
    const region = container.querySelector('[data-work-authority]');
    expect(region?.getAttribute('role')).toBe('region');
    expect(region?.getAttribute('aria-label')).toBe('Work content');
    expect(region?.getAttribute('tabindex')).toBe('0');
  });

  it('explains exactly which state is withheld', () => {
    render(<WorkPage />);

    expect(screen.getByText(GATE_SENTENCE)).toBeTruthy();
  });

  it('accounts for every withheld projection, command and stream', () => {
    const { container } = render(<WorkPage />);

    for (const surface of SURFACES) {
      const row = container.querySelector(`[data-work-surface="${surface.id}"]`);
      expect(row, `${surface.id} has no row`).not.toBeNull();
      // The contract the row waits on is the actionable part, so it is printed
      // rather than summarised, and the row's state is the one its reason maps
      // to — a command row must not borrow the read side's schema state.
      expect(row?.textContent).toContain(surface.requires);
      expect(row?.querySelector('[data-state]')?.getAttribute('data-state')).toBe(
        withheldPresentation(surface.reason).state,
      );
    }

    expect(container.querySelectorAll('[data-work-surface]')).toHaveLength(SURFACES.length);
  });

  it('renders no measurement, no fabricated zero, and no unavailable command', () => {
    const { container } = render(<WorkPage />);
    const ledger = container.querySelector('[data-work-ledger]');

    // The channel number in the header is a real reading; the data plane below
    // it must carry no figure at all, because there is nothing to measure.
    expect(ledger).not.toBeNull();
    expect(ledger?.querySelectorAll('[data-cell="numeric"]')).toHaveLength(0);
    expect(ledger?.textContent).not.toMatch(/\b0(?:\.0+)?\b/);
    expect(screen.queryAllByRole('button')).toHaveLength(0);
    expect(screen.queryAllByRole('link')).toHaveLength(0);
    expect(screen.queryByRole('button', { name: /create|admit|accept|cancel/i })).toBeNull();
  });

  it('does not fetch without a generated Work contract', () => {
    const fetchMock = vi.fn();
    vi.stubGlobal('fetch', fetchMock);

    render(<WorkPage />);

    expect(fetchMock).not.toHaveBeenCalled();
  });

  it('gives each ledger table a caption and column headers', () => {
    const { container } = render(<WorkPage />);
    const tables = container.querySelectorAll('table');

    expect(tables).toHaveLength(WITHHELD_WORK.length);
    for (const table of Array.from(tables)) {
      expect(table.querySelector('caption')?.textContent ?? '').not.toBe('');
      expect(table.querySelectorAll('thead th[scope="col"]').length).toBeGreaterThan(0);
      // Every row is titled by its surface name, so a screen reader announces
      // which surface a state belongs to.
      expect(table.querySelectorAll('tbody th[scope="row"]').length).toBe(
        table.querySelectorAll('tbody tr').length,
      );
    }
  });
});
