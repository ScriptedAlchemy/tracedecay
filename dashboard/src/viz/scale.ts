/**
 * Shared value-to-length scales for the dashboard's rails and rows.
 *
 * A scale is where a measurement becomes a picture, so it is exactly the code
 * that must not be copied per workspace: two pages drawing "the same"
 * distribution on quietly different bands is a reporting difference nobody can
 * see in a screenshot.
 */

/**
 * A value's position on a log-scaled band, 0–1, or null when there is no band.
 *
 * `log1p` rather than `log` so a count of zero maps to zero instead of negative
 * infinity, and so the scale is defined for every non-negative value an endpoint
 * can serve. A row with one event lands at 8% of the band against a ceiling of
 * 6,774 — visible, ranked, and honestly labelled as logarithmic — where a linear
 * scale puts it at 0.015% and draws nothing at all.
 *
 * Null, not zero, when the ceiling is missing or non-positive: a rail with no
 * ceiling behind it is an unknown share, which `Meter` draws as a bare track.
 * Lengths from this scale are not proportional to the numbers printed beside
 * them, so the surrounding figure has to say that it is a log band.
 */
export function logFraction(value: number, ceiling: number): number | null {
  if (!Number.isFinite(value) || !Number.isFinite(ceiling) || ceiling <= 0) return null;
  return Math.max(0, Math.min(1, Math.log1p(Math.max(0, value)) / Math.log1p(ceiling)));
}
