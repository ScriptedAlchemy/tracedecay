/**
 * FACT INSPECTOR — one fact, its retained content, where it came from, how its
 * trust moved, and exactly which of those readings this build can vouch for.
 *
 * Two modes, and the eyebrow says which is in force. INSPECTING previews the
 * bounded overview row for the fact under the pointer or under focus; nothing
 * is fetched, and the ladder says the canonical detail and audit are unread.
 * SELECTED reads the canonical detail envelope and the trust audit for the
 * selected fact and reports each read's own state.
 *
 * The provenance block prints the fact's persisted fields by their real names
 * — `source_label`, `created_at`, `last_recalled_at` — rather than the
 * concept plate's `repo`/`path` vocabulary, because no path or repository
 * authority is joined to a memory fact today. Where the plate shows a
 * verification checklist, the ladder shows typed absences.
 */
import { useEffect, type ReactNode } from 'react';
import { Orbit } from 'lucide-react';

import {
  type DashboardEnvelopeV1,
  type MemoryFactDetailPayloadV1,
  type MemoryFactRowV1,
  type MemoryReadStatusV1,
} from '../../contracts/generated.ts';
import type { EnvelopeResult } from '../../data/query/envelope.ts';
import { useFactTrustHistory } from '../../data/query/memory.ts';
import { InspectorPanel, KeyValueTree } from '../../ui/archetypes/ExplorerSplit.tsx';
import { cn } from '../../ui/cn';
import { formatMicrosUtc } from '../../ui/format.ts';
import { Readout } from '../../ui/instrument.tsx';
import { StateChip } from '../../ui/StateChip.tsx';
import { TrustHistorySection } from './FactTrustHistory.tsx';
import { detailLadder, payloadAccessState, type InspectorMode } from './inspector.ts';
import { shortFactId } from './ledger.ts';

export function FactInspector({
  factId,
  mode,
  row,
  detail,
  detailPending,
  relations,
  graphRead,
  onSelect,
  onDismiss,
  onOpenGeometry,
}: {
  factId: string;
  mode: InspectorMode;
  /** The bounded overview row for this fact, when the loaded slice holds it. */
  row: MemoryFactRowV1 | undefined;
  /** The canonical detail read. Only issued for a selected fact. */
  detail: EnvelopeResult<MemoryFactDetailPayloadV1> | undefined;
  detailPending: boolean;
  /** Relations touching this fact in the drawn constellation; `null` when the
   * fact is not among the drawn roots. */
  relations: number | null;
  graphRead: MemoryReadStatusV1 | undefined;
  onSelect: (factId: string) => void;
  onDismiss: () => void;
  onOpenGeometry: () => void;
}) {
  const selected = mode === 'selected';
  const history = useFactTrustHistory(selected ? factId : null);

  const canonical = selected && detail?.outcome === 'envelope' ? detail.envelope : undefined;
  const canonicalRow = canonical?.payload?.fact ?? undefined;
  const subject = canonicalRow ?? row;

  const ladder = detailLadder({
    mode,
    row,
    detail: selected ? (detailPending ? { pending: true } : { pending: false, result: detail }) : null,
    history: selected ? (history.isPending ? { pending: true } : { pending: false, result: history.data }) : null,
    relations,
    graphRead,
  });

  // Escape dismisses inspection anywhere on the page; the inspector is where a
  // keyboard reader is most likely to be when they want out of it.
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === 'Escape') onDismiss();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [onDismiss]);

  return (
    <InspectorPanel
      title={shortFactId(factId)}
      eyebrow={
        <span className="flex items-center gap-1.5" data-inspector-mode={mode}>
          <span
            aria-hidden
            className={cn('size-1.5 shrink-0', selected ? 'bg-accent' : 'border border-accent')}
          />
          {selected ? 'selected fact' : 'inspecting · hover or focus'}
        </span>
      }
      onClose={onDismiss}
    >
      <div className="flex flex-col gap-4" data-testid="fact-inspector" data-fact-id={factId}>
        {!selected ? (
          <div className="flex flex-wrap items-center gap-2 border border-edge-subtle bg-surface-2 px-2 py-1.5">
            <p className="min-w-0 flex-1 text-2xs leading-relaxed text-text-muted">
              Previewing the bounded overview row. Selecting loads the canonical detail and the
              trust audit.
            </p>
            <button
              type="button"
              onClick={() => onSelect(factId)}
              className="td-hit border border-edge-strong bg-surface-1 px-2 text-2xs font-medium text-text-secondary hover:text-text-primary"
            >
              Select fact
            </button>
          </div>
        ) : null}

        <TrustBlock subject={subject} />

        <Section title="canonical content">
          <ContentBlock
            mode={mode}
            row={row}
            canonical={canonical}
            detail={detail}
            detailPending={detailPending}
          />
        </Section>

        <Section title="provenance · evidence">
          <Provenance subject={subject} canonicalRow={canonicalRow} />
        </Section>

        {selected ? (
          <TrustHistorySection pending={history.isPending} result={history.data} />
        ) : (
          <Section title="trust history">
            <p className="text-2xs leading-relaxed text-text-muted">
              The feedback audit is read for the selected fact only.
            </p>
          </Section>
        )}

        <Section title="detail availability">
          <ol aria-label="Detail availability" className="flex flex-col gap-1">
            {ladder.map((rung) => (
              <li
                key={rung.id}
                className="flex flex-wrap items-baseline gap-x-2 gap-y-0.5"
                data-ladder-rung={rung.id}
                data-ladder-state={rung.state}
              >
                <span className="td-legend w-36 shrink-0">{rung.label}</span>
                <StateChip kind={rung.state} detail={rung.detail} className="min-w-0 flex-1" />
              </li>
            ))}
          </ol>
        </Section>

        <Section title="geometry · separate camera">
          <div className="flex flex-wrap items-center gap-2">
            <p className="min-w-0 flex-1 text-2xs leading-relaxed text-text-muted">
              No projection is read on the Facts camera. The Geometry camera derives its own
              bounded projection and similarity; membership there is its reading, not this one.
            </p>
            <button
              type="button"
              onClick={onOpenGeometry}
              className="td-hit border border-edge-subtle bg-surface-2 px-2 text-2xs font-medium text-text-secondary hover:text-text-primary"
            >
              <Orbit aria-hidden size={12} className="mr-1.5" />
              Open Geometry camera
            </button>
          </div>
        </Section>
      </div>
    </InspectorPanel>
  );
}

function Section({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="flex flex-col gap-2 border-t border-edge-subtle pt-3" aria-label={title}>
      <h3 className="td-legend">{title}</h3>
      {children}
    </section>
  );
}

/** Trust as what it is: one measured quantity on the 0–1 scale, printed and
 * given the same length every other readout in the product uses, with the
 * method named beside it. */
function TrustBlock({ subject }: { subject: MemoryFactRowV1 | undefined }) {
  if (!subject) {
    return (
      <p className="text-2xs text-text-muted">
        This fact is not in the loaded slice; its trust arrives with the canonical detail.
      </p>
    );
  }
  const trust = subject.trust_score;
  return (
    <div className="flex flex-col gap-2">
      {trust == null ? (
        <p className="text-2xs text-text-muted">
          trust unavailable while payload access is {subject.payload_access.replaceAll('_', ' ')}
        </p>
      ) : (
        <Readout
          label="trust"
          size="lg"
          value={Math.max(0, Math.min(trust, 1)).toFixed(2)}
          fraction={Math.max(0, Math.min(trust, 1))}
          note="feedback-weighted store score · not a truth claim"
        />
      )}
      <FeedbackSplit helpful={subject.helpful_count ?? null} unhelpful={subject.unhelpful_count ?? null} />
    </div>
  );
}

function ContentBlock({
  mode,
  row,
  canonical,
  detail,
  detailPending,
}: {
  mode: InspectorMode;
  row: MemoryFactRowV1 | undefined;
  canonical: DashboardEnvelopeV1<MemoryFactDetailPayloadV1> | undefined;
  detail: EnvelopeResult<MemoryFactDetailPayloadV1> | undefined;
  detailPending: boolean;
}) {
  if (mode === 'inspecting') {
    return row ? (
      <RetainedContent row={row} bounded />
    ) : (
      <p className="text-2xs text-text-muted">This fact is not in the loaded slice.</p>
    );
  }
  if (detailPending) {
    return (
      <div className="flex flex-col gap-2">
        <StateChip kind="loading" detail="reading canonical fact detail" />
        {row ? <RetainedContent row={row} bounded /> : null}
      </div>
    );
  }
  if (detail?.outcome === 'transport') {
    return (
      <div className="flex flex-col gap-2">
        <StateChip kind={detail.state} detail={detail.detail ?? 'canonical detail transport failed'} />
        {row ? <RetainedContent row={row} bounded /> : null}
      </div>
    );
  }
  const fact = canonical?.payload?.fact ?? null;
  if (!canonical || fact == null) {
    return (
      <div className="flex flex-col gap-2">
        <StateChip
          kind={canonical?.domain_state === 'complete_zero_findings' || !canonical ? 'unavailable' : canonical.domain_state}
          detail={
            canonical?.payload?.error && canonical.payload.error !== ''
              ? canonical.payload.error
              : 'the store holds no fact under this identity in the current scope'
          }
        />
        {row ? <RetainedContent row={row} bounded /> : null}
      </div>
    );
  }
  return (
    <div className="flex flex-col gap-2">
      {canonical.payload?.error && canonical.payload.error !== '' ? (
        <StateChip kind="partial" detail={canonical.payload.error} />
      ) : null}
      <RetainedContent row={fact} bounded={false} />
    </div>
  );
}

/** The retained content, or the typed reason it is withheld. A withheld fact
 * keeps its identity and its access state on screen; the value itself is
 * never reconstructed from the row's other fields. */
function RetainedContent({ row, bounded }: { row: MemoryFactRowV1; bounded: boolean }) {
  const access = payloadAccessState(row.payload_access);
  if (row.payload_access !== 'eligible') {
    return (
      <div className="flex flex-col gap-1.5" data-content-state={row.payload_access}>
        <StateChip kind={access.kind} detail={access.detail} />
        <p className="text-3xs leading-relaxed text-text-muted">
          Identity {row.fact_id} is retained; its content is not shown and is not inferred from
          any other field.
        </p>
      </div>
    );
  }
  if (row.content == null || row.content === '') {
    return (
      <p className="text-2xs text-text-muted" data-content-state="empty">
        eligible payload with no content recorded
      </p>
    );
  }
  return (
    <div className="flex flex-col gap-1">
      <p
        className="whitespace-pre-wrap border-l-2 border-edge-strong pl-2 text-xs leading-relaxed text-text-primary"
        data-content-state={bounded ? 'bounded' : 'canonical'}
      >
        {row.content}
      </p>
      {bounded ? (
        <p className="text-3xs text-text-muted">
          bounded overview text · the list route truncates long facts; the canonical row is
          complete
        </p>
      ) : null}
    </div>
  );
}

function Provenance({
  subject,
  canonicalRow,
}: {
  subject: MemoryFactRowV1 | undefined;
  canonicalRow: MemoryFactRowV1 | undefined;
}) {
  if (!subject) {
    return <p className="text-2xs text-text-muted">no row to read provenance from</p>;
  }
  const entities = canonicalRow?.linked_entities ?? null;
  const metadata = subject.metadata;
  const hasMetadata =
    metadata !== null &&
    metadata !== undefined &&
    typeof metadata === 'object' &&
    Object.keys(metadata as Record<string, unknown>).length > 0;
  return (
    <dl className="grid grid-cols-[minmax(6rem,8rem)_1fr] gap-x-3 gap-y-1 text-2xs">
      <Term label="source label">
        {subject.source_label ?? <span className="text-text-muted">none recorded</span>}
      </Term>
      <Term label="category">{subject.category ?? <span className="text-text-muted">—</span>}</Term>
      <Term label="tags">
        {subject.tags && subject.tags.length > 0 ? (
          <span className="flex flex-wrap gap-1">
            {subject.tags.map((tag) => (
              <span key={tag} className="td-value border border-edge-subtle px-1 text-3xs text-text-secondary">
                {tag}
              </span>
            ))}
          </span>
        ) : (
          <span className="text-text-muted">none</span>
        )}
      </Term>
      <Term label="entities">
        {entities && entities.length > 0 ? (
          <ul className="flex flex-col gap-0.5">
            {entities.map((entity) => (
              <li key={entity.entity_id} className="flex items-baseline gap-2">
                <span className="truncate">{entity.name}</span>
                <span className="td-value text-3xs text-text-muted" data-cell="numeric">
                  {entity.fact_count.toLocaleString()} facts
                </span>
              </li>
            ))}
          </ul>
        ) : subject.entities && subject.entities.length > 0 ? (
          <span className="break-words">{subject.entities.join(', ')}</span>
        ) : (
          <span className="text-text-muted">
            {canonicalRow ? 'none linked' : 'linked entities arrive with canonical detail'}
          </span>
        )}
      </Term>
      <Term label="created">{stamp(subject.created_at)}</Term>
      <Term label="updated">{stamp(subject.updated_at)}</Term>
      <Term label="last recalled">{formatMicrosUtc(subject.last_recalled_at, { nullAs: 'never recalled' })}</Term>
      <Term label="projected as of">{stamp(subject.projected_as_of)}</Term>
      <Term label="retrievals · accesses">
        <span className="td-value" data-cell="numeric">
          {subject.retrieval_count?.toLocaleString() ?? '—'} · {subject.access_count?.toLocaleString() ?? '—'}
        </span>
      </Term>
      <Term label="metadata">
        {hasMetadata ? (
          <KeyValueTree value={metadata} />
        ) : (
          <span className="text-text-muted">none recorded</span>
        )}
      </Term>
      <Term label="repository · path">
        <span className="text-text-muted">
          no repository or path authority is joined to memory facts
        </span>
      </Term>
    </dl>
  );
}

function stamp(micros: number | null | undefined): ReactNode {
  return (
    <span className="td-value" data-cell="numeric">
      {formatMicrosUtc(micros, { nullAs: 'not reported' })}
    </span>
  );
}

function Term({ label, children }: { label: string; children: ReactNode }) {
  return (
    <>
      <dt className="td-legend pt-px">{label}</dt>
      <dd className="min-w-0 break-words text-text-secondary">{children}</dd>
    </>
  );
}

/** Helpful vs unhelpful feedback as one proportional split bar. */
export function FeedbackSplit({
  helpful,
  unhelpful,
}: {
  helpful: number | null;
  unhelpful: number | null;
}) {
  if (helpful === null || unhelpful === null) {
    return <p className="text-2xs text-text-muted">feedback counts not reported</p>;
  }
  const total = helpful + unhelpful;
  if (total === 0) {
    return <p className="text-2xs text-text-muted">no feedback recorded</p>;
  }
  return (
    <figure className="flex flex-col gap-1">
      <div className="flex h-1.5 overflow-hidden bg-surface-3">
        <div className="bg-accent" style={{ width: `${(helpful / total) * 100}%` }} />
        <div className="bg-state-stale" style={{ width: `${(unhelpful / total) * 100}%` }} />
      </div>
      <figcaption className="tabular text-2xs text-text-muted">
        {helpful} helpful · {unhelpful} unhelpful
      </figcaption>
    </figure>
  );
}
