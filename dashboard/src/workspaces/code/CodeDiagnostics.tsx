/**
 * CODE DIAGNOSTICS — `GET /api/plugins/code-diagnostics`.
 *
 * The dashboard-owned LSP diagnostics broker: which analyzer engines are
 * mounted for this project, what state each is in, and the diagnostics they
 * currently hold, attributed where possible to the enclosing indexed symbol.
 *
 * Everything shown is the broker's own snapshot. Engine states (`ready`,
 * `crashed`, `unavailable`, …) are the server's words rendered directly; a
 * broker with no engines mounted is an honest empty, and an unreachable
 * authority renders as the boundary's unavailable state — never as a clean
 * zero-error report.
 */
import { z } from 'zod';
import { LegacyBoundary } from '../../ui/ReadSection.tsx';
import { StateChip, type DomainStateKind } from '../../ui/StateChip.tsx';
import { cn } from '../../ui/cn';
import { useLegacy } from '../../data/query/useLegacy.ts';

const SEVERITIES = ['error', 'warning', 'information', 'hint'] as const;

const DiagnosticRow = z
  .object({
    language: z.string(),
    source: z.string(),
    file: z.string(),
    line_start: z.number(),
    severity: z.enum(SEVERITIES),
    code: z.string().nullable(),
    message: z.string(),
    enclosing_node: z.string().nullable(),
  })
  .passthrough();

const EngineRow = z
  .object({
    language: z.string(),
    command: z.string(),
    enabled: z.boolean(),
    state: z.enum([
      'unavailable',
      'disabled',
      'inactive',
      'available',
      'ready',
      'refreshing',
      'crashed',
    ]),
    last_error: z.string().nullable(),
  })
  .passthrough();

const SnapshotSchema = z
  .object({
    summary: z
      .object({
        total_errors: z.number(),
        total_warnings: z.number(),
        pending_refreshes: z.number(),
        last_refresh_age_seconds: z.number().nullable(),
      })
      .passthrough(),
    engines: z.array(EngineRow),
    diagnostics: z.array(DiagnosticRow),
    settings_unavailable: z.object({ reason: z.string() }).passthrough().optional(),
  })
  .passthrough();

type Snapshot = z.infer<typeof SnapshotSchema>;
type EngineStatus = z.infer<typeof EngineRow>;
type Diagnostic = z.infer<typeof DiagnosticRow>;

/** The broker's engine words mapped onto the shared chip vocabulary; each is
 * a direct reading, not an inference. */
const ENGINE_CHIP: Record<EngineStatus['state'], DomainStateKind> = {
  unavailable: 'unavailable',
  disabled: 'cancelled',
  inactive: 'unknown',
  available: 'partial',
  ready: 'ready',
  refreshing: 'loading',
  crashed: 'error',
};

/** How many diagnostics the panel prints; the totals above state the rest. */
const SHOWN = 8;

export function CodeDiagnostics() {
  const snapshot = useLegacy(
    ['code-diagnostics'],
    '/api/plugins/code-diagnostics',
    SnapshotSchema,
    { refetchInterval: 30_000 },
  );
  return (
    <section className="flex flex-col gap-1.5" aria-label="Code diagnostics">
      <div className="td-legend">diagnostics</div>
      <LegacyBoundary title="Diagnostics" pending={snapshot.isPending} result={snapshot.data}>
        {(data) => <SnapshotBody data={data} />}
      </LegacyBoundary>
    </section>
  );
}

function SnapshotBody({ data }: { data: Snapshot }) {
  const shown = data.diagnostics.slice(0, SHOWN);
  const rest = data.diagnostics.length - shown.length;
  return (
    <div className="flex flex-col gap-2">
      {data.settings_unavailable ? (
        <p role="status" className="text-2xs leading-relaxed text-state-error">
          analyzer settings could not be read ({data.settings_unavailable.reason}); defaults are
          in effect and custom analyzers are missing from this snapshot
        </p>
      ) : null}
      <dl className="grid grid-cols-3 gap-x-3 gap-y-0.5 text-2xs">
        <Figure label="errors" value={data.summary.total_errors} emphasis="error" />
        <Figure label="warnings" value={data.summary.total_warnings} emphasis="warning" />
        <Figure label="refreshing" value={data.summary.pending_refreshes} />
      </dl>
      {data.engines.length === 0 ? (
        <p className="text-2xs text-text-muted">
          no diagnostic engines are mounted for this project
        </p>
      ) : (
        <ul className="flex flex-col gap-1" aria-label="Diagnostic engines">
          {data.engines.map((engine) => (
            <li key={engine.language} className="flex items-center gap-2 text-2xs">
              <StateChip kind={ENGINE_CHIP[engine.state]} detail={engine.last_error ?? undefined} />
              <span className="text-text-primary">{engine.language}</span>
              <span className="min-w-0 truncate text-text-muted" title={engine.command}>
                {engine.command}
              </span>
            </li>
          ))}
        </ul>
      )}
      {data.diagnostics.length === 0 ? (
        data.engines.some((engine) => engine.state === 'ready') ? (
          <p className="text-2xs text-text-muted">the mounted engines report no diagnostics</p>
        ) : null
      ) : (
        <ul className="flex flex-col gap-1" aria-label="Current diagnostics">
          {shown.map((row, index) => (
            <DiagnosticLine key={`${row.file}:${row.line_start}:${index}`} row={row} />
          ))}
          {rest > 0 ? (
            <li className="text-3xs leading-relaxed text-text-muted">
              {rest.toLocaleString()} more diagnostics are in the snapshot; the totals above
              count them all
            </li>
          ) : null}
        </ul>
      )}
    </div>
  );
}

function Figure({
  label,
  value,
  emphasis,
}: {
  label: string;
  value: number;
  emphasis?: 'error' | 'warning';
}) {
  return (
    <div className="flex flex-col">
      <dt className="td-legend">{label}</dt>
      <dd
        className={cn(
          'tabular text-sm',
          emphasis === 'error' && value > 0 && 'text-state-error',
          emphasis === 'warning' && value > 0 && 'text-state-partial',
        )}
      >
        {value.toLocaleString()}
      </dd>
    </div>
  );
}

function DiagnosticLine({ row }: { row: Diagnostic }) {
  return (
    <li className="flex flex-col gap-0.5 border-l-2 border-edge-subtle pl-2">
      <div className="flex items-baseline gap-1.5 text-3xs text-text-muted">
        <span
          className={cn(
            'uppercase tracking-wide',
            row.severity === 'error' ? 'text-state-error' : 'text-state-partial',
          )}
        >
          {row.severity}
        </span>
        <span className="min-w-0 truncate" title={`${row.file}:${row.line_start}`}>
          {row.file}:{row.line_start}
        </span>
        {row.code != null ? <span>[{row.code}]</span> : null}
      </div>
      <p className="text-2xs leading-relaxed text-text-secondary">{row.message}</p>
      {row.enclosing_node != null ? (
        <p className="text-3xs text-text-muted">in {row.enclosing_node}</p>
      ) : null}
    </li>
  );
}
