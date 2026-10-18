import { describe, expect, it } from 'vitest';
import {
  CHANNELS,
  channelForPathname,
  channelNumber,
  viewForPathname,
  type ViewPath,
} from './channels.ts';
import { LEGACY_PATHS } from './legacyRoutes.ts';
import { VIEW_CHUNKS } from './viewChunks.ts';

const VIEW_PATHS: readonly string[] = CHANNELS.flatMap((channel) =>
  channel.views.map((view) => view.path),
);

describe('the channel list', () => {
  it('names five task families, each opening on a view at its own path', () => {
    expect(CHANNELS.map((channel) => channel.path)).toEqual([
      'code',
      'memory',
      'work',
      'operations',
      'settings',
    ]);
    for (const channel of CHANNELS) {
      expect(channel.views[0]?.path).toBe(channel.path);
      for (const view of channel.views) {
        expect(view.path === channel.path || view.path.startsWith(`${channel.path}/`)).toBe(true);
      }
    }
  });

  it('lists every view once and routes exactly those', () => {
    expect(new Set(VIEW_PATHS).size).toBe(VIEW_PATHS.length);
    expect(Object.keys(VIEW_CHUNKS).sort()).toEqual([...VIEW_PATHS].sort());
  });

  it('keeps every retired path pointed at a view that exists', () => {
    for (const to of Object.values(LEGACY_PATHS)) {
      expect(VIEW_PATHS).toContain(to satisfies ViewPath);
    }
    // A retired path never shadows a live one.
    for (const from of Object.keys(LEGACY_PATHS)) {
      expect(VIEW_PATHS).not.toContain(from);
    }
  });
});

describe('channelNumber', () => {
  it('numbers a view by its family', () => {
    expect(channelNumber('code')).toBe('01');
    expect(channelNumber('memory/sessions')).toBe('02');
    expect(channelNumber('operations/costs')).toBe('04');
    expect(channelNumber('settings')).toBe('05');
  });

  it('never invents a channel for a path it does not have', () => {
    expect(channelNumber('doctor')).toBe('--');
    expect(channelNumber('sessions')).toBe('--');
    expect(channelNumber('')).toBe('--');
  });
});

describe('channelForPathname', () => {
  it('resolves the index route to Memory, the family Brain opens in', () => {
    expect(channelForPathname('/')?.path).toBe('memory');
    expect(channelForPathname('')?.path).toBe('memory');
  });

  it('resolves a route by its first segment, whatever the view', () => {
    expect(channelForPathname('/code')?.path).toBe('code');
    expect(channelForPathname('/settings/')?.path).toBe('settings');
    expect(channelForPathname('/memory/loom')?.path).toBe('memory');
    expect(channelForPathname('/operations/costs')?.path).toBe('operations');
  });

  it('answers null for a path that names no family', () => {
    expect(channelForPathname('/doctor')).toBeNull();
    expect(channelForPathname('/tools')).toBeNull();
    expect(channelForPathname('/sessions')).toBeNull();
  });
});

describe('viewForPathname', () => {
  it('names the view a path opens', () => {
    expect(viewForPathname('/memory')?.label).toBe('Brain');
    expect(viewForPathname('/memory/sessions')?.label).toBe('Sessions');
    expect(viewForPathname('/work/workflows')?.label).toBe('Workflows');
    expect(viewForPathname('/operations')?.label).toBe('Observatory');
  });

  it('falls back to the family landing view for an unknown sub-path', () => {
    expect(viewForPathname('/memory/nope')?.label).toBe('Brain');
    expect(viewForPathname('/doctor')).toBeNull();
  });
});
