import type { ProjectRepoGroup } from '../../contracts/generated.ts';
import type { LiveActivityPulse } from '../../data/sse/connect.ts';
import { relativeAge } from '../../ui/time.ts';
import {
  resolveZoom,
  type FieldScene,
  type SceneBody,
  type SceneCluster,
  type ScenePath,
} from '../../viz/graph/fieldRenderers/scene.ts';
import { bodyRadius, type RegistryField } from './field.ts';

/**
 * The Brain registry as a renderer-neutral scene: measured bodies, massless
 * repository hubs, exact relations, packed cells, and the activity rule that
 * decides which drawn identity an admitted pulse may light.
 */

/** Heat half-life for admitted activity on every Brain field. */
export const FIELD_HALF_LIFE_MS = 4200;

/** A hub is a relation junction, never holdings: one fixed categorical size. */
const HUB_RADIUS = 0.07;

/** Compose the measured registry field into the shared scene. Every printed
 * reading is a registry value; a missing one is printed as `absent`. */
export function buildRegistryScene(
  field: RegistryField,
  groups: readonly ProjectRepoGroup[],
  nowSeconds: number = Date.now() / 1000,
): FieldScene {
  const projects = new Map(
    groups.flatMap((group) => group.projects.map((project) => [project.project_id, { project, group }] as const)),
  );
  const bodies: SceneBody[] = field.nodes.map((node) => {
    const entry = projects.get(node.id);
    if (!entry) {
      const repository = groups.find((group) => `repo:${group.git_common_dir}` === node.id);
      return {
        id: node.id,
        label: node.label,
        role: 'hub',
        kind: node.kind,
        x: node.x,
        y: node.y,
        radius: HUB_RADIUS,
        mass: null,
        units: null,
        vitality: node.vitality,
        detail: [
          `repository · ${node.degree} checkouts`,
          repository?.git_common_dir ?? 'git directory absent',
        ],
        group: repository?.git_common_dir ?? null,
        cluster: null,
      };
    }
    const { project, group } = entry;
    return {
      id: node.id,
      label: node.label,
      role: 'body',
      kind: node.kind,
      x: node.x,
      y: node.y,
      radius: bodyRadius(node.degree, field.massCeiling),
      mass: node.degree,
      units: { stores: project.store_count, artifacts: project.artifact_count },
      vitality: node.vitality,
      detail: [
        `stores ${project.store_count.toLocaleString()}`,
        `artifacts ${project.artifact_count.toLocaleString()}`,
        `mass ${node.degree.toLocaleString()}`,
        `seen ${relativeAge(project.last_seen_at, nowSeconds) ?? 'absent'}`,
        project.default_branch ? `branch ${project.default_branch}` : 'branch absent',
        group.git_common_dir ? `repo ${group.label}` : 'repository absent',
      ],
      group: group.git_common_dir,
      cluster: node.cell,
    };
  });
  const paths: ScenePath[] = field.edges.map((edge) => ({
    source: edge.source,
    target: edge.target,
    relation: edge.kind,
    grade: 'EXACT',
  }));
  const clusters: SceneCluster[] = field.cells.map((cell) => ({
    id: cell.id,
    members: cell.members,
    x: cell.x,
    y: cell.y,
    width: cell.width,
    height: cell.height,
    resolveZoom: resolveZoom(cell.maxRadius, cell.spacing),
    mass: cell.mass,
    spacing: cell.spacing,
  }));
  return {
    bodies,
    paths,
    clusters,
    extent: field.extent,
    columns: field.columns.map((column) => ({ label: column.label, bound: column.bound, count: column.count })),
    neighbors: adjacency(paths),
  };
}

export interface GraphInput {
  id: string;
  label: string;
  kind: string;
  degree?: number | undefined;
  x: number;
  y: number;
}

/** A returned symbol graph with emergent coordinates, on the same scene
 * contract. Size is connectedness; an absent degree stays absent. */
export function buildGraphScene(
  nodes: readonly GraphInput[],
  edges: ReadonlyArray<{ source: string; target: string; kind?: string | undefined }>,
): FieldScene {
  const ceiling = nodes.reduce((max, node) => Math.max(max, node.degree ?? 0), 1);
  let minX = Infinity;
  let maxX = -Infinity;
  let minY = Infinity;
  let maxY = -Infinity;
  for (const node of nodes) {
    minX = Math.min(minX, node.x);
    maxX = Math.max(maxX, node.x);
    minY = Math.min(minY, node.y);
    maxY = Math.max(maxY, node.y);
  }
  const span = Math.max(maxX - minX, maxY - minY, 1e-6);
  const unit = span / Math.max(8, Math.sqrt(nodes.length) * 3);
  const ids = new Set(nodes.map((node) => node.id));
  const bodies: SceneBody[] = nodes.map((node) => ({
    id: node.id,
    label: node.label,
    role: 'body',
    kind: node.kind,
    x: node.x,
    y: node.y,
    radius: unit * (0.18 + 0.5 * Math.sqrt((node.degree ?? 0) / ceiling)),
    mass: node.degree ?? null,
    units: null,
    vitality: null,
    detail: [node.kind, node.degree == null ? 'connectedness absent' : `connectedness ${node.degree}`],
    group: null,
    cluster: null,
  }));
  const paths: ScenePath[] = edges
    .filter((edge) => ids.has(edge.source) && ids.has(edge.target))
    .map((edge) => ({
      source: edge.source,
      target: edge.target,
      relation: edge.kind ?? 'relation',
      grade: 'EXACT',
    }));
  const pad = span * 0.08;
  return {
    bodies,
    paths,
    clusters: [],
    extent: { x: [minX - pad, maxX + pad], y: [minY - pad, maxY + pad] },
    columns: null,
    neighbors: adjacency(paths),
  };
}

function adjacency(paths: readonly ScenePath[]): Map<string, string[]> {
  const neighbors = new Map<string, string[]>();
  const link = (from: string, to: string): void => {
    const list = neighbors.get(from);
    if (list) {
      if (!list.includes(to)) list.push(to);
    } else neighbors.set(from, [to]);
  };
  for (const path of paths) {
    link(path.source, path.target);
    link(path.target, path.source);
  }
  return neighbors;
}

/** What one admitted pulse lights on a drawn field, or null. Liveness
 * (`heartbeat`) is never activity, and a pulse naming no drawn body lights
 * nothing. The hop follows drawn relations only and stops after one step:
 * a checkout's hop is its repository hub, never a sibling checkout. */
export interface Strike {
  touched: string;
  hop: readonly string[];
  energy: number;
  label: string;
}

export function strikeFor(pulse: LiveActivityPulse, scene: FieldScene): Strike | null {
  if (pulse.family === 'heartbeat' || pulse.projectId == null) return null;
  const touched = scene.bodies.find((body) => body.id === pulse.projectId && body.role === 'body');
  if (!touched) return null;
  return {
    touched: touched.id,
    hop: scene.neighbors.get(touched.id) ?? [],
    energy: strikeEnergy(pulse.family),
    label: pulse.family.replace(/_activity$/, '').replace(/_/g, ' '),
  };
}

function strikeEnergy(family: string): number {
  if (family === 'project_registry_changed') return 0.95;
  if (family === 'hook_activity') return 0.85;
  if (family.startsWith('code_index')) return 0.8;
  if (family === 'tool_call_activity') return 0.7;
  if (family === 'session_ingest_activity') return 0.65;
  if (family === 'storage_telemetry_invalidated') return 0.6;
  return 0.5;
}

/** Keyboard traversal order: recency column left to right, then heavier
 * first, so arrow keys walk the field the way the eye reads it. */
export function traversalOrder(scene: FieldScene): string[] {
  return [...scene.bodies]
    .filter((body) => body.role === 'body')
    .sort((a, b) => Math.round(a.x) - Math.round(b.x) || b.y - a.y || a.id.localeCompare(b.id))
    .map((body) => body.id);
}
