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
  /** Visible task hint, also searchable from the command palette. */
  readonly taskHint: string;
}

export const CHANNELS: readonly Channel[] = [
  {
    path: 'brain',
    label: 'Brain',
    taskHint: 'Project health, scope and relationships',
  },
  {
    path: 'explorer',
    label: 'Explorer',
    taskHint: 'Search code, sessions and knowledge together',
  },
  {
    path: 'loom',
    label: 'Loom',
    taskHint: 'Agent timelines and temporal replay',
  },
  {
    path: 'sessions',
    label: 'Sessions',
    taskHint: 'Session transcripts and history',
  },
  {
    path: 'agents',
    label: 'Agents',
    taskHint: 'Agent identities and activity',
  },
  {
    path: 'code',
    label: 'Code',
    taskHint: 'Source files, symbols and dependencies',
  },
  {
    path: 'knowledge',
    label: 'Knowledge',
    taskHint: 'Memory facts and citations',
  },
  {
    path: 'delivery',
    label: 'Delivery',
    taskHint: 'PR reviews, CI checks and releases',
  },
  {
    path: 'automations',
    label: 'Automations',
    taskHint: 'Scheduled jobs, skills and runs',
  },
  {
    path: 'observatory',
    label: 'Observatory',
    taskHint: 'Diagnostics, storage health and telemetry',
  },
  {
    path: 'costs',
    label: 'Costs',
    taskHint: 'Token usage, savings and spending',
  },
  {
    path: 'settings',
    label: 'Settings',
    taskHint: 'Configuration, integrations and permissions',
  },
  {
    path: 'work',
    label: 'Work',
    taskHint: 'Task board, plans and progress',
  },
  {
    path: 'workflows',
    label: 'Workflows',
    taskHint: 'Workflow definitions, execution and runs',
  },
] as const;

/** Search the task described by a workspace, rather than only its proper name.
 * The rail continues to use the established label and route unchanged. */
export function workspaceSearchText(channel: Channel): string {
  return `${channel.label} ${channel.taskHint}`.toLowerCase();
}

/** Match all typed words, regardless of whitespace/case or the order in which
 * the workspace's label and task description supply them. Empty queries show
 * every available entry, including registered projects. */
export function paletteMatchesQuery(searchText: string, query: string): boolean {
  return query
    .trim()
    .toLowerCase()
    .split(/\s+/)
    .every((term) => searchText.includes(term));
}

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
