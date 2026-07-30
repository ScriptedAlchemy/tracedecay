/**
 * The Plan 11 responsive/zoom/media matrix, and the two measurements the plan
 * names that no axe rule performs.
 *
 * Split out of `axe-harness.ts` for the same reason `visibility.ts` was: the
 * probes are pure DOM measurement and the pass/fail rules are pure functions
 * over their reports, so neither needs a Playwright import and both can be
 * driven against a real DOM outside the browser harness.
 *
 * What the plan mandates (11-dashboard-frontend.md, "Responsive,
 * accessibility, performance, and usability gates"):
 *
 *   "Automated tests cover 320x568, 390x844, 768x1024, 1024x768, 1280x720,
 *    and 1440x900 CSS pixels, 200% and 400% zoom, prefers-reduced-motion,
 *    prefers-contrast: more, and forced colors. At 320 pixels and 400% zoom
 *    there is no page-level horizontal scroll, clipped truth state, lost
 *    scope/provenance, or inaccessible action; labeled code/table/graph
 *    regions may scroll internally. Touch targets are at least 44x44 CSS
 *    pixels."
 *
 * ZOOM IS A CSS VIEWPORT, NOT `deviceScaleFactor`. Playwright's
 * `deviceScaleFactor` raises rasterization density and leaves layout at the
 * unzoomed CSS size, so a page captured at `deviceScaleFactor: 4` reflows
 * exactly as it did at 1x — which is the one thing WCAG 1.4.10 is about. Real
 * browser zoom shrinks the CSS viewport: 400% zoom of a 1280x720 window is a
 * 320x180 CSS viewport, and that is what is emulated here. It is also why the
 * 400% row and the 320px row test the same reflow width from opposite
 * directions.
 */

/** WCAG 2.5.5 / Plan 11 minimum, in CSS pixels. */
export const MIN_TOUCH_TARGET_PX = 44;

/** The window this harness models browser zoom against. */
const ZOOM_BASE = { width: 1280, height: 720 } as const;

export interface Viewport {
  /** Stable id: file names, findings keys, console tags. */
  readonly id: string;
  readonly width: number;
  readonly height: number;
  /** Browser zoom this CSS viewport models, as a percentage. */
  readonly zoom: number;
  /**
   * Where the plan forbids page-level horizontal scroll outright — "at 320
   * pixels and 400% zoom". Elsewhere the measurement is still taken and
   * reported; only these two gate the run.
   */
  readonly reflowGated: boolean;
}

function zoomed(zoom: number): Viewport {
  const scale = zoom / 100;
  return {
    id: `zoom${zoom}`,
    width: Math.round(ZOOM_BASE.width / scale),
    height: Math.round(ZOOM_BASE.height / scale),
    zoom,
    reflowGated: zoom >= 400,
  };
}

function device(width: number, height: number, reflowGated = false): Viewport {
  return { id: `${width}x${height}`, width, height, zoom: 100, reflowGated };
}

/**
 * The three the full scenario sweep runs, in both themes.
 *
 * These are the plan's own sizes, not the harness's previous 320/768/1440 at a
 * uniform height of 900 — a 320-wide viewport 900 tall is a phone width with a
 * tablet's vertical room, which hides exactly the "does the truth state fit"
 * question the narrow row exists to ask.
 */
export const SHOWCASE_VIEWPORTS: readonly Viewport[] = [
  device(320, 568, true),
  device(768, 1024),
  device(1440, 900),
];

/** The rest of the plan's matrix, swept over a representative subset. */
export const MATRIX_VIEWPORTS: readonly Viewport[] = [
  device(390, 844),
  device(1024, 768),
  device(1280, 720),
  zoomed(200),
  zoomed(400),
];

export const PLAN_VIEWPORTS: readonly Viewport[] = [...SHOWCASE_VIEWPORTS, ...MATRIX_VIEWPORTS];

/**
 * `reduced-motion` is the harness default and the only mode the full sweep
 * runs; the other two are emulated live, without reloading, because both
 * resolve through CSS media queries.
 */
export type MediaMode = 'reduced-motion' | 'contrast-more' | 'forced-colors';

export const MEDIA_MODES: readonly MediaMode[] = ['reduced-motion', 'contrast-more', 'forced-colors'];

export type Theme = 'light' | 'dark';

export interface Combination {
  readonly viewport: Viewport;
  readonly theme: Theme;
  readonly media: MediaMode;
}

/**
 * Everything the matrix subset is scanned at, beyond what the full sweep
 * already covers. Thirty combinations per subset scenario:
 *
 *   - 10: the five viewports the full sweep does not reach (390x844, 1024x768,
 *     1280x720, and the two zoom levels), in both themes, under reduced motion;
 *   - 16: all eight plan viewports under `prefers-contrast: more`, and all
 *     eight under forced colors, in light;
 *   -  4: a dark spot check of both media modes at 1440x900 and 390x844, the
 *     widest and the narrowest, where a raised-contrast or forced palette is
 *     likeliest to collide with the dark token set.
 *
 * Forced colors replaces the palette wholesale, so sweeping it through both
 * themes at every size would buy near-identical renders; the dark rows are the
 * spot check rather than a full second pass. That is a deliberate cost
 * decision, recorded here rather than left implicit in a nested loop.
 */
export const RESPONSIVE_MATRIX: readonly Combination[] = [
  ...MATRIX_VIEWPORTS.flatMap((viewport): Combination[] => [
    { viewport, theme: 'light', media: 'reduced-motion' },
    { viewport, theme: 'dark', media: 'reduced-motion' },
  ]),
  ...PLAN_VIEWPORTS.flatMap((viewport): Combination[] => [
    { viewport, theme: 'light', media: 'contrast-more' },
    { viewport, theme: 'light', media: 'forced-colors' },
  ]),
  ...[device(1440, 900), device(390, 844)].flatMap((viewport): Combination[] => [
    { viewport, theme: 'dark', media: 'contrast-more' },
    { viewport, theme: 'dark', media: 'forced-colors' },
  ]),
];

export function combinationTag(c: Combination): string {
  return `${c.theme}/${c.viewport.id}/${c.media}`;
}

/* ==========================================================================
 * Reflow: page-level horizontal scroll, and what is running past the edge.
 * ========================================================================== */

export interface OverflowOffender {
  /**
   * `page-overflow` widens the document itself. `clipped` is content that runs
   * past the viewport inside an ancestor that hides its horizontal overflow —
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
 * name — the plan permits "labeled code/table/graph regions" to do this. */
export interface InternalScroller {
  readonly selector: string;
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
 * the overflow heuristics it needs no judgement — a scroller holding text with
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
 * would bury the rows, figures and controls that do — so an element is only an
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
    `${tag}: the page scrolls horizontally — document scrollWidth ${report.scrollWidth} > ` +
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

/* ==========================================================================
 * Forced colors: who opts out.
 * ========================================================================== */

export interface ForcedColorsOptOut {
  readonly selector: string;
  readonly color: string;
  readonly background: string;
  readonly text: string;
}

/**
 * Elements that decline the forced palette.
 *
 * This exists because axe's `color-contrast` rule cannot be trusted here, and
 * saying so without replacing it would be exactly the kind of quietly narrowed
 * check this harness was rebuilt to prevent. Measured against axe-core 4.12.1
 * under Playwright's `forcedColors: 'active'`:
 *
 *   - Chromium forces the palette correctly. Live `getComputedStyle` on the
 *     reported nodes returns `rgb(0, 0, 0)` text and `rgb(255, 255, 255)`
 *     backgrounds, and the captures are legible.
 *   - Axe nonetheless reports the AUTHORED foreground (`#ecedee`, the dark
 *     theme's text token) against the FORCED white background, so it scores
 *     143 serious contrast failures in dark and none in light — a verdict that
 *     tracks which theme is selected rather than anything a user sees.
 *   - axe-core 4.12.1 contains no `forced-colors` or high-contrast handling of
 *     any kind, so this is not a detection it is opting out of; it simply does
 *     not know the mode exists.
 *
 * So `color-contrast` is disabled in that one mode, and the real risk it was
 * standing in for is measured directly instead: forced colors can only fail a
 * reader where an element opts out with `forced-color-adjust: none`. Opting out
 * is legitimate where colour IS the information — a swatch, a chart series — so
 * this reports rather than gates, and the report is what makes an illegitimate
 * opt-out visible.
 */
export const FORCED_COLORS_PROBE = `(function () {${PROBE_PRELUDE}
  var out = [];
  var seen = {};
  var all = document.body ? document.body.querySelectorAll('*') : [];
  for (var i = 0; i < all.length; i += 1) {
    var el = all[i];
    var style = getComputedStyle(el);
    if (style.forcedColorAdjust !== 'none') continue;
    if (style.display === 'none' || style.visibility === 'hidden') continue;
    var selector = describe(el);
    if (seen[selector]) continue;
    seen[selector] = 1;
    out.push({
      selector: selector,
      color: style.color,
      background: style.backgroundColor,
      text: ownText(el).slice(0, 60),
    });
  }
  return out.slice(0, 20);
})()`;

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
 *   - the visually-hidden idiom — a 1px box that is also clipped, which is how
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
 * control written `min-h-11` or `size-11` — chosen because 11 x 4px reads as
 * 44 — lands at 38.5 CSS pixels. Several offenders were sized for this
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
 * A probe that examined nothing proved nothing — the same failure mode the
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

/* ==========================================================================
 * The workspace header's one line: does anything on it render outside the box.
 * ========================================================================== */

export interface HeaderOverflowChild {
  readonly header: string;
  readonly selector: string;
  readonly text: string;
  /**
   * Whether this offender fails the build.
   *
   * Only state chips are gated. A chip is a fixed-size indicator whose entire
   * job is to be read at a glance, so one rendering outside its header is a
   * defect with no judgement required — and it is the regression this check
   * exists for. Everything else on the line is reported instead, because the
   * full 564-scan sweep found a pre-existing 494.8px snapshot/revision strip in
   * the `/settings` header rendering up to 356.9px outside it (276px of that
   * off-screen) at 320 and 390 CSS px. That is the same defect class on a
   * surface this change does not own, and gating it here would convert a
   * hand-off into a red build for someone else's bug. It is printed with its
   * measurements so it gets fixed rather than forgotten.
   */
  readonly gated: boolean;
  /** CSS pixels past each edge of the header's padding box. 0 means inside. */
  readonly pastRight: number;
  readonly pastLeft: number;
  readonly pastTop: number;
  readonly pastBottom: number;
  readonly width: number;
  readonly height: number;
  readonly headerWidth: number;
  readonly contentWidth: number;
  /** Distance from the child's right edge to the viewport's. Negative means the
   * child is painted off-screen entirely. */
  readonly viewportSlack: number;
}

export interface HeaderBoxReport {
  /** Workspace headers on the page. Zero is legitimate — not every route
   * renders one — which is why this is reported rather than asserted. */
  readonly headers: number;
  readonly examined: number;
  readonly offenders: readonly HeaderOverflowChild[];
}

/**
 * Every rendered child of a workspace header, measured against that header's
 * padding box.
 *
 * This exists because `document.scrollWidth` cannot see the defect it is named
 * for. The state chip on `/work` rendered 19 CSS pixels outside its header at
 * 320px and at 400% zoom — bezel sheared, label flush against the screen edge —
 * through 78 clean scans, because the shell clips instead of scrolling, so the
 * document never widened and the reflow gate never fired. The two heuristics
 * either side of it missed it for reasons worth recording, since both are still
 * in place:
 *
 *   - `REFLOW_PROBE` skips anything whose `rect.right <= clientWidth + 1`, and
 *     the chip's LABEL ended at exactly 320 of 320. One pixel of luck.
 *   - It also skips elements carrying no text of their own, and the chip's text
 *     lives in child spans, so the chip's own box was never a candidate.
 *
 * Measuring against the container instead of the viewport closes both gaps: a
 * child that overruns its header is a defect at any width, whether or not the
 * viewport happens to be wider, and it needs no judgement about what the
 * element carries.
 *
 * Out-of-flow children are excluded. An absolutely positioned overlay does not
 * spend the line's width budget and is routinely meant to escape it; only
 * elements laid out IN the header are held to the header's box.
 */
export const HEADER_BOX_PROBE = `(function () {${PROBE_PRELUDE}
  var headers = document.querySelectorAll('[data-workspace-header]');
  var examined = 0;
  var offenders = [];
  var seen = {};
  var round = function (n) { return Math.round(n * 10) / 10; };
  for (var h = 0; h < headers.length; h += 1) {
    var header = headers[h];
    var hs = getComputedStyle(header);
    if (hs.display === 'none' || hs.visibility === 'hidden') continue;
    var hb = header.getBoundingClientRect();
    var innerLeft = hb.left + (parseFloat(hs.paddingLeft) || 0);
    var innerRight = hb.right - (parseFloat(hs.paddingRight) || 0);
    for (var i = 0; i < header.children.length; i += 1) {
      var el = header.children[i];
      var style = getComputedStyle(el);
      if (style.display === 'none' || style.visibility === 'hidden') continue;
      if (style.position === 'absolute' || style.position === 'fixed') continue;
      var r = el.getBoundingClientRect();
      if (r.width < 1 && r.height < 1) continue;
      examined += 1;
      var pastRight = r.right - innerRight;
      var pastLeft = innerLeft - r.left;
      var pastTop = hb.top - r.top;
      var pastBottom = r.bottom - hb.bottom;
      if (pastRight <= 0.5 && pastLeft <= 0.5 && pastTop <= 0.5 && pastBottom <= 0.5) continue;
      var key = describe(el);
      if (seen[key]) continue;
      seen[key] = 1;
      offenders.push({
        header: describe(header),
        selector: key,
        gated: el.matches('[data-state]'),
        text: (el.textContent || '').trim().replace(/\\s+/g, ' ').slice(0, 60),
        pastRight: round(Math.max(0, pastRight)),
        pastLeft: round(Math.max(0, pastLeft)),
        pastTop: round(Math.max(0, pastTop)),
        pastBottom: round(Math.max(0, pastBottom)),
        width: round(r.width),
        height: round(r.height),
        headerWidth: Math.round(hb.width),
        contentWidth: Math.round(innerRight - innerLeft),
        viewportSlack: round(window.innerWidth - r.right),
      });
    }
  }
  return { headers: headers.length, examined: examined, offenders: offenders.slice(0, 12) };
})()`;

/**
 * Gated at every width, unlike reflow.
 *
 * Reflow is gated only at 320 and 400% zoom because the plan's sentence is
 * about those two sizes. This is a different claim with no width in it: content
 * placed inside a container must render inside that container. A wide layout
 * that pushes its own header content out of the box is the same defect with a
 * bigger budget, so narrowing this to the reflow-gated viewports would only
 * hide the easier half.
 *
 * Returns the gated offenders only. `HeaderOverflowChild.gated` carries why the
 * rest are reported instead; `axe-report.ts` prints them either way, so nothing
 * measured here goes unsaid.
 */
export function headerBoxFailures(report: HeaderBoxReport, tag: string): string[] {
  return report.offenders
    .filter((o) => o.gated)
    .map((o) => {
      const past = [
        o.pastRight > 0 ? `${o.pastRight}px past its right edge` : '',
        o.pastLeft > 0 ? `${o.pastLeft}px past its left edge` : '',
        o.pastTop > 0 ? `${o.pastTop}px above it` : '',
        o.pastBottom > 0 ? `${o.pastBottom}px below it` : '',
      ].filter((part) => part !== '');
      return (
        `${tag}: ${o.selector} renders outside ${o.header} — ${past.join(', ')}. ` +
        `The child is ${o.width}x${o.height} CSS px and the header offers ` +
        `${o.contentWidth}px of content box within ${o.headerWidth}px` +
        (o.viewportSlack < 0 ? `; ${round1(-o.viewportSlack)}px of it is off-screen` : '') +
        (o.text === '' ? '' : `. Carrying: "${o.text}"`)
      );
    });
}

function round1(n: number): number {
  return Math.round(n * 10) / 10;
}
