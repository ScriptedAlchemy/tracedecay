/**
 * The Cortex inspector: symbols read against every authority the Code
 * workspace has, each in its own state.
 *
 * Two identities can be on the panel at once and they are never confused:
 *
 *   PINNED    the URL selection. It earns the neighbours read, so callers and
 *             callees are listed for it, and the Trace / Shared Code actions
 *             act on it.
 *   PREVIEW   the symbol the pointer or focus is on in the field or ledger.
 *             Inspection only: identity, position, layering, and how it is
 *             drawn against the pinned symbol, all from what is already in
 *             hand, because a pointer crossing eighty bodies must not cost
 *             eighty requests. It stacks ABOVE the pinned block rather than
 *             replacing it, so a hover never takes the selection's evidence
 *             off the glass.
 *
 * Rows inside this panel (the callers and callees) highlight their body on the
 * field when hovered or focused; they do not open a preview here, because a
 * preview that replaced the list under the pointer would unmount the row being
 * hovered and re-fire the hover when it came back.
 *
 * Every section is an independent authority and reports its own state:
 *
 *   identity       the graph row the field or list handed over
 *   strata         `GET /api/plugins/graph/strata`, the file's dependency depth
 *   callers/callees `GET /api/plugins/graph/node/{id}/neighbors`, one row per call site
 *   diagnostics    `GET /api/plugins/code-diagnostics`, engines, and rows in this file
 *   graph          the subgraph envelope's version and observation time
 *   index          `GET /api/code-index/freshness`, what generation this is a picture of
 *
 * A section that cannot answer prints the reason in its place. Nothing here
 * turns a missing authority into an empty list or a green zero.
 */
import type { ReactNode } from 'react';
import { Copy, Waypoints } from 'lucide-react';

import type {
  CodeIndexFreshnessPayloadV1,
  CodeIndexWorktreeFreshnessV1,
  DashboardDomainStateV1,
  DashboardEnvelopeV1,
  GraphEdgeV1,
  GraphNodeV1,
  GraphSubgraphPayloadV1,
  StrataMeasurementV1,
} from '../../contracts/generated.ts';
import { StructureReadV12Schema as StrataReadSchema } from '../../contracts/generated.ts';
import { useCodeDiagnostics, type EngineStatus } from '../../data/query/codeDiagnostics.ts';
import type { EnvelopeResult } from '../../data/query/envelope.ts';
import { absenceReason, useStructure, type StructureResult } from '../../data/query/structure.ts';
import { InspectorPanel } from '../../ui/archetypes/ExplorerSplit.tsx';
import { cn } from '../../ui/cn';
import { elideStart, formatMicrosUtc } from '../../ui/format.ts';
import { StateChip } from '../../ui/StateChip.tsx';
import { kindColorVars } from '../../viz/graph/kindColor.ts';
import { ENGINE_CHIP } from './CodeDiagnostics.tsx';
import {
  diagnosticsForFile,
  displayName,
  groupNeighbors,
  locationLabel,
  relationsBetween,
  strataForPath,
  type DistinctNeighbor,
  type FileDiagnostics,
  type RelationSide,
  type StrataReading,
} from './cortex.ts';
import { useIndexFreshness } from './IndexFreshness.tsx';
import { useSymbolNeighbors } from './traceNeighborhood.ts';
import type { TraceFocus } from './TraceView.tsx';

const BASE = '/api/plugins/graph';

/** The state a neighbours reading is in, or its rows. */
type NeighborsReading =
  | { kind: 'not_read'; reason: string }
  | { kind: 'loading' }
  | { kind: 'blocked'; state: DashboardDomainStateV1; detail?: string | undefined }
  | { kind: 'measured'; callers: RelationSide; callees: RelationSide };

export function CortexInspector({
  pinned,
  preview,
  drawnEdges,
  graphResult,
  tracing,
  onHighlight,
  onPin,
  onTrace,
  onSharedCode,
  onClose,
}: {
  /** The URL selection, or none. */
  pinned: TraceFocus | null;
  /** The symbol under the pointer or focus in the field or ledger, or none.
   * At least one of `pinned` and `preview` is present. */
  preview: TraceFocus | null;
  /** The drawn slice's edges, so a preview can say how it touches the selection. */
  drawnEdges: readonly GraphEdgeV1[];
  /** The subgraph read, whose envelope pins the graph version and time. */
  graphResult: EnvelopeResult<GraphSubgraphPayloadV1> | undefined;
  /** The Trace lens is already open on the pinned symbol. */
  tracing: boolean;
  /** A row in this panel is hovered or focused: isolate its body on the field. */
  onHighlight: (id: string | null) => void;
  onPin: (node: GraphNodeV1) => void;
  onTrace: (node: TraceFocus) => void;
  onSharedCode: (node: TraceFocus) => void;
  onClose: () => void;
}) {
  const subject = pinned ?? preview;
  const neighbors = useSymbolNeighbors(pinned?.id ?? '', { enabled: pinned !== null });
  const strata = useStructure<StrataMeasurementV1>(
    ['graph', 'strata'],
    `${BASE}/strata`,
    StrataReadSchema,
  );
  const freshness = useIndexFreshness();
  const diagnostics = useCodeDiagnostics();
  if (subject === null) return null;

  const neighborsReading: NeighborsReading =
    pinned === null
      ? { kind: 'not_read', reason: 'pin the symbol to read its callers and callees' }
      : neighbors.isPending
        ? { kind: 'loading' }
        : neighbors.data === undefined
          ? { kind: 'blocked', state: 'unknown' }
          : neighbors.data.outcome === 'transport'
            ? { kind: 'blocked', state: neighbors.data.state, detail: neighbors.data.detail }
            : {
                kind: 'measured',
                callers: groupNeighbors(
                  neighbors.data.envelope.payload.callers,
                  neighbors.data.envelope.payload.limit,
                ),
                callees: groupNeighbors(
                  neighbors.data.envelope.payload.callees,
                  neighbors.data.envelope.payload.limit,
                ),
              };
  const graphEnvelope = graphResult?.outcome === 'envelope' ? graphResult.envelope : undefined;
  const previewShown = preview !== null && preview.id !== pinned?.id;
  /** The symbol the actions and the diagnostics reading apply to. */
  const actionTarget = pinned ?? preview!;

  return (
    <InspectorPanel
      title={previewShown ? 'Inspecting' : 'Selection'}
      eyebrow={
        <>
          <span
            aria-hidden
            className={cn(
              'size-1.5 shrink-0 rounded-full',
              previewShown
                ? 'border border-accent/70 bg-transparent'
                : 'bg-accent shadow-[0_0_6px_var(--raw-accent)]',
            )}
          />
          <span data-inspect-mode={previewShown ? 'preview' : 'pinned'}>
            {previewShown ? 'hover preview · not pinned' : 'pinned · url identity'}
          </span>
        </>
      }
      onClose={onClose}
    >
      <div className="flex flex-col gap-3">
        {previewShown ? (
          <div className="flex flex-col gap-3" data-inspected={preview.id} data-inspect-block="preview">
            <SymbolName node={preview} tone="preview" />
            <IdentitySection node={preview} />
            {pinned !== null ? (
              <RelationToSelection
                relations={relationsBetween(drawnEdges, preview.id, pinned.id)}
                pinned={pinned}
              />
            ) : null}
            <Section label="strata">
              <StrataSection strata={strata} filePath={preview.file_path} />
            </Section>
            {pinned === null ? (
              <p className="text-2xs leading-relaxed text-text-muted">
                Click or Enter pins this symbol, re-seeds the field on its neighbourhood, and
                reads its callers and callees.
              </p>
            ) : null}
          </div>
        ) : null}

        {pinned !== null ? (
          <div
            className={cn(
              'flex flex-col gap-3',
              previewShown && 'border-t border-edge-subtle pt-3',
            )}
            data-inspected={pinned.id}
            data-inspect-block="pinned"
          >
            {previewShown ? (
              <div className="flex items-center gap-2">
                <span aria-hidden className="size-1.5 shrink-0 rounded-full bg-accent" />
                <span className="td-legend">selection · pinned</span>
                <span aria-hidden className="td-rule" />
              </div>
            ) : null}
            <SymbolName node={pinned} tone="pinned" />
            <div className="flex flex-wrap gap-1.5">
              <ActionButton
                icon={<Waypoints aria-hidden size={12} />}
                label={tracing ? 'Tracing this symbol' : 'Trace call topography'}
                disabled={tracing}
                onClick={() => onTrace(pinned)}
              />
              <ActionButton
                icon={<Copy aria-hidden size={12} />}
                label="Shared Code"
                onClick={() => onSharedCode(pinned)}
              />
            </div>
            {previewShown ? null : (
              <>
                <IdentitySection node={pinned} />
                <Section label="strata">
                  <StrataSection strata={strata} filePath={pinned.file_path} />
                </Section>
              </>
            )}
          </div>
        ) : null}

        <RelationSection
          label="callers"
          arrow="↑"
          reading={neighborsReading}
          side="callers"
          onHighlight={onHighlight}
          onPin={onPin}
        />
        <RelationSection
          label="callees"
          arrow="↓"
          reading={neighborsReading}
          side="callees"
          onHighlight={onHighlight}
          onPin={onPin}
        />

        <Section label="diagnostics">
          {diagnostics.isPending ? (
            <p className="text-2xs text-state-loading">reading the diagnostics broker…</p>
          ) : diagnostics.data === undefined ? (
            <Absent>no diagnostics response recorded</Absent>
          ) : diagnostics.data.outcome !== 'ok' ? (
            <StateChip
              kind={
                diagnostics.data.outcome === 'unavailable' ? 'unavailable' : diagnostics.data.outcome
              }
              detail={
                diagnostics.data.outcome === 'error'
                  ? diagnostics.data.detail
                  : diagnostics.data.outcome === 'unavailable'
                    ? (diagnostics.data.reason ?? diagnostics.data.status)
                    : undefined
              }
            />
          ) : (
            <DiagnosticsLines
              engines={diagnostics.data.data.engines}
              file={diagnosticsForFile(diagnostics.data.data, actionTarget.file_path)}
              fileName={actionTarget.file_path}
            />
          )}
        </Section>

        <Section label="graph generation">
          <GraphGeneration envelope={graphEnvelope} result={graphResult} />
        </Section>

        <Section label="from index">
          {freshness.isPending ? (
            <p className="text-2xs text-state-loading">reading scheduler state…</p>
          ) : freshness.data === undefined ? (
            <Absent>no freshness response recorded</Absent>
          ) : freshness.data.outcome === 'transport' ? (
            <StateChip kind={freshness.data.state} detail={freshness.data.detail ?? 'daemon unreachable'} />
          ) : (
            <IndexLine envelope={freshness.data.envelope} />
          )}
        </Section>
      </div>
    </InspectorPanel>
  );
}

/** The identifier keeps its own case: `td-title` uppercases, and an uppercased
 * symbol name is a different symbol. Mono, display size, a cyan gutter for the
 * pinned identity and a hairline one for a preview. */
function SymbolName({ node, tone }: { node: TraceFocus; tone: 'pinned' | 'preview' }) {
  return (
    <div
      className={cn(
        'flex min-w-0 items-center gap-2 border-l-2 pl-2',
        tone === 'pinned' ? 'border-accent' : 'border-edge-strong',
      )}
    >
      <span
        aria-hidden
        className="size-2 shrink-0 rounded-full bg-[var(--kind-dark)] [[data-theme=light]_&]:bg-[var(--kind-light)]"
        style={kindColorVars(node.kind)}
      />
      <h3
        className="td-value min-w-0 truncate text-base text-text-primary"
        title={node.qualified_name ?? displayName(node)}
      >
        {displayName(node)}
      </h3>
    </div>
  );
}

function IdentitySection({ node }: { node: TraceFocus }) {
  const location = locationLabel(node);
  return (
    <Section label="identity">
      <dl className="grid grid-cols-[minmax(4.5rem,auto)_1fr] gap-x-3 gap-y-1 text-2xs">
        <Term label="kind">{node.kind}</Term>
        <Term label="path" mono title={node.file_path ?? undefined}>
          {node.file_path ?? <Absent>no path served for this symbol</Absent>}
        </Term>
        <Term label="location" mono>
          {location ?? <Absent>no range served</Absent>}
        </Term>
        <Term label="module" mono title={node.qualified_name ?? undefined}>
          {node.qualified_name ?? <Absent>qualified name not served on this row</Absent>}
        </Term>
        <Term label="degree">
          {node.degree != null ? (
            `${node.degree.toLocaleString()} in + out`
          ) : (
            <Absent>connectedness not served on this row</Absent>
          )}
        </Term>
      </dl>
      {node.signature ? (
        <pre className="overflow-x-auto border border-edge-subtle bg-surface-2 p-2 font-mono text-2xs leading-relaxed">
          {node.signature}
        </pre>
      ) : null}
    </Section>
  );
}

function RelationToSelection({
  relations,
  pinned,
}: {
  relations: ReturnType<typeof relationsBetween>;
  pinned: TraceFocus;
}) {
  return (
    <Section label="relation to selection">
      {relations.length === 0 ? (
        <p className="text-2xs text-text-muted">
          no drawn relation between this and{' '}
          <span className="td-value text-text-secondary">{displayName(pinned)}</span> on the
          slice
        </p>
      ) : (
        <ul className="flex flex-col gap-0.5 text-2xs" aria-label="Drawn relations to the selection">
          {relations.map((relation, index) => (
            <li key={`${relation.kind}:${relation.direction}:${index}`} className="flex items-baseline gap-1.5">
              <span className="td-value text-text-primary">{relation.kind}</span>
              <span className="text-text-muted">
                {relation.direction === 'out' ? '→' : '←'} {displayName(pinned)}
              </span>
              {relation.line != null ? (
                <span className="td-value ml-auto text-3xs text-text-muted">line {relation.line}</span>
              ) : null}
            </li>
          ))}
        </ul>
      )}
    </Section>
  );
}

function StrataSection({
  strata,
  filePath,
}: {
  strata: { isPending: boolean; data: StructureResult<StrataMeasurementV1> | undefined };
  filePath: string | null | undefined;
}) {
  if (strata.isPending) {
    return <p className="text-2xs text-state-loading">scanning the dependency layering…</p>;
  }
  if (strata.data === undefined) return <Absent>no layering response recorded</Absent>;
  if (strata.data.outcome !== 'measured') return <Absent>{absenceReason(strata.data)}</Absent>;
  return <StrataLine reading={strataForPath(strata.data.measurement, filePath)} />;
}

function ActionButton({
  icon,
  label,
  disabled,
  onClick,
}: {
  icon: ReactNode;
  label: string;
  disabled?: boolean;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={disabled}
      className="flex min-h-[var(--touch-target-min)] flex-1 items-center justify-center gap-1.5 border border-edge-subtle bg-surface-1 px-2 py-1 text-2xs uppercase tracking-[0.08em] text-text-secondary hover:border-accent/60 hover:bg-surface-2 hover:text-text-primary focus-visible:outline focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-accent disabled:cursor-default disabled:text-text-muted disabled:hover:border-edge-subtle disabled:hover:bg-surface-1"
    >
      {icon}
      {label}
    </button>
  );
}

function Section({ label, children }: { label: string; children: ReactNode }) {
  return (
    <section aria-label={label} className="flex flex-col gap-1.5" data-inspector-section={label}>
      <div className="flex items-center gap-2">
        <h3 className="td-legend shrink-0">{label}</h3>
        <span aria-hidden className="td-rule" />
      </div>
      {children}
    </section>
  );
}

function Term({
  label,
  children,
  mono,
  title,
}: {
  label: string;
  children: ReactNode;
  mono?: boolean;
  title?: string | undefined;
}) {
  return (
    <>
      <dt className="td-legend pt-px">{label}</dt>
      <dd className={cn('min-w-0 break-words text-text-secondary', mono && 'td-value')} title={title}>
        {children}
      </dd>
    </>
  );
}

/** A named absence, in the dashed-unknown register rather than as blank. */
function Absent({ children }: { children: ReactNode }) {
  return (
    <span className="text-2xs leading-relaxed text-state-unknown" data-absent>
      {children}
    </span>
  );
}

function StrataLine({ reading }: { reading: StrataReading }) {
  switch (reading.kind) {
    case 'measured':
      return (
        <div className="flex flex-col gap-0.5 text-2xs" data-strata={reading.depth}>
          <span className="flex items-baseline gap-1.5">
            <span className="td-value text-sm text-text-primary" data-cell="numeric">
              {reading.depth}
            </span>
            <span className="td-unit">
              {reading.capped ? 'or deeper' : `of ${reading.maxDepth} deep · ${reading.idealDepth} ideal`}
            </span>
          </span>
          <span className="td-value truncate text-3xs text-text-muted" title={reading.directory}>
            {reading.directory === '.' ? './' : reading.directory}
            {reading.sccSize > 1 ? ` · in a ${reading.sccSize}-file cycle` : ''}
          </span>
        </div>
      );
    case 'directory_only':
      return (
        <Absent>
          this file is not in the layering scan; its directory{' '}
          <span className="td-value">{reading.directory}</span> sits at depth{' '}
          {reading.depths.join(', ')}
          {reading.capped ? ', the scan stopped at its budget' : ''}
        </Absent>
      );
    case 'not_in_scan':
      return (
        <Absent>
          not in the layering scan ({reading.filesLaidOut.toLocaleString()} files laid out
          {reading.capped ? ', budget reached' : ''})
        </Absent>
      );
    case 'no_path':
      return <Absent>no path served, so no layer can be looked up</Absent>;
    default: {
      const unhandled: never = reading;
      return unhandled;
    }
  }
}

/** How many neighbours a section prints before deferring to Trace. */
const SHOWN_NEIGHBORS = 6;

function RelationSection({
  label,
  arrow,
  reading,
  side,
  onHighlight,
  onPin,
}: {
  label: string;
  arrow: string;
  reading: NeighborsReading;
  side: 'callers' | 'callees';
  onHighlight: (id: string | null) => void;
  onPin: (node: GraphNodeV1) => void;
}) {
  const measured = reading.kind === 'measured' ? reading[side] : null;
  return (
    <section
      aria-label={label}
      className="flex flex-col gap-1.5"
      data-inspector-section={label}
      data-relation-state={reading.kind}
    >
      <div className="flex items-center gap-2">
        <h3 className="td-legend shrink-0">
          {label} <span aria-hidden>{arrow}</span>
        </h3>
        <span aria-hidden className="td-rule" />
        {measured ? (
          <span className="td-value shrink-0 text-2xs text-text-primary" data-cell="numeric">
            {measured.capped ? '≥ ' : ''}
            {measured.distinct.toLocaleString()}
          </span>
        ) : null}
      </div>
      {reading.kind === 'not_read' ? (
        <Absent>{reading.reason}</Absent>
      ) : reading.kind === 'loading' ? (
        <p className="text-2xs text-state-loading">reading the neighbourhood…</p>
      ) : reading.kind === 'blocked' ? (
        <StateChip kind={reading.state} detail={reading.detail} />
      ) : measured === null ? null : measured.distinct === 0 ? (
        <p className="text-2xs text-text-muted">
          0 {label} in this generation
        </p>
      ) : (
        <>
          <ul
            className="flex flex-col"
            aria-label={`${label} of the selected symbol`}
            onPointerLeave={() => onHighlight(null)}
          >
            {measured.neighbors.slice(0, SHOWN_NEIGHBORS).map((neighbor) => (
              <NeighborRow
                key={neighbor.node.id}
                neighbor={neighbor}
                onHighlight={onHighlight}
                onPin={onPin}
              />
            ))}
          </ul>
          <p className="text-3xs leading-relaxed text-text-muted">
            {measured.distinct > SHOWN_NEIGHBORS
              ? `+ ${(measured.distinct - SHOWN_NEIGHBORS).toLocaleString()} more · `
              : ''}
            {measured.sites.toLocaleString()} call {measured.sites === 1 ? 'site' : 'sites'}
            {measured.capped
              ? ` listed, the read stopped at its ${measured.limit.toLocaleString()}-row budget, so more may exist`
              : ' listed'}
          </p>
        </>
      )}
    </section>
  );
}

function NeighborRow({
  neighbor,
  onHighlight,
  onPin,
}: {
  neighbor: DistinctNeighbor;
  onHighlight: (id: string | null) => void;
  onPin: (node: GraphNodeV1) => void;
}) {
  const { node } = neighbor;
  const file = node.file_path ? node.file_path.slice(node.file_path.lastIndexOf('/') + 1) : null;
  const firstLine = neighbor.lines[0];
  return (
    <li>
      <button
        type="button"
        onClick={() => onPin(node)}
        onPointerEnter={() => onHighlight(node.id)}
        onFocus={() => onHighlight(node.id)}
        onBlur={() => onHighlight(null)}
        data-neighbor={node.id}
        title={node.qualified_name ?? displayName(node)}
        className={cn(
          'flex min-h-[var(--touch-target-min)] w-full items-center gap-2 border-b border-edge-subtle/70 px-1 text-left text-2xs',
          'hover:bg-surface-2 focus-visible:bg-surface-2 focus-visible:outline focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-accent',
        )}
      >
        <span
          aria-hidden
          className="size-1.5 shrink-0 rounded-full bg-[var(--kind-dark)] [[data-theme=light]_&]:bg-[var(--kind-light)]"
          style={kindColorVars(node.kind)}
        />
        <span className="flex min-w-0 flex-1 flex-col leading-tight">
          <span className="td-value truncate text-text-primary">{displayName(node)}</span>
          <span className="td-value truncate text-3xs text-text-muted" title={node.file_path ?? undefined}>
            {file ? `${file}${firstLine != null ? `:${firstLine}` : ''}` : 'no path served'}
            {' · '}
            {node.kind}
          </span>
        </span>
        <span className="td-value shrink-0 text-3xs text-text-secondary" data-cell="numeric">
          {neighbor.sites.toLocaleString()}
          <span className="td-unit ml-1">{neighbor.sites === 1 ? 'site' : 'sites'}</span>
        </span>
      </button>
    </li>
  );
}

function GraphGeneration({
  envelope,
  result,
}: {
  envelope: DashboardEnvelopeV1<GraphSubgraphPayloadV1> | undefined;
  result: EnvelopeResult<GraphSubgraphPayloadV1> | undefined;
}) {
  if (result === undefined) return <Absent>the graph slice has not answered yet</Absent>;
  if (result.outcome === 'transport') {
    return <StateChip kind={result.state} detail={result.detail} />;
  }
  if (!envelope) return <Absent>no graph envelope recorded</Absent>;
  return (
    <dl className="grid grid-cols-[minmax(4.5rem,auto)_1fr] gap-x-3 gap-y-1 text-2xs">
      <Term label="version" mono title={envelope.version.graph_version ?? undefined}>
        {envelope.version.graph_version ?? <Absent>not pinned by the envelope</Absent>}
      </Term>
      <Term label="observed" mono>
        {formatMicrosUtc(envelope.time.observation_time_micros)}
      </Term>
      <Term label="freshness">
        <span data-graph-freshness={envelope.freshness.state}>{envelope.freshness.state}</span>
        {envelope.freshness.watermark ? (
          <span className="td-value ml-1.5 text-3xs text-text-muted">{envelope.freshness.watermark}</span>
        ) : null}
      </Term>
    </dl>
  );
}

function IndexLine({ envelope }: { envelope: DashboardEnvelopeV1<CodeIndexFreshnessPayloadV1> }) {
  const { worktrees, note } = envelope.payload;
  return (
    <div className="flex flex-col gap-1.5" data-index-state={envelope.domain_state}>
      <StateChip kind={envelope.domain_state} />
      {worktrees.length === 0 ? (
        <p className="text-3xs leading-snug text-text-muted">{note}</p>
      ) : (
        <dl className="grid grid-cols-[minmax(4.5rem,auto)_1fr] gap-x-3 gap-y-1 text-2xs">
          {worktrees.map((worktree) => (
            <WorktreeTerms key={worktree.worktree_root} worktree={worktree} />
          ))}
        </dl>
      )}
    </div>
  );
}

function WorktreeTerms({ worktree }: { worktree: CodeIndexWorktreeFreshnessV1 }) {
  return (
    <>
      <Term label="staleness">
        <span data-worktree-staleness={worktree.staleness_state ?? 'unreported'}>
          {worktree.staleness_state ?? <Absent>not reported</Absent>}
        </span>
      </Term>
      <Term label="sealed" mono>
        {formatMicrosUtc(worktree.sealed_at_micros, { nullAs: 'not reported' })}
      </Term>
      <Term label="source" mono title={worktree.source_reference ?? undefined}>
        {worktree.source_reference ?? <Absent>not reported by the scheduler</Absent>}
      </Term>
      <Term label="generation" mono title={worktree.latest_generation_id ?? undefined}>
        {worktree.latest_generation_id ?? <Absent>no sealed generation yet</Absent>}
      </Term>
      <Term label="worktree" mono title={worktree.worktree_root}>
        {elideStart(worktree.worktree_root, 32)}
      </Term>
    </>
  );
}

function DiagnosticsLines({
  engines,
  file,
  fileName,
}: {
  engines: readonly EngineStatus[];
  file: FileDiagnostics | null;
  fileName: string | null | undefined;
}) {
  const anyReady = engines.some((engine) => engine.state === 'ready');
  const shortFile = fileName ? fileName.slice(fileName.lastIndexOf('/') + 1) : null;
  return (
    <div className="flex flex-col gap-1.5">
      {engines.length === 0 ? (
        <Absent>no diagnostic engines are mounted for this project</Absent>
      ) : (
        <ul className="flex flex-col gap-0.5" aria-label="Diagnostic engines">
          {engines.map((engine) => (
            <li key={engine.language} className="flex items-center gap-2 text-2xs">
              <StateChip kind={ENGINE_CHIP[engine.state]} detail={engine.last_error ?? undefined} />
              <span className="text-text-primary">{engine.language}</span>
            </li>
          ))}
        </ul>
      )}
      {file === null ? (
        <Absent>no path served, so no diagnostics can be matched to this symbol</Absent>
      ) : file.rows.length > 0 ? (
        <p className="text-2xs" data-file-diagnostics={file.rows.length}>
          <span className={cn(file.errors > 0 && 'text-state-error')}>{file.errors} errors</span>
          {' · '}
          <span className={cn(file.warnings > 0 && 'text-state-partial')}>{file.warnings} warnings</span>
          <span className="text-text-muted"> in {shortFile}</span>
        </p>
      ) : anyReady ? (
        <p className="text-2xs text-text-muted" data-file-diagnostics={0}>
          the ready engines report no diagnostics in {shortFile}
        </p>
      ) : (
        <Absent>no engine is ready, so {shortFile} has no diagnostic reading</Absent>
      )}
    </div>
  );
}
