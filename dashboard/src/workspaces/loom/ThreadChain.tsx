import { useMemo, type ReactNode } from 'react';
import { useSearchParams } from 'react-router';
import type { EnvelopeResult } from '../../data/query/envelope.ts';
import { ReadSection, envelopeReadState } from '../../ui/ReadSection.tsx';
import { StateChip } from '../../ui/StateChip';
import { Fact, Legend, Meter } from '../../ui/instrument.tsx';
import { formatCount } from '../../ui/format.ts';
import type { JourneyLane } from '../../viz/temporal/types.ts';
import { formatDurationSeconds, formatMoment } from './tracks.ts';
import { tokenCountLabel } from '../sessions/tokenLabel.ts';
import { summarizeChain } from './chain.ts';
import { initialPlaybackState, seekPlayback } from './playback.ts';
import { ThreadPlayback } from './ThreadPlayback.tsx';
import type { LoomPlayback } from './useLoomPlayback.ts';
import {
  type LcmMessageV1,
  type LcmSessionPayloadV1,
  type LcmSummaryNodeV1,
  type LoomBranchSpanV1,
  type LoomCommitV1,
  type LoomEditedFileV1,
  type LoomSourceStatusV1,
} from '../../contracts/generated.ts';

/**
 * The selected session's exact evidence: prompt → turns → tools, then the
 * provider-qualified edits, commits and branch/worktree spans the temporal
 * read attaches to the same session identity.
 *
 * The field draws these turns as glyphs on the selected lane; this workspace
 * is their exact text. Both read one playback cursor, so a turn the field has
 * not revealed is not listed here either.
 *
 * The LCM session endpoint may omit `timestamp`, so the chain is ordered by
 * the store's `ordinal` and presented as a sequence unless timestamps are
 * actually present — no elapsed times are inferred from ordinal positions.
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
  chain,
  chainPending,
  playback,
  summaryNodes,
  totalMessages,
  hasMoreMessages,
  hasMoreSummaryNodes,
  relations,
  onReturn,
}: {
  thread: JourneyLane;
  chain: EnvelopeResult<LcmSessionPayloadV1> | undefined;
  chainPending: boolean;
  playback: LoomPlayback;
  summaryNodes: readonly LcmSummaryNodeV1[];
  totalMessages: number;
  hasMoreMessages: boolean;
  hasMoreSummaryNodes: boolean;
  relations: ThreadRelations;
  onReturn?: () => void;
}) {
  const [params] = useSearchParams();
  const replaying = params.has('loomEvent');

  return (
    <section aria-label="Loaded execution" className="min-w-0">
      <div className="flex flex-col gap-2">
        <div className="flex flex-wrap items-start justify-between gap-2">
          <div className="flex min-w-0 flex-wrap items-center gap-3">
            {onReturn && (
              <button
                type="button"
                className="min-h-8 self-start text-xs text-text-secondary"
                onClick={onReturn}
              >
                ← All loaded sessions
              </button>
            )}
            <span className="text-xs font-medium leading-snug text-text-primary">{thread.label}</span>
          </div>

          <details>
            <summary className="min-h-8 flex items-center text-xs text-text-muted">Session metadata</summary>
            <dl className="grid grid-cols-2 gap-x-3 gap-y-1.5 text-2xs">
              <Fact label="session" value={thread.sessionId} />
              <Fact label="host" value={thread.provider} />
              <Fact label="agent" value={thread.agent ?? 'unrecorded'} muted={thread.agent == null} />
              <Fact label="started" value={formatMoment(thread.start)} />
              <Fact
                label="extent"
                value={thread.end != null ? formatDurationSeconds(thread.end - thread.start) : 'unrecorded'}
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
          state={envelopeReadState(chainPending, chain, {
            loading: 'reading transcript chain',
            transport: 'transcript chain could not be read',
          })}
        >
          {(envelope) => {
            const data = envelope.payload;
            if (data.exists === false) {
              return <StateChip kind="unknown" detail="no transcript recorded for this session" />;
            }
            return (
              <IsolatedChain
                key={thread.id}
                messages={data.messages}
                playback={playback}
                summaryNodes={summaryNodes}
                totalMessages={totalMessages}
                hasMoreMessages={hasMoreMessages}
                hasMoreSummaryNodes={hasMoreSummaryNodes}
                thread={thread}
              />
            );
          }}
        </ReadSection>
      </div>
      <details>
        <summary className="text-3xs text-text-muted">
          Session relations · {replaying ? 'withheld during replay' : 'loaded evidence'}
        </summary>
        {replaying ? (
          <StateChip
            kind="unavailable"
            detail="Session-wide commits, edits and branch rollups are withheld during replay: this read does not bind them to individual transcript events."
          />
        ) : (
          <ChainTerminus thread={thread} relations={relations} />
        )}
      </details>
    </section>
  );
}

function IsolatedChain({
  messages,
  playback,
  summaryNodes,
  totalMessages,
  hasMoreMessages,
  hasMoreSummaryNodes,
  thread,
}: {
  messages: readonly LcmMessageV1[];
  playback: LoomPlayback;
  summaryNodes: readonly LcmSummaryNodeV1[];
  totalMessages: number;
  hasMoreMessages: boolean;
  hasMoreSummaryNodes: boolean;
  thread: JourneyLane;
}) {
  const [params] = useSearchParams();
  const eventId = params.get('loomEvent');
  const { frames, state, setState, visible, active, cursor } = playback;
  const visibleIds = useMemo(() => new Set(visible.map((frame) => frame.id)), [visible]);
  const shownMessages = messages.filter((message) => visibleIds.has(message.message_id));
  const summary = summarizeChain(shownMessages, { message_count: shownMessages.length }, false);
  // A linked summary can have been created after its raw turn. Do not reveal
  // later compaction metadata just because the earlier message names its ID.
  const shownSummaries = state.followLive
    ? summaryNodes
    : summaryNodes.filter((node) => active?.timestamp != null && node.created_at <= active.timestamp);
  const toolCeiling = summary.tools.reduce((max, tool) => Math.max(max, tool.count), 0);
  return (
    <div className="flex flex-col gap-3">
      {cursor < 0 && eventId != null ? (
        <div role="status" className="flex flex-col gap-2">
          <StateChip
            kind="unavailable"
            detail={`Selected event ${eventId} is outside this loaded page; it may have been compacted or removed.`}
          />
          <button
            type="button"
            className="td-hit"
            onClick={() => setState(initialPlaybackState(frames.length))}
          >
            Return to loaded tail
          </button>
        </div>
      ) : null}
      <ThreadPlayback
        frames={frames}
        state={state}
        setState={setState}
        summaryNodes={shownSummaries}
        totalMessages={totalMessages}
        hasMoreMessages={hasMoreMessages}
        hasMoreSummaryNodes={hasMoreSummaryNodes}
      >
        {(toolbar, scrubber) => (
          <div className="flex flex-col gap-1 border border-edge-subtle px-1 py-1">
            {toolbar}
            {scrubber}
          </div>
        )}
      </ThreadPlayback>
      <p className="text-3xs text-text-muted">
        {visible.length} revealed · {frames.length - visible.length} withheld · source: {thread.provider} /{' '}
        {thread.sessionId}. Turns are drawn on the selected lane of the field above; a turn without a
        timestamp is placed in recorded order in the lane&apos;s sequence gutter.
      </p>
      <div className="flex flex-col gap-1">
        <Legend
          trailing={
            <span className="td-value shrink-0 text-3xs text-text-muted" data-cell="numeric">
              {formatCount(summary.messageCount)} turns
            </span>
          }
        >
          composition
        </Legend>
        <div className="flex flex-wrap gap-x-3 gap-y-1">
          {summary.roles.map((role) => (
            <span key={role.role} className="text-3xs text-text-secondary">
              <span className="tabular-nums text-text-primary">{role.count}</span> {role.role}
            </span>
          ))}
        </div>
      </div>

      <div className="flex flex-col gap-1.5">
        <Legend>tools invoked</Legend>
        {summary.tools.length === 0 ? (
          <StateChip kind="complete_zero_findings" detail="no turn named a tool" />
        ) : (
          <ul className="flex flex-col gap-1">
            {summary.tools.map((tool) => (
              <li key={tool.tool} className="flex items-center gap-2">
                <span className="min-w-0 flex-1 truncate text-3xs text-text-secondary">{tool.tool}</span>
                <Meter fraction={toolCeiling > 0 ? tool.count / toolCeiling : null} className="w-12 shrink-0" />
                <span className="td-value w-6 shrink-0 text-right text-3xs text-text-primary" data-cell="numeric">
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
            The store served no timestamp on any turn of this session, so this is the
            recorded order — not a timeline. No elapsed time between turns is known.
          </p>
        )}
        <ol className="max-h-56 overflow-auto border border-edge-subtle">
          {summary.steps.map((step, index) => (
            <li key={step.id} className="flex gap-2 border-b border-edge-subtle px-2 py-1 last:border-b-0">
              <span className="td-value w-5 shrink-0 text-right text-3xs text-text-muted" data-cell="numeric">
                {index + 1}
              </span>
              <button
                type="button"
                aria-label={`Inspect stored event ${step.id}`}
                onClick={() =>
                  setState(
                    seekPlayback(state, frames.length, frames.findIndex((frame) => frame.id === step.id)),
                  )
                }
                className="td-hit flex min-w-0 flex-1 flex-col gap-0.5 text-left"
              >
                <span className="flex items-baseline gap-1.5">
                  <span className="td-legend shrink-0 text-text-secondary">{step.role}</span>
                  {step.tool ? <span className="td-value truncate text-3xs text-text-primary">{step.tool}</span> : null}
                </span>
                {step.excerpt ? <span className="truncate text-3xs text-text-muted">{step.excerpt}</span> : null}
              </button>
              <span className="shrink-0 whitespace-nowrap text-3xs text-text-muted tabular">
                {chainStepTokenLabel(step)}
              </span>
            </li>
          ))}
        </ol>
        {summary.truncated ? (
          <span className="text-3xs text-text-muted">
            First {summary.steps.length} of {formatCount(summary.messageCount)} turns — the store has more
            than this page.
          </span>
        ) : null}
      </div>
    </div>
  );
}

function chainStepTokenLabel(step: ReturnType<typeof summarizeChain>['steps'][number]): string {
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
  thread: JourneyLane;
  relations: ThreadRelations;
}) {
  const { commits, editedFiles, branchSpans, commitStatus, branchStatus } = relations;
  return (
    <div className="flex flex-col gap-3">
      <CausalGroup label="→ edited files">
        {editedFiles.length > 0 ? (
          <ul className="flex flex-col gap-1">
            {editedFiles.map((file) => (
              <li key={`${file.path}:${file.change_type ?? ''}`} className="flex gap-2">
                <span className="min-w-0 flex-1 truncate text-3xs text-text-secondary">{file.path}</span>
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
                <span className="td-value truncate text-3xs text-text-primary">{commit.commit_sha}</span>
                <span className="text-3xs text-text-muted">
                  {commit.relation} · {commit.evidence}
                  {commit.span_overlap_kind ? ` · ${commit.span_overlap_kind}` : ''}
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
                : (commitStatus?.reason ?? commitStatus?.coverage.reason ?? 'commit attribution coverage is unavailable')
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
                  {formatDurationSeconds(span.last_at - span.first_at)} · {span.event_count}{' '}
                  {span.event_count === 1 ? 'event' : 'events'}
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
                : (branchStatus?.reason ?? branchStatus?.coverage.reason ?? 'branch/worktree span coverage is unavailable')
            }
          />
        )}
      </CausalGroup>
    </div>
  );
}

function CausalGroup({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="flex flex-col gap-1">
      <span className="td-legend text-text-secondary">{label}</span>
      {children}
    </div>
  );
}
