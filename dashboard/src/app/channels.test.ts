import { describe, expect, it } from 'vitest';
import { CHANNELS, channelForPathname, channelNumber } from './channels.ts';

/**
 * The canonical rail from `mockups/ui-concept-v2/NAVIGATION.md`, spelled out
 * rather than derived, so a reordering or a fifteenth channel fails here by
 * name. The rail, the register, the palette and every workspace header read
 * the same list, so this is the one place its order is asserted.
 */
const CANONICAL_RAIL = [
  ['01', 'brain', 'Brain'],
  ['02', 'explorer', 'Explorer'],
  ['03', 'loom', 'Loom'],
  ['04', 'sessions', 'Sessions'],
  ['05', 'agents', 'Agents'],
  ['06', 'code', 'Code'],
  ['07', 'knowledge', 'Knowledge'],
  ['08', 'delivery', 'Delivery'],
  ['09', 'automations', 'Automations'],
  ['10', 'observatory', 'Observatory'],
  ['11', 'costs', 'Costs'],
  ['12', 'settings', 'Settings'],
  ['13', 'work', 'Work'],
  ['14', 'workflows', 'Workflows'],
] as const;

describe('the canonical channel list', () => {
  it('is exactly the fourteen workspaces in NAVIGATION.md order', () => {
    expect(CHANNELS.map((channel) => [channelNumber(channel.path), channel.path, channel.label]))
      .toEqual(CANONICAL_RAIL.map((row) => [...row]));
  });

  it('never invents a channel for a path it does not have', () => {
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
