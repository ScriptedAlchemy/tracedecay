import { create } from 'zustand';
import { z } from 'zod';

/**
 * Whether the selected project is the one the daemon is actually serving.
 *
 * This is a scope fact, not a cosmetic one: the project gateway
 * (`src/dashboard/mod.rs` `project_scoped_api_gateway`) refuses every
 * non-GET/HEAD request for a project that is not the active one. So a control
 * that can write under one selected project cannot write under another, and
 * the difference is invisible from the project id alone.
 *
 * `unresolved` is the third state and it is load-bearing. A deep link carries
 * an opaque id and nothing else, so until the registry has been read the
 * dashboard does not know which of the other two this is — and must not guess,
 * in either direction: guessing `active` offers a write that will be refused,
 * guessing `selected` withdraws one that would have worked.
 */
export type ProjectActivation = 'active' | 'selected' | 'unresolved';

/** Dashboard-wide scope (plan 11a all-projects-first model): every workspace
 * renders the all-projects aggregate until a specific project is selected.
 * Project-scoped reads route through the `/api/projects/{id}/…` gateway. */
export type DashboardScope =
  | { kind: 'all' }
  | {
      kind: 'project';
      projectId: string;
      label: string;
      activation: ProjectActivation;
    };

/** A project as the registry lists it: the id the dashboard routes by, and the
 * label that id is actually called. */
export interface RegistryProject {
  projectId: string;
  label: string;
}

/**
 * What the project registry said, both about which project is active and about
 * what the listed projects are called.
 *
 * `measured` with a null id is a real answer — the daemon has no active
 * project — and is different from `unknown`, which is the registry declining
 * to answer at all. Folding the second into the first would turn an unread
 * registry into a confident "this project is not active", which is the false
 * negative this union exists to prevent.
 *
 * `projects` rides along because the same read settles both facts, and a scope
 * reconciled against one of them but not the other would route by a canonical
 * id while calling it by whatever name the URL supplied.
 */
export type RegistryReading =
  | {
      state: 'measured';
      activeProjectId: string | null;
      projects: readonly RegistryProject[];
    }
  | { state: 'unknown' };

interface ScopeState {
  scope: DashboardScope;
  selectProject: (projectId: string, label: string, activation?: ProjectActivation) => void;
  selectAllProjects: () => void;
  /** Reconcile the selected project — both its activation and its label —
   * against the registry. A no-op unless a project is currently selected. */
  reconcileScope: (reading: RegistryReading) => void;
}

export function activationFor(projectId: string, reading: RegistryReading): ProjectActivation {
  switch (reading.state) {
    case 'measured':
      return reading.activeProjectId === projectId ? 'active' : 'selected';
    case 'unknown':
      return 'unresolved';
    default: {
      const exhaustive: never = reading;
      return exhaustive;
    }
  }
}

/**
 * What this project is called, preferring the registry over the caller.
 *
 * A deep link's `scopeLabel` is an unverified claim: it can be stale from
 * before a rename, or simply be whatever text was pasted next to a real
 * project id. It is a display string that reaches prose about what a write
 * will affect, so once the registry has answered, its entry is the label —
 * anything else lets a URL choose the name the dashboard uses for a project it
 * is about to write to.
 *
 * Two cases are deliberately not the same:
 *
 *   the registry did not answer, so the URL label is all there is and is kept
 *   as the best available name; and
 *
 *   the registry answered and does not list this id, so the claimed label has
 *   been contradicted rather than merely unconfirmed. Keeping it would state a
 *   name no authority backs, so the id — which is at least what the dashboard
 *   routes by — stands in for it.
 */
export function reconciledLabel(
  projectId: string,
  claimed: string,
  reading: RegistryReading,
): string {
  switch (reading.state) {
    case 'measured':
      return reading.projects.find((p) => p.projectId === projectId)?.label ?? projectId;
    case 'unknown':
      return claimed;
    default: {
      const exhaustive: never = reading;
      return exhaustive;
    }
  }
}

export const useScope = create<ScopeState>((set) => ({
  scope: { kind: 'all' },
  // A caller that does not say otherwise has not consulted the registry, so
  // the default is the honest one rather than the convenient one.
  selectProject: (projectId, label, activation = 'unresolved') =>
    set({ scope: { kind: 'project', projectId, label, activation } }),
  selectAllProjects: () => set({ scope: { kind: 'all' } }),
  reconcileScope: (reading) =>
    set((state) => {
      if (state.scope.kind !== 'project') return state;
      const { projectId, label } = state.scope;
      const next = {
        ...state.scope,
        label: reconciledLabel(projectId, label, reading),
        activation: activationFor(projectId, reading),
      };
      // Identity-stable when nothing moved. Every consumer selects the scope
      // object, and this runs on each registry read, so returning a fresh one
      // each time would re-render the shell — and re-key nothing, since the
      // cache token is the id — on a 30-second poll that changed no fact.
      return next.label === state.scope.label && next.activation === state.scope.activation
        ? state
        : { scope: next };
    }),
}));

/**
 * Whether a write issued under this scope will be accepted, and what to say
 * when it will not.
 *
 * The single authority for the question. Controls ask it rather than deriving
 * an answer from `scope.kind`, because `kind` does not carry it: an `all`
 * scope and an active selected project both write to the active project, and
 * two selected projects differ from each other.
 *
 * `unknown` is never a disabled control with a shrug — it is a distinct
 * reading with its own sentence, so a control can stay disabled while saying
 * that the reason is a pending registry read rather than a refusal.
 *
 * The question is specifically whether a *mutation* is accepted. Three
 * POST-shaped reads (`feedback/get|expand|list`) are exempt in the gateway
 * because they change nothing, so a panel that only reads must not consult
 * this and disable itself against a refusal that will not happen.
 */
export type ScopeWritability =
  | { state: 'writable'; target: string }
  | { state: 'read_only'; reason: string }
  | { state: 'unknown'; reason: string };

export function scopeWritable(scope: DashboardScope): ScopeWritability {
  switch (scope.kind) {
    // Unprefixed routes reach `active_api_gateway`, which serves the active
    // project's state. So the aggregate view is writable, and a control that
    // said only "writable" here would let a reader believe a write under
    // "all projects" fans out across them. It lands on exactly one.
    case 'all':
      return { state: 'writable', target: 'the active project' };
    case 'project':
      switch (scope.activation) {
        case 'active':
          return { state: 'writable', target: scope.label };
        case 'selected':
          return {
            state: 'read_only',
            reason: `${scope.label} is not the active project, and the dashboard gateway serves every other project read-only. Switch scope to the active project to make this change.`,
          };
        case 'unresolved':
          return {
            state: 'unknown',
            reason: `Whether ${scope.label} accepts writes is not known yet: this scope came from a link and has not been checked against the project registry.`,
          };
        default: {
          const exhaustive: never = scope.activation;
          return exhaustive;
        }
      }
    default: {
      const exhaustive: never = scope;
      return exhaustive;
    }
  }
}

/** The `status` the project gateway answers a refused write with. */
export const READ_ONLY_SCOPE_STATUS = 'read_only_project';

/**
 * The gateway's own account of a refused write.
 *
 * Verbatim from `project_scoped_api_gateway`, which answers 405 with
 * `{status, detail, project_id}` when a non-GET/HEAD request names a project
 * that is not the active one. `detail` is carried rather than reworded so the
 * surface repeats the daemon's reason instead of inventing one.
 */
const ReadOnlyScopeRefusalSchema = z
  .object({
    status: z.literal(READ_ONLY_SCOPE_STATUS),
    detail: z.string(),
    project_id: z.string(),
  })
  .passthrough();

export interface ReadOnlyScopeRefusal {
  projectId: string;
  detail: string;
}

/**
 * Read a refusal out of a 405 body, or `null` if this is not one.
 *
 * Null is the important return. A 405 whose body does not match is still a
 * refused request, but not one this dashboard can explain — reporting it as a
 * read-only scope would attach a specific cause, and a specific remedy, to a
 * refusal that may have neither.
 */
export function readOnlyScopeRefusal(body: unknown): ReadOnlyScopeRefusal | null {
  const parsed = ReadOnlyScopeRefusalSchema.safeParse(body);
  return parsed.success
    ? { projectId: parsed.data.project_id, detail: parsed.data.detail }
    : null;
}

/** Never-scoped surfaces: the registry itself and the dashboard chrome. */
const UNSCOPED_PREFIXES = ['/api/projects', '/api/dashboard'];

/** Rewrites an `/api/...` URL for the current scope. A selected project
 * routes through the read-only project gateway, which rewrites
 * `/api/projects/{id}/{tail}` back to `/api/{tail}` against that project's
 * state; the all-projects default and the active project stay unprefixed. */
export function scopedUrl(scope: DashboardScope, url: string): string {
  if (scope.kind !== 'project') return url;
  if (!url.startsWith('/api/')) return url;
  if (UNSCOPED_PREFIXES.some((prefix) => url.startsWith(prefix))) return url;
  return `/api/projects/${encodeURIComponent(scope.projectId)}/${url.slice('/api/'.length)}`;
}

/** Cache-key token for the current scope (splits query caches per scope).
 *
 * Activation is deliberately absent: it says what the gateway will do with a
 * write, not which project's rows come back, so folding it in here would
 * discard every cached read the moment the registry resolved. */
export function scopeKey(scope: DashboardScope): string {
  return scope.kind === 'project' ? `project:${scope.projectId}` : 'all';
}
