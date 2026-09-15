import { useEffect, useMemo, useState, type ReactNode, type Dispatch, type SetStateAction } from 'react';
import { useSearchParams } from 'react-router';
import { LoadedEventCanvas } from './WeaveCanvas.tsx';
import { initialPlaybackState, playbackTickMillis, revealedFrames, seekPlayback, stepPlayback, type LoomPlaybackState, type LoomPlaybackSpeed } from './playback.ts';
import { ReadSection, envelopeReadState } from '../../ui/ReadSection.tsx';
import { StateChip } from '../../ui/StateChip';
import { Fact, Legend, Meter, Panel } from '../../ui/instrument.tsx';
import { formatCount } from '../../ui/format.ts';
import { useEnvelope } from '../../data/query/useEnvelope.ts';
import { formatDurationSeconds, formatMoment } from './tracks.ts';
import { tokenCountLabel } from '../sessions/tokenLabel.ts';
import { summarizeChain, type PlacedThread } from './weave.ts';
import { ThreadPlayback, playbackFrames } from './ThreadPlayback.tsx';
import {
  LcmSessionPayloadV1Schema,
  type LcmMessageV1,
  type LcmSummaryNodeV1,
  type LoomBranchSpanV1,
  type LoomCommitV1,
  type LoomEditedFileV1,
  type LoomSourceStatusV1,
} from '../../contracts/generated.ts';

/**
 * The selected thread's chain: prompt → turns → tools.
 *
 * This is the "selecting a thread isolates its chain" half of the Loom:
 * prompt → tools comes from session detail, then provider-qualified edits,
 * commits and branch/worktree spans continue from the Loom temporal read.
 *
 * One further honesty point drives the whole layout: the LCM session endpoint
 * may omit `timestamp`, so the chain is always ordered by the store's
 * `ordinal`. It is presented as a sequence unless timestamps are actually
 * present — no elapsed times or gaps are inferred from ordinal positions.
 */
export interface ThreadRelations {
  commits: readonly LoomCommitV1[];
  editedFiles: readonly LoomEditedFileV1[];
  branchSpans: readonly LoomBranchSpanV1[];
  commitStatus: LoomSourceStatusV1 | null;
  branchStatus: LoomSourceStatusV1 | null;
}

export function ThreadChain({
  thread,
  relations,
  onReturn,
}: {
  onReturn?: () => void;
  thread: PlacedThread | null;
  relations: ThreadRelations;
}) {
  const [params] = useSearchParams();
  const replaying = params.has('loomEvent');
  const chain = useEnvelope(
    ['loom', 'chain', thread?.id ?? 'none'],
    `/api/plugins/hermes-lcm/session/${encodeURIComponent(thread?.sessionId ?? '')}?limit=200`,
    LcmSessionPayloadV1Schema,
    { enabled: thread != null },
  );

  if (!thread) {
    return (
      <Panel legend="Thread">
        <p className="text-2xs leading-relaxed text-text-muted">
          Select a thread — on the weave or in the table — to isolate it and
          pull its chain of turns and tool calls from the session store.
        </p>
      </Panel>
    );
  }

  return (
    <section aria-label="Loaded execution" className="min-w-0">
      <div className="flex flex-col gap-2">
        <div className="flex flex-wrap items-start justify-between gap-2">
        <div className="flex min-w-0 flex-wrap items-center gap-3">
          {onReturn && <button type="button" className="min-h-8 self-start text-xs text-text-secondary" onClick={onReturn}>← All loaded sessions</button>}
          <span className="text-xs font-medium leading-snug text-text-primary">
            {thread.label}
          </span>
        </div>

        <details>
          <summary className="min-h-8 flex items-center text-xs text-text-muted">Session metadata</summary>
        <dl className="grid grid-cols-2 gap-x-3 gap-y-1.5 text-2xs">
          <Fact label="session" value={thread.sessionId} />
          <Fact label="host" value={thread.host} />
          <Fact label="started" value={formatMoment(thread.start)} />
          <Fact
            label="extent"
            value={
              thread.end != null
                ? formatDurationSeconds(thread.end - thread.start)
                : 'unrecorded'
            }
            muted={thread.end == null}
          />
          <Fact label="kind" value={thread.isSubagent ? 'subagent' : 'session'} />
        </dl>

        {!replaying && thread.models.length > 0 ? (
          <div className="flex flex-col gap-1">
            <Legend>models</Legend>
            <div className="flex flex-wrap gap-1">
              {thread.models.map((model) => (
                <span
                  key={model}
                  className="td-value border border-edge-subtle px-1.5 py-0.5 text-3xs text-text-secondary"
                >
                  {model}
                </span>
              ))}
            </div>
          </div>
        ) : null}

        </details>
        </div>

        <ReadSection
          title="Chain"
          chrome="centered"
          state={envelopeReadState(chain.isPending, chain.data, {
            loading: 'reading transcript chain',
            transport: 'transcript chain could not be read',
          })}
        >
          {(envelope) => {
            const data = envelope.payload;
            if (data.exists === false) {
              return (
                <StateChip
                  kind="unknown"
                  detail="no transcript recorded for this session"
                />
              );
            }
            return (
              <IsolatedChain
                key={thread.id}
                messages={data.messages}
                messageCount={data.counts.message_count}
                hasMoreMessages={data.has_more_messages || data.next_cursor != null}
                hasMoreSummaryNodes={data.has_more_summary_nodes}
                summaryNodes={data.summary_nodes}
                thread={thread}
              />
            );
          }}
        </ReadSection>
      </div>
      <details><summary className="text-3xs text-text-muted">Session relations · {replaying ? "withheld during replay" : "loaded evidence"}</summary>
      {replaying
      ? <StateChip kind="unavailable" detail="Session-wide commits, edits and branch rollups are withheld during replay: this read does not bind them to individual transcript events." />
      : <ChainTerminus thread={thread} relations={relations} />}
      </details>
    </section>
  );
}

function IsolatedChain({
  messages,
  messageCount,
  hasMoreMessages,
  hasMoreSummaryNodes,
  summaryNodes,
  thread,
}: {
  messages: readonly LcmMessageV1[];
  messageCount: number;
  hasMoreMessages: boolean;
  hasMoreSummaryNodes: boolean;
  summaryNodes: readonly LcmSummaryNodeV1[];
  thread: PlacedThread;
}) {
  const [params, setParams] = useSearchParams();
  const frames = useMemo(() => playbackFrames(messages), [messages]);
  const eventId = params.get('loomEvent');
  const [playing, setPlaying] = useState(false);
  const [speed, setSpeed] = useState<LoomPlaybackSpeed>(1);
  const cursor = eventId == null ? frames.length - 1 : frames.findIndex((frame) => frame.id === eventId);
  const state: LoomPlaybackState = { ...initialPlaybackState(frames.length), cursor, followLive: eventId == null, playing: playing && cursor >= 0, speed };
  const setPlayback: Dispatch<SetStateAction<LoomPlaybackState>> = (update) => {
    const next = typeof update === 'function' ? update(state) : update;
    setPlaying(next.playing);
    setSpeed(next.speed);
    const search = new URLSearchParams(params);
    if (next.followLive) { search.delete('loomEvent'); search.delete('loomWindow'); }
    else if (frames[next.cursor]) search.set('loomEvent', frames[next.cursor]!.id);
    setParams(search, { replace: true });
  };
  useEffect(() => {
    if (!state.playing || frames.length === 0) return;
    const timer = window.setTimeout(() => setPlayback(stepPlayback(state, frames.length, 1)), playbackTickMillis(speed));
    return () => window.clearTimeout(timer);
  }, [state.playing, cursor, speed, frames, params]);
  const visible = revealedFrames(frames, cursor, state.followLive);
  const visibleIds = new Set(visible.map((frame) => frame.id));
  const shownMessages = messages.filter((message) => visibleIds.has(message.message_id));
  const summary = summarizeChain(shownMessages, { message_count: shownMessages.length }, false);
  const active = frames[cursor];
  // A linked summary can have been created after its raw turn. Do not reveal
  // later compaction metadata just because the earlier message names its ID.
  const shownSummaries = state.followLive ? summaryNodes : summaryNodes.filter((node) => active?.timestamp != null && node.created_at <= active.timestamp);
  const toolCeiling = summary.tools.reduce(
    (max, tool) => Math.max(max, tool.count),
    0,
  );
  return (
    <div className="flex flex-col gap-3">
      {cursor < 0 && eventId != null ? (
        <div role="status" className="flex flex-col gap-2">
          <StateChip kind="unavailable" detail={`Selected event ${eventId} is outside this loaded page; it may have been compacted or removed.`} />
          <button type="button" className="td-hit" onClick={() => setPlayback(initialPlaybackState(frames.length))}>Return to loaded tail</button>
        </div>
      ) : null}
      <ThreadPlayback
        frames={frames}
        state={state}
        setState={setPlayback}
        summaryNodes={shownSummaries}
        totalMessages={messageCount}
        hasMoreMessages={hasMoreMessages}
        hasMoreSummaryNodes={hasMoreSummaryNodes}
      >
      {(toolbar, scrubber) => <LoadedEventCanvas
        toolbar={toolbar}
        scrubber={scrubber}
        frames={frames}
        visible={visible}
        activeId={active?.id ?? null}
        onSelect={(id) => setPlayback(seekPlayback(state, frames.length, frames.findIndex((frame) => frame.id === id)))}
        onInspect={() => setPlaying(false)}
      />}
      </ThreadPlayback>
      <p className="text-3xs text-text-muted">{visible.length} revealed · {frames.length - visible.length} withheld · source: {thread.host} / {thread.sessionId}. Spawn, handoff and rejoin evidence is not served by this read.</p>
                <div className="flex flex-col gap-1">
                  <Legend
                    trailing={
                      <span
                        className="td-value shrink-0 text-3xs text-text-muted"
                        data-cell="numeric"
                      >
                        {formatCount(summary.messageCount)} turns
                      </span>
                    }
                  >
                    composition
                  </Legend>
                  <div className="flex flex-wrap gap-x-3 gap-y-1">
                    {summary.roles.map((role) => (
                      <span key={role.role} className="text-3xs text-text-secondary">
                        <span className="tabular-nums text-text-primary">
                          {role.count}
                        </span>{' '}
                        {role.role}
                      </span>
                    ))}
                  </div>
                </div>

                <div className="flex flex-col gap-1.5">
                  <Legend>tools invoked</Legend>
                  {summary.tools.length === 0 ? (
                    <StateChip
                      kind="complete_zero_findings"
                      detail="no turn named a tool"
                    />
                  ) : (
                    <ul className="flex flex-col gap-1">
                      {summary.tools.map((tool) => (
                        <li key={tool.tool} className="flex items-center gap-2">
                          <span className="min-w-0 flex-1 truncate text-3xs text-text-secondary">
                            {tool.tool}
                          </span>
                          <Meter
                            fraction={toolCeiling > 0 ? tool.count / toolCeiling : null}
                            className="w-12 shrink-0"
                          />
                          <span
                            className="td-value w-6 shrink-0 text-right text-3xs text-text-primary"
                            data-cell="numeric"
                          >
                            {tool.count}
                          </span>
                        </li>
                      ))}
                    </ul>
                  )}
                </div>

                <div className="flex flex-col gap-1.5">
                  <Legend
                    trailing={
                      <span className="shrink-0 text-3xs text-text-muted">
                        {summary.timestamped ? 'time-ordered' : 'ordinal order'}
                      </span>
                    }
                  >
                    sequence
                  </Legend>
                  {summary.timestamped ? null : (
                    <p className="text-3xs leading-relaxed text-text-muted">
                      The store served no timestamp on any turn of this session,
                      so this is the recorded order — not a timeline. No elapsed
                      time between turns is known.
                    </p>
                  )}
                  <ol className="max-h-56 overflow-auto border border-edge-subtle">
                    {summary.steps.map((step, index) => (
                      <li
                        key={step.id}
                        className="flex gap-2 border-b border-edge-subtle px-2 py-1 last:border-b-0"
                      >
                        <span
                          className="td-value w-5 shrink-0 text-right text-3xs text-text-muted"
                          data-cell="numeric"
                        >
                          {index + 1}
                        </span>
                        <button type="button" aria-label={`Inspect stored event ${step.id}`} onClick={() => setPlayback(seekPlayback(state, frames.length, frames.findIndex((frame) => frame.id === step.id)))} className="td-hit flex min-w-0 flex-1 flex-col gap-0.5 text-left">
                          <span className="flex items-baseline gap-1.5">
                            <span className="td-legend shrink-0 text-text-secondary">
                              {step.role}
                            </span>
                            {step.tool ? (
                              <span className="td-value truncate text-3xs text-text-primary">
                                {step.tool}
                              </span>
                            ) : null}
                          </span>
                          {step.excerpt ? (
                            <span className="truncate text-3xs text-text-muted">
                              {step.excerpt}
                            </span>
                          ) : null}
                        </button>
                        <span className="shrink-0 whitespace-nowrap text-3xs text-text-muted tabular">
                          {chainStepTokenLabel(step)}
                        </span>
                      </li>
                    ))}
                  </ol>
                  {summary.truncated ? (
                    <span className="text-3xs text-text-muted">
                      First {summary.steps.length} of{' '}
                      {formatCount(summary.messageCount)} turns — the store has
                      more than this page.
                    </span>
                  ) : null}
                </div>
              </div>
  );
}

function chainStepTokenLabel(
  step: ReturnType<typeof summarizeChain>['steps'][number],
): string {
  // Unlike the transcript inspector, which omits the line, this rail keeps a
  // cell per step — so absence is worded rather than left blank.
  return tokenCountLabel(step.tokenCount, step.tokenCountProvenance) ?? 'tokens unknown';
}

/** Durable causal rows attached to the selected provider-qualified session.
 * Missing edited-file metadata remains unknown; empty durable commit/span
 * queries are true zero-findings. */
function ChainTerminus({
  thread,
  relations,
}: {
  thread: PlacedThread;
  relations: ThreadRelations;
}) {
  const {
    commits,
    editedFiles,
    branchSpans,
    commitStatus,
    branchStatus,
  } = relations;
  return (
    <div className="flex flex-col gap-3">
      <CausalGroup label="→ edited files">
        {editedFiles.length > 0 ? (
          <ul className="flex flex-col gap-1">
            {editedFiles.map((file) => (
              <li key={`${file.path}:${file.change_type ?? ''}`} className="flex gap-2">
                <span className="min-w-0 flex-1 truncate text-3xs text-text-secondary">
                  {file.path}
                </span>
                {file.hunks != null ? (
                  <span className="td-value shrink-0 text-3xs text-text-muted">
                    {file.hunks} {file.hunks === 1 ? 'hunk' : 'hunks'}
                  </span>
                ) : null}
              </li>
            ))}
          </ul>
        ) : (
          <StateChip
            kind={thread.editedFilesRecorded ? 'complete_zero_findings' : 'unknown'}
            detail={
              thread.editedFilesRecorded
                ? 'recorded edited-files rollup is empty'
                : 'this session has no recorded edited-files rollup'
            }
          />
        )}
      </CausalGroup>

      <CausalGroup label="→ commits">
        {commits.length > 0 ? (
          <ul className="flex flex-col gap-1">
            {commits.map((commit) => (
              <li key={commit.commit_sha} className="flex flex-col">
                <span className="td-value truncate text-3xs text-text-primary">
                  {commit.commit_sha}
                </span>
                <span className="text-3xs text-text-muted">
                  {commit.relation} · {commit.evidence} · confidence {commit.confidence}
                </span>
              </li>
            ))}
          </ul>
        ) : (
          <StateChip
            kind={commitStatus?.state === 'ready' ? 'complete_zero_findings' : 'unknown'}
            detail={
              commitStatus?.state === 'ready'
                ? 'commit_sessions has no attribution for this session'
                : (commitStatus?.reason ??
                  commitStatus?.coverage.reason ??
                  'commit attribution coverage is unavailable')
            }
          />
        )}
      </CausalGroup>

      <CausalGroup label="→ branch & worktree spans">
        {branchSpans.length > 0 ? (
          <ul className="flex flex-col gap-1">
            {branchSpans.map((span) => (
              <li key={`${span.worktree}:${span.first_at}`} className="flex flex-col">
                <span className="truncate text-3xs text-text-secondary">
                  {span.branch ?? 'branch unrecorded'} · {span.worktree}
                </span>
                <span className="text-3xs text-text-muted">
                  {formatDurationSeconds(span.last_at - span.first_at)} ·{' '}
                  {span.event_count} {span.event_count === 1 ? 'event' : 'events'}
                </span>
              </li>
            ))}
          </ul>
        ) : (
          <StateChip
            kind={branchStatus?.state === 'ready' ? 'complete_zero_findings' : 'unknown'}
            detail={
              branchStatus?.state === 'ready'
                ? 'session_git_spans has no span for this session'
                : (branchStatus?.reason ??
                  branchStatus?.coverage.reason ??
                  'branch/worktree span coverage is unavailable')
            }
          />
        )}
      </CausalGroup>

    </div>
  );
}

function CausalGroup({
  label,
  children,
}: {
  label: string;
  children: ReactNode;
}) {
  return (
    <div className="flex flex-col gap-1">
      <span className="td-legend text-text-secondary">{label}</span>
      {children}
    </div>
  );
}
