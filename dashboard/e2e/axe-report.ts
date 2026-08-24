/**
 * What the axe gate wrote down, and what it says out loud.
 *
 * Split from `axe-harness.ts` so the engine is the thing that drives a browser
 * and this is the thing that turns records into a verdict — the same division
 * `visibility.ts` and `responsive.ts` already follow. Everything here is pure:
 * records in, artifact and console lines out, no Playwright, no I/O beyond the
 * one file the caller asks for.
 *
 * The reason it is worth its own module is the aggregation. A widened matrix
 * measures the same nav rail at three hundred and ninety combinations, so a
 * raw per-scan list of undersized controls is four hundred repetitions of the
 * same eighteen defects — technically complete and useless to act on. Every
 * collection below is keyed by the OFFENDER, with a count and a couple of
 * example combinations, so the report is as long as the problem rather than as
 * long as the run.
 */
import { writeFileSync } from 'node:fs';
import {
  MEDIA_MODES,
  MIN_TOUCH_TARGET_PX,
  PLAN_VIEWPORTS,
  type ForcedColorsOptOut,
  type HeaderBoxReport,
  type HeaderOverflowChild,
  type MediaMode,
  type ReflowReport,
  type Theme,
  type TouchTargetReport,
} from './responsive.ts';

export interface Violation {
  id: string;
  impact: string;
  nodes: string[];
  help: string;
}

export interface ShotRecord {
  theme: Theme;
  /** The plan viewport id, e.g. `320x568` or `zoom400`. */
  viewport: string;
  width: number;
  height: number;
  /** Browser zoom this CSS viewport models, as a percentage. */
  zoom: number;
  media: MediaMode;
  file: string;
  violations: Violation[];
  /** Violations this scenario planted on purpose, recorded so the artifact
   * shows what the engine detected rather than only what it did not. */
  seeded?: Violation[];
  /** Document width against viewport width, plus what ran past the edge. */
  reflow?: ReflowReport;
  /** Operable targets measured, and those under 44x44 CSS pixels. */
  targets?: TouchTargetReport;
  /** Workspace-header children measured against their header's padding box. */
  headerBox?: HeaderBoxReport;
  /** Forced-colors mode only: elements declining the forced palette. */
  forcedColorOptOuts?: ForcedColorsOptOut[];
  /** Rules switched off for this mode, so the artifact never implies a scan
   * covered something it did not. */
  disabledRules?: string[];
  error?: string;
}

export interface ScenarioRecord {
  id: string;
  route: string;
  proves: string;
  assertion: 'passed' | string;
  matrix: boolean;
  shots: ShotRecord[];
}

/**
 * A plan assertion that failed, kept out of the per-scan `try` so one scan can
 * report a reflow failure, an undersized target and an axe violation together
 * rather than hiding two of them behind whichever threw first.
 */
export interface PlanFailure {
  scenario: string;
  route: string;
  tag: string;
  check: 'horizontal-scroll' | 'clipped-content' | 'touch-target' | 'header-overflow';
  detail: string;
}

export interface RunTotals {
  label: string;
  servedFrom: string;
  records: ScenarioRecord[];
  planFailures: PlanFailure[];
  pageErrors: string[];
  assertionFailures: number;
  themes: readonly Theme[];
}

/** Where a finding was seen, in the form a reader can navigate back to. */
function at(record: ScenarioRecord, shot: ShotRecord): string {
  return `${record.route}@${shot.viewport}/${shot.theme}/${shot.media}`;
}

export function summarise(run: RunTotals): Record<string, unknown> {
  const byViewport: Record<string, number> = {};
  const byRule: Record<string, number> = {};
  const byMedia: Record<string, number> = {};
  const seededByRule: Record<string, number> = {};
  let totalViolations = 0;
  let seededDetections = 0;
  const undersized = new Map<
    string,
    { width: number; height: number; name: string; scans: number; where: string[] }
  >();
  const overflowing = new Map<string, { kind: string; clipper: string; where: string[] }>();
  const unlabelledScrollers = new Map<string, string[]>();
  const optOuts = new Map<string, ForcedColorsOptOut & { where: string[] }>();
  const headerOverflow = new Map<
    string,
    HeaderOverflowChild & { scans: number; where: string[] }
  >();
  // Counted so a clean header verdict is readable: zero offenders out of zero
  // children measured proves nothing, and this gate has already been fooled
  // once by a measurement that could not see the defect.
  let headerScans = 0;
  let headerChildrenExamined = 0;

  for (const record of run.records) {
    for (const shot of record.shots) {
      const where = at(record, shot);
      totalViolations += shot.violations.length;
      byViewport[`${shot.theme}__${shot.viewport}__${shot.media}`] =
        (byViewport[`${shot.theme}__${shot.viewport}__${shot.media}`] ?? 0) +
        shot.violations.length;
      byMedia[shot.media] = (byMedia[shot.media] ?? 0) + shot.violations.length;
      for (const v of shot.violations) byRule[v.id] = (byRule[v.id] ?? 0) + 1;
      for (const v of shot.seeded ?? []) {
        seededDetections += 1;
        seededByRule[v.id] = (seededByRule[v.id] ?? 0) + 1;
      }
      for (const t of shot.targets?.undersized ?? []) {
        const seen = undersized.get(t.selector);
        if (seen === undefined) {
          undersized.set(t.selector, { ...t, scans: 1, where: [where] });
        } else {
          seen.scans += 1;
          if (seen.where.length < 4) seen.where.push(where);
        }
      }
      for (const o of shot.reflow?.offenders ?? []) {
        const seen = overflowing.get(o.selector);
        if (seen === undefined) {
          overflowing.set(o.selector, { kind: o.kind, clipper: o.clipper, where: [where] });
        } else if (seen.where.length < 4) seen.where.push(where);
      }
      for (const scroller of shot.reflow?.internalScrollers ?? []) {
        if (scroller.label !== '') continue;
        const seen = unlabelledScrollers.get(scroller.selector) ?? [];
        if (seen.length < 4) unlabelledScrollers.set(scroller.selector, [...seen, where]);
        else unlabelledScrollers.set(scroller.selector, seen);
      }
      for (const o of shot.forcedColorOptOuts ?? []) {
        const seen = optOuts.get(o.selector);
        if (seen === undefined) optOuts.set(o.selector, { ...o, where: [where] });
        else if (seen.where.length < 4) seen.where.push(where);
      }
      if (shot.headerBox !== undefined) {
        if (shot.headerBox.headers > 0) headerScans += 1;
        headerChildrenExamined += shot.headerBox.examined;
        for (const o of shot.headerBox.offenders) {
          const seen = headerOverflow.get(o.selector);
          if (seen === undefined) {
            headerOverflow.set(o.selector, { ...o, scans: 1, where: [where] });
          } else {
            seen.scans += 1;
            if (seen.where.length < 4) seen.where.push(where);
          }
        }
      }
    }
  }

  return {
    generatedAt: new Date().toISOString(),
    label: run.label,
    servedFrom: run.servedFrom,
    // The Plan 11 matrix, recorded so the artifact states what was covered
    // rather than leaving it to be reconstructed from the file names.
    viewports: PLAN_VIEWPORTS,
    mediaModes: MEDIA_MODES,
    themes: run.themes,
    minTouchTargetPx: MIN_TOUCH_TARGET_PX,
    axeTags: ['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa'],
    scenarioCount: run.records.length,
    matrixScenarios: run.records.filter((r) => r.matrix).map((r) => r.id),
    shotCount: run.records.reduce((n, r) => n + r.shots.length, 0),
    totalViolations,
    assertionFailures: run.assertionFailures,
    pageErrors: run.pageErrors,
    violationsByViewport: byViewport,
    violationsByMedia: byMedia,
    violationsByRule: byRule,
    // The engine's own receipt. `totalViolations: 0` is only readable as a
    // clean bill of health alongside a non-empty `seededDetectionsByRule`.
    seededDetections,
    seededDetectionsByRule: seededByRule,
    planFailures: run.planFailures,
    undersizedTargets: [...undersized.entries()].map(([selector, v]) => ({ selector, ...v })),
    // Every child measured out of its header's box. "Inside its own container"
    // needs no judgement about what the element carries, which is exactly what
    // kept the reflow heuristics from being able to fail on it — and why every
    // one of these now fails the build rather than only the state chips.
    headerScans,
    headerChildrenExamined,
    headerOverflowChildren: [...headerOverflow.values()],
    // Diagnostics rather than gates. `clipped` content is a strong signal of
    // the plan's "clipped truth state", and an unlabelled internal scroller is
    // outside the plan's "labeled ... regions may scroll internally" licence —
    // but both are detected by heuristic, and only the document-width
    // measurement is precise enough to fail a build on.
    overflowingElements: [...overflowing.entries()].map(([selector, v]) => ({ selector, ...v })),
    unlabelledInternalScrollers: [...unlabelledScrollers.entries()].map(([selector, where]) => ({
      selector,
      where,
    })),
    // What forced colors could actually break, measured directly, because axe's
    // `color-contrast` is disabled in that mode. `responsive.ts` carries the
    // measurements behind that decision.
    forcedColorsRuleDisabled: 'color-contrast',
    forcedColorOptOuts: [...optOuts.values()],
    scenarios: run.records,
  };
}

/** Write the artifact, print the verdict, and answer whether the run failed. */
export function reportRun(run: RunTotals, findingsPath: string): boolean {
  const findings = summarise(run);
  writeFileSync(findingsPath, `${JSON.stringify(findings, null, 2)}\n`);

  const n = (key: string): number => (findings[key] as number) ?? 0;
  const list = <T>(key: string): T[] => (findings[key] as T[]) ?? [];
  const reflowFailed = run.planFailures.filter((f) => f.check === 'horizontal-scroll');
  const clippedFailed = run.planFailures.filter((f) => f.check === 'clipped-content');
  const targetFailed = run.planFailures.filter((f) => f.check === 'touch-target');
  const headerFailed = run.planFailures.filter((f) => f.check === 'header-overflow');
  const undersized = list<{
    selector: string;
    width: number;
    height: number;
    name: string;
    scans: number;
    where: string[];
  }>('undersizedTargets');

  console.log('');
  console.log('[axe] ===== summary =====');
  console.log(`[axe] scenarios=${n('scenarioCount')} shots=${n('shotCount')}`);
  console.log(
    `[axe] axe violations=${n('totalViolations')} ` +
      `byViewport=${JSON.stringify(findings['violationsByViewport'])}`,
  );
  console.log(
    `[axe] byMedia=${JSON.stringify(findings['violationsByMedia'])} ` +
      `byRule=${JSON.stringify(findings['violationsByRule'])}`,
  );
  console.log(
    `[axe] seeded violations DETECTED=${n('seededDetections')} ` +
      `byRule=${JSON.stringify(findings['seededDetectionsByRule'])} — ` +
      `this is what makes the zero above readable`,
  );
  console.log(
    `[axe] plan reflow (320 CSS px and 400% zoom): ` +
      `${reflowFailed.length} scan(s) scrolled horizontally`,
  );
  for (const f of reflowFailed.slice(0, 8)) console.log(`         ${f.detail}`);
  console.log(
    `[axe] plan clipped truth state (320 CSS px and 400% zoom): ` +
      `${clippedFailed.length} scan(s) hid content in a collapsed scroller`,
  );
  for (const f of [...new Map(clippedFailed.map((f) => [f.detail, f])).values()].slice(0, 8)) {
    console.log(`         ${f.detail}`);
  }
  console.log(
    `[axe] plan touch targets (>= ${MIN_TOUCH_TARGET_PX}x${MIN_TOUCH_TARGET_PX} CSS px): ` +
      `${targetFailed.length} scan(s) carried an undersized control, ` +
      `${undersized.length} distinct control(s)`,
  );
  for (const t of undersized) {
    console.log(
      `         ${t.width}x${t.height}  ${t.selector}` +
        (t.name === '' ? '' : `  "${t.name}"`) +
        `  in ${t.scans} scan(s), e.g. ${t.where[0]}`,
    );
  }
  console.log(
    `[axe] workspace header box (every width, every child): ${headerFailed.length} scan(s) ` +
      `put a child outside its header, from ${n('headerChildrenExamined')} child element(s) ` +
      `measured across ${n('headerScans')} scan(s) that rendered one`,
  );
  for (const f of [...new Map(headerFailed.map((f) => [f.detail, f])).values()].slice(0, 8)) {
    console.log(`         ${f.detail}`);
  }
  const optOuts = list<ForcedColorsOptOut & { where: string[] }>('forcedColorOptOuts');
  console.log(
    `[axe] forced colors: axe color-contrast disabled (it reads the authored palette, not the ` +
      `forced one); ${optOuts.length} element(s) decline the forced palette`,
  );
  for (const o of optOuts.slice(0, 8)) {
    console.log(`         ${o.selector} color=${o.color} bg=${o.background} e.g. ${o.where[0]}`);
  }
  const overflowing = list<{ kind: string; selector: string; clipper: string; where: string[] }>(
    'overflowingElements',
  );
  if (overflowing.length > 0) {
    console.log(`[axe] diagnostic: ${overflowing.length} element(s) reach past the viewport`);
    for (const o of overflowing.slice(0, 8)) {
      console.log(`         [${o.kind}] ${o.selector} clipped-by=${o.clipper} e.g. ${o.where[0]}`);
    }
  }
  const scrollers = list<{ selector: string; where: string[] }>('unlabelledInternalScrollers');
  if (scrollers.length > 0) {
    console.log(
      `[axe] diagnostic: ${scrollers.length} internal horizontal scroller(s) announce no name`,
    );
    for (const s of scrollers.slice(0, 8)) console.log(`         ${s.selector} e.g. ${s.where[0]}`);
  }
  console.log(`[axe] assertion/visibility failures=${run.assertionFailures}`);
  console.log(`[axe] page errors=${run.pageErrors.length}`);

  return (
    n('totalViolations') > 0 ||
    run.assertionFailures > 0 ||
    run.pageErrors.length > 0 ||
    run.planFailures.length > 0
  );
}
