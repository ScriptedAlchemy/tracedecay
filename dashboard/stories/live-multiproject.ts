/**
 * Multi-project live payload sweep.
 *
 * `live-sweep.ts` proves each workspace *renders*: it loads a route and checks
 * the main element is non-empty and did not fall into React Router's error
 * boundary. That is necessary but not sufficient — a workspace whose data
 * source is unavailable still renders its chrome (headers, empty-state cards,
 * nav) and so passes that sweep while showing nothing real. The Code workspace
 * did exactly that: `/api/plugins/graph/overview` answered 200 with
 * `payload: null`, and the render sweep called it a pass.
 *
 * This sweep asserts on the payload instead of the pixels, for every enrolled
 * project rather than only the active one. A read that failed is not silently
 * equivalent to a read that found nothing: the dashboard envelope distinguishes
 * them, and this harness surfaces that distinction rather than flattening it.
 *
 *   `domain_state: "ready"`                  -> payload present, counts below
 *   `domain_state: "complete_zero_findings"` -> genuinely empty, and says so
 *   `domain_state: "unknown"` + null payload -> the read FAILED; the reason is
 *                                               in `coverage.omission_reasons`
 *
 * That last case is the one worth reading carefully. `DashboardEnvelopeV1::
 * unavailable` sets `Unknown` and pushes the reason onto
 * `coverage.omission_reasons`, so a null payload always carries a machine-
 * readable cause. Printing the state without the reason (as a bare 200/!=200
 * check does) throws away the only part that says what to fix.
 *
 * Usage:
 *   SWEEP_BASE_URL=http://127.0.0.1:8397 npx tsx stories/live-multiproject.ts
 *
 * Exit code is non-zero if any enrolled project fails to serve a graph
 * overview, so this is usable as a gate and not only as a report.
 */

// This file has no imports, and top-level `await` needs it to be a module.
export {};

const BASE = process.env['SWEEP_BASE_URL'] ?? 'http://127.0.0.1:8397';

/** Envelope fields this harness reads. Deliberately loose: the point is to
 *  report whatever a live daemon actually sent, including shapes a stricter
 *  schema would reject outright. */
interface Envelope {
  domain_state?: string;
  coverage?: { omission_reasons?: string[] };
  payload?: unknown;
}

interface ProjectRow {
  id: string;
  root: string;
}

/** A transport failure is reported as a row, not as an unhandled rejection:
 *  "the daemon is not listening" is a result this sweep should print next to
 *  the projects it did reach, not a stack trace that hides them. */
async function getJson(path: string): Promise<{ status: number; body: Envelope }> {
  let res: Response;
  try {
    res = await fetch(`${BASE}${path}`, { headers: { accept: 'application/json' } });
  } catch (cause) {
    return {
      status: 0,
      body: { domain_state: 'unreachable', coverage: { omission_reasons: [String(cause).slice(0, 120)] } },
    };
  }
  let body: Envelope = {};
  try {
    body = (await res.json()) as Envelope;
  } catch {
    body = {};
  }
  return { status: res.status, body };
}

/** Registry rows name their project differently across surfaces; accept any of
 *  the documented spellings rather than guessing one and reporting zero. */
function readProjects(payload: unknown): ProjectRow[] {
  const rows = (payload as { projects?: unknown } | undefined)?.projects;
  if (!Array.isArray(rows)) return [];
  return rows.flatMap((raw) => {
    const row = raw as Record<string, unknown>;
    const id = row['project_id'] ?? row['id'] ?? row['projectId'];
    const root = row['root'] ?? row['project_root'] ?? row['path'] ?? '';
    return typeof id === 'string' ? [{ id, root: String(root) }] : [];
  });
}

function num(value: unknown): number {
  return typeof value === 'number' && Number.isFinite(value) ? value : 0;
}

/** Totals are reported by the server; fall back to counting the by-kind arrays
 *  so a payload that omits a total is described rather than scored as zero. */
function overviewCounts(payload: unknown): {
  nodes: number;
  edges: number;
  files: number;
  languages: number;
  topLanguages: string;
} {
  const p = (payload ?? {}) as Record<string, unknown>;
  const totals = (p['totals'] ?? {}) as Record<string, unknown>;
  const byLanguage = Array.isArray(p['files_by_language'])
    ? (p['files_by_language'] as Record<string, unknown>[])
    : [];
  const sum = (rows: unknown): number =>
    Array.isArray(rows)
      ? (rows as Record<string, unknown>[]).reduce((acc, r) => acc + num(r['count']), 0)
      : 0;
  const nodes = num(totals['nodes']) || sum(p['nodes_by_kind']);
  const edges = num(totals['edges']) || sum(p['edges_by_kind']);
  const files = num(totals['files']) || sum(byLanguage);
  const topLanguages = byLanguage
    .slice()
    .sort((a, b) => num(b['count']) - num(a['count']))
    .slice(0, 3)
    .map((r) => `${String(r['language'] ?? '?')}:${num(r['count'])}`)
    .join(',');
  return { nodes, edges, files, languages: byLanguage.length, topLanguages };
}

function reasons(env: Envelope): string {
  const list = env.coverage?.omission_reasons;
  return Array.isArray(list) && list.length > 0 ? list.join('; ').slice(0, 160) : '';
}

const registry = await getJson('/api/projects');
const projects = readProjects(registry.body.payload);
console.log(`registry: status=${registry.status} domain_state=${registry.body.domain_state ?? '?'} projects=${projects.length}`);
if (projects.length === 0) {
  console.log(`registry reasons: ${reasons(registry.body) || '(none)'}`);
}

let failures = 0;
const rows: string[] = [];

for (const project of projects) {
  const overview = await getJson(`/api/projects/${encodeURIComponent(project.id)}/plugins/graph/overview`);
  const state = overview.body.domain_state ?? '?';
  const counts = overviewCounts(overview.body.payload);
  const served = overview.body.payload != null && counts.nodes > 0;
  if (!served) failures++;
  const name = project.root.split('/').filter(Boolean).pop() ?? project.id;
  rows.push(
    [
      name.padEnd(16),
      String(overview.status).padEnd(4),
      state.padEnd(22),
      `nodes=${String(counts.nodes).padStart(7)}`,
      `edges=${String(counts.edges).padStart(7)}`,
      `files=${String(counts.files).padStart(6)}`,
      `langs=${String(counts.languages).padStart(3)}`,
      served ? `ok  ${counts.topLanguages}` : `FAIL ${reasons(overview.body) || '(no reason given)'}`,
    ].join('  '),
  );
}

for (const row of rows) console.log(row);
// An empty registry is a failure with its own sentence rather than a vacuous
// pass: "0 of 0 projects failed" is true and useless.
if (projects.length === 0) {
  console.log('MULTIPROJECT SWEEP FAIL: the registry returned no enrolled projects');
} else if (failures === 0) {
  console.log(
    `MULTIPROJECT SWEEP PASS: ${projects.length} project(s) served a non-empty graph overview`,
  );
} else {
  console.log(
    `MULTIPROJECT SWEEP FAIL: ${failures}/${projects.length} project(s) served no graph payload`,
  );
}
process.exitCode = failures === 0 && projects.length > 0 ? 0 : 1;
