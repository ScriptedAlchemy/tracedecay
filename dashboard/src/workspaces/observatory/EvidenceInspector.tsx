import { RefreshCw } from 'lucide-react';
import type { ReactNode } from 'react';
import { Link } from 'react-router';
import type { DoctorReportEntryV1 } from '../../contracts/generated.ts';
import { scopedWorkspacePath, type DashboardScope } from '../../data/scope/store.ts';
import { cn } from '../../ui/cn.ts';
import { Corners, Meter } from '../../ui/instrument.tsx';
import { formatMicrosUtc } from '../../ui/format.ts';
import { doctorEvidencePresentation, doctorFamilyLabel } from './doctorModel.ts';
import { EVIDENCE_INSPECTOR_ID, EvidenceChip } from './EvidencePanel.tsx';
import {
  coveragePercent,
  coverageSentence,
  type EvidenceSourceId,
  type EvidenceSummary,
} from './evidence.ts';
import { storageFindingLabel } from './storageModel.ts';

export type InspectorMode = 'selected' | 'preview' | 'none';

/** Where a source's exact evidence also lives, when a shipping surface owns
 * it. Only destinations that exist are listed; the link carries project scope
 * and nothing else. */
function crossLinks(id: EvidenceSourceId): readonly { path: string; label: string }[] {
  switch (id) {
    case 'pipeline':
      return [{ path: 'code', label: 'Code · index freshness' }];
    case 'telemetry':
    case 'findings':
      return [{ path: 'settings', label: 'Settings · storage budgets' }];
    case 'analytics':
      return [{ path: 'settings', label: 'Settings · analytics' }];
    case 'hooks':
      return [{ path: 'agents', label: 'Agents · hook and tool activity' }];
    case 'topology':
      return [{ path: 'work', label: 'Work · execution topology' }];
    case 'observations':
    case 'doctor':
    case 'adoption':
    case 'retrieval':
    case 'budgets':
      return [];
    default: {
      const unhandled: never = id;
      return unhandled;
    }
  }
}

function Fact({
  label,
  children,
  attrs,
}: {
  label: string;
  children: ReactNode;
  attrs?: Record<string, string>;
}) {
  return (
    <div className="flex min-w-0 flex-col gap-0.5" {...attrs}>
      <dt className="td-legend">{label}</dt>
      <dd className="min-w-0 break-words text-2xs leading-relaxed text-text-secondary">
        {children}
      </dd>
    </div>
  );
}

/**
 * The workspace-owned inspector. It renders exactly what the selected (or
 * hovered) authority published about itself — source, observation time,
 * typed state, coverage, freshness, scope, declared actions — and states as
 * absences the things the contracts do not carry: severity, impact, and a
 * recovery route. A control appears only for an action the daemon declared
 * and this dashboard can submit, which today is `refresh`.
 */
export function EvidenceInspector({
  summary,
  mode,
  finding,
  scope,
  refreshing,
  onRefresh,
  onClearFinding,
  className,
}: {
  summary: EvidenceSummary | null;
  mode: InspectorMode;
  finding: { index: number; entry: DoctorReportEntryV1 } | null;
  scope: DashboardScope;
  refreshing: boolean;
  onRefresh: () => void;
  onClearFinding: () => void;
  className?: string;
}) {
  return (
    <aside
      id={EVIDENCE_INSPECTOR_ID}
      aria-label="Evidence inspector"
      data-inspector-mode={mode}
      data-inspector-source={summary?.id ?? 'none'}
      className={cn('relative flex min-h-0 flex-col border border-edge-subtle bg-surface-1', className)}
    >
      <Corners tone={mode === 'selected' ? 'signal' : 'edge'} />
      <header className="flex h-8 shrink-0 items-center gap-2 border-b border-edge-subtle px-2.5">
        <span className="td-legend text-text-secondary">Evidence inspector</span>
        <span aria-hidden className="td-rule" />
        <span
          className={cn('td-legend', mode === 'preview' ? 'text-accent' : undefined)}
          data-inspector-mode-word
        >
          {mode === 'preview' ? 'preview · hover' : mode === 'selected' ? 'selected' : 'no selection'}
        </span>
      </header>
      <div
        role="region"
        aria-label="Evidence inspector detail"
        tabIndex={0}
        className="min-h-0 flex-1 overflow-auto p-3"
      >
        {summary ? (
          <InspectorBody
            summary={summary}
            finding={finding}
            scope={scope}
            refreshing={refreshing}
            onRefresh={onRefresh}
            onClearFinding={onClearFinding}
          />
        ) : (
          <NoSelection />
        )}
      </div>
    </aside>
  );
}

function NoSelection() {
  return (
    <div className="flex flex-col gap-2" data-inspector-empty>
      <p className="td-title text-text-primary">Nothing selected</p>
      <p className="text-2xs leading-relaxed text-text-muted">
        Hover a panel or a timeline mark to preview its evidence here. Click, or press Enter on it,
        to select it and open its exact evidence below the grid. Escape returns to the selection.
      </p>
      <p className="text-3xs leading-relaxed text-text-muted">
        Each panel reports its own state. There is no aggregate health here: a measured panel says
        nothing about its neighbours.
      </p>
    </div>
  );
}

function InspectorBody({
  summary,
  finding,
  scope,
  refreshing,
  onRefresh,
  onClearFinding,
}: {
  summary: EvidenceSummary;
  finding: { index: number; entry: DoctorReportEntryV1 } | null;
  scope: DashboardScope;
  refreshing: boolean;
  onRefresh: () => void;
  onClearFinding: () => void;
}) {
  const percent = coveragePercent(summary.coverage);
  const links = crossLinks(summary.id);
  return (
    <div className="flex flex-col gap-3">
      <div className="flex flex-col gap-1.5">
        <h2 className="td-title text-sm normal-case tracking-normal text-text-primary">
          {summary.title}
        </h2>
        <EvidenceChip state={summary.state} detail={summary.stateDetail} size="title" className="w-fit" />
      </div>

      {finding ? <FindingDetail finding={finding} onClear={onClearFinding} /> : null}

      <dl className="grid gap-2.5">
        <Fact label="Source" attrs={{ 'data-inspector-fact': 'source' }}>
          <span className="font-mono text-text-primary">{summary.route}</span>
          <br />
          {summary.authority}
        </Fact>
        <Fact label="Observed" attrs={{ 'data-inspector-fact': 'observed' }}>
          {summary.observedAtMicros == null
            ? 'not published · the authority carried no observation time'
            : formatMicrosUtc(summary.observedAtMicros)}
        </Fact>
        <Fact label="Coverage" attrs={{ 'data-inspector-fact': 'coverage' }}>
          <span className="flex items-center gap-2">
            <Meter fraction={percent == null ? null : percent / 100} height="row" className="w-16 shrink-0" />
            <span className="td-value text-2xs" data-cell="numeric">
              {percent == null ? '—' : `${percent}%`}
            </span>
          </span>
          {coverageSentence(summary.coverage)}
        </Fact>
        <Fact label="Freshness" attrs={{ 'data-inspector-fact': 'freshness' }}>
          {summary.freshness
            ? `${summary.freshness.state}${
                summary.freshness.observedAtMicros != null
                  ? ` · observed ${formatMicrosUtc(summary.freshness.observedAtMicros)}`
                  : ''
              }${summary.freshness.watermark ? ` · watermark ${summary.freshness.watermark}` : ''}`
            : 'not published'}
        </Fact>
        <Fact label="Scope" attrs={{ 'data-inspector-fact': 'scope' }}>
          {summary.scope
            ? `${summary.scope.projectId ?? 'no project id'} · ${summary.scope.storageMode} · ${summary.scope.storeRoot}`
            : 'not carried by this read'}
        </Fact>
        <Fact label="Authorization" attrs={{ 'data-inspector-fact': 'authorization' }}>
          {summary.authorization ?? 'not carried by this read'}
        </Fact>
        <Fact label="Source watermark" attrs={{ 'data-inspector-fact': 'watermark' }}>
          {summary.watermark ?? 'not published'}
        </Fact>
        <Fact label="Affected objects" attrs={{ 'data-inspector-fact': 'affected' }}>
          {summary.affected ?? 'nothing counted · the read produced no objects to count'}
        </Fact>
        <Fact label="Severity" attrs={{ 'data-inspector-fact': 'severity' }}>
          not published · no Observatory authority carries a severity grade
        </Fact>
        <Fact label="Declared actions" attrs={{ 'data-inspector-fact': 'declared-actions' }}>
          {summary.declaredActions.length === 0 ? (
            'none declared by the daemon'
          ) : (
            <ul className="space-y-0.5">
              {summary.declaredActions.map((action) => (
                <li key={`${action.kind}:${action.operation}`} className="font-mono text-3xs">
                  {action.kind} · {action.operation}
                </li>
              ))}
            </ul>
          )}
        </Fact>
        <Fact label="Recovery" attrs={{ 'data-inspector-fact': 'recovery', 'data-recovery': 'unavailable' }}>
          <span className="text-state-error">unavailable</span> · no authorized production recovery
          route is composed in this dashboard; declared actions above are listed, not invocable
        </Fact>
        <Fact label="Re-read" attrs={{ 'data-inspector-fact': 'refresh' }}>
          {summary.refreshOperation ? (
            <button
              type="button"
              className="td-hit group -my-2 disabled:cursor-wait disabled:opacity-60"
              onClick={onRefresh}
              disabled={refreshing}
              title={summary.refreshOperation}
              data-operation={summary.refreshOperation}
            >
              <span className="inline-flex h-7 items-center gap-1.5 border border-edge-subtle bg-surface-2 px-2.5 text-2xs font-medium text-text-secondary group-hover:text-text-primary">
                <RefreshCw aria-hidden size={12} className={refreshing ? 'animate-spin' : undefined} />
                {refreshing ? 'Re-reading' : 'Re-read'}
              </span>
            </button>
          ) : (
            'the daemon declared no refresh action for this read'
          )}
        </Fact>
        <Fact label="Last read by this browser" attrs={{ 'data-inspector-fact': 'last-read' }}>
          {summary.lastReadMs > 0 ? new Date(summary.lastReadMs).toISOString() : 'never'}
          {' · '}client-side receipt, not an authority observation
        </Fact>
        {summary.note ? (
          <Fact label="Notes" attrs={{ 'data-inspector-fact': 'note' }}>
            {summary.note}
          </Fact>
        ) : null}
        {links.length > 0 ? (
          <Fact label="Open in" attrs={{ 'data-inspector-fact': 'links' }}>
            <ul className="flex flex-wrap gap-2">
              {links.map((link) => (
                <li key={link.path}>
                  <Link
                    to={scopedWorkspacePath(scope, link.path)}
                    className="td-hit -my-2 border-b border-accent/60 text-2xs text-text-primary hover:border-accent"
                    data-cross-link={link.path}
                  >
                    {link.label}
                  </Link>
                </li>
              ))}
            </ul>
          </Fact>
        ) : null}
      </dl>
    </div>
  );
}

/** One selected finding, exactly as the Doctor contract carries it: family,
 * evidence state, coverage statement, and the evidence references. */
function FindingDetail({
  finding,
  onClear,
}: {
  finding: { index: number; entry: DoctorReportEntryV1 };
  onClear: () => void;
}) {
  const { entry } = finding;
  const presentation = doctorEvidencePresentation(entry.finding.state);
  return (
    <section
      aria-label="Selected finding"
      data-inspector-finding={finding.index}
      className="relative flex flex-col gap-1.5 border border-edge-strong bg-surface-2 p-2.5"
    >
      <Corners tone="signal" />
      <p className="flex items-center gap-2">
        <span className="td-legend">Finding</span>
        <span aria-hidden className="td-rule" />
        <button
          type="button"
          className="td-hit -my-2 text-3xs text-text-muted hover:text-text-primary"
          onClick={onClear}
          aria-label="Clear selected finding"
        >
          clear
        </button>
      </p>
      <p className="text-xs text-text-primary">
        {doctorFamilyLabel(entry.finding.family)}
        {entry.storage_kind ? ` · ${storageFindingLabel(entry.storage_kind)}` : ''}
      </p>
      <p className="flex items-center gap-1.5 text-2xs" data-evidence-state={entry.finding.state}>
        <span aria-hidden className={cn('size-1.5', presentation.dotClass)} />
        <span className={presentation.tokenClass}>{presentation.label}</span>
        <span className="text-text-muted">· coverage {entry.finding.coverage.completeness}</span>
      </p>
      <p className="text-2xs leading-relaxed text-text-secondary">{entry.finding.coverage.statement}</p>
      <ul className="space-y-0.5" aria-label="Finding evidence references">
        {entry.finding.evidence.length === 0 ? (
          <li className="text-3xs text-text-muted">no evidence references carried</li>
        ) : (
          entry.finding.evidence.map((evidence, index) => (
            <li key={`${evidence.family}:${evidence.reference}:${index}`} className="break-all font-mono text-3xs text-text-muted">
              {evidence.family} · {evidence.reference}
            </li>
          ))
        )}
      </ul>
    </section>
  );
}
