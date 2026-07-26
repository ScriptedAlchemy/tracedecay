/**
 * The Costs workspace's readings, as pure functions.
 *
 * The page had a hierarchy inversion and two redundant plates. Actual spend —
 * the one number on this surface anybody acts on — sat in the third panel of a
 * grid while a bar chart re-plotted four figures that were already printed at
 * display size directly above it. And the per-project savings rail drew
 * twenty-five bars of which twenty were the same length, because on a machine
 * where every worktree shares one cache every worktree records almost exactly
 * the same lifetime saving. Per plan 11a's degenerate-distribution rule that
 * flatness is the reading, and it belongs in a sentence.
 */

export interface ProjectSaving {
  path?: string | null | undefined;
  tokens_saved?: number | null | undefined;
}

export interface RankedProject {
  path: string;
  tokens: number;
  /** Signed fraction away from the median, e.g. `0.63` for 63% above. */
  deviation: number;
}

export interface ProjectSpread {
  count: number;
  median: number;
  /** Rows within `tolerance` of the median — the flat body of the set. */
  typicalCount: number;
  /** Lowest and highest value inside that flat body. */
  typicalLow: number;
  typicalHigh: number;
  /** Rows outside it, ranked by absolute deviation, biggest first. */
  deviations: RankedProject[];
  /** True when the flat body is most of the set: draw only the deviations. */
  flat: boolean;
}

function median(values: readonly number[]): number {
  if (values.length === 0) return 0;
  const sorted = [...values].sort((a, b) => a - b);
  const middle = Math.floor(sorted.length / 2);
  return sorted.length % 2 === 0
    ? ((sorted[middle - 1] ?? 0) + (sorted[middle] ?? 0)) / 2
    : (sorted[middle] ?? 0);
}

/**
 * Split the per-project savings into "the flat body" and "the rows that
 * actually differ".
 *
 * `tolerance` is a fraction of the median, not an absolute token count,
 * because the quantity spans a factor of eight here and will span more
 * elsewhere. 0.10 is chosen so that a rail whose length differs by less than a
 * tenth — which is under three pixels on a 24px track — is treated as the same
 * length, since that is what it looks like.
 */
export function summarizeProjectSpread(
  projects: readonly ProjectSaving[],
  tolerance = 0.1,
): ProjectSpread | null {
  const rows = projects
    .map((project) => ({
      path: project.path ?? '',
      tokens: project.tokens_saved ?? 0,
    }))
    .filter((row) => row.path !== '' && Number.isFinite(row.tokens) && row.tokens > 0);
  if (rows.length === 0) return null;
  const centre = median(rows.map((row) => row.tokens));
  if (centre <= 0) return null;

  const typical: number[] = [];
  const deviations: RankedProject[] = [];
  for (const row of rows) {
    const deviation = row.tokens / centre - 1;
    if (Math.abs(deviation) <= tolerance) typical.push(row.tokens);
    else deviations.push({ ...row, deviation });
  }
  deviations.sort((a, b) => Math.abs(b.deviation) - Math.abs(a.deviation));

  return {
    count: rows.length,
    median: centre,
    typicalCount: typical.length,
    typicalLow: typical.length > 0 ? Math.min(...typical) : centre,
    typicalHigh: typical.length > 0 ? Math.max(...typical) : centre,
    deviations,
    // "Most of them are the same" is only worth saying when it is true of most
    // of them. Below half, the set has no flat body and every row is a reading.
    flat: typical.length > rows.length / 2,
  };
}

export interface TokenClass {
  label: string;
  tokens: number;
  /** Share of the total, 0–1. */
  share: number;
}

export interface TokenMix {
  total: number;
  classes: TokenClass[];
  leader: TokenClass | null;
  /** One class carries so much that the others cannot share its axis. */
  dominant: boolean;
}

/**
 * How the session ledger's tokens divide between fresh input, output and
 * cache.
 *
 * This is where the money actually goes and the page never showed it. It is
 * also degenerate in the extreme — cache reads are around 98% of all tokens on
 * a real profile — so the leader is stated and the rest is drawn on a log
 * band, the same treatment the Agents composition plate uses for the same
 * reason.
 */
export function summarizeTokenMix(actual: {
  input_tokens?: number | undefined;
  output_tokens?: number | undefined;
  cache_read_tokens?: number | undefined;
  cache_write_tokens?: number | undefined;
}): TokenMix | null {
  const raw = [
    { label: 'cache read', tokens: actual.cache_read_tokens ?? 0 },
    { label: 'input', tokens: actual.input_tokens ?? 0 },
    { label: 'output', tokens: actual.output_tokens ?? 0 },
    { label: 'cache write', tokens: actual.cache_write_tokens ?? 0 },
  ].filter((entry) => Number.isFinite(entry.tokens) && entry.tokens > 0);
  const total = raw.reduce((sum, entry) => sum + entry.tokens, 0);
  if (total === 0) return null;
  const classes = raw
    .map((entry) => ({ ...entry, share: entry.tokens / total }))
    .sort((a, b) => b.tokens - a.tokens);
  const leader = classes[0] ?? null;
  return {
    total,
    classes,
    leader,
    dominant: leader != null && leader.share >= 0.6 && classes.length > 1,
  };
}

/** Position on a log band, 0–1 — see `agents/usage.ts` for why a linear rail
 * cannot draw these. Null rather than a fabricated length when there is no
 * ceiling to measure against. */
export function logFraction(value: number, ceiling: number): number | null {
  if (!Number.isFinite(value) || !Number.isFinite(ceiling) || ceiling <= 0) return null;
  const denominator = Math.log1p(ceiling);
  if (denominator <= 0) return null;
  return Math.max(0, Math.min(1, Math.log1p(Math.max(0, value)) / denominator));
}

/** Cost per turn, derived. Null when either side is missing — a dash is a
 * reading, a zero is a claim. */
export function costPerTurn(
  totalCostUsd: number | null | undefined,
  turnCount: number | null | undefined,
): number | null {
  if (totalCostUsd == null || turnCount == null || !Number.isFinite(totalCostUsd)) return null;
  if (!Number.isFinite(turnCount) || turnCount <= 0) return null;
  return totalCostUsd / turnCount;
}

export interface LedgerCoverage {
  messages: number;
  usage: number;
  tokenized: number;
  estimated: number;
  unknownModel: number;
  /** Share of messages whose token counts came from the provider, 0–1. */
  measuredShare: number;
}

/**
 * How much of the session ledger is measured versus inferred.
 *
 * `cost_basis: "mixed"` on the wire is the whole story in one word, and the
 * page printed it as a header annotation with no way to see what the mix was.
 */
export function summarizeCoverage(sessions: {
  messages?: number | null | undefined;
  usage_messages?: number | null | undefined;
  tokenized_messages?: number | null | undefined;
  estimated_messages?: number | null | undefined;
  unknown_model_messages?: number | null | undefined;
}): LedgerCoverage | null {
  const messages = sessions.messages ?? 0;
  if (!Number.isFinite(messages) || messages <= 0) return null;
  const usage = sessions.usage_messages ?? 0;
  return {
    messages,
    usage,
    tokenized: sessions.tokenized_messages ?? 0,
    estimated: sessions.estimated_messages ?? 0,
    unknownModel: sessions.unknown_model_messages ?? 0,
    measuredShare: usage / messages,
  };
}
