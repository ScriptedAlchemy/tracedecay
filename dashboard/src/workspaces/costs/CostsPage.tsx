import { OverviewCard, OverviewGrid } from '../../ui/archetypes/OverviewGrid';
import { Chart } from '../../viz/chart/Chart.tsx';
import { LegacyBoundary } from '../../ui/LegacyStates.tsx';
import { ReadoutBar } from '../../ui/instrument.tsx';
import { useLegacy } from '../../data/query/useLegacy.ts';
import { SavingsOverviewPayloadSchema } from './contracts.ts';

const BASE = '/api/plugins/savings';

/** Costs: savings ledger over four windows, actual turn spend, and the
 * per-project lifetime savings distribution. ECharts series land with the
 * charting phase; truthful numbers ship first. */
export function CostsPage() {
  const overview = useLegacy(
    ['savings', 'overview'],
    `${BASE}/overview`,
    SavingsOverviewPayloadSchema,
  );

  return (
    <LegacyBoundary title="Costs" pending={overview.isPending} result={overview.data}>
      {(data) => {
        const ledger = data.savings.available ? data.savings.ledger : undefined;
        const lifetime = data.savings.lifetime_counters;
        const projects = [...(lifetime?.projects ?? [])]
          .filter((p) => (p.tokens_saved ?? 0) > 0)
          .sort((a, b) => (b.tokens_saved ?? 0) - (a.tokens_saved ?? 0))
          .slice(0, 12);
        const projectMax = projects[0]?.tokens_saved ?? 1;
        return (
          <div
            className="flex h-full flex-col overflow-auto"
            tabIndex={0}
            role="region"
            aria-label="Costs content"
          >
            <div className="flex items-center gap-3 border-b border-edge-subtle px-4 py-2">
              <h1 className="text-sm font-semibold tracking-tight">Costs</h1>
              <span className="text-2xs text-text-muted">
                {data.turns.available && data.turns.cost_basis
                  ? `cost basis: ${data.turns.cost_basis}`
                  : 'turn ledger unavailable'}
              </span>
            </div>
            {/* The four windows are nested: today is inside 7d is inside 30d
             * is inside all-time. So each one's rail is truthfully its share
             * of the lifetime figure, and all-time is by definition full --
             * the row reads as one accumulating quantity seen at four depths
             * rather than four unrelated tiles. */}
            <ReadoutBar
              label="Saved tokens by window"
              size="xl"
              elevation="raised"
              items={[
                {
                  label: 'saved today',
                  ...splitTokens(ledger?.today.saved_tokens),
                  fraction: share(ledger?.today.saved_tokens, ledger?.all_time.saved_tokens),
                },
                {
                  label: 'saved 7d',
                  ...splitTokens(ledger?.last_7d.saved_tokens),
                  fraction: share(ledger?.last_7d.saved_tokens, ledger?.all_time.saved_tokens),
                },
                {
                  label: 'saved 30d',
                  ...splitTokens(ledger?.last_30d.saved_tokens),
                  fraction: share(ledger?.last_30d.saved_tokens, ledger?.all_time.saved_tokens),
                },
                {
                  label: 'saved all-time',
                  ...splitTokens(ledger?.all_time.saved_tokens),
                  fraction: ledger ? 1 : null,
                },
              ]}
            />
            <OverviewGrid>
              <OverviewCard title="Savings by window">
                {ledger ? (
                  <Chart
                    ariaLabel="Saved tokens across today, last 7 days, last 30 days, and all time; the stat tiles above carry the exact values"
                    height={180}
                    option={{
                      xAxis: {
                        type: 'category',
                        data: ['today', '7d', '30d', 'all time'],
                      },
                      // "50,000,000" spent eleven glyphs and most of the plot
                      // width saying "50M". The axis speaks the same compact
                      // magnitude language as every other number on the page.
                      yAxis: {
                        type: 'value',
                        axisLabel: { formatter: (value: number) => formatTokens(value) },
                      },
                      series: [
                        {
                          type: 'bar',
                          barWidth: 22,
                          itemStyle: { borderRadius: [3, 3, 0, 0] },
                          data: [
                            ledger.today.saved_tokens,
                            ledger.last_7d.saved_tokens,
                            ledger.last_30d.saved_tokens,
                            ledger.all_time.saved_tokens,
                          ],
                        },
                      ],
                    }}
                  />
                ) : (
                  <p className="text-2xs text-text-muted">ledger unavailable</p>
                )}
              </OverviewCard>
              <OverviewCard title="Actual spend (turn ledger)">
                {data.turns.available ? (
                  <dl className="grid grid-cols-2 gap-x-4 gap-y-1 text-xs tabular">
                    <dt className="text-text-muted">turns</dt>
                    <dd data-cell="numeric">{(data.turns.turn_count ?? 0).toLocaleString()}</dd>
                    <dt className="text-text-muted">total tokens</dt>
                    <dd data-cell="numeric">{formatTokens(data.turns.total_tokens ?? 0)}</dd>
                    <dt className="text-text-muted">total cost</dt>
                    <dd data-cell="numeric">
                      {data.turns.total_cost_usd != null
                        ? `$${data.turns.total_cost_usd.toFixed(2)}`
                        : '—'}
                    </dd>
                  </dl>
                ) : (
                  <p className="text-2xs text-text-muted">turn ledger unavailable</p>
                )}
              </OverviewCard>
              <OverviewCard title="Savings by project (lifetime)">
                {projects.length > 0 ? (
                  <div className="flex flex-col gap-1.5">
                    {projects.map((project, i) => (
                      <div key={`${project.path ?? i}`} className="flex items-center gap-2">
                        <span
                          className="min-w-0 flex-1 truncate font-mono text-2xs text-text-secondary"
                          title={project.path ?? ''}
                        >
                          {shortPath(project.path ?? '')}
                        </span>
                        <span className="relative h-1 w-24 shrink-0 overflow-hidden rounded-full bg-surface-3">
                          <span
                            className="absolute inset-y-0 left-0 rounded-full bg-accent/70"
                            style={{
                              width: `${((project.tokens_saved ?? 0) / projectMax) * 100}%`,
                            }}
                          />
                        </span>
                        <span className="tabular w-16 shrink-0 text-right text-2xs text-text-muted">
                          {formatTokens(project.tokens_saved ?? 0)}
                        </span>
                      </div>
                    ))}
                  </div>
                ) : (
                  <p className="text-2xs text-text-muted">no per-project savings recorded</p>
                )}
              </OverviewCard>
              <OverviewCard title="Pricing">
                <dl className="grid grid-cols-2 gap-x-4 gap-y-1 text-xs">
                  <dt className="text-text-muted">source</dt>
                  <dd>{String(data.pricing.source ?? '—')}</dd>
                  <dt className="text-text-muted">models priced</dt>
                  <dd className="tabular">{String(data.pricing.model_count ?? '—')}</dd>
                  <dt className="text-text-muted">offline</dt>
                  <dd>{String(data.pricing.offline ?? '—')}</dd>
                </dl>
              </OverviewCard>
            </OverviewGrid>
          </div>
        );
      }}
    </LegacyBoundary>
  );
}

function formatTokens(tokens: number): string {
  if (tokens >= 1_000_000_000) return `${(tokens / 1_000_000_000).toFixed(1)}B`;
  if (tokens >= 1_000_000) return `${(tokens / 1_000_000).toFixed(1)}M`;
  if (tokens >= 1_000) return `${(tokens / 1_000).toFixed(1)}k`;
  return tokens.toLocaleString();
}

/** The same magnitude language with the unit split off, so the display tier can
 * set the figure large and its unit small on the shared baseline. */
function splitTokens(tokens: number | null | undefined): {
  value: string;
  unit?: string;
} {
  if (tokens == null || !Number.isFinite(tokens)) return { value: '—' };
  if (tokens >= 1_000_000_000)
    return { value: (tokens / 1_000_000_000).toFixed(1), unit: 'B' };
  if (tokens >= 1_000_000) return { value: (tokens / 1_000_000).toFixed(1), unit: 'M' };
  if (tokens >= 1_000) return { value: (tokens / 1_000).toFixed(1), unit: 'K' };
  return { value: tokens.toLocaleString() };
}

/** A window's share of the lifetime figure it is nested inside. Null whenever
 * either end is missing — an absent denominator must never render as a full
 * bar. */
function share(part: number | undefined, whole: number | undefined): number | null {
  if (part == null || whole == null || !Number.isFinite(whole) || whole <= 0) return null;
  return part / whole;
}

function shortPath(path: string): string {
  const parts = path.split('/').filter(Boolean);
  return parts.slice(-2).join('/') || path;
}
