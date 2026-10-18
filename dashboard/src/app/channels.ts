/**
 * The instrument's channel list: the five task families in their fixed panel
 * order (`docs/dashboard-task-map.md`). A family's channel number is part of
 * its identity in this design, the nav rail numbers them, the scope register
 * repeats the active one, every workspace header repeats its own, so the order
 * lives in exactly one place.
 *
 * A family groups the views that answer one kind of question. Each view keeps
 * its own page and its own URL under the family (`/memory/sessions`), the
 * family's first view answers at the bare family path (`/memory`), and a visible
 * tab strip on the family page reaches every sibling. Issue #4220 replaced the
 * fourteen top-level workspaces with these five; `LEGACY_REDIRECTS` in
 * `app/routes.tsx` carries every retired path to its new home.
 *
 * This is a plain data module with no JSX so the shell chrome and the pages
 * can both read it without an import cycle through the router; `app/routes.tsx`
 * attaches a lazy page to each view path.
 */
export interface ChannelView {
  /** Router path without a leading slash; the first segment is the family. */
  readonly path: string;
  readonly label: string;
}

export interface Channel {
  readonly path: string;
  readonly label: string;
  /** The family's views in tab order. The first answers at `path` itself. */
  readonly views: readonly ChannelView[];
}

export const CHANNELS = [
  {
    path: 'code',
    label: 'Code',
    views: [
      { path: 'code', label: 'Code' },
      { path: 'code/delivery', label: 'Delivery' },
    ],
  },
  {
    path: 'memory',
    label: 'Memory',
    views: [
      { path: 'memory', label: 'Brain' },
      { path: 'memory/explorer', label: 'Explorer' },
      { path: 'memory/knowledge', label: 'Knowledge' },
      { path: 'memory/sessions', label: 'Sessions' },
      { path: 'memory/loom', label: 'Loom' },
      { path: 'memory/agents', label: 'Agents' },
    ],
  },
  {
    path: 'work',
    label: 'Work',
    views: [
      { path: 'work', label: 'Work' },
      { path: 'work/workflows', label: 'Workflows' },
    ],
  },
  {
    path: 'operations',
    label: 'Operations',
    views: [
      { path: 'operations', label: 'Observatory' },
      { path: 'operations/automations', label: 'Automations' },
      { path: 'operations/costs', label: 'Costs' },
    ],
  },
  {
    path: 'settings',
    label: 'Settings',
    views: [{ path: 'settings', label: 'Settings' }],
  },
] as const satisfies readonly Channel[];

/** Every routable view path, as a literal union so a page cannot be mounted
 * at a path the channel list does not name. */
export type ViewPath = (typeof CHANNELS)[number]['views'][number]['path'];

/** The family `/` opens: Brain, the memory family's first view. */
const LANDING_CHANNEL = 'memory';

function firstSegment(path: string): string {
  return path.split('/').find((segment) => segment.length > 0) ?? '';
}

/** Zero-padded channel number for a view or family path (`memory/loom` → `02`).
 * Unknown paths get `--`: the instrument never invents a channel it does not
 * have. */
export function channelNumber(path: string): string {
  const family = firstSegment(path);
  const index = CHANNELS.findIndex((channel) => channel.path === family);
  return index < 0 ? '--' : String(index + 1).padStart(2, '0');
}

/**
 * The channel a router pathname is tuned to, or `null` when the path names no
 * family. `/` resolves to Memory, the family whose first view (Brain) the root
 * redirect opens, so the register never shows an empty title on the landing
 * route. Only the first segment is consulted: a family's sub-paths belong to
 * the family.
 */
export function channelForPathname(pathname: string): Channel | null {
  const first = firstSegment(pathname);
  const family = first === '' ? LANDING_CHANNEL : first;
  return CHANNELS.find((channel) => channel.path === family) ?? null;
}

/** The view a router pathname names, or `null` when it names none. */
export function viewForPathname(pathname: string): ChannelView | null {
  const channel = channelForPathname(pathname);
  if (channel === null) return null;
  const segments = pathname.split('/').filter((segment) => segment.length > 0);
  const path = segments.length === 0 ? channel.path : segments.slice(0, 2).join('/');
  return channel.views.find((view) => view.path === path) ?? channel.views[0] ?? null;
}
