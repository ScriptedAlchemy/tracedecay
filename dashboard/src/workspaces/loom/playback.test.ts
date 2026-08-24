import { describe, expect, it } from 'vitest';
import {
  clampPlaybackCursor,
  initialPlaybackState,
  playbackTickMillis,
  reconcilePlaybackState,
  returnToLive,
  seekPlayback,
  stepPlayback,
  type LoomPlaybackFrame,
} from './playback.ts';

function frame(id: string): LoomPlaybackFrame {
  return {
    id,
    ordinal: null,
    timestamp: null,
    role: 'assistant',
    tool: null,
    content: null,
    excerpt: '',
    summaryNodeIds: [],
  };
}

describe('Loom playback presentation state', () => {
  it('starts paused at the latest loaded canonical event', () => {
    expect(initialPlaybackState(3)).toEqual({
      cursor: 2,
      playing: false,
      speed: 1,
      followLive: true,
    });
  });

  it('steps only among loaded frames and never wraps from the end', () => {
    const state = { ...initialPlaybackState(3), cursor: 1, followLive: false };
    expect(stepPlayback(state, 3, 1)).toMatchObject({ cursor: 2, followLive: false });
    expect(stepPlayback(state, 3, -1)).toMatchObject({ cursor: 0, followLive: false });
    expect(stepPlayback(state, 3, 1).cursor).toBe(2);
    expect(clampPlaybackCursor(99, 3)).toBe(2);
  });

  it('keeps a stable event across a refetch unless following the loaded tail', () => {
    const frames = [frame('a'), frame('b'), frame('c')];
    const pausedAtB = { ...initialPlaybackState(2), cursor: 1, followLive: false };
    expect(reconcilePlaybackState(pausedAtB, 'b', frames).cursor).toBe(1);
    expect(reconcilePlaybackState(initialPlaybackState(2), 'b', frames).cursor).toBe(2);
  });

  it('suspends follow-live on a seek and restores it only by returning to tail', () => {
    const sought = seekPlayback(initialPlaybackState(4), 4, 1);
    expect(sought).toMatchObject({ cursor: 1, playing: false, followLive: false });
    expect(returnToLive(sought, 4)).toMatchObject({
      cursor: 3,
      playing: false,
      followLive: true,
    });
  });

  it('treats viewing speed as presentation pacing, not source time', () => {
    expect(playbackTickMillis(0.5)).toBe(1600);
    expect(playbackTickMillis(4)).toBe(200);
  });
});
