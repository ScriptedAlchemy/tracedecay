import type { ReactNode } from 'react';
import type {
  AnalyticsDiagnosticsPayloadV1,
  AnalyticsHintsPayloadV1,
  CodeIndexBuildProgressV1,
  CodeIndexFreshnessPayloadV1,
  CodeIndexWorktreeFreshnessV1,
  DoctorEvidenceStateV1,
  DoctorFindingsPayloadV1,
  ExecutionTopologyMetricsV1,
  ObservatoryReadModelV1,
  StorageTelemetryPayloadV1,
} from '../../contracts/generated.ts';
import { envelopePayload } from '../../data/query/useEnvelope.ts';
import { cn } from '../../ui/cn.ts';
import { MeterRow } from '../../ui/instrument.tsx';
import { humanizeMetric } from '../../ui/metricModel.ts';
import type { DomainStateKind } from '../../ui/StateChip.tsx';
import type { WorkResult } from '../work/workApi.ts';
import { adoptionCoverageBands, coverageAnchors } from './adoptionCoverage.ts';
import {
  analyticsModeReading,
  egressFailureReading,
  retentionBacklogReading,
  shareStagingReading,
} from './analyticsControls.ts';
import { codeIndexPhaseLabel, codeIndexProgressPercentage, formatDurationMicros, graphServingLabel } from './CodeIndexPipeline.tsx';
import { cloneIndexState } from './cloneIndexModel.ts';
import { doctorEvidencePresentation, doctorFamilyLabel } from './doctorModel.ts';
import { BlockedBody, EvidenceChip } from './EvidencePanel.tsx';
import {
  diagnosticsWindowWord,
  evidenceStateOf,
  type EvidenceRead,
  type EvidenceSummary,
} from './evidence.ts';
import { budgetAnchors, performanceBudgetBands } from './performanceBudgets.ts';
import {
  planDimensionPresentation,
  type PlanDimension,
  type ReadAnchors,
} from './planDimension.ts';
import { retrievalAnchors, retrievalQualityBands } from './retrievalQuality.ts';
import { formatBytes, storageFindingLabel } from './storageModel.ts';

/**
 * The compact read-out inside each overview panel.
 *
 * Every body draws the instrument the authority actually supports and nothing
 * more: a dimension the projector did not publish is a labelled row with an
 * em dash and an empty track, never a bar at zero; a read that produced no
 * payload is the daemon's word, not a skeleton that looks like data.
 */

function Rows({ children, label }: { children: ReactNode; label: string }) {
  return (
    <div className="flex flex-col gap-1" role="list" aria-label={label}>
      {children}
    </div>
  );
}

function Row({
  label,
  value,
  fraction,
  tone,
  title,
  attrs,
  figureWidth = 'wide',
}: {
  label: ReactNode;
  value: ReactNode;
  fraction: number | null;
  tone?: string;
  title?: string;
  attrs?: Record<string, string>;
  figureWidth?: 'standard' | 'wide' | 'byte';
}) {
  return (
    <div role="listitem" {...attrs}>
      <MeterRow
        label={label}
        value={value}
        fraction={fraction}
        tone={tone}
        title={title}
        figureWidth={figureWidth}
      />
    </div>
  );
}

function Kicker({ children, tone }: { children: ReactNode; tone?: string }) {
  return (
    <p className={cn('td-legend mb-1 flex items-center gap-1.5', tone)}>
      <span aria-hidden className={cn('size-1.5 shrink-0', tone ? tone.replace('text-', 'bg-') : 'bg-edge-strong')} />
      {children}
    </p>
  );
}

// ---------------------------------------------------------------------------
// Plan 26 dimension bands (adoption, retrieval, budgets)
// ---------------------------------------------------------------------------

function dimensionRows(
  dimensions: readonly PlanDimension[],
  anchors: ReadAnchors,
  limit: number,
): ReactNode {
  return dimensions.slice(0, limit).map((dimension) => {
    const presentation = planDimensionPresentation(dimension, anchors);
    const reading = dimension.reading;
    const fraction =
      reading.kind === 'measured' && reading.metric.unit === 'ratio' && reading.metric.value != null
        ? reading.metric.value
        : null;
    const value =
      presentation.available && presentation.unit ? (
        <>
          {presentation.figure}
          <span className="td-unit ml-1">{presentation.unit}</span>
        </>
      ) : (
        presentation.figure
      );
    return (
      <Row
        key={dimension.id}
        label={dimension.label}
        value={value}
        fraction={fraction}
        title={presentation.reason ?? presentation.requirement}
        attrs={{ 'data-dimension': dimension.id, 'data-dimension-state': presentation.state }}
        figureWidth="byte"
      />
    );
  });
}

export function AdoptionBody({
  summary,
  observatory,
  diagnostics,
}: {
  summary: EvidenceSummary;
  observatory: EvidenceRead<ObservatoryReadModelV1>;
  diagnostics: EvidenceRead<AnalyticsDiagnosticsPayloadV1>;
}) {
  const model = envelopePayload(observatory.result);
  if (!model) return <BlockedBody summary={summary} />;
  const bands = adoptionCoverageBands(model);
  const anchors = coverageAnchors(model);
  const window = diagnosticsWindowWord(diagnostics);
  return (
    <>
      <Rows label="Adoption dimensions">
        {dimensionRows(bands.flatMap((band) => band.dimensions), anchors, 6)}
      </Rows>
      <p className="mt-2 flex flex-wrap items-center gap-1.5 text-sm text-text-muted">
        <span className="td-legend">record counts</span>
        <EvidenceChip state={window.state} detail={window.detail} />
      </p>
    </>
  );
}

export function RetrievalBody({
  summary,
  observatory,
  diagnostics,
}: {
  summary: EvidenceSummary;
  observatory: EvidenceRead<ObservatoryReadModelV1>;
  diagnostics: EvidenceRead<AnalyticsDiagnosticsPayloadV1>;
}) {
  const model = envelopePayload(observatory.result);
  if (!model) return <BlockedBody summary={summary} />;
  const bands = retrievalQualityBands(model);
  const anchors = retrievalAnchors(model);
  const window = diagnosticsWindowWord(diagnostics);
  return (
    <>
      <Rows label="Retrieval dimensions">
        {dimensionRows(bands.flatMap((band) => band.dimensions), anchors, 6)}
      </Rows>
      <p className="mt-2 flex flex-wrap items-center gap-1.5 text-sm text-text-muted">
        <span className="td-legend">record counts</span>
        <EvidenceChip state={window.state} detail={window.detail} />
      </p>
      <p className="mt-1 text-sm text-text-muted">
        no time series is published for these dimensions · figures are the current read only
      </p>
    </>
  );
}

export function BudgetsBody({
  summary,
  observatory,
}: {
  summary: EvidenceSummary;
  observatory: EvidenceRead<ObservatoryReadModelV1>;
}) {
  const model = envelopePayload(observatory.result);
  if (!model) return <BlockedBody summary={summary} />;
  const anchors = budgetAnchors(model);
  const dimensions = performanceBudgetBands(model).flatMap((band) => band.dimensions).slice(0, 7);
  return (
    <>
      <table className="w-full border-collapse text-sm" aria-label="Budget ledger">
        <thead>
          <tr className="td-legend border-b border-edge-subtle text-left">
            <th scope="col" className="py-0.5 pr-2 font-normal">
              budget
            </th>
            <th scope="col" className="py-0.5 pr-2 text-right font-normal">
              current
            </th>
            <th scope="col" className="py-0.5 text-right font-normal">
              state
            </th>
          </tr>
        </thead>
        <tbody>
          {dimensions.map((dimension) => {
            const presentation = planDimensionPresentation(dimension, anchors);
            return (
              <tr
                key={dimension.id}
                className="border-b border-edge-subtle last:border-b-0"
                data-dimension={dimension.id}
                data-dimension-state={presentation.state}
              >
                <th scope="row" className="truncate py-0.5 pr-2 text-left font-normal text-text-primary">
                  {dimension.label}
                </th>
                <td className="td-value py-0.5 pr-2 text-right text-text-secondary" data-cell="numeric">
                  {presentation.figure}
                  {presentation.unit ? <span className="td-unit ml-1">{presentation.unit}</span> : null}
                </td>
                <td className="td-legend py-0.5 text-right">{presentation.state.replaceAll('_', ' ')}</td>
              </tr>
            );
          })}
        </tbody>
      </table>
      <p className="mt-2 text-sm text-text-muted">
        comparison · {model.comparison.disposition.replaceAll('_', ' ')} · no budget threshold or
        7-day baseline is published, so no delta is drawn
      </p>
    </>
  );
}

// ---------------------------------------------------------------------------
// Doctor
// ---------------------------------------------------------------------------

export function DoctorBody({
  summary,
  doctor,
  selectedFinding,
  onSelectFinding,
}: {
  summary: EvidenceSummary;
  doctor: EvidenceRead<DoctorFindingsPayloadV1>;
  selectedFinding: number | null;
  onSelectFinding: (index: number) => void;
}) {
  const payload = envelopePayload(doctor.result);
  if (!payload) return <BlockedBody summary={summary} />;
  const families = payload.report_coverage?.families ?? [];
  return (
    <>
      <Rows label="Doctor finding families">
        {payload.known_families.map((family) => {
          const entries = payload.entries.filter((entry) => entry.finding.family === family);
          const consultation = families.find((entry) => entry.family === family)?.consultation;
          const word =
            consultation?.status === 'unavailable'
              ? `unavailable · ${consultation.reason.replaceAll('_', ' ')}`
              : consultation?.status === 'consulted'
                ? entries.length === 0
                  ? 'consulted · no findings'
                  : `${entries.length} finding${entries.length === 1 ? '' : 's'}`
                : payload.report_coverage == null
                  ? 'coverage not published'
                  : 'not consulted';
          return (
            <div
              key={family}
              role="listitem"
              className="flex items-baseline justify-between gap-2 text-body"
              data-doctor-family={family}
              data-doctor-family-consultation={consultation?.status ?? 'unpublished'}
            >
              <span className="truncate text-text-primary">{doctorFamilyLabel(family)}</span>
              <span className="td-legend shrink-0 text-right">{word}</span>
            </div>
          );
        })}
      </Rows>
      {payload.entries.length > 0 ? (
        <FindingRows
          label="Doctor findings"
          entries={payload.entries.map((entry, index) => ({
            index,
            title: `${doctorFamilyLabel(entry.finding.family)}${entry.storage_kind ? ` · ${storageFindingLabel(entry.storage_kind)}` : ''}`,
            state: entry.finding.state,
            citations: entry.finding.evidence.length,
          }))}
          selected={selectedFinding}
          onSelect={onSelectFinding}
        />
      ) : null}
      {payload.schema_convergences.length > 0 ? (
        <p className="mt-2 text-sm text-text-muted">
          {payload.schema_convergences.length} schema convergence
          {payload.schema_convergences.length === 1 ? '' : 's'} reported
        </p>
      ) : null}
    </>
  );
}

/** Rows shown on the overview; the rest are in the exact evidence. */
const FINDING_ROW_LIMIT = 4;

/** Finding rows are real controls above the panel's select overlay: choosing
 * one narrows the inspector to that finding without changing which panel is
 * selected. */
function FindingRows({
  label,
  entries,
  selected,
  onSelect,
}: {
  label: string;
  entries: readonly {
    index: number;
    title: string;
    state: DoctorEvidenceStateV1;
    citations: number;
  }[];
  selected: number | null;
  onSelect: (index: number) => void;
}) {
  return (
    <ul className="relative z-[1] mt-2 flex flex-col gap-0.5" aria-label={label}>
      {entries.slice(0, FINDING_ROW_LIMIT).map((entry) => {
        const presentation = doctorEvidencePresentation(entry.state);
        const isSelected = selected === entry.index;
        return (
          <li key={entry.index}>
            <button
              type="button"
              data-evidence-finding={entry.index}
              aria-pressed={isSelected}
              className={cn(
                'td-hit flex w-full items-center gap-2 border px-2 text-left text-body',
                isSelected
                  ? 'border-edge-strong bg-surface-3'
                  : 'border-transparent hover:border-edge-subtle hover:bg-surface-2',
              )}
              onClick={() => onSelect(entry.index)}
            >
              <span aria-hidden className={cn('size-1.5 shrink-0', presentation.dotClass)} />
              <span className="min-w-0 flex-1 truncate text-text-primary">{entry.title}</span>
              <span className={cn('td-legend shrink-0', presentation.tokenClass)}>
                {presentation.label}
              </span>
              <span className="td-legend shrink-0 tabular">{entry.citations} cit.</span>
              <span aria-hidden className="text-text-muted">{isSelected ? '→' : ''}</span>
            </button>
          </li>
        );
      })}
      {entries.length > FINDING_ROW_LIMIT ? (
        <li className="td-legend px-2 pt-1">
          {entries.length - FINDING_ROW_LIMIT} more in exact evidence
        </li>
      ) : null}
    </ul>
  );
}

// ---------------------------------------------------------------------------
// Code-index pipeline
// ---------------------------------------------------------------------------

const PHASES: readonly CodeIndexBuildProgressV1['phase'][] = [
  'source_scan',
  'relational_preparation',
  'bulk_commit',
  'index_build',
  'verification',
  'ready',
];

export function PipelineBody({
  summary,
  freshness,
}: {
  summary: EvidenceSummary;
  freshness: EvidenceRead<CodeIndexFreshnessPayloadV1>;
}) {
  const payload = envelopePayload(freshness.result);
  if (!payload) return <BlockedBody summary={summary} />;
  if (payload.worktrees.length === 0) {
    return (
      <p className="text-body text-text-muted" data-evidence-blocked="empty">
        no mounted code-index worktree · the stage rail has nothing to place
      </p>
    );
  }
  return (
    <div className="flex flex-col gap-2" role="list" aria-label="Code-index worktrees">
      {payload.worktrees.slice(0, 4).map((worktree) => (
        <WorktreeRail key={worktree.worktree_root} worktree={worktree} />
      ))}
      {payload.worktrees.length > 4 ? (
        <p className="td-legend">{payload.worktrees.length - 4} more worktrees in exact evidence</p>
      ) : null}
    </div>
  );
}

function WorktreeRail({ worktree }: { worktree: CodeIndexWorktreeFreshnessV1 }) {
  const progress = worktree.progress;
  const reached = progress
    ? PHASES.indexOf(progress.phase)
    : worktree.latest_generation_id
      ? PHASES.length - 1
      : -1;
  const percent = progress ? codeIndexProgressPercentage(progress) : null;
  const clone = worktree.clone_index ? cloneIndexState(worktree.clone_index) : null;
  return (
    <div
      role="listitem"
      className="flex flex-col gap-1.5"
      data-pipeline-worktree={worktree.worktree_root}
      data-pipeline-phase={progress?.phase ?? (worktree.latest_generation_id ? 'sealed' : 'none')}
    >
      <p className="flex items-baseline justify-between gap-2 text-body">
        <span className="min-w-0 truncate font-mono text-text-secondary" title={worktree.worktree_root}>
          {worktree.worktree_root}
        </span>
        <span className="td-legend shrink-0">
          {progress
            ? `${codeIndexPhaseLabel(progress.phase)} · ${percent?.toFixed(1)}%`
            : worktree.latest_generation_id
              ? `sealed · ${worktree.staleness_state ?? 'unknown'}`
              : 'no sealed generation'}
        </span>
      </p>
      <ol className="grid grid-cols-6 gap-px" aria-label="Build stages">
        {PHASES.map((phase, index) => {
          const done = reached > index || (reached === index && phase === 'ready' && !progress);
          const active = progress != null && reached === index;
          return (
            <li
              key={phase}
              className="flex flex-col gap-1"
              data-stage={phase}
              data-stage-state={done ? 'done' : active ? 'active' : 'pending'}
            >
              <span
                aria-hidden
                className={cn(
                  'h-[3px] w-full',
                  done ? 'bg-accent' : active ? 'bg-alert' : 'bg-surface-3',
                )}
              />
              <span
                className={cn(
                  'td-legend truncate',
                  done ? 'text-text-secondary' : active ? 'text-alert' : undefined,
                )}
              >
                {codeIndexPhaseLabel(phase)}
              </span>
            </li>
          );
        })}
      </ol>
      <dl className="grid grid-cols-2 gap-x-3 gap-y-0.5 text-sm sm:grid-cols-4">
        <dt className="text-text-muted">files</dt>
        <dd className="td-value text-right text-text-secondary" data-cell="numeric">
          {progress ? `${progress.completed_files.toLocaleString()} / ${progress.total_files.toLocaleString()}` : '—'}
        </dd>
        <dt className="text-text-muted">eta</dt>
        <dd className="td-value text-right text-text-secondary" data-cell="numeric">
          {progress?.estimated_remaining_seconds != null
            ? formatDurationMicros(progress.estimated_remaining_seconds * 1_000_000)
            : '—'}
        </dd>
        <dt className="text-text-muted">graph</dt>
        <dd className="truncate text-right text-text-secondary">
          {graphServingLabel(worktree.code_graph_serving)}
        </dd>
        <dt className="text-text-muted">clone</dt>
        <dd className="truncate text-right text-text-secondary">
          {clone ? clone.replaceAll('_', ' ') : 'not published'}
        </dd>
      </dl>
      {progress?.blocked_reason ? (
        <p className="text-sm text-alert">blocked · {progress.blocked_reason.replaceAll('_', ' ')}</p>
      ) : null}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Hook hints / rejected arguments
// ---------------------------------------------------------------------------

export function HooksBody({
  summary,
  hints,
  observatory,
}: {
  summary: EvidenceSummary;
  hints: EvidenceRead<AnalyticsHintsPayloadV1>;
  observatory: EvidenceRead<ObservatoryReadModelV1>;
}) {
  const payload = envelopePayload(hints.result);
  const model = envelopePayload(observatory.result);
  const rejected = model?.rejected_arguments ?? null;
  const categories = payload?.available
    ? [...payload.by_category].sort((left, right) => right.emitted - left.emitted).slice(0, 4)
    : [];
  const maxEmitted = Math.max(0, ...categories.map((category) => category.emitted));
  return (
    <>
      <Kicker tone="text-accent">hook hints · emitted</Kicker>
      {!payload ? (
        <BlockedBody summary={summary} />
      ) : !payload.available ? (
        <p className="text-body text-text-muted">
          unavailable · {payload.error ?? 'the hint analytics source is unavailable'}
        </p>
      ) : categories.length === 0 ? (
        <p className="text-body text-text-muted">measured · no hook hints recorded in the window</p>
      ) : (
        <Rows label="Hook hint categories">
          {categories.map((category) => (
            <Row
              key={category.category}
              label={category.category}
              value={category.emitted.toLocaleString()}
              fraction={maxEmitted > 0 ? category.emitted / maxEmitted : null}
              title={`followed ${category.followed} · ignored ${category.ignored} · suppressed ${category.suppressed}`}
              attrs={{ 'data-hint-category': category.category }}
            />
          ))}
        </Rows>
      )}
      <Kicker tone="text-alert">rejected arguments</Kicker>
      {!rejected ? (
        <p className="text-body text-text-muted">canonical read model unavailable · no rejection figures</p>
      ) : rejected.rejected_total == null ? (
        <p className="text-body text-text-muted" data-rejected-arguments="unavailable">
          unavailable · {rejected.unavailable_reason?.replaceAll('_', ' ') ?? 'no reason published'}
        </p>
      ) : (
        <dl className="grid grid-cols-2 gap-x-3 gap-y-0.5 text-body" data-rejected-arguments="measured">
          <dt className="text-text-muted">rejected</dt>
          <dd className="td-value text-right" data-cell="numeric">
            {rejected.rejected_total.toLocaleString()}
          </dd>
          <dt className="text-text-muted">eligible attempts</dt>
          <dd className="td-value text-right" data-cell="numeric">
            {rejected.eligible_attempts?.toLocaleString() ?? '—'}
          </dd>
          <dt className="text-text-muted">rate</dt>
          <dd className="td-value text-right" data-cell="numeric">
            {rejected.rejection_rate != null ? `${(rejected.rejection_rate * 100).toFixed(1)}%` : '—'}
          </dd>
          <dt className="text-text-muted">groups</dt>
          <dd className="td-value text-right" data-cell="numeric">
            {rejected.groups.length.toLocaleString()}
          </dd>
        </dl>
      )}
    </>
  );
}

// ---------------------------------------------------------------------------
// Execution topology
// ---------------------------------------------------------------------------

export function TopologyBody({
  summary,
  topology,
}: {
  summary: EvidenceSummary;
  topology: { result: WorkResult<ExecutionTopologyMetricsV1> | undefined };
}) {
  const result = topology.result;
  if (result?.outcome !== 'value') return <BlockedBody summary={summary} />;
  const model = result.value;
  const groups = new Map<string, { measured: number; total: number }>();
  for (const cell of model.measurements) {
    const group = groups.get(cell.value.metric) ?? { measured: 0, total: 0 };
    group.total += 1;
    if (cell.value.value != null) group.measured += 1;
    groups.set(cell.value.metric, group);
  }
  return (
    <>
      <Rows label="Execution-topology metric families">
        {[...groups.entries()].slice(0, 6).map(([metric, group]) => (
          <Row
            key={metric}
            label={humanizeMetric(metric)}
            value={`${group.measured} / ${group.total}`}
            fraction={group.total > 0 ? group.measured / group.total : null}
            attrs={{ 'data-topology-metric': metric }}
          />
        ))}
      </Rows>
      {groups.size === 0 ? (
        <p className="text-body text-text-muted">the projection returned no measurement cells</p>
      ) : null}
      <p className="mt-2 text-sm text-text-muted">
        {model.coverage.state} family coverage · {model.coverage.observed.toLocaleString()} observed ·{' '}
        {model.coverage.censored.toLocaleString()} censored · no node topology is published, so
        none is drawn
      </p>
    </>
  );
}

// ---------------------------------------------------------------------------
// Analytics controls
// ---------------------------------------------------------------------------

export function AnalyticsBody({
  summary,
  observatory,
  findings,
}: {
  summary: EvidenceSummary;
  observatory: EvidenceRead<ObservatoryReadModelV1>;
  findings: EvidenceRead<DoctorFindingsPayloadV1>;
}) {
  const model = envelopePayload(observatory.result);
  if (!model) return <BlockedBody summary={summary} />;
  const mode = analyticsModeReading(model.analytics_mode);
  const egress = egressFailureReading(model.metrics);
  const staging = shareStagingReading(model.metrics);
  const findingsPayload = envelopePayload(findings.result);
  const retention = findingsPayload
    ? retentionBacklogReading(findingsPayload.storage_kind_statuses)
    : null;
  const rows: { label: string; word: string; state: DomainStateKind; attr: string }[] = [
    { label: 'collection mode', word: mode.label, state: mode.state, attr: 'mode' },
    {
      label: 'retention backlog',
      word: retention
        ? retention.observedEntries == null
          ? retention.state
          : `${retention.observedEntries.toLocaleString()} entries`
        : 'findings read unavailable',
      state: retention?.state ?? 'unknown',
      attr: 'retention',
    },
    {
      label: 'egress failures',
      word: egress.failures == null ? egress.state : egress.failures.toLocaleString(),
      state: egress.state,
      attr: 'egress',
    },
    {
      label: 'share staging age',
      word: staging.ageSeconds == null ? staging.state : `${staging.ageSeconds.toLocaleString()} s`,
      state: staging.state,
      attr: 'staging',
    },
  ];
  return (
    <Rows label="Analytics controls">
      {rows.map((row) => (
        <div
          key={row.attr}
          role="listitem"
          className="flex items-center justify-between gap-2 text-body"
          data-analytics-control={row.attr}
        >
          <span className="truncate text-text-primary">{row.label}</span>
          <EvidenceChip state={evidenceStateOf(row.state)} detail={row.word} />
        </div>
      ))}
    </Rows>
  );
}

// ---------------------------------------------------------------------------
// Storage telemetry
// ---------------------------------------------------------------------------

export function TelemetryBody({
  summary,
  telemetry,
}: {
  summary: EvidenceSummary;
  telemetry: EvidenceRead<StorageTelemetryPayloadV1>;
}) {
  const payload = envelopePayload(telemetry.result);
  if (!payload) return <BlockedBody summary={summary} />;
  if (payload.stores.length === 0) {
    return <p className="text-body text-text-muted">telemetry payload contained no stores</p>;
  }
  const sized = payload.stores.filter((store) => store.total_bytes != null);
  const largest = Math.max(0, ...sized.map((store) => store.total_bytes ?? 0));
  return (
    <Rows label="Store sizes">
      {payload.stores.slice(0, 7).map((store) => {
        const measured = store.read.kind === 'observed' || store.read.kind === 'observed_bytes';
        const over =
          store.budget.state === 'evaluated' && store.budget.evaluation.state === 'over_budget';
        return (
          <Row
            key={store.path}
            label={
              <span className="flex items-center gap-1.5">
                <span className="truncate">{store.store}</span>
                {over ? <span className="td-legend text-alert">over budget</span> : null}
              </span>
            }
            value={measured ? formatBytes(store.total_bytes) : store.read.kind}
            fraction={measured && largest > 0 ? (store.total_bytes ?? 0) / largest : null}
            tone={over ? 'bg-alert' : store.read.kind === 'observed_bytes' ? 'bg-state-partial' : undefined}
            title={store.path}
            attrs={{ 'data-store': store.store, 'data-store-read': store.read.kind }}
            figureWidth="byte"
          />
        );
      })}
      {payload.stores.length > 7 ? (
        <p className="td-legend">{payload.stores.length - 7} more stores in exact evidence</p>
      ) : null}
    </Rows>
  );
}

// ---------------------------------------------------------------------------
// Storage findings
// ---------------------------------------------------------------------------

export function FindingsBody({
  summary,
  findings,
  selectedFinding,
  onSelectFinding,
}: {
  summary: EvidenceSummary;
  findings: EvidenceRead<DoctorFindingsPayloadV1>;
  selectedFinding: number | null;
  onSelectFinding: (index: number) => void;
}) {
  const payload = envelopePayload(findings.result);
  if (!payload) return <BlockedBody summary={summary} />;
  return (
    <>
      <Rows label="Storage finding producers">
        {payload.storage_kind_statuses.map((status) => (
          <div
            key={status.kind}
            role="listitem"
            className="flex items-baseline justify-between gap-2 text-body"
            data-finding-producer={status.kind}
            data-finding-producer-state={status.state}
          >
            <span className="truncate text-text-primary">{storageFindingLabel(status.kind)}</span>
            <span className="td-legend shrink-0">
              {status.state}
              {status.observed_entries > 0 ? ` · ${status.observed_entries}` : ''}
            </span>
          </div>
        ))}
      </Rows>
      {payload.entries.length === 0 ? (
        <p className="mt-2 text-sm text-text-muted">{payload.note}</p>
      ) : (
        <FindingRows
          label="Storage findings"
          entries={payload.entries.map((entry, index) => ({
            index,
            title: entry.storage_kind
              ? storageFindingLabel(entry.storage_kind)
              : 'unclassified storage finding',
            state: entry.finding.state,
            citations: entry.finding.evidence.length,
          }))}
          selected={selectedFinding}
          onSelect={onSelectFinding}
        />
      )}
    </>
  );
}

