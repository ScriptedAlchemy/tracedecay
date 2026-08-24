import { describe, expect, it } from 'vitest';
import { VISIBILITY_PROBE, assertVisibilityReport, type VisibilityReport } from './visibility.ts';

/**
 * The probe ships as source text so `page.evaluate` can take it as an
 * expression; evaluating that same string here means the test covers the
 * artifact the harness actually runs, not a re-typed copy of it.
 */
function probe(): VisibilityReport {
  // eslint-disable-next-line no-new-func -- exercising the shipped source text
  return new Function(`return ${VISIBILITY_PROBE};`)() as VisibilityReport;
}

/** jsdom has no layout engine, so painted size never comes back from the DOM. */
function withPaintedMain(report: VisibilityReport): VisibilityReport {
  return { ...report, mainW: 1440, mainH: 900 };
}

function renderMain(inner: string): void {
  document.documentElement.dataset['motion'] = 'reduced';
  document.body.innerHTML = `<main id="td-main">${inner}</main>`;
}

const PARAGRAPHS = `
  <p>The daemon reported four measured storage roles for this project.</p>
  <p>Two of those roles are over their configured budget right now.</p>
`;

describe('visibility probe', () => {
  it('counts the content-bearing regions it looked at', () => {
    renderMain(PARAGRAPHS);
    const report = probe();
    expect(report.sampled).toBe(2);
    expect(report.faded).toBe(0);
    expect(report.textLen).toBeGreaterThan(40);
  });

  it('catches a text region pinned invisible by a stylesheet', () => {
    document.head.innerHTML = `<style>.faded { opacity: 0; }</style>`;
    renderMain(`${PARAGRAPHS}<p class="faded">Awaiting-review counts are unknown, not zero.</p>`);
    const report = probe();
    expect(report.sampled).toBe(3);
    expect(report.faded).toBe(1);
    expect(report.worst).toBe(0);
    expect(report.worstSample).toContain('Awaiting-review counts');
    document.head.innerHTML = '';
  });

  it('does not treat a deliberately muted control as invisible', () => {
    renderMain(`${PARAGRAPHS}<button style="opacity: 0.6">Review remediation</button>`);
    const report = probe();
    expect(report.sampled).toBe(3);
    expect(report.faded).toBe(0);
  });

  it('ignores regions the page removed from the flow', () => {
    renderMain(`${PARAGRAPHS}<p style="display: none">Hidden by the app on purpose.</p>`);
    expect(probe().sampled).toBe(2);
  });
});

describe('visibility policy', () => {
  const healthy: VisibilityReport = {
    mainW: 1440,
    mainH: 900,
    textLen: 400,
    sampled: 12,
    faded: 0,
    worst: 1,
    worstSample: '',
    motion: 'reduced',
  };

  it('passes a page that rendered', () => {
    expect(() => assertVisibilityReport(healthy, 'brain')).not.toThrow();
  });

  it('rejects a main region with no painted size', () => {
    expect(() => assertVisibilityReport({ ...healthy, mainW: 0, mainH: 0 }, 'brain')).toThrow(
      /no painted size/,
    );
  });

  it('rejects a main region with almost no text', () => {
    expect(() => assertVisibilityReport({ ...healthy, textLen: 3 }, 'brain')).toThrow(
      /almost no text/,
    );
  });

  it('rejects a sweep that measured nothing instead of calling it clean', () => {
    expect(() => assertVisibilityReport({ ...healthy, sampled: 0 }, 'brain')).toThrow(
      /matched no content-bearing element/,
    );
  });

  it('names what was invisible', () => {
    expect(() =>
      assertVisibilityReport(
        { ...healthy, faded: 1, worst: 0, worstSample: 'p: Awaiting-review counts' },
        'automations-unreadable',
      ),
    ).toThrow(/opacity 0 — the capture is blank/);
  });
});

describe('the guard as the harness runs it', () => {
  it('refuses a capture whose content is pinned invisible', () => {
    document.head.innerHTML = `<style>.faded { opacity: 0; }</style>`;
    renderMain(`${PARAGRAPHS}<p class="faded">Every required source completed.</p>`);
    expect(() => assertVisibilityReport(withPaintedMain(probe()), 'automations')).toThrow(
      /the capture is blank/,
    );
    document.head.innerHTML = '';
  });

  it('accepts a capture that actually rendered', () => {
    renderMain(PARAGRAPHS);
    expect(() => assertVisibilityReport(withPaintedMain(probe()), 'automations')).not.toThrow();
  });
});
