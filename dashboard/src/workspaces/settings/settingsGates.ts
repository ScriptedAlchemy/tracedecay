/**
 * Whether a settings scope may be written, and why not when it may not.
 *
 * Two independent authorities have to agree, and a boolean could only report
 * their conjunction — which left a disabled editor claiming "this dashboard is
 * not authorized" when the real obstacle was that the selected project is not
 * the active one. They stay distinguishable:
 *
 *   - `unauthorized`: the envelope advertises no apply action for this scope,
 *     so the daemon has no mounted authority for it.
 *   - `read_only` / `unknown`: the scope this dashboard is pointed at, from
 *     `scopeWritable`.
 */

import type { DashboardLegalActionRefV1 } from '../../contracts/generated.ts';
import type { ScopeWritability } from '../../data/scope/store.ts';

export type SettingsWriteGate =
  | { readonly state: 'writable'; readonly target: string }
  | { readonly state: 'unauthorized' }
  | { readonly state: 'read_only'; readonly reason: string }
  | { readonly state: 'unknown'; readonly reason: string };

export interface WritableScopes {
  readonly project: SettingsWriteGate;
  readonly user: SettingsWriteGate;
  readonly codeIndexWorkers: SettingsWriteGate;
}

/** The profile-global ProfileSessions resource is written in profile scope
 * whatever project the dashboard is pointed at. */
export const PROFILE_WORKER_WRITABILITY: ScopeWritability = {
  state: 'writable',
  target: 'your TraceDecay profile',
};

/**
 * Fold the two authorities into one gate.
 *
 * Server authorization is checked first: without an advertised apply action
 * there is nothing to write in any scope, so naming the scope would point at
 * the wrong obstacle. Exhaustive over `ScopeWritability`.
 */
export function settingsWriteGate(
  authorized: boolean,
  writability: ScopeWritability,
): SettingsWriteGate {
  if (!authorized) return { state: 'unauthorized' };
  switch (writability.state) {
    case 'writable':
      return { state: 'writable', target: writability.target };
    case 'read_only':
      return { state: 'read_only', reason: writability.reason };
    case 'unknown':
      return { state: 'unknown', reason: writability.reason };
    default: {
      const exhaustive: never = writability;
      return exhaustive;
    }
  }
}

/**
 * Which settings scopes the server currently authorizes a write for.
 *
 * Project and ordinary user settings settle through `configuration_batch` and
 * the selected project's gateway. Code-index workers are a profile-global
 * ProfileSessions resource: only its own advertised operation controls its
 * availability, never the selected-project gateway.
 */
export function writableScopes(
  legalActions: readonly DashboardLegalActionRefV1[],
  writability: ScopeWritability,
): WritableScopes {
  const authorizes = (operation: string) =>
    legalActions.some(
      (action) => action.kind === 'request_apply' && action.operation === operation,
    );
  return {
    project: settingsWriteGate(authorizes('configuration_batch'), writability),
    user: settingsWriteGate(authorizes('configuration_batch'), writability),
    codeIndexWorkers: settingsWriteGate(
      authorizes('profile_code_index_worker_selection'),
      PROFILE_WORKER_WRITABILITY,
    ),
  };
}
