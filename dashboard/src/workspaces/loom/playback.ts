/**
 * Presentation state for an immutable, server-supplied sequence.
 *
 * This module deliberately has no clock and no session cache. A Loom frame is
 * one exact LCM message. The UI may advance between those frames at a chosen
 * viewing speed, but it never invents an event, a timestamp, or duration.
 */
export interface LoomPlaybackFrame {
  /** Stable LCM message identity — selection survives a render or refetch. */
  id: string;
  /** The session store's ordering key, when it supplied one. */
  ordinal: number | null;
  /** Recorded event timestamp, not a browser-produced playback time. */
  timestamp: number | null;
  role: string;
  tool: string | null;
  content: string | null;
  excerpt: string;
  /** Exact compaction nodes named by this raw message, if the store linked any. */
  summaryNodeIds: readonly string[];
}

export type LoomPlaybackSpeed = 0.5 | 1 | 2 | 4;

export const LOOM_PLAYBACK_SPEEDS: readonly LoomPlaybackSpeed[] = [0.5, 1, 2, 4];

export interface LoomPlaybackState {
  /** Index in the currently loaded, canonically ordered page. */
  cursor: number;
  playing: boolean;
  speed: LoomPlaybackSpeed;
  /** New canonical frames advance the cursor only while this is true. */
  followLive: boolean;
}

export function initialPlaybackState(frameCount: number): LoomPlaybackState {
  return {
    cursor: latestCursor(frameCount),
    playing: false,
    speed: 1,
    followLive: true,
  };
}

/** The loaded tail is the only honest "live" point when no stream exists. */
export function latestCursor(frameCount: number): number {
  return Math.max(frameCount - 1, 0);
}

export function clampPlaybackCursor(cursor: number, frameCount: number): number {
  return Math.min(Math.max(cursor, 0), latestCursor(frameCount));
}

/**
 * Preserve the current stable event across a canonical refetch. A live
 * follower instead moves to the new loaded tail. If the selected event was
 * compacted or fell outside the loaded page, clamp it rather than pretending a
 * nearby event is the same event.
 */
export function reconcilePlaybackState(
  previous: LoomPlaybackState,
  previousFrameId: string | null,
  frames: readonly LoomPlaybackFrame[],
): LoomPlaybackState {
  if (frames.length === 0) return { ...previous, cursor: 0, playing: false };
  if (previous.followLive) {
    return { ...previous, cursor: latestCursor(frames.length) };
  }
  const retained = previousFrameId == null
    ? -1
    : frames.findIndex((frame) => frame.id === previousFrameId);
  return {
    ...previous,
    cursor: retained >= 0 ? retained : clampPlaybackCursor(previous.cursor, frames.length),
  };
}

/** One discrete frame at a time. End-of-page stops rather than wrapping. */
export function stepPlayback(
  state: LoomPlaybackState,
  frameCount: number,
  direction: -1 | 1,
): LoomPlaybackState {
  const cursor = clampPlaybackCursor(state.cursor + direction, frameCount);
  return {
    ...state,
    cursor,
    playing: direction > 0 && cursor < latestCursor(frameCount) ? state.playing : false,
    followLive: false,
  };
}

/** A seek is a reader intent, so it suspends follow-live until explicitly resumed. */
export function seekPlayback(
  state: LoomPlaybackState,
  frameCount: number,
  cursor: number,
): LoomPlaybackState {
  return {
    ...state,
    cursor: clampPlaybackCursor(cursor, frameCount),
    playing: false,
    followLive: false,
  };
}

export function returnToLive(
  state: LoomPlaybackState,
  frameCount: number,
): LoomPlaybackState {
  return {
    ...state,
    cursor: latestCursor(frameCount),
    playing: false,
    followLive: true,
  };
}

/** Viewing speed controls only the interval between discrete frame changes. */
export function playbackTickMillis(speed: LoomPlaybackSpeed): number {
  return 800 / speed;
}
