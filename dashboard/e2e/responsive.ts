/**
 * The two Plan 11 measurements no axe rule performs: page-level reflow and
 * touch-target size.
 *
 * The probes are pure DOM measurement and the pass/fail rules are pure
 * functions over their reports, so neither needs a Playwright import and both
 * can be driven against a real DOM outside the browser harness.
 */

/** WCAG 2.5.5 / Plan 11 minimum, in CSS pixels. */
export const MIN_TOUCH_TARGET_PX = 44;

/* ==========================================================================
 * Reflow: page-level horizontal scroll, and what is running past the edge.
 * ========================================================================== */

export interface OverflowOffender {
  /**
   * `page-overflow` widens the document itself. `clipped` is content that runs
   * past the viewport inside an ancestor that hides its horizontal overflow.
   * unreachable rather than merely off-screen, which is the plan's "clipped
   * truth state".
   */
  readonly kind: 'page-overflow' | 'clipped';
  readonly selector: string;
  readonly clipper: string;
  readonly right: number;
  readonly text: string;
}

/** A region that scrolls horizontally on its own, and whether it announces a
 * name. The plan permits "labeled code/table/graph regions" to do this. */
export interface InternalScroller {
  readonly selector: string;
  /** Lower-case tag name; `aside` is a side rail. */
  readonly tag: string;
  readonly label: string;
  readonly role: string;
}

/**
 * A scroll container with content in it and no height to show it in.
 *
 * `overflow: auto` on a flex child that lost its height resolves to
 * `clientHeight: 0`, which clips every row inside it AND removes the scrollbar
 * that would have reached them. Nothing on the page looks broken: the header
 * above it still reports "4 loaded of 4 matching rows".
 *
 * This is the plan's "clipped truth state" in its unambiguous form, and unlike
 * the overflow heuristics it needs no judgement. A scroller holding text with
 * a zero-height viewport is never intentional.
 */
export interface CollapsedScroller {
  readonly selector: string;
  readonly scrollHeight: number;
  readonly clientHeight: number;
  readonly hidden: string;
}

export interface ReflowReport {
  readonly scrollWidth: number;
  readonly clientWidth: number;
  readonly offenders: readonly OverflowOffender[];
  readonly internalScrollers: readonly InternalScroller[];
  readonly collapsedScrollers: readonly CollapsedScroller[];
}

/**
 * Everything a pointer or the keyboard can operate. Shared by both probes so
 * "an inaccessible action" and "an undersized target" are drawn from one
 * definition of what an action is.
 */
const INTERACTIVE_SELECTOR =
  'a[href],area[href],button,input:not([type="hidden"]),select,textarea,summary,' +
  '[role="button"],[role="link"],[role="tab"],[role="checkbox"],[role="radio"],' +
  '[role="switch"],[role="menuitem"],[role="menuitemcheckbox"],[role="menuitemradio"],' +
  '[role="option"],[tabindex]';

/** Shared in-page helpers, prepended to both probe sources. */
const PROBE_PRELUDE = `
  var INTERACTIVE = ${JSON.stringify(INTERACTIVE_SELECTOR)};
  var describe = function (el) {
    if (!el) return '(none)';
    var out = el.tagName.toLowerCase();
    if (el.id) out += '#' + el.id;
    for (var a = 0; a < el.attributes.length; a += 1) {
      var attr = el.attributes[a];
      if (attr.name.indexOf('data-') === 0) { out += '[' + attr.name + ']'; break; }
    }
    var cls = typeof el.className === 'string' ? el.className.trim() : '';
    if (cls !== '') out += '.' + cls.split(/\\s+/).slice(0, 3).join('.');
    return out.slice(0, 120);
  };
  var ownText = function (el) {
    var out = '';
    for (var c = 0; c < el.childNodes.length; c += 1) {
      if (el.childNodes[c].nodeType === 3) out += el.childNodes[c].nodeValue || '';
    }
    return out.trim().replace(/\\s+/g, ' ');
  };
`;

/**
 * Measured inside the page, as source text rather than a function: `tsx`
 * compiles this file with esbuild's `keepNames`, whose `__name` helper does
 * not exist in the page, so a named callback handed to `page.evaluate` throws
 * on arrival.
 *
 * Decorative geometry is deliberately excluded. A glow layer or a gradient
 * that bleeds past the right edge carries no truth state, and listing those
 * would bury the rows, figures and controls that do, so an element is only an
 * offender if it holds its own text or is something a person operates or
 * reads.
 */
export const REFLOW_PROBE = `(function () {${PROBE_PRELUDE}
  var doc = document.documentElement;
  var limit = doc.clientWidth;
  var offenders = [];
  var scrollers = [];
  var collapsed = [];
  var seen = {};
  var all = document.body ? document.body.querySelectorAll('*') : [];
  for (var i = 0; i < all.length; i += 1) {
    var el = all[i];
    var style = getComputedStyle(el);
    if (style.display === 'none' || style.visibility === 'hidden') continue;
    // Checked BEFORE the size guard below, because a collapsed scroller is
    // precisely an element with no painted height, and skipping it as "not
    // rendered" is how it stayed invisible to this probe in the first place.
    if ((style.overflowY === 'auto' || style.overflowY === 'scroll') &&
        el.clientHeight < 1 && el.scrollHeight > 1) {
      var trapped = (el.textContent || '').trim().replace(/\\s+/g, ' ');
      if (trapped !== '') {
        collapsed.push({
          selector: describe(el),
          scrollHeight: el.scrollHeight,
          clientHeight: el.clientHeight,
          hidden: trapped.slice(0, 80),
        });
      }
    }
    var rect = el.getBoundingClientRect();
    if (rect.width < 1 || rect.height < 1) continue;
    if ((style.overflowX === 'auto' || style.overflowX === 'scroll') &&
        el.scrollWidth > el.clientWidth + 1) {
      scrollers.push({
        selector: describe(el),
        tag: el.tagName.toLowerCase(),
        label: (el.getAttribute('aria-label') || el.getAttribute('aria-labelledby') ||
                el.getAttribute('title') || '').trim(),
        role: el.getAttribute('role') || '',
      });
    }
    if (rect.right <= limit + 1) continue;
    var carriesContent =
      ownText(el) !== '' || el.matches(INTERACTIVE) ||
      el.tagName === 'IMG' || el.tagName === 'CANVAS' || el.tagName === 'SVG';
    if (!carriesContent) continue;
    var clipper = null;
    var reachable = false;
    for (var up = el.parentElement; up; up = up.parentElement) {
      var ov = getComputedStyle(up).overflowX;
      if (ov === 'auto' || ov === 'scroll') { reachable = true; break; }
      if (ov === 'hidden' || ov === 'clip') { clipper = up; break; }
    }
    if (reachable) continue;
    var kind = clipper ? 'clipped' : 'page-overflow';
    var key = describe(el) + '|' + kind;
    if (seen[key]) continue;
    seen[key] = 1;
    offenders.push({
      kind: kind,
      selector: describe(el),
      clipper: clipper ? describe(clipper) : '',
      right: Math.round(rect.right),
      text: ownText(el).slice(0, 80),
    });
  }
  return {
    scrollWidth: doc.scrollWidth,
    clientWidth: limit,
    offenders: offenders.slice(0, 12),
    internalScrollers: scrollers.slice(0, 12),
    collapsedScrollers: collapsed.slice(0, 12),
  };
})()`;

/**
 * The plan's own sentence, as a predicate: at 320 CSS pixels and at 400% zoom
 * the document itself must not scroll sideways.
 *
 * Returns the failures rather than throwing, so one scan can report a reflow
 * failure, an undersized target and an axe violation together instead of
 * hiding two of them behind whichever threw first.
 */
export function reflowFailures(report: ReflowReport, tag: string): string[] {
  if (report.scrollWidth <= report.clientWidth + 1) return [];
  const worst = report.offenders
    .filter((o) => o.kind === 'page-overflow')
    .slice(0, 4)
    .map((o) => `${o.selector} reaches ${o.right}px${o.text === '' ? '' : ` (${o.text})`}`);
  return [
    `${tag}: the page scrolls horizontally. Document scrollWidth ${report.scrollWidth} > ` +
      `clientWidth ${report.clientWidth}. ` +
      (worst.length === 0
        ? 'No single content element reaches past the edge, so the width comes from a ' +
          'decorative or zero-text box.'
        : `Widest content past the edge: ${worst.join('; ')}`),
  ];
}

/**
 * The other half of the plan's sentence: no clipped truth state at 320 CSS
 * pixels or 400% zoom.
 *
 * Only collapsed scrollers are gated. `OverflowOffender.kind === 'clipped'`
 * stays a diagnostic because deciding whether an element that reaches past the
 * viewport inside a hidden ancestor has actually lost anything takes judgement;
 * a scroller with content and zero height has lost all of it, measurably.
 */
export function clippedContentFailures(report: ReflowReport, tag: string): string[] {
  return report.collapsedScrollers.map(
    (s) =>
      `${tag}: ${s.selector} scrolls vertically but has collapsed to ${s.clientHeight}px tall ` +
      `while holding ${s.scrollHeight}px of content, so none of it can be reached and no ` +
      `scrollbar offers to. Trapped: "${s.hidden}"`,
  );
}

/**
 * A side rail scrolls vertically and never sideways, at any width.
 *
 * Rails are the readouts beside a workspace's main field. Unlike the code,
 * table and graph regions the plan lets scroll internally, nothing in a rail is
 * meant to be panned to: when one scrolls sideways, some child sized itself
 * to its unwrapped content and every row after it is clipped at the rail's
 * right edge. The Brain registry rail did exactly this on a registry with one
 * long checkout path, by 16 CSS pixels, with no page-level overflow to report.
 */
export function sidewaysRailFailures(report: ReflowReport, tag: string): string[] {
  return report.internalScrollers
    .filter((s) => s.tag === 'aside')
    .map(
      (s) =>
        `${tag}: the side rail ${s.label === '' ? s.selector : `"${s.label}"`} scrolls ` +
        `horizontally (scrollWidth > clientWidth), so its rows are clipped at the right edge`,
    );
}

/* ==========================================================================
 * Touch targets.
 * ========================================================================== */

export interface UndersizedTarget {
  readonly selector: string;
  readonly name: string;
  readonly width: number;
  readonly height: number;
}

export interface TouchTargetReport {
  readonly examined: number;
  readonly exempt: number;
  readonly undersized: readonly UndersizedTarget[];
}

/**
 * Every operable target's rendered box.
 *
 * Exemptions, and why each is not a loophole:
 *
 *   - `[data-axe-canary]`: markup this harness plants to prove the scan is
 *     live. It is 24px square on purpose and is not product UI.
 *   - disabled / `aria-disabled`: not operable, so there is no target.
 *   - `tabindex="-1"` on a non-native control: programmatically focusable, not
 *     in the tab order and not a pointer target.
 *   - not rendered: `display: none`, `visibility: hidden`, or a zero box.
 *   - the visually-hidden idiom. A 1px box that is also clipped, which is how
 *     `sr-only` parks the skip link off-screen until it takes focus. It is not
 *     a pointer target at 1x1; it is a keyboard affordance that becomes
 *     full-size on focus. Both halves are required, so a genuinely tiny
 *     unclipped control is still reported.
 *   - WCAG 2.5.5's inline exception: a link inside a sentence is sized by the
 *     line box around it and cannot be enlarged without breaking the text.
 *     Detected as `display: inline` with sibling text in the same parent.
 *
 * Known limit, stated rather than papered over: the measurement is the
 * element's border box. A control that enlarges its hit area with an absolutely
 * positioned pseudo-element reads as undersized here even though a pointer can
 * reach the larger area. No component in this dashboard uses that pattern
 * today; if one adopts it, this probe needs the pseudo-element box, not a
 * suppression.
 *
 * Reading a failure: CSS pixels, not `rem`. `tailwind.css` sets
 * `html { font-size: 14px }`, and Tailwind's spacing scale is rem-based, so a
 * control written `min-h-11` or `size-11`, chosen because 11 x 4px reads as
 * 44, lands at 38.5 CSS pixels. Several offenders were sized for this
 * threshold and still miss it for that reason, so check the computed box before
 * concluding a utility class is wrong.
 */
export const TOUCH_TARGET_PROBE = `(function () {${PROBE_PRELUDE}
  var MIN = ${MIN_TOUCH_TARGET_PX};
  var NATIVE = 'a[href],area[href],button,input,select,textarea,summary';
  var nodes = document.querySelectorAll(INTERACTIVE);
  var examined = 0;
  var exempt = 0;
  var undersized = [];
  var seen = {};
  for (var i = 0; i < nodes.length; i += 1) {
    var el = nodes[i];
    if (el.closest('[data-axe-canary]') !== null) { exempt += 1; continue; }
    if (el.disabled === true || el.getAttribute('aria-disabled') === 'true') { exempt += 1; continue; }
    var ti = el.getAttribute('tabindex');
    if (ti !== null && Number(ti) < 0 && !el.matches(NATIVE)) { exempt += 1; continue; }
    var style = getComputedStyle(el);
    if (style.display === 'none' || style.visibility === 'hidden') { exempt += 1; continue; }
    var rect = el.getBoundingClientRect();
    if (rect.width < 1 || rect.height < 1) { exempt += 1; continue; }
    var clipped = (style.clipPath !== 'none' && style.clipPath !== '') ||
      (style.clip !== 'auto' && style.clip !== '');
    if (rect.width <= 1 && rect.height <= 1 && clipped) { exempt += 1; continue; }
    if (style.display === 'inline' && el.parentElement !== null &&
        ownText(el.parentElement) !== '') { exempt += 1; continue; }
    examined += 1;
    if (rect.width >= MIN - 0.5 && rect.height >= MIN - 0.5) continue;
    var selector = describe(el);
    if (seen[selector]) continue;
    seen[selector] = 1;
    undersized.push({
      selector: selector,
      name: (el.getAttribute('aria-label') || el.textContent || '')
        .trim().replace(/\\s+/g, ' ').slice(0, 60),
      width: Math.round(rect.width * 10) / 10,
      height: Math.round(rect.height * 10) / 10,
    });
  }
  return { examined: examined, exempt: exempt, undersized: undersized.slice(0, 20) };
})()`;

/**
 * A probe that examined nothing proved nothing. The same failure mode the
 * visibility sweep was rewritten to close. Every audited surface renders a nav
 * rail full of links, so zero operable targets means the probe did not run
 * against the page it was aimed at.
 */
export function touchTargetFailures(report: TouchTargetReport, tag: string): string[] {
  if (report.examined === 0) {
    return [
      `${tag}: the touch-target probe found no operable target at all, so it ` +
        `proved nothing about this capture (${report.exempt} exempt)`,
    ];
  }
  if (report.undersized.length === 0) return [];
  const listed = report.undersized
    .slice(0, 6)
    .map((t) => `${t.selector} ${t.width}x${t.height}${t.name === '' ? '' : ` "${t.name}"`}`);
  return [
    `${tag}: ${report.undersized.length}/${report.examined} operable targets are smaller than ` +
      `${MIN_TOUCH_TARGET_PX}x${MIN_TOUCH_TARGET_PX} CSS pixels: ${listed.join('; ')}`,
  ];
}
