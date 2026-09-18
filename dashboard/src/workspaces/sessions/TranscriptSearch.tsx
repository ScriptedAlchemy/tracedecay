/**
 * TRANSCRIPT SEARCH, `GET /api/plugins/hermes-lcm/search?q=…`.
 *
 * Full-text over the persisted transcripts and retained summaries. It is a
 * separate authority from the session index: a hit names a provider-qualified
 * session, and selecting it opens the same provenance inspector, which then
 * says whether that session is on the loaded index page or not.
 */
import { useRef } from 'react';
import type { DashboardEnvelopeV1, LcmMessageV1, LcmSearchPayloadV1 } from '../../contracts/generated.ts';
import { DataRow } from '../../ui/archetypes/ExplorerSplit.tsx';
import { toStripCoverage, toStripFreshness } from '../../ui/EnvelopeTruth.tsx';
import { EvidenceTruthStrip } from '../../ui/EvidenceTruthStrip.tsx';
import { ReadSection, type ReadState } from '../../ui/ReadSection.tsx';
import { StateChip } from '../../ui/StateChip';
import { Panel } from '../../ui/instrument.tsx';
import { formatStamp } from '../../ui/format.ts';
import { rovingRowsKeyDown } from '../../ui/rovingRows.ts';
import { sameSelection, type SessionSelection } from './model.ts';

export interface TranscriptSearchResultsProps {
  read: ReadState<DashboardEnvelopeV1<LcmSearchPayloadV1>>;
  submitted: string;
  selection: SessionSelection | null;
  onSelect: (selection: SessionSelection | null) => void;
}

export function TranscriptSearchResults({ read, submitted, selection, onSelect }: TranscriptSearchResultsProps) {
  const rowsRef = useRef<HTMLDivElement | null>(null);
  return (
    <Panel
      legend="Transcript search"
      elevation="well"
      bodyClassName="p-0"
      actions={
        <span className="td-value truncate text-3xs text-text-muted" title={submitted}>
          "{submitted}"
        </span>
      }
    >
      <ReadSection title="Transcript search" chrome="centered" state={read}>
        {(envelope) => {
          const data = envelope.payload;
          if (data.exists === false) {
            return (
              <div className="p-3">
                <StateChip kind="unknown" detail="LCM session store is unavailable; nothing was searched" />
              </div>
            );
          }
          const hits = data.matches.messages;
          return (
            <div className="flex flex-col">
              <p className="tabular flex flex-wrap items-baseline gap-x-2 border-b border-edge-subtle px-3 py-1.5 text-2xs text-text-muted">
                <span className="text-text-secondary">
                  {data.total.messages.toLocaleString()} message {data.total.messages === 1 ? 'match' : 'matches'}
                </span>
                {data.total.summary_nodes > 0 ? (
                  <span>· {data.total.summary_nodes.toLocaleString()} retained summaries</span>
                ) : null}
                <span>· engine {data.engine}</span>
                {data.next_cursor != null ? <span>· more matches follow (not loaded)</span> : null}
              </p>
              {hits.length === 0 ? (
                <div className="p-3">
                  <StateChip
                    kind="complete_zero_findings"
                    detail={`no transcript message matches "${submitted}"`}
                  />
                </div>
              ) : (
                <div ref={rowsRef} onKeyDown={(event) => rovingRowsKeyDown(rowsRef.current, event)}>
                  {hits.map((hit, i) => (
                    <SearchHit
                      key={hitKey(hit, i)}
                      hit={hit}
                      selected={sameSelection(selection, hitSelection(hit))}
                      onSelect={() => {
                        const next = hitSelection(hit);
                        onSelect(sameSelection(selection, next) ? null : next);
                      }}
                    />
                  ))}
                </div>
              )}
              <div className="flex flex-wrap items-center gap-x-3 border-t border-edge-subtle px-3 py-1.5">
                <EvidenceTruthStrip
                  coverage={toStripCoverage(envelope.coverage)}
                  freshness={toStripFreshness(envelope.freshness)}
                  omissions={envelope.coverage.omitted ?? undefined}
                />
                {envelope.coverage.omission_reasons.map((reason) => (
                  <span key={reason} className="text-3xs text-text-muted">
                    {reason}
                  </span>
                ))}
              </div>
            </div>
          );
        }}
      </ReadSection>
    </Panel>
  );
}

function hitSelection(hit: LcmMessageV1): SessionSelection {
  return { provider: hit.source, sessionId: hit.session_id };
}

function hitKey(hit: LcmMessageV1, index: number): string {
  if (hit.store_id != null) return `store:${hit.store_id}`;
  return `${hit.source ?? ''}:${hit.session_id}:${hit.message_id}:${index}`;
}

function SearchHit({
  hit,
  selected,
  onSelect,
}: {
  hit: LcmMessageV1;
  selected: boolean;
  onSelect: () => void;
}) {
  const snippet = hit.snippet ?? hit.content;
  return (
    <DataRow selected={selected} onSelect={onSelect} align="start" height={56}>
      <span className="td-legend w-14 shrink-0 truncate max-md:hidden">{hit.source ?? 'provider unrecorded'}</span>
      <span className="td-legend w-14 shrink-0 border border-edge-subtle px-1 py-1 text-center">
        {hit.role ?? 'role unrecorded'}
      </span>
      <span className="flex min-w-0 flex-1 flex-col gap-0.5">
        {snippet == null ? (
          <span className="text-3xs italic text-text-muted">body not held by the store</span>
        ) : (
          <span className="line-clamp-2 text-2xs leading-snug text-text-primary">{snippet}</span>
        )}
        <span className="td-value truncate text-3xs text-text-muted" title={hit.session_id}>
          {hit.session_id}
        </span>
      </span>
      <span
        className="td-value w-28 shrink-0 whitespace-nowrap text-right text-2xs text-text-muted max-md:hidden"
        data-cell="numeric"
      >
        {hit.timestamp != null ? formatStamp(hit.timestamp) : 'no timestamp'}
      </span>
    </DataRow>
  );
}
