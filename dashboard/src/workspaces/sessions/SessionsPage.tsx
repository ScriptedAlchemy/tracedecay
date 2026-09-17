/**
 * SESSIONS — channel 04: complete message timeline and session index.
 *
 * Four typed authorities compose the surface and stay separately qualified:
 *
 *   timeline   `GET /api/plugins/hermes-lcm/timeline?bucket=day|hour&limit=N`
 *              — the hero aperture: message volume per UTC bucket with token
 *              provenance, over the most-recent N dated buckets.
 *   index      `GET /api/loom/temporal?limit=rows&offset=…` — the retained
 *              session store's provider-qualified rows, one real page at a
 *              time, with the Git relations recorded against that page.
 *   overview   `GET /api/plugins/hermes-lcm/overview` — the LCM window's own
 *              counts, providers, roles and compaction.
 *   search     `GET /api/plugins/hermes-lcm/search?q=…` — full text over the
 *              persisted transcripts; a hit selects a provider-qualified
 *              session like any index row.
 *
 * Selection, page, rows per page, bucket and window live in the URL so a
 * deep link reopens the same row on the same page over the same field.
 * Hover inspects and never selects: a hovered row marks its start bucket on
 * the time field; a hovered bucket dims the rows that did not start in it.
 */
import { useMemo, useRef, useState } from 'react';
import { useSearchParams } from 'react-router';
import {
  LcmOverviewPayloadV1Schema,
  LcmSearchPayloadV1Schema,
  LcmTimelinePayloadV1Schema,
  LoomTemporalPayloadV1Schema,
  type LoomSessionRowV1,
} from '../../contracts/generated.ts';
import { useEnvelope } from '../../data/query/useEnvelope.ts';
import { envelopeReadState } from '../../ui/ReadSection.tsx';
import { SearchField } from '../../ui/search/SearchField.tsx';
import { WorkspaceHeader } from '../../ui/instrument.tsx';
import { useScrollTabStop } from '../../ui/useScrollTabStop.ts';
import { LcmOverviewPanel } from './LcmOverviewPanel.tsx';
import { SessionIndex } from './SessionIndex.tsx';
import { SessionInspector } from './SessionInspector.tsx';
import { TranscriptSearchResults } from './TranscriptSearch.tsx';
import { VolumeTimeline } from './VolumeTimeline.tsx';
import {
  bucketKeyFor,
  pageOffset,
  readViewState,
  writeViewState,
  type SessionSelection,
  type SessionsViewState,
} from './model.ts';

const LCM = '/api/plugins/hermes-lcm';
const SEARCH_LIMIT = 50;

export function SessionsPage() {
  const [params, setParams] = useSearchParams();
  const view = useMemo(() => readViewState(params), [params]);
  const update = (patch: Partial<SessionsViewState>) =>
    setParams(writeViewState(params, { ...view, ...patch }));

  const timeline = useEnvelope(
    ['lcm', 'timeline', view.bucket, view.window],
    `${LCM}/timeline?bucket=${view.bucket}&limit=${view.window}`,
    LcmTimelinePayloadV1Schema,
  );
  const overview = useEnvelope(['lcm', 'overview'], `${LCM}/overview`, LcmOverviewPayloadV1Schema);
  const index = useEnvelope(
    ['sessions', 'index', view.rows, view.page],
    `/api/loom/temporal?limit=${view.rows}&offset=${pageOffset(view.page, view.rows)}`,
    LoomTemporalPayloadV1Schema,
  );

  const [query, setQuery] = useState('');
  const [submitted, setSubmitted] = useState('');
  const search = useEnvelope(
    ['lcm', 'search', submitted],
    `${LCM}/search?q=${encodeURIComponent(submitted)}&limit=${SEARCH_LIMIT}`,
    LcmSearchPayloadV1Schema,
    { enabled: submitted !== '' },
  );

  // Two inspect channels, one in each direction; neither is selection.
  const [inspectedBucket, setInspectedBucket] = useState<string | null>(null);
  const [inspectedRow, setInspectedRow] = useState<LoomSessionRowV1 | null>(null);
  const rowBucket =
    inspectedRow?.started_at != null ? bucketKeyFor(inspectedRow.started_at, view.bucket) : null;

  const timelineRead = envelopeReadState(timeline.isPending, timeline.data, {
    loading: 'reading LCM timeline',
    transport: 'LCM timeline could not be read',
  });
  const overviewRead = envelopeReadState(overview.isPending, overview.data, {
    loading: 'reading LCM overview',
    transport: 'LCM overview could not be read',
  });
  const indexRead = envelopeReadState(index.isPending, index.data, {
    loading: 'reading the session index',
    transport: 'the session index could not be read',
  });
  const searchRead = envelopeReadState(search.isPending, search.data, {
    loading: 'searching transcripts',
    transport: 'transcript search could not be read',
  });

  const selection = view.selection;
  const select = (next: SessionSelection | null) => update({ selection: next });

  const apertureRef = useRef<HTMLDivElement | null>(null);
  const apertureTabStop = useScrollTabStop(apertureRef);

  // From `lg` up the workspace is a fixed-height instrument: the aperture
  // column scrolls (or the index rows scroll inside their panel) and the
  // inspector scrolls beside it. Below `lg` the two stack and the page grows to
  // its content, so `main#td-main` scrolls the whole workspace instead of the
  // column overflowing into the inspector.
  return (
    <div className="flex flex-col lg:h-full lg:min-h-0">
      <WorkspaceHeader
        path="sessions"
        title="Sessions"
        note="complete message timeline and session index"
      />
      <div className="flex flex-1 max-lg:flex-col lg:min-h-0">
        <div
          ref={apertureRef}
          role="region"
          aria-label="Timeline and session index"
          tabIndex={apertureTabStop}
          className="flex min-w-0 flex-1 flex-col gap-3 p-3 lg:min-h-0 lg:overflow-auto"
        >
          <div className="shrink-0">
            <VolumeTimeline
              read={timelineRead}
              bucket={view.bucket}
              window={view.window}
              onBucketChange={(bucket) => update({ bucket })}
              onWindowChange={(window) => update({ window })}
              inspectedBucket={rowBucket}
              onInspectBucket={setInspectedBucket}
            />
          </div>
          <div className="max-w-2xl shrink-0">
            <SearchField
              value={query}
              onChange={setQuery}
              onSubmit={() => setSubmitted(query.trim())}
              onClear={() => {
                setQuery('');
                setSubmitted('');
              }}
              label="Search transcripts"
              placeholder="Search transcripts"
              hint="press / to focus, Esc to clear · full text over persisted transcripts, separate from the index page"
              submitted={submitted}
            />
          </div>
          {submitted !== '' ? (
            <TranscriptSearchResults
              read={searchRead}
              submitted={submitted}
              selection={selection}
              onSelect={select}
            />
          ) : (
            <SessionIndex
              read={indexRead}
              page={view.page}
              rows={view.rows}
              onPageChange={(page) => update({ page })}
              onRowsChange={(rows) => update({ rows, page: 1 })}
              selection={selection}
              onSelect={select}
              onInspectRow={setInspectedRow}
              inspectedBucket={inspectedBucket}
              bucket={view.bucket}
            />
          )}
        </div>
        <aside
          aria-label="Inspector"
          className="shrink-0 overflow-auto border-edge-subtle bg-surface-1 max-lg:h-[28rem] max-lg:border-t lg:w-80 lg:border-l xl:w-[22rem]"
        >
          {selection ? (
            <SessionInspector
              key={`${selection.provider ?? ''}\u0000${selection.sessionId}`}
              selection={selection}
              index={indexRead}
              indexPage={view.page}
              onSelectProvider={(provider) =>
                update({ selection: { provider, sessionId: selection.sessionId } })
              }
              onClose={() => select(null)}
            />
          ) : (
            <LcmOverviewPanel
              overview={overviewRead}
              timeline={timelineRead}
              index={indexRead}
              bucket={view.bucket}
              page={view.page}
              rows={view.rows}
            />
          )}
        </aside>
      </div>
    </div>
  );
}
