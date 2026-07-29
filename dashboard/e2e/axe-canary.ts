/**
 * The canary machinery, and the eleven canaries the run composes.
 *
 * Own module rather than more of `axe-audit.ts`, for the reason
 * `axe-workspaces.ts` gives about its own five surfaces: this is one mechanism
 * with builders nothing else uses. It is also the piece of the gate every other
 * zero leans on — a scan that quietly stopped reporting anything scores every
 * surface clean — so the rule it implements is worth reading in one sitting
 * rather than as eleven calls scattered down a scenario list.
 *
 * The two tiers are here together on purpose: the five matrix canaries carry
 * the Plan 11 viewport/zoom/media combinations for the whole run, and the six
 * showcase canaries answer only for their own routes' liveness. Which is which
 * is a claim about coverage, and it is legible only side by side.
 */
import type { Page } from '@playwright/test';
import { type Scenario } from './axe-harness.ts';
import { openTranscript } from './axe-sessions.ts';

/**
 * Two rules, chosen because they are detected by different means: `image-alt`
 * is a missing-attribute check, `button-name` runs axe's accessible-name
 * computation. A scan that reports both is exercising more than one code path
 * inside the engine.
 */
const SEEDED_DEFECTS = ['image-alt', 'button-name'] as const;

/**
 * Plant the known-inaccessible markup the canary scan must find.
 *
 * Injected into the live page rather than served as a static fixture so the
 * proof runs through the *same* path every real scenario uses — this build's
 * bundle, this context, this stillness init, this `AxeBuilder` tag set. A
 * separate hand-written HTML page would prove that axe works somewhere, which
 * is not the question; the question is whether these scans would have caught
 * anything.
 */
async function seedKnownDefects(page: Page): Promise<void> {
  const planted = await page.evaluate(() => {
    const main = document.querySelector('main#td-main');
    if (!main) return -1;
    const host = document.createElement('div');
    host.setAttribute('data-axe-canary', '');
    // A 1x1 transparent GIF inlined as a data URI: the scan must never depend
    // on a network fetch, and axe only needs the element, not the pixels.
    const img = document.createElement('img');
    img.src = 'data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7';
    img.style.cssText = 'width:24px;height:24px';
    host.append(img);
    // No text, no aria-label, no title: nothing for the accessible-name
    // computation to find.
    const button = document.createElement('button');
    button.type = 'button';
    button.style.cssText = 'width:24px;height:24px';
    host.append(button);
    main.prepend(host);
    return host.childElementCount;
  });
  if (planted !== SEEDED_DEFECTS.length) {
    throw new Error(
      `the canary planted ${planted} elements, expected ${SEEDED_DEFECTS.length}` +
        (planted === -1 ? ' (main#td-main was not in the page)' : ''),
    );
  }
}

/**
 * The canary, bound to one route.
 *
 * One canary per audited route rather than one per run. The engine is not
 * route-scoped, so a single canary does prove the scan can see a defect — but
 * it proves it about the page it ran on. Every route reaches `analyze()`
 * through its own render: a surface that throws during hydration, or renders
 * nothing an accessibility scan can reach, produces the same `violations: 0` as
 * a clean one. Seeding on each route means the zero recorded for that route was
 * measured on that route.
 */
function canary(
  id: string,
  route: string,
  drive?: (page: Page) => Promise<void>,
  // `matrix` canaries carry the Plan 11 viewport/zoom/media matrix for their
  // routes, so every 390x844, 400%-zoom and forced-colors scan in the run is
  // one where a planted violation had to be reported for the scan to count.
  // A widened matrix whose new combinations silently stopped detecting
  // anything would otherwise read as five more routes scoring clean. Five of
  // these are enough to witness every combination in the run, so a canary
  // added purely for a new route's own liveness may stay on the showcase tier.
  tier: 'matrix' | 'showcase' = 'matrix',
): Scenario {
  return {
    id,
    route,
    proves: `THE GATE ITSELF on ${route} — a known-inaccessible element is reported here, so this route's zeros are measurements`,
    overrides: {},
    matrix: tier === 'matrix',
    // Checked on every scan, not once: the seeding is re-applied after each
    // navigation, so each viewport and theme is an independent confirmation
    // that the scan running at that size is live. It also means a breakage
    // that only shows up at one width cannot hide behind five good scans.
    expectViolations: SEEDED_DEFECTS,
    drive: async (page) => {
      if (drive !== undefined) await drive(page);
      await seedKnownDefects(page);
    },
    assert: async (page) => {
      // The planted nodes must really be on the page and really be visible. An
      // element the browser never laid out is one axe skips, which would turn
      // the whole canary into a check that cannot fail.
      const planted = page.locator('[data-axe-canary]');
      const hosts = await planted.count();
      if (hosts !== 1) throw new Error(`expected one canary host in the page, found ${hosts}`);
      if (!(await planted.isVisible())) {
        throw new Error('the canary markup is present but not visible, so axe would skip it');
      }
    },
  };
}

/**
 * The five that carry the matrix.
 *
 * They run first in the composed list because everything after them is only
 * worth reading once the scan has been shown to be live on the route it was
 * taken on. `sessions-canary` opens the transcript drill-down before seeding —
 * the list behind it is a different render, and the zeros being vouched for
 * are the drill-down's.
 */
export const MATRIX_CANARIES: readonly Scenario[] = [
  canary('axe-engine-canary', '/automations'),
  canary('observatory-canary', '/observatory'),
  canary('costs-canary', '/costs'),
  canary('code-canary', '/code'),
  canary('sessions-canary', '/sessions', openTranscript),
];

/**
 * The six on the showcase tier, deliberately.
 *
 * A canary answers "did THIS route render something a scan can see", which is
 * a question about hydration and not about width, and the showcase tier already
 * asks it at 320, 768 and 1440 in both themes. The thirty-combination tier is
 * carried by the five above, so every 390x844, 400%-zoom, contrast-more and
 * forced-colors scan in the run is still one where a planted violation had to
 * be reported.
 */
export const SHOWCASE_CANARIES: readonly Scenario[] = [
  canary('settings-canary', '/settings', undefined, 'showcase'),
  canary('knowledge-canary', '/knowledge', undefined, 'showcase'),
  canary('delivery-canary', '/delivery', undefined, 'showcase'),
  canary('loom-canary', '/loom', undefined, 'showcase'),
  canary('agents-canary', '/agents', undefined, 'showcase'),
  canary('work-canary', '/work', undefined, 'showcase'),
];
