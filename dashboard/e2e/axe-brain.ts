/**
 * Definition lists — Brain.
 *
 * Own module rather than more of `axe-audit.ts` because it is the one audited
 * route whose claim is purely structural: no payload is overridden, no figure
 * is read, and the check is a DOM walk axe cannot perform. It needs no fixture
 * builder at all, which is exactly why it does not belong inside a module built
 * around one.
 *
 * Brain is also the one audited route with no canary, so it carries the matrix
 * for /brain itself; the scenario says why beside the flag.
 */
import type { Page } from '@playwright/test';
import { openRow, type Scenario } from './axe-harness.ts';

export const BRAIN_SCENARIOS: readonly Scenario[] = [
  {
    id: 'brain',
    route: '/brain',
    proves: 'Brain definition lists are well-formed (definition-list / dlitem)',
    overrides: {},
    // Brain is the one audited route with no canary, and its definition lists
    // are the densest reflow subject in the app, so it carries the matrix for
    // /brain. Its zeros lean on the five canaried routes scanning the same
    // combinations in the same run rather than on a planted defect of its own.
    matrix: true,
    assert: async (page) => {
      // Structural check independent of axe: every dt/dd sits directly in a dl
      // or in a div wrapper whose parent is the dl, and each group's dt
      // precedes its dd in DOM order.
      const bad = await page.evaluate(() => {
        const problems: string[] = [];
        for (const dl of Array.from(document.querySelectorAll('dl'))) {
          for (const child of Array.from(dl.children)) {
            const tag = child.tagName;
            if (tag !== 'DT' && tag !== 'DD' && tag !== 'DIV') {
              problems.push(`dl has a ${tag} child`);
            }
            if (tag === 'DIV') {
              const kids = Array.from(child.children)
                .map((k) => k.tagName)
                .filter((t) => t !== 'SPAN');
              const dtAt = kids.indexOf('DT');
              const ddAt = kids.indexOf('DD');
              if (dtAt >= 0 && ddAt >= 0 && dtAt > ddAt) {
                problems.push(`dd precedes dt in a dl group: ${kids.join(',')}`);
              }
            }
          }
        }
        return problems;
      });
      if (bad.length > 0) throw new Error(`malformed definition lists: ${bad.join(' | ')}`);
      const dlCount = await page.locator('dl').count();
      if (dlCount === 0) throw new Error('Brain rendered no definition lists at all');
    },
  },
  {
    id: 'brain-scoped',
    route: '/brain',
    proves:
      'the per-project Brain reached from the registry is well-formed, and its holdings rail takes a tab stop only at the widths where it is really a scroller',
    overrides: {},
    // Scoping to a project swaps in a different set of definition lists, which
    // is why the registry view passing does not vouch for this one.
    drive: (page) => openRow(page, /tracedecay/i),
    matrix: true,
    assert: async (page) => {
      const dlCount = await page.locator('dl').count();
      if (dlCount === 0) throw new Error('scoped Brain rendered no definition lists at all');
    },
    assertEachScan: assertHoldingsTabStop,
  },
];

/**
 * The holdings rail is keyboard-reachable exactly when it needs to be.
 *
 * A scroll container whose contents hold nothing focusable has to take the tab
 * stop itself (WCAG 2.1.1) — the rail is read-out text, so there is nothing
 * inside it to tab to. But its overflow is applied at `lg`, and below that it
 * is an ordinary block in the page flow: measured at 320 and 768 CSS px it
 * computes `overflow-y: visible`, and a literal `tabIndex={0}` there put a stop
 * that does nothing in front of the holdings, on the screens where tabbing is
 * most of the navigation.
 *
 * So the assertion is the correspondence, in both directions, at every width in
 * the matrix — not the presence or the absence.
 */
async function assertHoldingsTabStop(page: Page, tag: string): Promise<void> {
  const reading = await page.evaluate(() => {
    const rail = document.querySelector('aside[aria-label^="What TraceDecay holds"]');
    if (rail === null) return null;
    return {
      overflowY: getComputedStyle(rail).overflowY,
      tabindex: rail.getAttribute('tabindex'),
    };
  });
  if (reading === null) {
    throw new Error(`${tag}: the scoped Brain rendered no holdings rail`);
  }
  const scroller = reading.overflowY === 'auto' || reading.overflowY === 'scroll';
  const focusable = reading.tabindex === '0';
  if (scroller && !focusable) {
    throw new Error(
      `${tag}: the holdings rail is a scroll container (overflow-y: ${reading.overflowY}) with no ` +
        `tab stop, so a keyboard reader cannot scroll it — nothing inside it is focusable`,
    );
  }
  if (!scroller && focusable) {
    throw new Error(
      `${tag}: the holdings rail is not a scroll container (overflow-y: ${reading.overflowY}) but ` +
        `still takes a tab stop, so every keyboard reader at this width tabs through a stop that ` +
        `does nothing`,
    );
  }
}
