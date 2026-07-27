import { Command, Moon, Sun, X } from 'lucide-react';
import { z } from 'zod';
import { useLegacy } from '../../data/query/useLegacy.ts';
import type { LegacyResult } from '../../data/query/legacy.ts';
import { cn } from '../../ui/cn';
import { useScope } from '../../data/scope/store.ts';

const ScopeProjectsSchema = z
  .object({
    status: z.string(),
    projects: z
      .array(
        z
          .object({
            project_id: z.string(),
            label: z.string(),
          })
          .passthrough(),
      )
      .optional(),
  })
  .passthrough();
type ScopeProjects = z.infer<typeof ScopeProjectsSchema>;

function toggleTheme() {
  const root = document.documentElement;
  const next = root.dataset['theme'] === 'light' ? 'dark' : 'light';
  root.dataset['theme'] = next;
  localStorage.setItem('td-theme', next);
}

/** Always-visible active scope (plan 11: every view preserves and displays
 * scope; transitions are explicit) rendered as the console's setting register:
 * each field is an engraved legend over a monospaced value, divided by
 * hairlines like the switch bank on an instrument front panel. */
export function ScopeBar({ onOpenPalette }: { onOpenPalette?: () => void }) {
  const scope = useScope((s) => s.scope);
  const selectAllProjects = useScope((s) => s.selectAllProjects);
  const registry = useLegacy(['shell', 'project-registry'], '/api/projects', ScopeProjectsSchema, {
    enabled: scope.kind === 'project',
  });
  const projectLabel =
    scope.kind === 'project' ? resolvedProjectLabel(scope.projectId, registry.data) : undefined;
  return (
    // Every control on this bar is stretched to the bar's own height, so the
    // bar's height IS their touch target. `h-12` is 3rem, and the root font
    // size is 14px, so it measured 42px — 41px of content once the hairline is
    // taken out — and put the palette, scope and theme controls under the
    // minimum together. Sized from the token plus that hairline, so the
    // content box lands exactly on 44. `NavRail`'s brand block matches it.
    <header className="flex h-[calc(var(--touch-target-min)+1px)] shrink-0 items-stretch border-b border-edge-subtle bg-surface-1">
      <div
        className="flex min-w-0 flex-1 items-stretch overflow-hidden"
        aria-label="Active scope"
      >
        {scope.kind === 'project' ? (
          <button
            type="button"
            onClick={selectAllProjects}
            aria-label={`Clear project scope ${projectLabel}`}
            className={cn(
              'group flex min-w-0 flex-col justify-center gap-1 border-r border-edge-subtle px-3 text-left',
              'bg-accent/10 hover:bg-accent/20',
            )}
          >
            <span className="td-legend text-text-secondary">Project</span>
            <span className="flex min-w-0 items-center gap-1.5">
              <span className="td-value truncate text-xs">{projectLabel}</span>
              <X aria-hidden size={10} className="shrink-0 text-text-muted" />
            </span>
          </button>
        ) : (
          <ScopeField label="Project" value="all" muted />
        )}
        <span aria-hidden className="flex-1 border-r border-edge-subtle" />
      </div>
      <button
        type="button"
        onClick={onOpenPalette}
        className={cn(
          'flex shrink-0 flex-col items-start justify-center gap-1 border-r border-edge-subtle px-3',
          'hover:bg-surface-2',
        )}
        aria-label="Open command palette"
      >
        <span className="td-legend">Command</span>
        <span className="flex items-center gap-1 text-text-secondary">
          <Command aria-hidden size={11} />
          <span className="td-value text-xs">K</span>
        </span>
      </button>
      <button
        type="button"
        onClick={toggleTheme}
        aria-label="Toggle theme"
        // `w-11` was written for 44 and rendered 38.5; the glyph is unchanged.
        className="flex w-[var(--touch-target-min)] shrink-0 items-center justify-center text-text-muted hover:bg-surface-2 hover:text-text-primary"
      >
        <Sun aria-hidden size={14} className="hidden [[data-theme=light]_&]:block" />
        <Moon aria-hidden size={14} className="[[data-theme=light]_&]:hidden" />
      </button>
    </header>
  );
}

function resolvedProjectLabel(
  projectId: string,
  result: LegacyResult<ScopeProjects> | undefined,
): string {
  if (!result) return 'resolving';
  switch (result.outcome) {
    case 'ok': {
      if (result.data.status !== 'ok') return `registry unavailable · ${projectId}`;
      return (
        result.data.projects?.find((project) => project.project_id === projectId)?.label ??
        `unknown project · ${projectId}`
      );
    }
    case 'offline':
      return `registry offline · ${projectId}`;
    case 'unauthorized':
      return `registry unauthorized · ${projectId}`;
    case 'denied':
      return `registry denied · ${projectId}`;
    case 'error':
      return `registry error · ${projectId}`;
    case 'unsupported_schema':
      return `unsupported registry schema · ${projectId}`;
    default: {
      const exhaustive: never = result;
      return exhaustive;
    }
  }
}

function ScopeField({
  label,
  value,
  muted,
}: {
  label: string;
  value: string;
  muted?: boolean;
}) {
  return (
    <span className="flex min-w-0 shrink-0 flex-col justify-center gap-1 border-r border-edge-subtle px-3">
      <span className="td-legend">{label}</span>
      <span className={cn('td-value truncate text-xs', muted && 'text-text-muted')}>
        {value}
      </span>
    </span>
  );
}
