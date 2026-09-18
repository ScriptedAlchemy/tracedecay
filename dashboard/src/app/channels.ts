/**
 * The instrument's channel list: the fourteen workspaces in their fixed panel
 * order (mockups/ui-concept-v2/NAVIGATION.md "Canonical rail"). A workspace's
 * channel number is part of its identity in this design, the nav rail
 * numbers them, the scope register repeats the active one, every workspace
 * header repeats its own, so the order lives in exactly one place.
 *
 * This mirrors `WORKSPACES` in `app/routes.tsx`; it is a plain data module
 * with no JSX so the shell chrome and the pages can both read it without an
 * import cycle through the router.
 *
 * The rail is one flat register. It used to file channels under Workspace /
 * Ops / Config dividers and pin Settings to the foot, which put channel 12
 * after 14 on screen; the canonical rail draws all fourteen in numeric order
 * with no grouping, and Settings stays at 12 where its number says it is.
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

/**
 * The channel a router pathname is tuned to, or `null` when the path names no
 * workspace. `/` resolves to Brain, the same surface the index route mounts
 * (`app/routes.tsx`), so the register never shows an empty title on the
 * landing route. Only the first segment is consulted: workspace-owned
 * sub-paths, if any arrive, still belong to their channel.
 */
export function channelForPathname(pathname: string): Channel | null {
  const [first = ''] = pathname.split('/').filter((segment) => segment.length > 0);
  if (first === '') return CHANNELS[0] ?? null;
  return CHANNELS.find((channel) => channel.path === first) ?? null;
}
