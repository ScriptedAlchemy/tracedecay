/**
 * Code-driven story registry: every route surface the shell renders (plan 11a
 * R56-R59). The visual-audit harness (`stories/audit.ts`) walks this list to
 * screenshot each surface across themes and widths, and the axe pass reports
 * per-surface accessibility violations against the same set.
 *
 * This mirrors `WORKSPACES` in `src/app/routes.tsx`. It is kept as a standalone
 * data module (no JSX / CSS imports) so it can be consumed from a plain Node /
 * Playwright context without dragging in the React app bundle. When a surface
 * is added or removed in routes.tsx, update this list in the same change — the
 * audit's coverage is only as complete as this registry.
 */

export interface StorySurface {
  /** Stable id, used for screenshot / manifest filenames. */
  readonly id: string;
  /** Router path (leading slash) navigated to in the running app. */
  readonly path: string;
  /** Human label shown in the nav rail. */
  readonly label: string;
  /** One-line description of what the surface renders. */
  readonly description: string;
  /**
   * Whether the surface currently renders a wired workspace page or a truthful
   * contract gate. Recorded in the manifest so a reviewer can distinguish an
   * unavailable backend read model from a broken surface.
   *
   * Every surface is wired as of the Work routes landing. The field stays
   * because the distinction it records is the one a reviewer needs the moment
   * it is false again, and because a manifest that stopped reporting it would
   * make a gated surface indistinguishable from a wired one.
   */
  readonly wired: boolean;
}

export const STORY_SURFACES: readonly StorySurface[] = [
  {
    id: 'brain',
    path: '/brain',
    label: 'Brain',
    description: 'Whole-system and scoped summaries, health, activity, and freshness.',
    wired: true,
  },
  {
    id: 'explorer',
    path: '/explorer',
    label: 'Explorer',
    description: 'Pivotable search across messages, sessions, facts, code, and time.',
    wired: true,
  },
  {
    id: 'loom',
    path: '/loom',
    label: 'Loom',
    description: 'Temporal and causal traces linking prompts, tools, code, and outcomes.',
    wired: true,
  },
  {
    id: 'sessions',
    path: '/sessions',
    label: 'Sessions',
    description: 'Transcript search, LCM summaries, and raw-message drill-down.',
    wired: true,
  },
  {
    id: 'agents',
    path: '/agents',
    label: 'Agents',
    description: 'Agent trees, status, handoffs, tool activity, and failure context.',
    wired: true,
  },
  {
    id: 'code',
    path: '/code',
    label: 'Code',
    description: 'Symbol search, references, diagnostics, and graph freshness.',
    wired: true,
  },
  {
    id: 'knowledge',
    path: '/knowledge',
    label: 'Knowledge',
    description: 'Facts, evidence, contradictions, supersession, and curation.',
    wired: true,
  },
  {
    id: 'delivery',
    path: '/delivery',
    label: 'Delivery',
    description: 'Changes, commits, branches, worktrees, PRs, CI, and releases.',
    wired: true,
  },
  {
    id: 'automations',
    path: '/automations',
    label: 'Automations',
    description: 'Schedules, run history, artifacts, approvals, and skills.',
    wired: true,
  },
  {
    id: 'observatory',
    path: '/observatory',
    label: 'Observatory',
    description: 'Hook hints, event flow, latency, daemon and storage health.',
    wired: true,
  },
  {
    id: 'costs',
    path: '/costs',
    label: 'Costs',
    description: 'Provider and model usage, tokens, latency, and estimated cost.',
    wired: true,
  },
  {
    id: 'settings',
    path: '/settings',
    label: 'Settings',
    description: 'Effective layered configuration and validated changes.',
    wired: true,
  },
  {
    id: 'work',
    path: '/work',
    label: 'Work',
    description:
      'The canonical task graph for the active project, over nine mounted routes.',
    wired: true,
  },
  {
    id: 'workflows',
    path: '/workflows',
    label: 'Workflows',
    description:
      'Registered workflow definitions, lifecycle control, and run projections.',
    wired: true,
  },
] as const;

export type StorySurfaceId = (typeof STORY_SURFACES)[number]['id'];
