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
    proves: 'the per-project Brain reached from the registry is also well-formed',
    overrides: {},
    // Scoping to a project swaps in a different set of definition lists, which
    // is why the registry view passing does not vouch for this one.
    drive: (page) => openRow(page, /tracedecay/i),
    assert: async (page) => {
      const dlCount = await page.locator('dl').count();
      if (dlCount === 0) throw new Error('scoped Brain rendered no definition lists at all');
    },
  },
];
