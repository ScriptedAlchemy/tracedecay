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
      // Every surface row is titled by its surface name, so a screen reader
      // announces which surface a state belongs to. Counted per row rather than
      // as a total: the narrow-width detail rows carry no header of their own
      // and point at their surface's instead, so a total would now be satisfied
      // by a table that had lost one row's header and gained a stray one.
      const surfaceRows = Array.from(table.querySelectorAll('tbody tr[data-work-surface]'));
      expect(surfaceRows.length).toBeGreaterThan(0);
      for (const row of surfaceRows) {
        expect(row.querySelectorAll('th[scope="row"]')).toHaveLength(1);
      }
      expect(table.querySelectorAll('tbody th[scope="row"]').length).toBe(surfaceRows.length);
    }
  });
});

/** Collapsed text, so an assertion is about wording rather than about the
 * incidental whitespace JSX leaves between elements. */
function text(node: Element | null | undefined): string {
  return (node?.textContent ?? '').replace(/\s+/g, ' ').trim();
}

/** One boundary term's reading, read from the aside the way it is presented. */
function boundaryReading(container: HTMLElement, term: string): string {
  const dt = Array.from(container.querySelectorAll('dt')).find((node) => text(node) === term);
  if (dt === undefined) throw new Error(`the boundary aside states no "${term}"`);
  const dd = dt.nextElementSibling;
  if (dd === null || dd.tagName !== 'DD') throw new Error(`"${term}" has no reading`);
  return text(dd);
}

/** One group panel's footer. `Panel` labels its section with the legend, which
 * is what makes a group's own sentence addressable rather than a substring of
 * the whole ledger. */
function panelFooter(container: HTMLElement, legend: string): string {
  const panel = container.querySelector(`section[aria-label="${legend}"]`);
  if (panel === null) throw new Error(`no panel legended "${legend}"`);
  const footer = panel.querySelector('footer');
  if (footer === null) throw new Error(`the "${legend}" panel has no footer`);
  return text(footer);
}

/** Which boundary term speaks for which group, and the words it uses when every
 * row in that group really is absent. */
const BOUNDARY_BY_GROUP: Readonly<Record<string, { term: string; absent: string }>> = {
  projections: { term: 'Work read model', absent: 'Not generated' },
  commands: { term: 'Commands', absent: 'Not exposed' },
  streams: { term: 'Activity stream', absent: 'Not registered' },
};

describe('the Work boundary aside', () => {
  /**
   * The aside and the ledger describe the same four pieces of wire, so they
   * cannot disagree about one.
   *
   * They did. `boundaryReading` counted landed contracts, and the streams group
   * has none — its row is a mounted subscription, not a generated contract — so
   * the aside reported the task-activity stream as "Not registered" while the
   * chip one column over reported it subscribed and live. Asserted against the
   * rendered chips rather than against the expected copy, so the two readings
   * are checked against each other rather than both against a guess.
   */
  it('never reports a group as absent while one of its rows is reaching', () => {
    const { container } = render(<WorkPage />);

    for (const [groupId, { term, absent }] of Object.entries(BOUNDARY_BY_GROUP)) {
      const group = WITHHELD_WORK.find((candidate) => candidate.id === groupId);
      expect(group, `no such group: ${groupId}`).toBeDefined();
      const reaching = (group?.surfaces ?? []).filter(
        (surface) =>
          container.querySelector(`[data-work-surface="${surface.id}"] [data-state="partial"]`)
          !== null,
      );
      if (reaching.length === 0) continue;
      expect(
        boundaryReading(container, term),
        `"${term}" reads "${absent}" while ${reaching.map((s) => s.id).join(', ')} is reaching`,
      ).not.toBe(absent);
    }
  });

  /** Today's readings, stated exactly. The read and write sides are genuinely
   * absent and must keep saying so; the stream is not, and must not. */
  it('states each piece of the wire in its own terms', () => {
    const { container } = render(<WorkPage />);

    expect(boundaryReading(container, 'Work read model')).toBe('Not generated');
    expect(boundaryReading(container, 'Commands')).toBe('Not exposed');
    expect(boundaryReading(container, 'Activity stream')).toBe('Subscribed, not read');
    // The one fixed reading: it describes this page, not the backend.
    expect(boundaryReading(container, 'Projections')).toBe('Not rendered');
  });
});

describe('the Work group footer', () => {
  /**
   * Two defects met in one sentence, and both came from keying on the wire state.
   *
   * The streams group's only row is a live subscription whose contract has not
   * landed, so a split on `state.kind` counted it as withheld and introduced it
   * as such. Its summary is also a participle phrase rather than a noun, so the
   * shared "there is …" frame produced "there is subscribed, with no projection
   * to refetch".
   */
  it('does not introduce a live subscription as withheld', () => {
    const { container } = render(<WorkPage />);
    const footer = panelFooter(container, 'live activity');

    expect(footer, 'the live row is not withheld').not.toMatch(/withheld because/i);
    expect(footer, 'a participle phrase was framed as a noun').not.toContain('there is subscribed');
    expect(footer).toBe('None withheld here: subscribed, with no projection to refetch.');
  });

  /** The groups that really are uniformly absent keep the reading they had. */
  it('still states why a uniformly withheld group is withheld', () => {
    const { container } = render(<WorkPage />);

    expect(panelFooter(container, 'projections over the canonical graph')).toBe(
      'Withheld here because there is no generated read model.',
    );
    expect(panelFooter(container, 'commands, each separately authorized')).toBe(
      'Withheld here because there is no generated command.',
    );
  });
});

describe('the Work row header', () => {
  /**
   * A row header is re-announced at every cell a reader visits, so what it holds
   * is paid for four times per row and sixteen times per ledger. The description
   * used to sit inside it, which opened every one of those announcements with the
   * same long sentence before the state the reader was actually navigating to.
   */
  it('names the surface and nothing else', () => {
    const { container } = render(<WorkPage />);

    for (const surface of SURFACES) {
      const header = container.querySelector(
        `[data-work-surface="${surface.id}"] th[scope="row"]`,
      );
      expect(header, `${surface.id} has no row header`).not.toBeNull();
      expect(text(header), `${surface.id}'s header carries more than its name`).toBe(surface.name);
    }
  });

  /**
   * Concise must not mean lost. The sentence is the shape of the absence — the
   * point of the ledger — and at narrow widths its column is not drawn, so it
   * has to be stated somewhere a reader still reaches and tied to the surface it
   * belongs to. Asserted as an explicit `headers` association rather than by
   * position, because a continuation row's cell would otherwise be titled by
   * whichever column header happens to sit above it.
   */
  it('keeps the description readable and associated at narrow widths', () => {
    const { container } = render(<WorkPage />);

    for (const surface of SURFACES) {
      const cell = container.querySelector(
        `[data-work-surface-detail="${surface.id}"] td[headers]`,
      );
      expect(cell, `${surface.id} states nothing at narrow widths`).not.toBeNull();
      expect(text(cell)).toBe(surface.draws);

      const headers = cell?.getAttribute('headers') ?? '';
      expect(
        container.querySelector(`th[scope="row"][id="${headers}"]`),
        `${surface.id}'s detail row points at no row header`,
      ).not.toBeNull();
    }
  });

  /**
   * The detail row spans the columns that are drawn below `md`, and that count is
   * a literal. Derived here from the headers that are not hidden there, so adding
   * or hiding a column cannot leave the span short or over-wide with nothing
   * saying so.
   */
  it('spans exactly the columns drawn at narrow widths', () => {
    const { container } = render(<WorkPage />);

    for (const table of Array.from(container.querySelectorAll('table'))) {
      const drawnBelowMd = Array.from(table.querySelectorAll('thead th[scope="col"]')).filter(
        (header) => !header.classList.contains('max-md:hidden'),
      );
      expect(drawnBelowMd.length).toBeGreaterThan(0);

      for (const cell of Array.from(table.querySelectorAll('tbody tr[data-work-surface-detail] td'))) {
        expect(cell.getAttribute('colspan')).toBe(String(drawnBelowMd.length));
      }
    }
  });
});

describe('the Work page state indicator', () => {
  /**
   * The header chip carried `max-sm:hidden`, so below 640px the page's own state
   * was left to prose — at the width least able to afford reading a paragraph to
   * find out where it stands. The detail is what should have gone, not the state.
   *
   * jsdom applies no media queries, so the two chips are asserted to partition
   * the `sm` boundary rather than measured: `classList` matches whole tokens, so
   * `sm:hidden` and `max-sm:hidden` are told apart rather than one containing the
   * other as a substring.
   */
  it('states the page state on both sides of the sm boundary', () => {
    const { container } = render(<WorkPage />);
    const header = container.querySelector('header');
    expect(header).not.toBeNull();
    const chips = Array.from(header?.querySelectorAll('[data-state]') ?? []);

    const belowSm = chips.filter((chip) => chip.classList.contains('sm:hidden'));
    const fromSm = chips.filter((chip) => chip.classList.contains('max-sm:hidden'));

    expect(belowSm, 'no page state is stated below sm').toHaveLength(1);
    expect(fromSm, 'no page state is stated from sm').toHaveLength(1);
    // Neither may carry the other's rule, or a band would render none at all.
    expect(belowSm[0]?.classList.contains('max-sm:hidden')).toBe(false);
    expect(fromSm[0]?.classList.contains('sm:hidden')).toBe(false);
    // One state, two widths: only the detail beside it differs.
    expect(belowSm[0]?.getAttribute('data-state')).toBe(fromSm[0]?.getAttribute('data-state'));
  });
});
