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

import type {
  DashboardEnvelopeV1,
  MemoryReadStatusV1,
} from '../../contracts/generated.ts';
import type { EnvelopeResult } from '../../data/query/envelope.ts';
import { cn } from '../../ui/cn';
import { stateLampClass, type DomainStateKind } from '../../ui/StateChip.tsx';
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
}: {
  memory: RegisterReading;
  facts: RegisterReading;
  entities: RegisterReading;
  graph: RegisterReading;
  status: RegisterReading;
  camera: KnowledgeViewKind;
}) {
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
      <div className="flex min-w-0 shrink-0 items-center gap-2 border-r border-edge-subtle px-3">
        <span aria-hidden className="td-value text-3xs text-text-muted" data-cell="numeric">
          06
        </span>
        <span className="td-legend">Camera</span>
        <span className="td-value text-2xs uppercase text-accent" data-register="camera">
          {knowledgeViewLabel(camera)}
        </span>
      </div>
      <span aria-hidden className="flex-1" />
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
      className="flex min-w-0 shrink-0 items-center gap-2 border-r border-edge-subtle px-3"
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
        {reading.detail ? (
          <span
            className="td-value max-w-48 truncate text-3xs normal-case text-text-muted"
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
