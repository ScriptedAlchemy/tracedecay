import { OverviewCard, OverviewGrid } from '../../ui/archetypes/OverviewGrid';
import { ReadFailure } from '../../ui/LegacyStates.tsx';
import { LegacyBoundary } from '../../ui/ReadSection.tsx';
import { Meter, MeterRow, ReadoutBar } from '../../ui/instrument.tsx';
import { formatCount, splitCount } from '../../ui/format.ts';
import { useLegacy } from '../../data/query/useLegacy.ts';
import {
  SavingsOverviewPayloadV1Schema,
} from '../../contracts/generated.ts';
import { logFraction } from '../../viz/scale.ts';
import {
  costPerTurn,
  summarizeCoverage,
  summarizeProjectSpread,
  summarizeTokenMix,
  type ProjectSpread,
  type TokenMix,
} from './spend.ts';
import { CanonicalCosts } from './CanonicalCosts.tsx';

const BASE = '/api/plugins/savings';

/**
 * Costs: what was actually spent, what the tokens went on, and what the cache
 * saved — in that order, because that is the order of the questions.
 *
 * The page previously opened with four savings figures at display size, then
 * plotted those same four figures again as a bar chart directly underneath,
 * and put actual spend — the only number on this surface anyone acts on — in
 * the third panel of a grid as a three-row definition list. The pricing
 * provenance (source, model count, offline) held a whole panel of its own,
 * which is a legend's worth of information given a plate's worth of space.
 */
export function CostsPage() {
  return (
    <div
      className="flex h-full flex-col overflow-auto"
      tabIndex={0}
      role="region"
      aria-label="Costs content"
    >
      <div className="flex items-baseline gap-3 border-b border-edge-subtle px-4 py-2">
        <h1 className="text-sm font-semibold tracking-tight">Costs</h1>
        <span className="min-w-0 truncate text-2xs text-text-muted">
          priced turn ledger, cache savings, and the canonical cost observations
        </span>
      </div>
      {/* Two independent reads. The legacy savings overview and the canonical
        * Plan 26 projection answer with different stores behind them, so a
        * failure in one must not blank the other — which is exactly what
        * happened while the whole page sat inside a single boundary. */}
      <SavingsLedger />
      <CanonicalCosts />
    </div>
  );
}

function SavingsLedger() {
  const overview = useLegacy(
    ['savings', 'overview'],
    `${BASE}/overview`,
    SavingsOverviewPayloadV1Schema,
  );

  return (
    <LegacyBoundary title="Costs" pending={overview.isPending} result={overview.data}>
      {(data) => {
        const ledger = data.savings.available ? data.savings.ledger : undefined;
        const lifetime = data.savings.lifetime_counters;
        const spread = summarizeProjectSpread(lifetime?.projects ?? []);
        const mix =
          data.sessions.available && data.sessions.actual
            ? summarizeTokenMix(data.sessions.actual)
            : null;
        const coverage = data.sessions.available ? summarizeCoverage(data.sessions) : null;
        const perTurn = data.turns.available
          ? costPerTurn(data.turns.total_cost_usd, data.turns.turn_count)
          : null;
        // The truncated title needs both counts to say anything; the contract
        // makes them non-null whenever the counters block itself is, so the
        // rows served stand in only when the slice came back empty. Neither end
        // is coalesced to zero — "top 0 of 0 projects" is a sentence about no
        // data, not about a capped slice.
        const shownProjects = lifetime
          ? lifetime.projects.length || lifetime.projects_limit
          : null;
        const projectTitle =
          lifetime?.projects_truncated && shownProjects != null
            ? `Savings by project (top ${shownProjects.toLocaleString()} of ${lifetime.project_total.toLocaleString()} projects)`
            : 'Savings by project (lifetime)';
        return (
          <>
            <p className="border-b border-edge-subtle px-4 py-1.5 text-2xs text-text-muted">
              {data.turns.available
                ? `turn ledger · ${data.turns.cost_basis ?? 'unknown'} cost basis`
                : 'turn ledger unavailable'}
            </p>

            {/* Spend first, and at the display tier. Everything below it is
              * either an explanation of this number or a counterfactual about
              * it; neither outranks it. */}
            <ReadoutBar
              label="Actual spend"
              size="xl"
              elevation="raised"
              items={[
                {
                  label: 'total cost',
                  value:
                    data.turns.available && data.turns.total_cost_usd != null
                      ? `$${data.turns.total_cost_usd.toLocaleString(undefined, {
                          minimumFractionDigits: 2,
                          maximumFractionDigits: 2,
                        })}`
                      : '—',
                  note: data.turns.cost_basis
                    ? `${data.turns.cost_basis} basis`
                    : 'basis unreported',
                },
                {
                  label: 'per turn',
                  value: perTurn != null ? `$${perTurn.toFixed(3)}` : '—',
                  note: 'derived: cost ÷ turns',
                },
                {
                  label: 'turns',
                  ...splitTokens(data.turns.available ? data.turns.turn_count : undefined),
                  note: 'priced turn ledger',
                },
                {
                  label: 'tokens',
                  ...splitTokens(data.turns.available ? data.turns.total_tokens : undefined),
                  note: 'across those turns',
                },
              ]}
            />
            {!data.turns.available ? (
              <ReadFailure band label="Priced turn ledger read failed" detail={data.turns.error} />
            ) : null}

            {/* The four windows are nested: today is inside 7d is inside 30d
             * is inside all-time. So each one's rail is truthfully its share
             * of the lifetime figure, and all-time is by definition full --
             * the row reads as one accumulating quantity seen at four depths
             * rather than four unrelated tiles. The bar chart that used to sit
             * under this row plotted these same four numbers a second time,
             * which is not a second reading. */}
            <ReadoutBar
              label="Saved tokens by window"
              size="md"
              items={[
                {
                  label: 'saved today',
                  ...splitTokens(ledger?.today.saved_tokens),
                  fraction: share(ledger?.today.saved_tokens, ledger?.all_time.saved_tokens),
                  note: perCall(ledger?.today),
                },
                {
                  label: 'saved 7d',
                  ...splitTokens(ledger?.last_7d.saved_tokens),
                  fraction: share(ledger?.last_7d.saved_tokens, ledger?.all_time.saved_tokens),
                  note: perCall(ledger?.last_7d),
                },
                {
                  label: 'saved 30d',
                  ...splitTokens(ledger?.last_30d.saved_tokens),
                  fraction: share(ledger?.last_30d.saved_tokens, ledger?.all_time.saved_tokens),
                  note: perCall(ledger?.last_30d),
                },
                {
                  label: 'saved all-time',
                  ...splitTokens(ledger?.all_time.saved_tokens),
                  fraction: ledger ? 1 : null,
                  note: perCall(ledger?.all_time),
                },
              ]}
            />
            {!data.savings.available ? (
              <ReadFailure band label="Savings ledger read failed" detail={data.savings.error} />
            ) : null}

            <OverviewGrid>
              <OverviewCard title="Where the tokens go">
                {!data.sessions.available ? (
                  <ReadFailure
                    label="Session ledger read failed"
                    detail={data.sessions.error}
                  />
                ) : mix ? (
                  <TokenMixPlate mix={mix} sessions={data.sessions} />
                ) : (
                  <p className="text-2xs text-text-muted">
                    the session ledger reported no token breakdown
                  </p>
                )}
              </OverviewCard>

              <OverviewCard title={projectTitle}>
                {!data.savings.available ? (
                  <ReadFailure label="Savings ledger read failed" detail={data.savings.error} />
                ) : spread ? (
                  <ProjectSpreadPlate spread={spread} />
                ) : (
                  <p className="text-2xs text-text-muted">no per-project savings recorded</p>
                )}
              </OverviewCard>

              <OverviewCard title="How much of the ledger is measured">
                {!data.sessions.available ? (
                  <ReadFailure
                    label="Session ledger read failed"
                    detail={data.sessions.error}
                  />
                ) : coverage ? (
                  <figure className="flex flex-col gap-2">
                    <p className="text-xs leading-relaxed text-text-primary">
                      {coverage.measuredShare != null ? (
                        <>
                          {Math.round(coverage.measuredShare * 100)}% of{' '}
                          {coverage.messages.toLocaleString()} messages carry token counts
                          the provider reported. The remainder separates locally tokenized
                          messages from estimates; together those sources form the{' '}
                          <span className="td-value">
                            {String(data.sessions.cost_basis ?? 'mixed')}
                          </span>{' '}
                          cost basis.
                        </>
                      ) : (
                        <>
                          The ledger holds {coverage.messages.toLocaleString()} messages and
                          reported no provider-measured count among them, so the measured
                          share is unknown — not none. The rows below print only the classes
                          the ledger did report, against the{' '}
                          <span className="td-value">
                            {String(data.sessions.cost_basis ?? 'mixed')}
                          </span>{' '}
                          cost basis it names.
                        </>
                      )}
                    </p>
                    <ShareRow
                      label="provider-reported"
                      value={coverage.usage}
                      total={coverage.messages}
                    />
                    <ShareRow
                      label="tokenized"
                      value={coverage.tokenized}
                      total={coverage.messages}
                    />
                    <ShareRow
                      label="estimated"
                      value={coverage.estimated}
                      total={coverage.messages}
                    />
                    <ShareRow
                      label="model not identified"
                      value={coverage.unknownModel}
                      total={coverage.messages}
                    />
                    {data.sessions.estimated ? (
                      <p className="text-2xs leading-relaxed text-text-secondary">
                        The estimated side accounts for{' '}
                        {formatTokens(data.sessions.estimated.input_tokens)} input and{' '}
                        {formatTokens(data.sessions.estimated.output_tokens)} output tokens —
                        the part of the spend above that is inferred rather than reported.
                      </p>
                    ) : null}
                    <figcaption className="text-3xs leading-relaxed text-text-muted">
                      Model-not-identified overlaps the two rows above it: it counts
                      messages whose model could not be resolved, whatever their token
                      source. It is drawn against the same total, not stacked on them.
                    </figcaption>
                  </figure>
                ) : (
                  <p className="text-2xs text-text-muted">the session ledger reported no messages</p>
                )}
              </OverviewCard>
            </OverviewGrid>

            {/* Provenance, as a legend. Source, model count and offline state
              * are things you check once and then stop looking at; they were
              * holding a full panel in a three-panel grid. */}
            <p className="flex flex-wrap items-baseline gap-x-4 gap-y-1 border-t border-edge-subtle px-4 py-2 text-3xs text-text-muted">
              <span className="td-legend">pricing</span>
              <span>source {String(data.pricing.source ?? '—')}</span>
              <span>{String(data.pricing.model_count ?? '—')} models priced</span>
              <span>offline {String(data.pricing.offline ?? '—')}</span>
              {data.sessions.scope ? <span>scope {String(data.sessions.scope)}</span> : null}
              {data.sessions.model_count != null ? (
                <span>{data.sessions.model_count.toLocaleString()} models seen</span>
              ) : null}
            </p>
          </>
        );
      }}
    </LegacyBoundary>
  );
}

/**
 * The token mix, which is where the spend actually comes from and which this
 * page never showed.
 *
 * Cache reads are around 98% of every token the session ledger holds. Drawn on
 * one linear axis the other three classes are invisible, so the leader is
 * stated and the remainder gets a log band — captioned as logarithmic, because
 * a length a reader cannot compare linearly has to say so.
 */
function TokenMixPlate({
  mix,
  sessions,
}: {
  mix: TokenMix;
  sessions: {
    session_count?: number | null | undefined;
    messages?: number | null | undefined;
  };
}) {
  const rest = mix.dominant ? mix.classes.slice(1) : mix.classes;
  const ceiling = rest.reduce((max, entry) => Math.max(max, entry.tokens), 0);
  return (
    <div className="flex flex-col gap-3">
      {mix.dominant && mix.leader ? (
        <p className="text-xs leading-relaxed text-text-primary">
          <span className="td-value">{mix.leader.label}</span> is{' '}
          <span className="td-value">{Math.round(mix.leader.share * 100)}%</span> of every
          token in the session ledger — {formatTokens(mix.leader.tokens)} of{' '}
          {formatTokens(mix.total)}.
        </p>
      ) : null}
      <figure className="flex flex-col gap-1.5">
        <figcaption className="td-legend">
          {mix.dominant ? 'everything else · log scale' : 'token classes'}
        </figcaption>
        {rest.map((entry) => (
          <MeterRow
            key={entry.label}
            label={entry.label}
            fraction={
              mix.dominant ? logFraction(entry.tokens, ceiling) : entry.tokens / ceiling
            }
            value={formatTokens(entry.tokens)}
            figureWidth="wide"
          />
        ))}
        {/* The denominator is only worth stating when the ledger served it. A
          * "0 messages in 0 sessions" caption under real token figures reads as
          * a measurement of an empty ledger, which is the opposite of what an
          * unreported count means. */}
        <figcaption className="text-3xs leading-relaxed text-text-muted">
          {sessions.messages != null && sessions.session_count != null
            ? `The provider-reported token breakdown across ${sessions.messages.toLocaleString()} messages in ${sessions.session_count.toLocaleString()} sessions — a different, wider denominator than the priced turn ledger above.`
            : 'The provider-reported token breakdown, over a span of messages and sessions the ledger did not report — a different, wider denominator than the priced turn ledger above.'}
        </figcaption>
      </figure>
    </div>
  );
}

/**
 * Per-project savings, drawn only where they differ.
 *
 * Twenty-five rows of which twenty are the same length is not a useful plot.
 * The sameness is stated without inventing a cause the wire does not provide,
 * and only the rows that genuinely deviate are plotted — against their
 * deviation, which is the quantity that varies.
 */
function ProjectSpreadPlate({ spread }: { spread: ProjectSpread }) {
  if (!spread.flat) {
    const ceiling = spread.deviations.reduce((max, row) => Math.max(max, row.tokens), 0);
    return (
      <figure className="flex flex-col gap-1.5">
        <figcaption className="td-legend">{spread.count} projects</figcaption>
        {spread.deviations.map((row) => (
          <div key={row.path} className="flex items-center gap-2">
            <span
              className="min-w-0 flex-1 truncate font-mono text-2xs text-text-secondary"
              title={row.path}
            >
              {shortPath(row.path)}
            </span>
            <Meter
              fraction={ceiling > 0 ? row.tokens / ceiling : null}
              className="w-24 shrink-0 max-sm:hidden"
            />
            <span
              className="td-value w-14 shrink-0 text-right text-2xs text-text-muted"
              data-cell="numeric"
            >
              {formatTokens(row.tokens)}
            </span>
          </div>
        ))}
      </figure>
    );
  }
  const ceiling = spread.deviations.reduce(
    (max, row) => Math.max(max, Math.abs(row.deviation)),
    0,
  );
  return (
    <div className="flex flex-col gap-3">
      <p className="text-xs leading-relaxed text-text-primary">
        {spread.typicalCount} of {spread.count} projects saved between{' '}
        {formatTokens(spread.typicalLow)} and {formatTokens(spread.typicalHigh)} — within a
        tenth of the {formatTokens(spread.median)} median. The wire does not report why
        these values cluster, so no cache topology is inferred.
      </p>
      {spread.deviations.length > 0 ? (
        <figure className="flex flex-col gap-1.5">
          <figcaption className="td-legend">
            the {spread.deviations.length} that differ · vs median
          </figcaption>
          {spread.deviations.map((row) => (
            <div key={row.path} className="flex items-center gap-2">
              <span
                className="min-w-0 flex-1 truncate font-mono text-2xs text-text-secondary"
                title={row.path}
              >
                {shortPath(row.path)}
              </span>
              <Meter
                fraction={ceiling > 0 ? Math.abs(row.deviation) / ceiling : null}
                className="w-16 shrink-0 max-sm:hidden"
                tone={row.deviation < 0 ? 'bg-state-stale' : undefined}
              />
              <span
                className="td-value w-12 shrink-0 text-right text-2xs text-text-secondary"
                data-cell="numeric"
              >
                {row.deviation > 0 ? '+' : '−'}
                {Math.round(Math.abs(row.deviation) * 100)}%
              </span>
              <span
                className="td-value w-14 shrink-0 text-right text-2xs text-text-muted max-md:hidden"
                data-cell="numeric"
              >
                {formatTokens(row.tokens)}
              </span>
            </div>
          ))}
        </figure>
      ) : (
        <p className="text-2xs text-text-muted">
          No project deviates from the median by more than a tenth.
        </p>
      )}
    </div>
  );
}

/** One part of a known whole, printed and given a length.
 *
 * A `null` value is a class the ledger never reported, which is a different
 * fact from a class it counted at zero: the row says so in words, prints an em
 * dash rather than a figure, and leaves the rail unfilled. */
function ShareRow({
  label,
  value,
  total,
}: {
  label: string;
  value: number | null;
  total: number;
}) {
  return (
    <div className="flex items-center gap-2 text-xs">
      <span className="min-w-0 flex-1 truncate text-text-primary">{label}</span>
      {value == null ? (
        <span className="td-legend shrink-0 text-text-muted">not reported</span>
      ) : null}
      <Meter
        fraction={value != null && total > 0 ? value / total : null}
        height="row"
        className="w-20 shrink-0 max-sm:hidden"
      />
      <span
        className="td-value w-14 shrink-0 text-right text-2xs text-text-secondary"
        data-cell="numeric"
      >
        {value != null ? value.toLocaleString() : '—'}
      </span>
    </div>
  );
}

/** Tokens abbreviate from a thousand rather than the ten thousand the shared
 * magnitude language defaults to, because four-figure token counts are the
 * exception on this surface. That threshold is the only thing this ledger
 * disagrees with the rest of the dashboard about, so it is all this states —
 * the ladder itself, and `null` in / em dash out, come from `format.ts`. */
function formatTokens(tokens: number | null | undefined): string {
  return formatCount(tokens, 1_000);
}

/** The same magnitude language with the unit split off, so the display tier can
 * set the figure large and its unit small on the shared baseline. */
function splitTokens(tokens: number | null | undefined): {
  value: string;
  unit?: string;
} {
  return splitCount(tokens, 1_000);
}

/** A window's share of the lifetime figure it is nested inside. Null whenever
 * either end is missing — an absent denominator must never render as a full
 * bar. */
function share(part: number | undefined, whole: number | undefined): number | null {
  if (part == null || whole == null || !Number.isFinite(whole) || whole <= 0) return null;
  return part / whole;
}

/** A window's second channel: how many cache hits produced its saving, and
 * what each one was worth. The ledger has carried `calls` all along and the row
 * printed only `saved_tokens`, so the reader could not tell a window with a few
 * enormous hits from one with very many small ones. */
function perCall(window: { saved_tokens: number; calls: number } | undefined): string {
  if (!window || !Number.isFinite(window.calls) || window.calls <= 0) return 'no calls recorded';
  return `${window.calls.toLocaleString()} calls · ${formatTokens(
    Math.round(window.saved_tokens / window.calls),
  )}/call`;
}

function shortPath(path: string): string {
  const parts = path.split('/').filter(Boolean);
  return parts.slice(-2).join('/') || path;
}
