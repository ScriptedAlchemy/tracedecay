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
 *
 * `absent` is the registry having answered that it holds no such project —
 * `GET /api/projects/{id}` replying 404 `not_found`. It is separate from
 * `unresolved` because the two say opposite things about what is known: one is
 * a pending read, the other a completed one, and a stale deep link that
 * reported "not checked yet" forever would never tell its reader why nothing
 * on the page ever resolved.
 */
export type ProjectActivation = 'active' | 'selected' | 'unresolved' | 'absent';

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

/**
 * What the registry established about the *selected* project.
 *
 * Two facts, each separately measurable, and each `null` when the answer did
 * not carry it. That shape is the fix for a specific defect: reconciliation
 * used to search the `/api/projects` listing, which the daemon truncates to a
 * page (100 by default, 250 at most). A selected project past the end of that
 * page is missing from the response for a reason that has nothing to do with
 * whether it exists, and treating it as absent replaced a perfectly good label
 * with a raw id and announced "not in registry" about a project that was in the
 * registry. Nothing here can express absence, so nothing can infer it from a
 * page — the reading is sourced from `GET /api/projects/{id}`, which answers
 * about one project and is bounded no matter how many are registered.
 *
 * `unknown` is the registry declining to answer. Folding it into `measured`
 * would turn an unread registry into a confident "this project is not active",
 * which is the false negative this union exists to prevent.
 *
 * `absent` is the opposite risk, and needs its own state for the same reason:
 * the bounded lookup answering 404 `not_found` is a real measurement — this id
 * is not registered — and collapsing it into `unknown` would leave a dead deep
 * link resolving forever.
 */
export type RegistryReading =
  | {
      state: 'measured';
      /** The canonical label, or `null` when the answer carried no project
       * record — unconfirmed, which is not the same as contradicted. */
      label: string | null;
      /** Whether this is the active project, or `null` when the answer did not
       * say. The daemon computes it against the same `active_project_id` that
       * decides whether the gateway accepts a write. */
      isActive: boolean | null;
    }
  | {
      state: 'absent';
      /** The registry's own `error` sentence, when it sent one. Carried rather
       * than reworded so the surface repeats the daemon's account. */
      reason: string | null;
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

export function activationFor(reading: RegistryReading): ProjectActivation {
  switch (reading.state) {
    case 'measured':
      // `null` is the registry not saying. It is not a no: answering `selected`
      // here would withdraw a write the gateway would have accepted, on the
      // strength of a field the daemon left out.
      if (reading.isActive === null) return 'unresolved';
      return reading.isActive ? 'active' : 'selected';
    case 'absent':
      return 'absent';
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
 * will affect, so once the registry has named the project, that name is the
 * label — anything else lets a URL choose what the dashboard calls a project it
 * is about to write to.
 *
 * Short of a name from the registry, the claim stands. It is the only name
 * available, and substituting a raw id would be a correction in its own right —
 * asserted on no measurement, and (because corrections propagate to the address
 * bar) written back over a label that may well have been right.
 */
export function reconciledLabel(claimed: string, reading: RegistryReading): string {
  switch (reading.state) {
    case 'measured':
      return reading.label ?? claimed;
    // The registry holds no project under this id, so it has no name to offer
    // in place of the claim. The claim is left standing — it is the only thing
    // that identifies the link its reader followed — and `absent` activation is
    // what says the name belongs to nothing.
    case 'absent':
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
      const next = {
        ...state.scope,
        label: reconciledLabel(state.scope.label, reading),
        activation: activationFor(reading),
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
        // Refused before dispatch, not left unknown: the registry answered,
        // and a write against an id it does not hold cannot be accepted by
        // the gateway that would have to route it.
        case 'absent':
          return {
            state: 'read_only',
            reason: `The project registry holds no project with id ${scope.projectId}, so there is nothing here to write to. This scope came from a link that names a project that has been removed or never existed — switch to a registered project.`,
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

/**
 * The sentence a control carries about what this scope means for it.
 *
 * One exhaustive switch for all of them. The two refusing states are answered
 * with the authority's own `reason` — reworded per call site they would drift
 * apart, and the reason is the same fact everywhere — while `writable` is the
 * only state whose wording is genuinely local, because what is being written
 * differs (a scheduler toggle, a remediation) and the target has to be named.
 *
 * `writable` is answered rather than thrown on even where a call site cannot
 * reach it: producing a sentence is what this is for.
 */
export function scopeWriteSentence(
  writability: ScopeWritability,
  sentences: {
    writable: (target: string) => string;
    /** For a site that frames the refusal before repeating it. Defaults to the
     * reason alone. */
    refused?: (reason: string) => string;
  },
): string {
  switch (writability.state) {
    case 'writable':
      return sentences.writable(writability.target);
    case 'read_only':
    case 'unknown':
      return sentences.refused ? sentences.refused(writability.reason) : writability.reason;
    default: {
      const exhaustive: never = writability;
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

/**
 * Is this route one the project gateway never rewrites?
 *
 * A property of the ROUTE, asked without reference to the current scope — the
 * registry is the thing that lists projects and the chrome sits above all of
 * them, so neither is ever served per project. Both `scopedUrl` and
 * `requestScopeKey` ask this one question, so the rewrite and the cache key
 * cannot disagree about which routes carry a project.
 */
function unscopedRoute(url: string): boolean {
  if (!url.startsWith('/api/')) return true;
  return UNSCOPED_PREFIXES.some((prefix) => url.startsWith(prefix));
}

/** Rewrites an `/api/...` URL for the current scope. A selected project
 * routes through the read-only project gateway, which rewrites
 * `/api/projects/{id}/{tail}` back to `/api/{tail}` against that project's
 * state; the all-projects default and the active project stay unprefixed. */
export function scopedUrl(scope: DashboardScope, url: string): string {
  if (scope.kind !== 'project') return url;
  if (unscopedRoute(url)) return url;
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

/** The token for a request that carries no project at all. */
export const UNSCOPED_CACHE_KEY = 'unscoped';

/**
 * Cache-key token for one REQUEST, which is not always the token for the scope
 * it was made under.
 *
 * `/api/projects` and `/api/dashboard` are never rewritten — the registry is
 * the thing that lists projects, and the chrome is above all of them — so the
 * same URL is fetched under every scope, and keying them by scope splits one
 * answer into an entry per project.
 *
 * The question is about the ROUTE, not about whether `scopedUrl` rewrote this
 * particular request: `scopedUrl` rewrites nothing under the all-projects
 * scope, so asking it collapses every route into the unscoped bucket there and
 * stops agreeing with `scopeKey` — which lets a writer put a fresh reading into
 * a key no reader is watching. So a scoped route keys by scope in every scope,
 * including `all`, and only the genuinely unscoped routes share one entry.
 */
export function requestScopeKey(scope: DashboardScope, url: string): string {
  return unscopedRoute(url) ? UNSCOPED_CACHE_KEY : scopeKey(scope);
}

/**
 * The canonical React Query key for a request made under a dashboard scope.
 *
 * A scope is part of a key only when the request carries that scope. Registry
 * and chrome routes deliberately do not, so they retain one cache entry while
 * a project is selected instead of becoming stale copies that no registry
 * invalidation can enumerate.
 */
export function scopedQueryKey(
  scope: DashboardScope,
  key: readonly unknown[],
  url: string,
): readonly unknown[] {
  return [...key, requestScopeKey(scope, url)];
}
