import { useEffect, useMemo, useRef, useState } from 'react';
import type { ReactNode } from 'react';
import type { LcmMessageV1, LcmSummaryNodeV1 } from '../../contracts/generated.ts';
import { StateChip } from '../../ui/StateChip.tsx';
import { Legend } from '../../ui/instrument.tsx';
import { formatStamp } from '../../ui/format.ts';
import { orderChainMessages } from './weave.ts';
import {
  initialPlaybackState,
  LOOM_PLAYBACK_SPEEDS,
  playbackTickMillis,
  reconcilePlaybackState,
  returnToLive,
  seekPlayback,
  stepPlayback,
  type LoomPlaybackFrame,
  type LoomPlaybackSpeed,
} from './playback.ts';

/**
 * A presentation cursor over the selected session's canonical LCM page.
 *
 * It never fetches, persists, or creates session events. "Follow live" means
 * follow the latest event in a subsequent canonical response; it deliberately
 * does not imply an SSE subscription that this route does not provide.
 */
export function ThreadPlayback({
  messages,
  summaryNodes,
  totalMessages,
  hasMoreMessages,
  hasMoreSummaryNodes,
}: {
  messages: readonly LcmMessageV1[];
  summaryNodes: readonly LcmSummaryNodeV1[];
  totalMessages: number;
  hasMoreMessages: boolean;
  hasMoreSummaryNodes: boolean;
}) {
  const frames = useMemo(() => playbackFrames(messages), [messages]);
  const signature = frames.map((frame) => frame.id).join('\u0000');
  const [state, setState] = useState(() => initialPlaybackState(frames.length));
  const active = frames[state.cursor] ?? null;
  const activeId = active?.id ?? null;
  const priorActiveId = useRef<string | null>(activeId);

  // A refetch can append or remove a page member. Stable identity wins over a
  // numeric cursor; live following is the explicit exception and takes tail.
  useEffect(() => {
    const previousActiveId = priorActiveId.current;
    setState((previous) => reconcilePlaybackState(previous, previousActiveId, frames));
  }, [frames, signature]);

  useEffect(() => {
    priorActiveId.current = activeId;
  }, [activeId]);

  useEffect(() => {
    if (!state.playing || frames.length === 0) return;
    const timer = window.setTimeout(() => {
      setState((previous) => stepPlayback(previous, frames.length, 1));
    }, playbackTickMillis(state.speed));
    return () => window.clearTimeout(timer);
  }, [frames.length, state.playing, state.speed, state.cursor]);

  if (frames.length === 0) {
    return (
      <section className="flex flex-col gap-1.5" aria-label="Session replay">
        <Legend>replay</Legend>
        <StateChip kind="complete_zero_findings" detail="this loaded transcript page holds no raw turns" />
      </section>
    );
  }

  const latest = frames.length - 1;
  const linkedNodes = active == null
    ? []
    : active.summaryNodeIds.map((id) => summaryNodes.find((node) => node.node_id === id) ?? null);

  return (
    <section className="flex flex-col gap-2" aria-label="Session replay">
      <Legend
        trailing={
          <span className="td-value shrink-0 text-3xs text-text-muted tabular-nums">
            {state.cursor + 1} / {frames.length} loaded turns
          </span>
        }
      >
        replay
      </Legend>

      <div
        role="toolbar"
        aria-label="Replay controls"
        className="flex flex-wrap items-center gap-1 border border-edge-subtle bg-surface-1 p-1"
      >
        <PlaybackButton
          label={state.playing ? 'Pause replay' : 'Play replay'}
          disabled={!state.playing && state.cursor >= latest}
          onClick={() => {
            setState((previous) => ({
              ...previous,
              playing: !previous.playing,
              followLive: previous.playing ? previous.followLive : false,
            }));
          }}
        >
          {state.playing ? 'pause' : 'play'}
        </PlaybackButton>
        <PlaybackButton
          label="Step to previous stored event"
          disabled={state.cursor === 0}
          onClick={() => setState((previous) => stepPlayback(previous, frames.length, -1))}
        >
          prev
        </PlaybackButton>
        <PlaybackButton
          label="Step to next stored event"
          disabled={state.cursor >= latest}
          onClick={() => setState((previous) => stepPlayback(previous, frames.length, 1))}
        >
          next
        </PlaybackButton>
        <label className="flex min-h-[var(--touch-target-min)] items-center gap-1 px-1 text-3xs text-text-secondary">
          speed
          <select
            aria-label="Replay speed"
            value={String(state.speed)}
            className="border border-edge-subtle bg-surface-2 px-1 py-0.5 text-3xs text-text-primary"
            onChange={(event) => {
              const speed = playbackSpeedFrom(event.currentTarget.value);
              setState((previous) => ({ ...previous, speed }));
            }}
          >
            {LOOM_PLAYBACK_SPEEDS.map((speed) => (
              <option key={speed} value={speed}>{speed}×</option>
            ))}
          </select>
        </label>
        {state.followLive ? (
          <span className="px-1 text-3xs text-text-muted">following loaded tail</span>
        ) : (
          <PlaybackButton
            label="Return replay to latest loaded event"
            onClick={() => setState((previous) => returnToLive(previous, frames.length))}
          >
            return to latest
          </PlaybackButton>
        )}
      </div>

      <label className="flex flex-col gap-1 text-3xs text-text-muted">
        Seek loaded event
        <input
          aria-label="Seek loaded event"
          type="range"
          min={0}
          max={latest}
          step={1}
          value={state.cursor}
          onChange={(event) => {
            setState((previous) => seekPlayback(previous, frames.length, Number(event.currentTarget.value)));
          }}
        />
      </label>

      {active ? (
        <div className="flex flex-col gap-1 border border-edge-subtle bg-surface-0 px-2 py-1.5">
          <span className="flex flex-wrap gap-x-2 gap-y-0.5 text-3xs text-text-muted">
            <span>{framePosition(active, state.cursor)}</span>
            <span>{active.timestamp == null ? 'timestamp unrecorded' : formatStamp(active.timestamp)}</span>
            <span>{active.role}</span>
            {active.tool ? <span>{active.tool}</span> : null}
          </span>
          {active.content == null ? (
            <span className="text-3xs italic text-text-muted">body not held by the store</span>
          ) : (
            <span className="line-clamp-6 whitespace-pre-wrap break-words text-3xs leading-snug text-text-secondary">
              {active.content}
            </span>
          )}
        </div>
      ) : null}

      <CompactionLinks nodes={linkedNodes} linkCount={active?.summaryNodeIds.length ?? 0} />

      <p className="text-3xs leading-relaxed text-text-muted">
        Playback advances only through the {frames.length.toLocaleString()} raw turns this
        response loaded, in the session authority&apos;s ordinal order. Its speed changes
        viewing pace, not recorded elapsed time.{' '}
        {hasMoreMessages
          ? `${totalMessages.toLocaleString()} turns exist for this session; later pages remain outside this replay until the canonical transcript page is opened.`
          : `This response contains all ${totalMessages.toLocaleString()} recorded turns.`}{' '}
        {hasMoreSummaryNodes
          ? 'More compaction boundaries exist outside this response page.'
          : 'Compaction links are kept separate from the event cursor unless the store linked them to this raw turn.'}
      </p>
    </section>
  );
}

function playbackFrames(messages: readonly LcmMessageV1[]): LoomPlaybackFrame[] {
  return orderChainMessages(messages).map((message) => ({
    id: message.message_id,
    ordinal: message.ordinal,
    timestamp: message.timestamp,
    role: message.role?.trim() || 'role unrecorded',
    tool: message.tool_name?.trim() || null,
    content: message.content,
    excerpt: message.snippet?.trim() || message.content?.replace(/\s+/g, ' ').trim() || '',
    summaryNodeIds: message.summary_node_ids,
  }));
}

function playbackSpeedFrom(value: string): LoomPlaybackSpeed {
  switch (value) {
    case '0.5':
      return 0.5;
    case '2':
      return 2;
    case '4':
      return 4;
    default:
      return 1;
  }
}

function framePosition(frame: LoomPlaybackFrame, index: number): string {
  return frame.ordinal == null ? `server position ${index + 1}` : `stored ordinal ${frame.ordinal}`;
}

function CompactionLinks({
  nodes,
  linkCount,
}: {
  nodes: readonly (LcmSummaryNodeV1 | null)[];
  linkCount: number;
}) {
  if (linkCount === 0) {
    return <StateChip kind="complete_zero_findings" detail="this raw turn has no compaction boundary link" />;
  }
  return (
    <div className="flex flex-col gap-1">
      <Legend>linked compaction boundaries</Legend>
      {nodes.map((node, index) =>
        node == null ? (
          <StateChip key={`missing-${index}`} kind="partial" detail="linked boundary is outside this loaded transcript page" />
        ) : (
          <div key={node.node_id} className="flex flex-col border border-edge-subtle px-2 py-1 text-3xs">
            <span className="text-text-secondary">{node.category} · depth {node.depth}</span>
            <span className="text-text-muted">created {formatStamp(node.created_at)} · {node.source_type}</span>
          </div>
        ),
      )}
    </div>
  );
}

function PlaybackButton({
  label,
  disabled,
  onClick,
  children,
}: {
  label: string;
  disabled?: boolean;
  onClick: () => void;
  children: ReactNode;
}) {
  return (
    <button
      type="button"
      aria-label={label}
      title={label}
      disabled={disabled}
      onClick={onClick}
      className="td-hit border border-edge-subtle bg-surface-2 px-1.5 py-0.5 text-3xs text-text-secondary disabled:text-text-muted"
    >
      {children}
    </button>
  );
}
