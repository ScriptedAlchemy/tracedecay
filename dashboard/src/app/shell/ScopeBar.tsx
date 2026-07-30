import { useEffect } from 'react';
import { Command, Moon, Sun, X } from 'lucide-react';
import { ProjectsPayloadSchema, type ProjectsPayload } from '../../contracts/wire.ts';
import { useLegacy } from '../../data/query/useLegacy.ts';
import type { LegacyResult } from '../../data/query/legacy.ts';
import { cn } from '../../ui/cn';
import { useScope, type RegistryReading } from '../../data/scope/store.ts';

/**
 * What the registry establishes about the projects it lists.
 *
 * `/api/projects` is the only authority for both facts the scope needs — which
 * project is active, and what each project is called — which is why the
 * hand-written schema this file used to hold had to go: it modelled `projects`
 * without `active_project_id`, so one of the two was not even in the parse.
 *
 * Only a `status: "ok"` body is a measurement. A registry that answered with
 * any other status, or did not answer, establishes nothing — and returning
 * `measured` for it would resolve every selected project to "not active",
 * disabling writes that would in fact have been accepted, and would discard
 * the URL's label without having anything to replace it with.
 */
function registryReading(result: LegacyResult<ProjectsPayload> | undefined): RegistryReading {
  if (!result) return { state: 'unknown' };
  switch (result.outcome) {
    case 'ok':
      return result.data.status === 'ok'
        ? {
            state: 'measured',
            activeProjectId: result.data.active_project_id,
            projects: (result.data.projects ?? []).map((project) => ({
              projectId: project.project_id,
              label: project.label,
            })),
          }
        : { state: 'unknown' };
    case 'offline':
    case 'unauthorized':
    case 'denied':
    case 'error':
    case 'unsupported_schema':
      return { state: 'unknown' };
    default: {
      const exhaustive: never = result;
      return exhaustive;
    }
  }
}

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
  const reconcileScope = useScope((s) => s.reconcileScope);
  const registry = useLegacy(['shell', 'project-registry'], '/api/projects', ProjectsPayloadSchema, {
    enabled: scope.kind === 'project',
  });

  // The one place a selected project is reconciled against the registry, for
  // both its activation and its label. Every entry into a project scope — deep
  // link, command palette, Remote Brain, this bar — arrives `unresolved` with
  // an unverified label, and this read is what settles both. Controls consult
  // `scopeWritable`, so until this lands they report writability as unknown
  // rather than offering a write the gateway would refuse.
  useEffect(() => {
    reconcileScope(registryReading(registry.data));
  }, [registry.data, reconcileScope]);

  // The label is the reconciled one from the store, not a second lookup: the
  // bar has to call the project what the write-target prose calls it, and two
  // lookups over the same payload are two things to keep in agreement. What
  // the bar adds is why the name may not be canonical yet.
  const annotation =
    scope.kind === 'project' ? registryAnnotation(scope.projectId, registry.data) : null;
  const projectLabel = scope.kind === 'project' ? scope.label : undefined;
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
            aria-label={
              annotation
                ? `Clear project scope ${projectLabel} · ${annotation}`
                : `Clear project scope ${projectLabel}`
            }
            className={cn(
              'group flex min-w-0 flex-col justify-center gap-1 border-r border-edge-subtle px-3 text-left',
              'bg-accent/10 hover:bg-accent/20',
            )}
          >
            <span className="td-legend text-text-secondary">Project</span>
            <span className="flex min-w-0 items-center gap-1.5">
              <span className="td-value truncate text-xs">{projectLabel}</span>
              {/* Kept out of the truncating value so a long label cannot clip
                * the caveat away and leave the name looking confirmed. */}
              {annotation ? (
                <span
                  data-scope-label-annotation={annotation}
                  className="shrink-0 text-3xs text-text-secondary"
                >
                  · {annotation}
                </span>
              ) : null}
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

/**
 * Why the displayed name is not a name the registry confirmed, or `null` when
 * it is.
 *
 * An annotation rather than a replacement label. This used to return the whole
 * string — `registry offline · proj_x` in place of a label — which made the bar
 * the only surface with an opinion about whether the name could be trusted,
 * while the write-target prose went on using the URL's claim unqualified. The
 * name now comes from the reconciled scope everywhere; this says what is known
 * about it.
 */
function registryAnnotation(
  projectId: string,
  result: LegacyResult<ProjectsPayload> | undefined,
): string | null {
  if (!result) return 'resolving';
  switch (result.outcome) {
    case 'ok': {
      if (result.data.status !== 'ok') return 'registry unavailable';
      // Answered, and this id is not among the projects it listed — so the
      // name beside it is the id, and saying nothing here would present that
      // as an ordinary label.
      return (result.data.projects ?? []).some((project) => project.project_id === projectId)
        ? null
        : 'not in registry';
    }
    case 'offline':
      return 'registry offline';
    case 'unauthorized':
      return 'registry unauthorized';
    case 'denied':
      return 'registry denied';
    case 'error':
      return 'registry error';
    case 'unsupported_schema':
      return 'unsupported registry schema';
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
