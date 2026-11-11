import { describe, expect, it } from 'vitest';
import {
  CHANNELS,
  channelForPathname,
  channelNumber,
  paletteMatchesQuery,
  workspaceSearchText,
} from './channels.ts';

describe('channelNumber', () => {
  it('never invents a channel for a path it does not have', () => {
    expect(channelNumber('code')).toBe('06');
    expect(channelNumber('doctor')).toBe('--');
    expect(channelNumber('')).toBe('--');
  });
});

describe('channelForPathname', () => {
  it('resolves the index route to Brain, the surface the index route mounts', () => {
    expect(channelForPathname('/')?.path).toBe('brain');
    expect(channelForPathname('')?.path).toBe('brain');
  });

  it('resolves a workspace route by its first segment', () => {
    expect(channelForPathname('/code')?.path).toBe('code');
    expect(channelForPathname('/settings/')?.path).toBe('settings');
    expect(channelForPathname('/loom/thread/abc')?.path).toBe('loom');
  });

  it('answers null for a path that names no workspace', () => {
    expect(channelForPathname('/doctor')).toBeNull();
    expect(channelForPathname('/tools')).toBeNull();
  });
});

describe('command-palette task discovery', () => {
  const matches = (query: string) =>
    CHANNELS.filter((channel) =>
      paletteMatchesQuery(workspaceSearchText(channel), query),
    ).map((channel) => channel.path);

  it('finds task destinations when their workspace names are unknown', () => {
    expect(matches('temporal replay')).toContain('loom');
    expect(matches('transcripts')).toContain('sessions');
    expect(matches('source symbols')).toContain('code');
    expect(matches('  PR   REVIEWS  ')).toContain('delivery');
    expect(matches('task board')).toContain('work');
    expect(matches('scheduled jobs')).toContain('automations');
  });

  it('keeps existing workspace-name and project-name matches', () => {
    expect(matches('code')).toContain('code');
    expect(matches('code')).toContain('explorer');
    expect(paletteMatchesQuery('garden project', '  GARDEN ')).toBe(true);
    expect(paletteMatchesQuery('garden project', 'temporal')).toBe(false);
  });

  it('preserves every existing route on empty input and does not invent destinations', () => {
    expect(matches('')).toEqual(CHANNELS.map((channel) => channel.path));
    expect(matches('not-a-real-destination')).toEqual([]);
  });
});
