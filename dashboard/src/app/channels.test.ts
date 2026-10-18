import { describe, expect, it } from 'vitest';
import { channelForPathname, channelNumber } from './channels.ts';

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
