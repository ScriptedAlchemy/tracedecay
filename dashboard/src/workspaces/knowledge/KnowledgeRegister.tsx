/**
 * The Knowledge workspace's own status word: one numbered register per
 * authority the Facts camera reads, each carrying that authority's state and
 * nothing else's.
 *
 * The shell's bottom strip reports the transport (link, feed, source, query).
 * It says nothing about whether the memory store answered, whether the fact
 * rows were bounded, or whether the graph came back partial — and those are
 * independent facts that a reader of this workspace needs side by side. So
 * the workspace carries its own register, in the same grammar as the strip,
 * and the camera position is the last cell so a linked position can be read
 * off the screen.
 */
import type { ReactNode } from 'react';
import { RefreshCw } from 'lucide-react';

import type { DashboardEnvelopeV1, MemoryReadStatusV1 } from '../../contracts/generated.ts';
import type { EnvelopeResult } from '../../data/query/envelope.ts';
import { cn } from '../../ui/cn';
import {
  authorizationState,
  toStripCoverage,
  toStripFreshness,
} from '../../ui/EnvelopeTruth.tsx';
import { EvidenceTruthStrip } from '../../ui/EvidenceTruthStrip.tsx';
import { StateChip, stateLampClass, type DomainStateKind } from '../../ui/StateChip.tsx';
import { knowledgeViewLabel, type KnowledgeViewKind } from './KnowledgeViews.tsx';

export interface RegisterReading {
  state: DomainStateKind;
  /** The one reason a reader needs that the word cannot hold. */
  detail?: string | undefined;
}

/** An envelope read as one register reading: pending, transport, or the
 * daemon's own domain state. */
export function envelopeReading(
  pending: boolean,
  result: EnvelopeResult<unknown> | undefined,
): RegisterReading {
  if (pending) return { state: 'loading' };
  if (!result) return { state: 'unknown', detail: 'has not answered' };
  if (result.outcome === 'transport') return { state: result.state, detail: result.detail };
  return { state: result.envelope.domain_state };
}

/** A memory sub-read (`holographic.reads[...]`) as one register reading. */
export function subReadReading(read: MemoryReadStatusV1 | undefined): RegisterReading {
  if (!read) return { state: 'unknown', detail: 'not reported' };
  return { state: read.state, detail: read.error ?? read.code ?? undefined };
}

export function KnowledgeRegister({
  memory,
  facts,
  entities,
  graph,
  status,
  camera,
  envelope,
  refreshing,
  onRefresh,
}: {
  memory: RegisterReading;
  facts: RegisterReading;
  entities: RegisterReading;
  graph: RegisterReading;
  status: RegisterReading;
  camera: KnowledgeViewKind;
  /** The memory overview envelope, when one was served: its coverage,
   * freshness, authorization and refresh action ride the same row as the
   * sub-read states, so the camera has one status word rather than two. */
  envelope: DashboardEnvelopeV1<unknown> | null;
  refreshing: boolean;
  onRefresh: () => void;
}) {
  // The refresh control is drawn only when the server offered the action: a
  // control the application did not offer is not a control.
  const refresh = envelope?.legal_actions.find((action) => action.kind === 'refresh')?.operation;
  const authorization = envelope ? authorizationState(envelope.authorization) : null;
  return (
    <div
      role="group"
      aria-label="Knowledge authorities"
      className="flex min-h-8 shrink-0 flex-wrap items-stretch border-b border-edge-subtle bg-surface-1"
      data-testid="knowledge-register"
    >
      <Cell code="01" label="Memory" reading={memory} />
      <Cell code="02" label="Facts" reading={facts} />
      <Cell code="03" label="Entities" reading={entities} />
      <Cell code="04" label="Graph" reading={graph} />
      <Cell code="05" label="Status" reading={status} />
      <div className="flex min-w-0 items-center gap-2 border-r border-edge-subtle px-3">
        <span aria-hidden className="td-value text-3xs text-text-muted" data-cell="numeric">
          06
        </span>
        <span className="td-legend">Camera</span>
        <span className="td-value text-2xs uppercase text-accent" data-register="camera">
          {knowledgeViewLabel(camera)}
        </span>
      </div>
      {envelope ? (
        // A full row of its own below `lg`, where wrapping it beside the cells
        // stacked its three phrases into a narrow column; the trailing end of
        // the same row from `lg`.
        <div className="flex min-w-0 basis-full flex-wrap items-center gap-x-3 gap-y-1 border-t border-edge-subtle px-3 py-1 lg:flex-1 lg:basis-auto lg:justify-end lg:border-t-0">
          {authorization ? <StateChip kind={authorization} detail="read authorization" /> : null}
          <EvidenceTruthStrip
            coverage={toStripCoverage(envelope.coverage)}
            freshness={toStripFreshness(envelope.freshness)}
            omissions={envelope.coverage.omitted ?? undefined}
          />
          {refresh ? (
            <button
              type="button"
              className="td-hit group disabled:cursor-wait disabled:opacity-60"
              onClick={onRefresh}
              disabled={refreshing}
              title={refresh}
              data-operation={refresh}
            >
              <span className="inline-flex h-6 items-center gap-1.5 border border-edge-subtle bg-surface-2 px-2 text-2xs font-medium text-text-secondary group-hover:text-text-primary">
                <RefreshCw aria-hidden size={11} className={refreshing ? 'animate-spin' : undefined} />
                {refreshing ? 'Refreshing' : 'Refresh'}
              </span>
            </button>
          ) : null}
        </div>
      ) : (
        <span aria-hidden className="flex-1" />
      )}
    </div>
  );
}

function Cell({
  code,
  label,
  reading,
}: {
  code: string;
  label: string;
  reading: RegisterReading;
}) {
  const word = reading.state.replaceAll('_', ' ');
  return (
    <div
      className="flex min-w-0 max-w-full items-center gap-2 border-r border-edge-subtle px-3"
      data-register={label.toLowerCase()}
      data-register-state={reading.state}
    >
      <span aria-hidden className="td-value text-3xs text-text-muted" data-cell="numeric">
        {code}
      </span>
      <span className="td-legend">{label}</span>
      <span className="flex min-w-0 items-center gap-1.5">
        <span aria-hidden className={cn('size-2 shrink-0', stateLampClass(reading.state))} />
        <Word>{word}</Word>
        {/* The reason is withdrawn below `sm`, where a cell wide enough to
          * hold it ran past the viewport; the state word stays, and the full
          * reason remains on the coverage statements the camera prints. */}
        {reading.detail ? (
          <span
            className="td-value max-w-48 truncate text-3xs normal-case text-text-muted max-sm:hidden"
            title={reading.detail}
          >
            {reading.detail}
          </span>
        ) : null}
      </span>
    </div>
  );
}

function Word({ children }: { children: ReactNode }) {
  return <span className="td-value text-2xs uppercase">{children}</span>;
}
