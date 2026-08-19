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
export interface Channel {
  readonly path: string;
  readonly label: string;
}

export const CHANNELS: readonly Channel[] = [
  { path: 'brain', label: 'Brain' },
  { path: 'explorer', label: 'Explorer' },
  { path: 'loom', label: 'Loom' },
  { path: 'sessions', label: 'Sessions' },
  { path: 'agents', label: 'Agents' },
  { path: 'code', label: 'Code' },
  { path: 'knowledge', label: 'Knowledge' },
  { path: 'delivery', label: 'Delivery' },
  { path: 'automations', label: 'Automations' },
  { path: 'observatory', label: 'Observatory' },
  { path: 'costs', label: 'Costs' },
  { path: 'settings', label: 'Settings' },
  { path: 'work', label: 'Work' },
  { path: 'workflows', label: 'Workflows' },
] as const;

/** Zero-padded channel number for a workspace path (`code` → `06`). Unknown
 * paths get `--`: the instrument never invents a channel it does not have. */
export function channelNumber(path: string): string {
  const index = CHANNELS.findIndex((channel) => channel.path === path);
  return index < 0 ? '--' : String(index + 1).padStart(2, '0');
}
