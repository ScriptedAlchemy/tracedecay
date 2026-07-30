import { render } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

/**
 * What this page does on the day the wire opens.
 *
 * The channel's whole value is that it never claims more than this build can
 * prove, and the sharpest way to break that is to leave the closed reading in
 * place after a contract lands. So the generated module is stood in for with one
 * carrying a Work schema — a name, not a payload shape, since the real shape
 * arrives as a generated zod schema and is not mirrored anywhere here — and the
 * page is checked for having noticed.
 *
 * It must notice without pretending: a landed contract nothing reads yet is not
 * a rendered projection, and this page still draws no lane, edge or figure.
 */
vi.mock('../../contracts/index.ts', () => ({
  WorkSnapshotV1Schema: { parse: (value: unknown) => value },
  WorkTopologyPolicyV1Schema: { parse: (value: unknown) => value },
}));

const { WorkPage } = await import('./WorkPage.tsx');

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('Work once a contract lands', () => {
  it('reports the contract instead of the closed gate, and still reads nothing', () => {
    const { container } = render(<WorkPage />);

    expect(
      container.querySelector('[data-work-authority]')?.getAttribute('data-work-authority'),
      'the measured authority reading has to change when the wire opens',
    ).toBe('partially-contracted');

    // The closed sentence is the page's strongest claim, and it is now false.
    expect(container.textContent).not.toContain('No generated Work read model is available');
    expect(container.textContent).toContain('not read');

    const kanban = container.querySelector('[data-work-surface="kanban"]');
    expect(kanban?.querySelector('[data-state]')?.getAttribute('data-state')).toBe('partial');
    expect(kanban?.textContent).toContain('WorkSnapshotV1Schema');

    // Rows whose contract did not land keep their withheld state: a single
    // arrival must not read as the whole channel opening.
    const admission = container.querySelector('[data-work-surface="admission"]');
    expect(admission?.querySelector('[data-state]')?.getAttribute('data-state')).toBe('unsupported');
  });

  /**
   * The lifecycle hazard between a landed schema and a wired row.
   *
   * A generated contract says a payload shape exists, not that a route serves
   * it. Reaching for one on the strength of the schema alone would put the page
   * into a failed-request state over a backend that never claimed to answer, so
   * a landed contract must not start a fetch until a row is deliberately wired
   * to a registered route.
   */
  it('reaches for no route on the strength of a schema alone', () => {
    const fetchMock = vi.fn();
    vi.stubGlobal('fetch', fetchMock);

    render(<WorkPage />);

    expect(fetchMock).not.toHaveBeenCalled();
  });

  /** The structure the accessibility gate depends on has to survive the
   * transition: it is asserted against the closed state elsewhere, and a state
   * change is exactly when a landmark or a table caption gets dropped. */
  it('keeps its landmarks, labels and captions once the wire opens', () => {
    const { container } = render(<WorkPage />);

    expect(container.querySelector('main'), 'the shell owns the main landmark').toBeNull();
    const region = container.querySelector('[data-work-authority]');
    expect(region?.getAttribute('role')).toBe('region');
    expect(region?.getAttribute('aria-label')).toBe('Work content');

    for (const table of Array.from(container.querySelectorAll('table'))) {
      expect(table.querySelector('caption')?.textContent ?? '').not.toBe('');
      expect(table.querySelectorAll('thead th').length).toBeGreaterThan(0);
    }

    // A landed row still reports a state a screen reader can read out, rather
    // than an unlabelled colour change.
    const chip = container.querySelector('[data-work-surface="kanban"] [data-state]');
    expect((chip?.textContent ?? '').trim()).not.toBe('');
  });

  it('still draws no projection it has not read', () => {
    const { container } = render(<WorkPage />);
    const ledger = container.querySelector('[data-work-ledger]');

    // The same three absences the accessibility gate pins for this route: a
    // figure to read, a control to press, a link to follow. State-chip glyphs
    // are not in that set — a chip is the absence being reported, not a drawing
    // of data.
    expect(ledger?.querySelector('[data-cell="numeric"]')).toBeNull();
    expect(ledger?.querySelector('button')).toBeNull();
    expect(ledger?.querySelector('a[href]')).toBeNull();
    expect(ledger?.querySelector('canvas')).toBeNull();
  });
});
