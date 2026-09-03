/**
 * The instrument's channel list: the fourteen workspaces in their fixed panel
 * order. A workspace's channel number is part of its identity in this design
 * (the nav rail numbers them, every workspace header repeats the number), so
 * the order lives in exactly one place.
 *
 * This mirrors `WORKSPACES` in `app/routes.tsx` and `MAIN` in the nav rail; it
 * is a plain data module with no JSX so the shell chrome and the pages can both
 * read it without an import cycle through the router.
 */
/** Which register of the tactical dock a channel files under. `workspace` is
 * where the operator reads and explores; `ops` is where the system reports on
 * itself and its deliveries; `config` is the one channel that changes the
 * instrument instead of reading it. */
export type ChannelGroup = 'workspace' | 'ops' | 'config';

export interface Channel {
  readonly path: string;
  readonly label: string;
  readonly group: ChannelGroup;
}

export const CHANNELS: readonly Channel[] = [
  { path: 'brain', label: 'Brain', group: 'workspace' },
  { path: 'explorer', label: 'Explorer', group: 'workspace' },
  { path: 'loom', label: 'Loom', group: 'workspace' },
  { path: 'sessions', label: 'Sessions', group: 'workspace' },
  { path: 'agents', label: 'Agents', group: 'workspace' },
  { path: 'code', label: 'Code', group: 'workspace' },
  { path: 'knowledge', label: 'Knowledge', group: 'workspace' },
  { path: 'delivery', label: 'Delivery', group: 'ops' },
  { path: 'automations', label: 'Automations', group: 'ops' },
  { path: 'observatory', label: 'Observatory', group: 'ops' },
  { path: 'costs', label: 'Costs', group: 'ops' },
  // Settings keeps channel 12: the number is the channel's identity, so
  // regrouping the rail must never renumber an instrument.
  { path: 'settings', label: 'Settings', group: 'config' },
  { path: 'work', label: 'Work', group: 'ops' },
  { path: 'workflows', label: 'Workflows', group: 'ops' },
] as const;

/** Zero-padded channel number for a workspace path (`code` → `06`). Unknown
 * paths get `--`: the instrument never invents a channel it does not have. */
export function channelNumber(path: string): string {
  const index = CHANNELS.findIndex((channel) => channel.path === path);
  return index < 0 ? '--' : String(index + 1).padStart(2, '0');
}
