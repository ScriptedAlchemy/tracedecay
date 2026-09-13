/**
 * Presentation model for the Plan 26 canonical measurement, `MetricValueV1`.
 *
 * Observatory and Costs both render this exact wire type, so the reading rules
 * live here once rather than being re-derived per workspace. Every rule below
 * exists because the wire draws a distinction the UI is not allowed to lose:
 *
 *   value: null      is NOT zero. It is a measurement that does not exist, and
 *                    `unavailable_reason` says why. A null never renders 0, an
 *                    empty bar, or a unit.
 *   denominator      names the eligible population by identifier and, when the
 *                    server knows it, its size. An unknown denominator never
 *                    becomes a percentage or a progress meter.
 *   coverage         carries observed/completed/censored/excluded/unknown as
 *                    separate counts. Censored and excluded are not "missing";
 *                    collapsing them would turn a bounded read into a total.
 *   evidence_class   is an axis of its own — a measurement, an association, and
 *                    a calibrated prediction are different KINDS of claim, not
 *                    different confidence levels of one claim.
 *   calibration      is the only thing that licenses the word "calibrated".
 *                    Absent calibration means the estimator/cohort/support/
 *                    drift facts do not exist, so no interval is dressed up as
 *                    one.
 *
 * The composer in `src/application/observability.rs` writes a degenerate
 * uncertainty interval (`lower == upper == value`) for every known value. That
 * is a placeholder for a real interval, not a measured bound, so it is not
 * rendered as one.
 */
import type {
  MetricCoverageV1,
  MetricEvidenceClassV1,
  MetricValueV1,
} from '../contracts/generated.ts';
import { assertNever } from '../contracts/generated.ts';
import type { EvidenceQuality } from './EvidencePattern.tsx';

/** One measurement, reduced to the strings a plate renders. */
export interface MetricPresentation {
  /** Underscored wire identifier turned into words. The identifier itself
   * stays available to callers as `metric.metric`. */
  label: string;
  /** Formatted figure, or an em dash when the server reported no value. */
  figure: string;
  /** Display unit — `null` whenever there is no figure, because a bare unit
   * beside an em dash reads as a measurement that was taken and lost. */
  unit: string | null;
  /** The unconverted server figure, when the display figure is a conversion of
   * it (microseconds shown as milliseconds, a ratio shown as a percent). */
  exact: string | null;
  available: boolean;
  /** Server-supplied reason, verbatim. Never paraphrased into "no data". */
  unavailableReason: string | null;
  denominator: string;
  coverage: string;
  evidenceClass: MetricEvidenceClassV1;
  evidenceQuality: EvidenceQuality;
  provenance: string;
  /** Only present for a real interval — see the module note on degenerate
   * bounds. */
  interval: string | null;
  /** Only present when the wire carries a baseline to compare against. */
  delta: string | null;
  /** Only present when the server attached a calibration record. */
  calibration: string | null;
}

export function metricPresentation(metric: MetricValueV1): MetricPresentation {
  const figure = metricFigure(metric);
  return {
    label: humanizeMetric(metric.metric),
    figure: figure.value,
    unit: figure.unit,
    exact: figure.exact,
    available: metric.value != null,
    unavailableReason: metric.unavailable_reason,
    denominator: denominatorSentence(metric),
    coverage: coverageSentence(metric.coverage),
    evidenceClass: metric.evidence_class,
    evidenceQuality: evidenceQuality(metric.evidence_class),
    provenance: provenanceSentence(metric),
    interval: intervalSentence(metric),
    delta: deltaSentence(metric),
    calibration: calibrationSentence(metric),
  };
}

/** `feedback_latency_p95` → `feedback latency p95`. Words only; the exact wire
 * identifier is never replaced, only spaced. */
export function humanizeMetric(identifier: string): string {
  return identifier.replaceAll('_', ' ');
}

/**
 * The figure a plate prints.
 *
 * Two units are converted for legibility, and both keep the unconverted server
 * figure in `exact` so the conversion is auditable rather than a substitution:
 * microseconds become milliseconds, and a ratio becomes a percent. Every other
 * unit is printed exactly as the server measured it.
 */
export function metricFigure(metric: MetricValueV1): {
  value: string;
  unit: string | null;
  exact: string | null;
} {
  const { value, unit } = metric;
  if (value == null || !Number.isFinite(value)) {
    return { value: '—', unit: null, exact: null };
  }
  if (unit === 'microseconds') {
    return {
      value: formatNumber(value / 1000),
      unit: 'ms',
      exact: `${formatNumber(value)} µs`,
    };
  }
  if (unit === 'ratio') {
    return {
      value: formatNumber(value * 100),
      unit: '%',
      exact: `${formatNumber(value)} ratio`,
    };
  }
  return { value: formatNumber(value), unit, exact: null };
}

function formatNumber(value: number): string {
  if (Number.isInteger(value)) return value.toLocaleString();
  const magnitude = Math.abs(value);
  const digits = magnitude >= 100 ? 1 : magnitude >= 1 ? 2 : 4;
  return value.toLocaleString(undefined, { maximumFractionDigits: digits });
}

/** `per eligible_observations (128)`, or the same line saying the size is not
 * known. The population identifier is always named: "128" alone does not say
 * 128 of what. */
export function denominatorSentence(metric: MetricValueV1): string {
  const population = humanizeMetric(metric.denominator);
  return metric.denominator_value == null
    ? `per ${population} · size not reported`
    : `per ${population} · ${metric.denominator_value.toLocaleString()}`;
}

/**
 * Coverage as counts, never as a percentage.
 *
 * `observed` and `completed` always print, because a read where those two
 * disagree is the whole reason coverage exists. The remaining three print only
 * when nonzero — a run with nothing censored should not carry three zeroes that
 * make the eye work — and `eligible` prints its unknown state in words rather
 * than as a missing figure.
 */
export function coverageSentence(coverage: MetricCoverageV1): string {
  const parts = [
    `${coverage.observed.toLocaleString()} observed`,
    `${coverage.completed.toLocaleString()} completed`,
  ];
  if (coverage.censored > 0) parts.push(`${coverage.censored.toLocaleString()} censored`);
  if (coverage.excluded > 0) parts.push(`${coverage.excluded.toLocaleString()} excluded`);
  if (coverage.unknown > 0) parts.push(`${coverage.unknown.toLocaleString()} unknown`);
  const eligible =
    coverage.eligible == null
      ? 'eligible population unknown'
      : `${coverage.eligible.toLocaleString()} eligible`;
  return `${coverage.state} · ${parts.join(', ')} · ${eligible}`;
}

function provenanceSentence(metric: MetricValueV1): string {
  const { source, source_revision, projector_revision, watermark } = metric.provenance;
  return `${humanizeMetric(source)} · ${source_revision} → ${projector_revision} · watermark ${watermark}`;
}

/**
 * A bound is only a bound when it bounds something. The observability composer
 * fills `lower`/`upper` with the value itself whenever a value exists, so an
 * interval identical to the point estimate is dropped rather than printed as
 * a measured range.
 */
function intervalSentence(metric: MetricValueV1): string | null {
  const { lower, upper } = metric.uncertainty;
  if (lower == null || upper == null) return null;
  if (lower === upper && lower === metric.value) return null;
  return `${formatNumber(lower)} – ${formatNumber(upper)} ${metric.unit}`;
}

function deltaSentence(metric: MetricValueV1): string | null {
  const { delta, baseline_watermark: baseline } = metric.temporal;
  if (delta == null) return null;
  const signed = delta > 0 ? `+${formatNumber(delta)}` : formatNumber(delta);
  return baseline == null
    ? `${signed} ${metric.unit} against an unnamed baseline`
    : `${signed} ${metric.unit} against ${baseline}`;
}

/** The only sentence allowed to use the word "calibrated": it names estimator,
 * calibration revision, cohort, support, and drift validity, which is exactly
 * what the plan requires before a value may claim calibration. */
function calibrationSentence(metric: MetricValueV1): string | null {
  const calibration = metric.calibration;
  if (calibration == null) return null;
  const drift = calibration.drift_valid ? 'drift valid' : 'drift invalid';
  return `estimator ${calibration.estimator_revision} · calibration ${calibration.calibration_revision} · cohort ${calibration.cohort_revision} · support ${calibration.support.toLocaleString()} · ${drift}`;
}

/** Maps the wire's evidence class onto the shared pattern axis. Severity is a
 * separate axis and never borrows this one. */
export function evidenceQuality(evidenceClass: MetricEvidenceClassV1): EvidenceQuality {
  switch (evidenceClass) {
    case 'measurement':
      return 'measured';
    case 'association':
      return 'associated';
    case 'calibrated_prediction':
      return 'predicted';
    default:
      return assertNever(evidenceClass);
  }
}

/** Metrics grouped by the producing source, preserving server order inside each
 * group. Source is the wire's own attribution, so grouping by it invents no
 * taxonomy the daemon did not already state. */
export interface MetricGroup {
  source: string;
  label: string;
  metrics: MetricValueV1[];
}

export function groupBySource(metrics: MetricValueV1[]): MetricGroup[] {
  const groups: MetricGroup[] = [];
  for (const metric of metrics) {
    const source = metric.provenance.source;
    const existing = groups.find((group) => group.source === source);
    if (existing) {
      existing.metrics.push(metric);
      continue;
    }
    groups.push({ source, label: humanizeMetric(source), metrics: [metric] });
  }
  return groups;
}

/** How many of a set carry a value. Used for a group header that must not imply
 * a complete reading when half its plates are unavailable. */
export function availableCount(metrics: MetricValueV1[]): number {
  return metrics.filter((metric) => metric.value != null).length;
}
