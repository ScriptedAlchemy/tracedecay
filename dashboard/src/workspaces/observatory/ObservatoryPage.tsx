import { useCallback, useEffect, useRef, useState, type KeyboardEvent } from 'react';
import { useSearchParams } from 'react-router';
import type { DoctorReportEntryV1 } from '../../contracts/generated.ts';
import { useScope } from '../../data/scope/store.ts';
import { WorkspaceHeader } from '../../ui/instrument.tsx';
import { EvidenceInspector, type InspectorMode } from './EvidenceInspector.tsx';
import { EvidencePanel, panelRovingKeyDown } from './EvidencePanel.tsx';
import {
  AdoptionBody,
  AnalyticsBody,
  BudgetsBody,
  DoctorBody,
  FindingsBody,
  HooksBody,
  PipelineBody,
  RetrievalBody,
  TelemetryBody,
  TopologyBody,
} from './EvidenceSummaries.tsx';
import { ExactEvidence } from './ExactEvidence.tsx';
import { ObservationTimeline } from './ObservationTimeline.tsx';
import {
  EVIDENCE_SOURCES,
  isEvidenceSourceId,
  type EvidenceSourceId,
} from './evidence.ts';
import { useObservatoryReads, type ObservatoryReads } from './useObservatoryReads.ts';

/** The address of the selection: which authority is open in the inspector,
 * and, for the two Doctor-backed sources, which finding row. Replaced, not
 * pushed, so panning across eleven panels is not eleven places to go back to. */
export const OBSERVATORY_INSPECT_PARAM = 'inspect';
export const OBSERVATORY_FINDING_PARAM = 'finding';

/**
 * Observatory: system evidence from independent authorities on one time
 * context.
 *
 * Eleven sources, the canonical observations horizon, Doctor, adoption,
 * retrieval, the code-index pipeline, hook hints, performance budgets,
 * execution topology, analytics controls, store telemetry, and storage
 * findings, each report their own typed state, coverage, and observation
 * time. Nothing here averages them into a health grade. Hovering a panel or a
 * timeline mark previews its evidence in the inspector; selecting it opens the
 * exact read model beneath the grid. Every read is requested once and shared
 * by the timeline, the grid, the inspector, and the exact evidence.
 */
export function ObservatoryPage() {
  const reads = useObservatoryReads();
  const scope = useScope((state) => state.scope);
  const [params, setParams] = useSearchParams();
  const requested = params.get(OBSERVATORY_INSPECT_PARAM);
  const selected: EvidenceSourceId | null = isEvidenceSourceId(requested) ? requested : null;
  const findingParam = params.get(OBSERVATORY_FINDING_PARAM);
  const selectedFinding =
    findingParam != null && /^\d+$/.test(findingParam) ? Number(findingParam) : null;
  const [previewed, setPreviewed] = useState<EvidenceSourceId | null>(null);
  const root = useRef<HTMLDivElement>(null);

  const select = useCallback(
    (id: EvidenceSourceId) => {
      const next = new URLSearchParams(params);
      next.set(OBSERVATORY_INSPECT_PARAM, id);
      if (params.get(OBSERVATORY_INSPECT_PARAM) !== id) next.delete(OBSERVATORY_FINDING_PARAM);
      setParams(next, { replace: true });
    },
    [params, setParams],
  );
  const selectFinding = useCallback(
    (source: EvidenceSourceId, index: number) => {
      const next = new URLSearchParams(params);
      next.set(OBSERVATORY_INSPECT_PARAM, source);
      next.set(OBSERVATORY_FINDING_PARAM, String(index));
      setParams(next, { replace: true });
    },
    [params, setParams],
  );
  const clearFinding = useCallback(() => {
    const next = new URLSearchParams(params);
    next.delete(OBSERVATORY_FINDING_PARAM);
    setParams(next, { replace: true });
  }, [params, setParams]);

  const preview = useCallback((id: EvidenceSourceId) => setPreviewed(id), []);
  const previewEnd = useCallback(() => setPreviewed(null), []);

  // Escape returns to the selection: the preview is dropped and focus lands
  // on the selected panel's control, so the keyboard reader is back where the
  // inspector says they are.
  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.key !== 'Escape') return;
    setPreviewed(null);
    if (selected) {
      root.current
        ?.querySelector<HTMLButtonElement>(`[data-evidence-panel-button="${selected}"]`)
        ?.focus();
    }
  };

  // A finding index that no longer exists in the served report is dropped
  // rather than pointing the inspector at a row that is not there.
  const finding = resolveFinding(reads, selected, selectedFinding);
  useEffect(() => {
    if (selectedFinding != null && finding == null && findingSource(selected) != null) {
      const source = findingSource(selected);
      const read = source === 'doctor' ? reads.doctor : reads.findings;
      if (read.result?.outcome === 'envelope') clearFinding();
    }
  }, [selectedFinding, finding, selected, reads.doctor, reads.findings, clearFinding]);

  const shown = previewed ?? selected;
  const mode: InspectorMode = previewed && previewed !== selected ? 'preview' : shown ? 'selected' : 'none';
  const shownSummary = shown ? reads.summaries[shown] : null;

  return (
    <div
      ref={root}
      className="flex min-h-full flex-col"
      data-observatory-selected={selected ?? 'none'}
      data-observatory-previewed={previewed ?? 'none'}
      onKeyDown={onKeyDown}
    >
      <WorkspaceHeader
        path="observatory"
        title="Observatory"
        note="system evidence · independent authorities · one time context · no aggregate health"
      />
      <ObservationTimeline
        summaries={EVIDENCE_SOURCES.map((id) => reads.summaries[id])}
        observatory={reads.observatory}
        selected={selected}
        previewed={previewed}
        onSelect={select}
        onPreview={preview}
        onPreviewEnd={previewEnd}
      />
      <div className="grid gap-2 p-2 xl:grid-cols-[minmax(0,1fr)_21rem]">
        <div
          role="group"
          aria-label="System evidence panels"
          className="td-stagger grid min-w-0 gap-2 md:grid-cols-2 xl:grid-cols-3"
          onKeyDown={panelRovingKeyDown}
        >
          {GRID_ORDER.map((id) => (
            <EvidencePanel
              key={id}
              summary={reads.summaries[id]}
              selected={selected === id}
              previewed={previewed === id}
              onSelect={select}
              onPreview={preview}
              onPreviewEnd={previewEnd}
              className={id === 'pipeline' ? 'md:col-span-2 xl:col-span-3' : undefined}
            >
              <PanelBody
                id={id}
                reads={reads}
                selectedFinding={findingSource(selected) === id ? selectedFinding : null}
                onSelectFinding={(index) => selectFinding(id, index)}
              />
            </EvidencePanel>
          ))}
        </div>
        <EvidenceInspector
          className="xl:sticky xl:top-2 xl:max-h-[calc(100dvh-7rem)] xl:self-start"
          summary={shownSummary}
          mode={mode}
          finding={mode === 'selected' ? finding : null}
          scope={scope}
          refreshing={shown ? reads.refreshing(shown) : false}
          onRefresh={() => {
            if (shown) reads.refresh(shown);
          }}
          onClearFinding={clearFinding}
        />
      </div>
      {selected ? (
        <div className="px-2 pb-2">
          <ExactEvidence id={selected} reads={reads} />
        </div>
      ) : null}
    </div>
  );
}

/** The grid, row by row: the pipeline spans the middle row on its own, the
 * way the plate lays it out; the timeline is the eleventh source and lives
 * above the grid. */
const GRID_ORDER: readonly Exclude<EvidenceSourceId, 'observations'>[] = [
  'doctor',
  'adoption',
  'retrieval',
  'pipeline',
  'hooks',
  'budgets',
  'topology',
  'analytics',
  'telemetry',
  'findings',
];

function findingSource(selected: EvidenceSourceId | null): 'doctor' | 'findings' | null {
  return selected === 'doctor' || selected === 'findings' ? selected : null;
}

function resolveFinding(
  reads: ObservatoryReads,
  selected: EvidenceSourceId | null,
  index: number | null,
): { index: number; entry: DoctorReportEntryV1 } | null {
  const source = findingSource(selected);
  if (source == null || index == null) return null;
  const read = source === 'doctor' ? reads.doctor : reads.findings;
  if (read.result?.outcome !== 'envelope') return null;
  const entry = read.result.envelope.payload.entries[index];
  return entry ? { index, entry } : null;
}

/** Exhaustive over the grid's sources, so a source added to the grid cannot
 * be left without a body. */
function PanelBody({
  id,
  reads,
  selectedFinding,
  onSelectFinding,
}: {
  id: Exclude<EvidenceSourceId, 'observations'>;
  reads: ObservatoryReads;
  selectedFinding: number | null;
  onSelectFinding: (index: number) => void;
}) {
  const summary = reads.summaries[id];
  switch (id) {
    case 'doctor':
      return (
        <DoctorBody
          summary={summary}
          doctor={reads.doctor}
          selectedFinding={selectedFinding}
          onSelectFinding={onSelectFinding}
        />
      );
    case 'adoption':
      return (
        <AdoptionBody summary={summary} observatory={reads.observatory} diagnostics={reads.diagnostics} />
      );
    case 'retrieval':
      return (
        <RetrievalBody summary={summary} observatory={reads.observatory} diagnostics={reads.diagnostics} />
      );
    case 'pipeline':
      return <PipelineBody summary={summary} freshness={reads.freshness} />;
    case 'hooks':
      return <HooksBody summary={summary} hints={reads.hints} observatory={reads.observatory} />;
    case 'budgets':
      return <BudgetsBody summary={summary} observatory={reads.observatory} />;
    case 'topology':
      return <TopologyBody summary={summary} topology={reads.topology} />;
    case 'analytics':
      return <AnalyticsBody summary={summary} observatory={reads.observatory} findings={reads.findings} />;
    case 'telemetry':
      return <TelemetryBody summary={summary} telemetry={reads.telemetry} />;
    case 'findings':
      return (
        <FindingsBody
          summary={summary}
          findings={reads.findings}
          selectedFinding={selectedFinding}
          onSelectFinding={onSelectFinding}
        />
      );
    default: {
      const unhandled: never = id;
      return unhandled;
    }
  }
}
