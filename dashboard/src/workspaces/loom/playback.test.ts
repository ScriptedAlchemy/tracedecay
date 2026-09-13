import { describe, expect, it } from 'vitest';
import {
  clampPlaybackCursor,
  initialPlaybackState,
  playbackTickMillis,
  revealedFrames,
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

it('withholds later content by both source position and recorded timestamp', () => {
  const frames = [
    { ...frame('earlier-ordinal-later-time'), timestamp: 50 },
    { ...frame('selected'), timestamp: 20 },
    { ...frame('later-ordinal-earlier-time'), timestamp: 10 },
    frame('undated-future'),
  ];
  expect(revealedFrames(frames, 1, false).map(({ id }) => id)).toEqual(['selected']);
  expect(revealedFrames(frames, -1, false)).toEqual([]);
  expect(revealedFrames(frames, 3, true)).toEqual(frames);
  expect(revealedFrames(frames, 3, false)).toEqual(frames);
});
