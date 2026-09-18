import { useEffect, useMemo, useState, type Dispatch, type SetStateAction } from 'react';
import { useSearchParams } from 'react-router';
import type { LcmMessageV1 } from '../../contracts/generated.ts';
import type { RevealBoundary } from '../../viz/temporal/types.ts';
import { LOOM_PARAMS } from './loomUrl.ts';
import {
  initialPlaybackState,
  playbackTickMillis,
  revealedFrames,
  stepPlayback,
  type LoomPlaybackFrame,
  type LoomPlaybackSpeed,
  type LoomPlaybackState,
} from './playback.ts';
import { playbackFrames } from './ThreadPlayback.tsx';

export interface LoomPlayback {
  readonly frames: readonly LoomPlaybackFrame[];
  readonly state: LoomPlaybackState;
  readonly setState: Dispatch<SetStateAction<LoomPlaybackState>>;
  /** Frames at or before the cursor; every frame while following. */
  readonly visible: readonly LoomPlaybackFrame[];
  readonly active: LoomPlaybackFrame | null;
  readonly cursor: number;
  /** The one reveal boundary the field, the minimap and the exact rows share.
   * Null while following the loaded tail. */
  readonly reveal: RevealBoundary | null;
  /** True when the URL names an event this loaded page does not hold. */
  readonly eventMissing: boolean;
}

/**
 * Presentation cursor over the selected session's canonically ordered page.
 *
 * The cursor is the `loomEvent` URL parameter, a stable message identity , 
 * so a link reproduces the same reveal boundary, and a refetch that drops the
 * named message reports it missing instead of silently snapping to the tail.
 * Playing and speed are ephemeral component state: a link never autoplays.
 */
export function useLoomPlayback(
  laneId: string | null,
  messages: readonly LcmMessageV1[] | undefined,
): LoomPlayback {
  const [params, setParams] = useSearchParams();
  const frames = useMemo(() => playbackFrames(messages ?? []), [messages]);
  const eventId = params.get(LOOM_PARAMS.event);
  const [playing, setPlaying] = useState(false);
  const [speed, setSpeed] = useState<LoomPlaybackSpeed>(1);
  const cursor = eventId == null
    ? frames.length - 1
    : frames.findIndex((frame) => frame.id === eventId);
  const state: LoomPlaybackState = {
    ...initialPlaybackState(frames.length),
    cursor,
    followLive: eventId == null,
    playing: playing && cursor >= 0,
    speed,
  };
  const setState: Dispatch<SetStateAction<LoomPlaybackState>> = (update) => {
    const next = typeof update === 'function' ? update(state) : update;
    setPlaying(next.playing);
    setSpeed(next.speed);
    const search = new URLSearchParams(params);
    if (next.followLive) search.delete(LOOM_PARAMS.event);
    else if (frames[next.cursor]) search.set(LOOM_PARAMS.event, frames[next.cursor]!.id);
    setParams(search, { replace: true });
  };
  useEffect(() => {
    if (!state.playing || frames.length === 0) return;
    const timer = window.setTimeout(
      () => setState(stepPlayback(state, frames.length, 1)),
      playbackTickMillis(speed),
    );
    return () => window.clearTimeout(timer);
  }, [state.playing, cursor, speed, frames, params]);
  const visible = revealedFrames(frames, cursor, state.followLive);
  const active = frames[cursor] ?? null;
  // A named event the page no longer holds cannot place a boundary, so the
  // lane reveals nothing rather than everything: the reader asked to stop
  // somewhere, and "somewhere" is not "at the tail".
  const reveal: RevealBoundary | null =
    laneId != null && !state.followLive
      ? active
        ? { time: active.timestamp, laneId, sequence: cursor }
        : { time: null, laneId, sequence: -1 }
      : null;
  return {
    frames,
    state,
    setState,
    visible,
    active,
    cursor,
    reveal,
    eventMissing: eventId != null && cursor < 0,
  };
}
