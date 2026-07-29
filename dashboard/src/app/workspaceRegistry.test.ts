// @vitest-environment jsdom

import { describe, expect, it } from 'vitest';
import { STORY_SURFACES } from '../../stories/registry.ts';
import { CHANNELS } from './channels.ts';
import { WORKSPACES } from './routes.tsx';

const EXISTING_WORKSPACES = [
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
] as const;

function descriptors(items: readonly { path: string; label: string }[]) {
  return items.map(({ path, label }) => [path.replace(/^\//, ''), label] as const);
}

describe('workspace registry', () => {
  it('preserves the twelve existing workspaces and adds Work as the thirteenth', () => {
    expect(descriptors(WORKSPACES).slice(0, 12)).toEqual(EXISTING_WORKSPACES);
    expect(descriptors(WORKSPACES)[12]).toEqual(['work', 'Work']);
  });

  it('keeps routes, shell channels, and visual audit surfaces aligned', () => {
    const routes = descriptors(WORKSPACES);

    expect(descriptors(CHANNELS)).toEqual(routes);
    expect(descriptors(STORY_SURFACES)).toEqual(routes);
  });
});
