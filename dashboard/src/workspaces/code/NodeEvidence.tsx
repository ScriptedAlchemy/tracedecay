/**
 * EVIDENCE — what is known about the traced symbol beyond its call edges.
 *
 * Three node-scoped structure reads, side by side under the field:
 *
 *   facts     `GET /api/plugins/graph/node/{id}/facts`     memory facts whose
 *             subject matches this symbol's name.
 *   tests     `GET /api/plugins/graph/node/{id}/tests`     tests that reach it.
 *   sessions  `GET /api/plugins/graph/node/{id}/sessions`  agent sessions that
 *             touched it.
 *
 * All three answer in `StructureReadV1`, so absence arrives *typed* — see
 * `data/query/structure.ts`. This surface's whole job is to keep that typing
 * intact all the way to the glass, because each of these three has a specific
 * way of being quietly wrong:
 *
 *   FACTS match on NAME, not identity. The producer says so itself, in
 *   `identity_semantics` and `same_name_collision_possible`, and a repository
 *   with two `render` functions will hand back one arm's facts for both. The
 *   count is therefore printed as "facts matching this NAME", never "this
 *   symbol's facts", and the collision warning is not tucked into a tooltip.
 *
 *   TESTS carry `applicable`. A symbol the algorithm does not apply to is not
 *   a symbol with zero tests, and rendering "0 tests" for it would be a
 *   fabricated result. When `applicable` is false the count is not drawn at
 *   all — the reason takes its place.
 *
 *   SESSIONS are the sharpest of the three. The route resolves session linkage
 *   at FILE granularity and states that in `symbol_granularity_available` and
 *   `symbol_granularity_reason`. Printing that number beside a symbol's name
 *   without the qualifier would attribute a whole file's session traffic to one
 *   function, which is the exact falsification this console exists to refuse.
 *   The granularity is therefore part of the reading, not a footnote to it.
 */
import { useMemo } from 'react';

import {
  StructureReadV13Schema as FactMatchesReadSchema,
  StructureReadV14Schema as TestMapReadSchema,
  StructureReadV15Schema as NodeSessionsReadSchema,
  type FactMatchesMeasurementV1,
  type NodeSessionsMeasurementV1,
  type TestMapMeasurementV1,
} from '../../contracts/generated.ts';
import { absenceReason, useStructure, type StructureResult } from '../../data/query/structure.ts';
import { cn } from '../../ui/cn';
import { elideStart } from '../../ui/format.ts';

const BASE = '/api/plugins/graph';

function nodeUrl(id: string, leaf: string): string {
  return `${BASE}/node/${encodeURIComponent(id)}/${leaf}`;
}

export function NodeEvidence({ nodeId, nodeName }: { nodeId: string; nodeName: string }) {
  const enabled = nodeId !== '';
  const facts = useStructure<FactMatchesMeasurementV1>(
    ['graph', 'node-facts', nodeId],
    nodeUrl(nodeId, 'facts'),
    FactMatchesReadSchema,
    { enabled },
  );
  const tests = useStructure<TestMapMeasurementV1>(
    ['graph', 'node-tests', nodeId],
    nodeUrl(nodeId, 'tests'),
    TestMapReadSchema,
    { enabled },
  );
  const sessions = useStructure<NodeSessionsMeasurementV1>(
    ['graph', 'node-sessions', nodeId],
    nodeUrl(nodeId, 'sessions'),
    NodeSessionsReadSchema,
    { enabled },
  );

  return (
    <section
      className="flex flex-col border-b border-edge-subtle"
      aria-label={`Evidence for ${nodeName}`}
      data-testid="node-evidence"
    >
      <div className="flex items-center gap-2.5 px-3 py-2">
        <h3 className="td-legend">evidence</h3>
        <span aria-hidden className="td-rule" />
        <span className="td-legend shrink-0 normal-case tracking-normal text-text-muted">
          three independent reads · each states its own granularity
        </span>
      </div>
      <div className="grid grid-cols-1 gap-px bg-edge-subtle lg:grid-cols-3">
        <EvidenceCard
          label="facts"
          pending={facts.isPending && enabled}
          result={facts.data}
          render={(m) => <FactsReading measurement={m} />}
        />
        <EvidenceCard
          label="tests"
          pending={tests.isPending && enabled}
          result={tests.data}
          render={(m) => <TestsReading measurement={m} />}
        />
        <EvidenceCard
          label="sessions"
          pending={sessions.isPending && enabled}
          result={sessions.data}
          render={(m) => <SessionsReading measurement={m} />}
        />
      </div>
    </section>
  );
}

/**
 * One card. Absence is a first-class rendering with the producer's own words
 * in it, not a blank and never a zero — the distinction this whole panel is
 * built to preserve dies here if it dies anywhere.
 */
function EvidenceCard<T>({
  label,
  pending,
  result,
  render,
}: {
  label: string;
  pending: boolean;
  result: StructureResult<T> | undefined;
  render: (measurement: T) => React.ReactNode;
}) {
  return (
    <div className="flex min-w-0 flex-col gap-1 bg-surface-0 px-3 py-2">
      <div className="flex items-center gap-1.5">
        <span className="td-legend">{label}</span>
        <span aria-hidden className="td-rule" />
        <StatusTag pending={pending} result={result} />
      </div>
      {pending ? (
        <p className="text-2xs text-state-loading">reading…</p>
      ) : result === undefined ? (
        <p className="text-2xs text-state-unknown">no response recorded</p>
      ) : result.outcome === 'measured' ? (
        render(result.measurement)
      ) : (
        <p className="text-2xs leading-relaxed text-state-unknown">{absenceReason(result)}</p>
      )}
    </div>
  );
}

/** The wire's own `status`, printed. A reader can tell a producer that declined
 * to run from one that failed from one that never answered. */
function StatusTag<T>({
  pending,
  result,
}: {
  pending: boolean;
  result: StructureResult<T> | undefined;
}) {
  if (pending) return null;
  if (result === undefined) return null;
  const tone =
    result.outcome === 'measured'
      ? 'text-text-muted'
      : result.outcome === 'failed'
        ? 'text-state-error'
        : 'text-state-unknown';
  return <span className={cn('td-legend shrink-0', tone)}>{result.outcome}</span>;
}

/** A counted quantity with its unit and the qualifier that makes it honest. */
function Count({
  value,
  unit,
  qualifier,
}: {
  value: number;
  unit: string;
  qualifier: string;
}) {
  return (
    <span className="flex min-w-0 flex-col gap-0.5">
      <span className="flex items-baseline gap-1.5">
        <span className="td-value text-sm text-text-primary" data-cell="numeric">
          {value.toLocaleString()}
        </span>
        <span className="td-unit">{unit}</span>
      </span>
      <span className="text-3xs leading-snug text-text-secondary">{qualifier}</span>
    </span>
  );
}

/**
 * Facts.
 *
 * `facts` inside each arm is `Vec<Value>` on the wire, so schemars types it as
 * `unknown` and this surface genuinely cannot render a fact's contents without
 * inventing a shape for it. It counts them and says that is what it is doing;
 * the Knowledge workspace is where facts are read.
 */
function FactsReading({ measurement }: { measurement: FactMatchesMeasurementV1 }) {
  const total = useMemo(
    () => measurement.arms.reduce((sum, arm) => sum + arm.coverage.returned, 0),
    [measurement.arms],
  );
  const truncated = measurement.arms.some((arm) => arm.coverage.truncated);
  return (
    <div className="flex flex-col gap-1.5">
      <Count
        value={total}
        unit={total === 1 ? 'fact' : 'facts'}
        qualifier={`matching the name "${measurement.normalized_name}" · ${measurement.identity_semantics}`}
      />
      {measurement.same_name_collision_possible ? (
        <p className="text-3xs leading-snug text-state-unknown">
          another symbol in this repository shares this name, so some of these facts may
          belong to it — the match is by name, not identity
        </p>
      ) : null}
      {measurement.arms.length > 0 ? (
        <ul className="flex flex-col gap-0.5">
          {measurement.arms.map((arm) => (
            <li key={arm.match_basis} className="flex items-baseline gap-1.5 text-3xs">
              <span className="td-legend shrink-0">{arm.match_basis}</span>
              <span className="td-value shrink-0 text-text-secondary" data-cell="numeric">
                {arm.coverage.returned}
              </span>
              <span className="min-w-0 flex-1 truncate text-text-muted">
                {arm.strength}
                {arm.coverage.truncated ? ` · capped at ${arm.coverage.limit}` : ''}
              </span>
            </li>
          ))}
        </ul>
      ) : null}
      {truncated ? (
        <p className="text-3xs leading-snug text-state-unknown">
          at least one arm hit its limit, so this count is a floor rather than a total
        </p>
      ) : null}
    </div>
  );
}

/**
 * Tests.
 *
 * `applicable: false` is not "no tests" and is never drawn as a count. The
 * producer's `reason` is the reading in that case.
 */
function TestsReading({ measurement }: { measurement: TestMapMeasurementV1 }) {
  if (!measurement.applicable) {
    return (
      <p className="text-2xs leading-relaxed text-state-unknown">
        the {measurement.algorithm} test map does not apply to this symbol
        {measurement.reason ? ` — ${measurement.reason}` : ''}. That is not a claim that no
        test covers it.
      </p>
    );
  }
  return (
    <div className="flex flex-col gap-1.5">
      <Count
        value={measurement.tests.length}
        unit={measurement.tests.length === 1 ? 'covering test' : 'covering tests'}
        qualifier={`${measurement.algorithm} · ${measurement.caller_depth} caller ${
          measurement.caller_depth === 1 ? 'hop' : 'hops'
        } · ${measurement.test_files.length} ${
          measurement.test_files.length === 1 ? 'file' : 'files'
        }`}
      />
      {measurement.tests.length > 0 ? (
        <ul className="flex flex-col gap-0.5">
          {measurement.tests.slice(0, 6).map((test) => (
            <li key={test.id} className="flex items-baseline gap-1.5 text-3xs">
              <span className="td-value min-w-0 flex-1 truncate text-text-secondary">
                {test.name}
              </span>
              <span className="td-legend shrink-0">{test.qualification}</span>
            </li>
          ))}
          {measurement.tests.length > 6 ? (
            <li className="text-3xs text-text-muted">
              and {measurement.tests.length - 6} more
            </li>
          ) : null}
        </ul>
      ) : null}
    </div>
  );
}

/**
 * Sessions.
 *
 * The granularity disclosure leads, because the number underneath it is a
 * FILE's session count and the surface it sits on is a SYMBOL. Reversing that
 * order would be the falsification.
 */
function SessionsReading({ measurement }: { measurement: NodeSessionsMeasurementV1 }) {
  const { linkage } = measurement;
  return (
    <div className="flex flex-col gap-1.5">
      {!measurement.symbol_granularity_available ? (
        <p className="text-3xs leading-snug text-state-unknown">
          resolved at <strong className="font-medium">{linkage.granularity}</strong>{' '}
          granularity, not per symbol — {measurement.symbol_granularity_reason}
        </p>
      ) : null}
      <Count
        value={linkage.matched_sessions}
        unit={linkage.matched_sessions === 1 ? 'session' : 'sessions'}
        qualifier={`touched ${
          measurement.symbol_granularity_available
            ? 'this symbol'
            : elideStart(measurement.node.file_path, 34)
        } · of ${linkage.eligible_sessions.toLocaleString()} eligible · ${linkage.authority}`}
      />
      {linkage.providers.length > 0 ? (
        <p className="text-3xs leading-snug text-text-muted">
          providers: {linkage.providers.join(', ')}
        </p>
      ) : null}
      {measurement.available_granularities.length > 0 ? (
        <p className="text-3xs leading-snug text-text-muted">
          available granularities: {measurement.available_granularities.join(', ')}
        </p>
      ) : null}
    </div>
  );
}
