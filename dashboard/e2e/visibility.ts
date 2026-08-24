/**
 * The pre-scan visibility guard, split out of `axe-harness.ts` so it carries no
 * Playwright import and can be exercised against a real DOM in `vitest`.
 *
 * A clean Axe score on a blank page is the worst kind of evidence: the scan
 * reads `textContent`, which `opacity: 0` does not remove. So every capture is
 * gated on the page having actually rendered.
 */

/** What the in-page probe measures. Plain data, so the policy is pure. */
export interface VisibilityReport {
  /** Painted width/height of `main#td-main`. */
  readonly mainW: number;
  readonly mainH: number;
  /** Trimmed text length of the main region. */
  readonly textLen: number;
  /** Content-bearing elements the opacity sweep actually looked at. */
  readonly sampled: number;
  /** How many of those rendered at effectively zero opacity. */
  readonly faded: number;
  /** Lowest opacity seen, and a sample of where. */
  readonly worst: number;
  readonly worstSample: string;
  /** `data-motion` on `<html>`, to name the stillness mode in failures. */
  readonly motion: string;
}

/**
 * Measured inside the page. Kept as source text rather than a function for the
 * same reason `STILLNESS_INIT` is: `tsx` compiles this file with esbuild's
 * `keepNames`, whose `__name` helper does not exist in the page, so a named
 * function handed to `page.evaluate` throws on arrival. A string is evaluated
 * as an expression, and this one evaluates to the report.
 *
 * The sweep is deliberately layout-free — no `getBoundingClientRect` per
 * element — because that keeps it identical under jsdom, where the unit test
 * runs, and a real browser, where the harness runs.
 */
export const VISIBILITY_PROBE = `(function () {
  var main = document.querySelector('main#td-main');
  var rect = main ? main.getBoundingClientRect() : null;
  var sampled = 0;
  var faded = 0;
  var worst = 1;
  var worstSample = '';
  var candidates = main ? main.querySelectorAll('*') : [];
  for (var i = 0; i < candidates.length; i += 1) {
    var el = candidates[i];
    var style = getComputedStyle(el);
    if (style.display === 'none' || style.visibility === 'hidden') continue;
    var own = '';
    for (var c = 0; c < el.childNodes.length; c += 1) {
      var node = el.childNodes[c];
      if (node.nodeType === 3) own += node.nodeValue || '';
    }
    if (own.trim().length === 0) continue;
    sampled += 1;
    // Opacity does not inherit as a computed value, so a faded container with
    // opaque text children reads as opaque unless the chain is walked.
    var o = 1;
    for (var up = el; up; up = up.parentElement) {
      var step = Number.parseFloat(getComputedStyle(up).opacity || '1');
      if (Number.isFinite(step) && step < o) o = step;
    }
    if (o < 0.05) {
      faded += 1;
      if (o < worst) {
        worst = o;
        worstSample = el.tagName.toLowerCase() + ': ' + own.trim().slice(0, 60);
      }
    }
  }
  return {
    mainW: rect ? rect.width : 0,
    mainH: rect ? rect.height : 0,
    textLen: ((main && main.textContent) || '').trim().length,
    sampled: sampled,
    faded: faded,
    worst: worst,
    worstSample: worstSample,
    motion: document.documentElement.dataset['motion'] || 'unset',
  };
})()`;

/**
 * Turn a report into a pass or a thrown failure.
 *
 * The `sampled === 0` arm is the point of the rewrite. The previous guard swept
 * `.td-enter, .td-stagger > *`, and no component in the app uses either
 * primitive, so the loop matched nothing on every page and reported zero faded
 * regions whatever was on screen. A check that cannot fail is not a check, so
 * measuring nothing is now itself the failure.
 */
export function assertVisibilityReport(report: VisibilityReport, tag: string): void {
  if (report.mainW < 100 || report.mainH < 100) {
    throw new Error(`${tag}: main region has no painted size (${report.mainW}x${report.mainH})`);
  }
  if (report.textLen < 40) {
    throw new Error(`${tag}: main region rendered almost no text (${report.textLen} chars)`);
  }
  if (report.sampled === 0) {
    throw new Error(
      `${tag}: the visibility sweep matched no content-bearing element, so it ` +
        `proved nothing about this capture (data-motion=${report.motion})`,
    );
  }
  if (report.faded > 0) {
    throw new Error(
      `${tag}: ${report.faded}/${report.sampled} rendered regions sit at opacity ` +
        `${report.worst} — the capture is blank where they are ` +
        `(${report.worstSample}) (data-motion=${report.motion})`,
    );
  }
}
