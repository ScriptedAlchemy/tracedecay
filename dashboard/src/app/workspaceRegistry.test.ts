// @vitest-environment jsdom

import { describe, expect, it } from 'vitest';
import { STORY_SURFACES } from '../../stories/registry.ts';
import { CHANNELS } from './channels.ts';
import { WORKSPACES } from './routes.tsx';

/**
 * The channel order, spelled out.
 *
 * A workspace's channel number is part of its identity — the nav rail prints it
 * and every workspace header repeats it — so the order is pinned here rather
 * than derived from the registry it is checking. Reordering this list renumbers
 * live surfaces, which is exactly the change that should have to be deliberate.
 */
const CHANNEL_ORDER = [
  ['brain', 'Brain'],
  ['explorer', 'Explorer'],
  ['loom', 'Loom'],
  ['sessions', 'Sessions'],
  ['agents', 'Agents'],
  ['code', 'Code'],
  ['knowledge', 'Knowledge'],
  ['delivery', 'Delivery'],
  ['automations', 'Automations'],
  ['observatory', 'Observatory'],
  ['costs', 'Costs'],
  ['settings', 'Settings'],
  ['work', 'Work'],
  ['workflows', 'Workflows'],
] as const;

function descriptors(items: readonly { path: string; label: string }[]) {
  return items.map(({ path, label }) => [path.replace(/^\//, ''), label] as const);
}

describe('workspace registry', () => {
  it('routes the fourteen workspaces in their fixed channel order', () => {
    expect(descriptors(WORKSPACES)).toEqual(CHANNEL_ORDER);
  });

  it('keeps routes, shell channels, and visual audit surfaces aligned', () => {
    const routes = descriptors(WORKSPACES);

    expect(descriptors(CHANNELS)).toEqual(routes);
    expect(descriptors(STORY_SURFACES)).toEqual(routes);
  });
});
