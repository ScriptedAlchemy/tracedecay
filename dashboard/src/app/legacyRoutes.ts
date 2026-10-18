import type { ViewPath } from './channels.ts';

/**
 * Where each path that existed before issue #4220 now lives.
 *
 * Fourteen top-level workspaces became five task families, so eleven of the old
 * paths moved. Every one still resolves: a bookmark, a link another view built
 * before this change, or a payload field that names an old route lands on the
 * equivalent view with its query string intact, including the project scope
 * (`scope`, `scopeLabel`) and every view-local parameter that view still reads.
 *
 * This is a plain data module with no JSX and no imports from the workspaces,
 * so the shell chunk that carries it stays light. `/code`, `/work` and
 * `/settings` kept their paths and are not listed.
 */
export const LEGACY_PATHS = {
  brain: 'memory',
  explorer: 'memory/explorer',
  knowledge: 'memory/knowledge',
  sessions: 'memory/sessions',
  loom: 'memory/loom',
  agents: 'memory/agents',
  delivery: 'code/delivery',
  workflows: 'work/workflows',
  observatory: 'operations',
  automations: 'operations/automations',
  costs: 'operations/costs',
} as const satisfies Record<string, ViewPath>;

export type LegacyPath = keyof typeof LEGACY_PATHS;

export interface Destination {
  readonly pathname: `/${string}`;
  readonly search: string;
}

/**
 * The destination of a retired path.
 *
 * The one rewrite beyond the new path: the Agents timeline view was removed
 * because Loom draws the same delegation tree against time from the same read,
 * so `?view=timeline` opens Loom, with the linked session (`sessionId`,
 * `sessionProvider`) carried over as Loom's `loomSession` lane. A link with no
 * recorded provider keeps the bare session id, which Loom resolves by id.
 */
export function legacyDestination(from: LegacyPath, search: string): Destination {
  const params = new URLSearchParams(search);
  if (from === 'agents' && params.get('view') === 'timeline') {
    params.delete('view');
    const sessionId = params.get('sessionId');
    const provider = params.get('sessionProvider');
    if (sessionId !== null && sessionId !== '') {
      params.delete('sessionId');
      params.delete('sessionProvider');
      params.set(
        'loomSession',
        provider === null || provider === '' ? sessionId : JSON.stringify([provider, sessionId]),
      );
    }
    return { pathname: `/${LEGACY_PATHS.loom}`, search: searchOf(params) };
  }
  return { pathname: `/${LEGACY_PATHS[from]}`, search };
}

function searchOf(params: URLSearchParams): string {
  const text = params.toString();
  return text === '' ? '' : `?${text}`;
}
