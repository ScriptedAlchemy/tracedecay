import type {
  DeliveryInboxPullRequestV1,
  DeliveryInboxV1,
} from '../../contracts/generated.ts';
import type { EvidenceGrade } from './evidence.ts';
import { activeAttention } from './inboxFilter.ts';
import type { Umbrella, UmbrellaProjection } from './umbrella.ts';

/**
 * Deterministic geometry for the Delivery field at outcome zoom: project hubs
 * on a ring, admitted pull requests orbiting their hub, and umbrella roots
 * placed at the centroid of their members with a graded edge to each. No
 * force simulation — identical inputs give identical coordinates, and a node
 * moves only when the inbox it is drawn from changes.
 */
export interface FieldHub {
  readonly projectId: string;
  readonly label: string;
  readonly x: number;
  readonly y: number;
  readonly radius: number;
  readonly rows: number;
}

export interface FieldNode {
  readonly id: string;
  readonly row: DeliveryInboxPullRequestV1;
  readonly projectId: string;
  readonly x: number;
  readonly y: number;
  readonly radius: number;
  readonly active: number;
}

export interface FieldRoot {
  readonly umbrella: Umbrella;
  readonly x: number;
  readonly y: number;
  readonly radius: number;
}

export interface FieldEdge {
  readonly id: string;
  readonly umbrellaId: string;
  readonly grade: EvidenceGrade;
  readonly from: { readonly x: number; readonly y: number };
  readonly to: { readonly x: number; readonly y: number };
}

export interface FieldLayout {
  readonly width: number;
  readonly height: number;
  readonly hubs: readonly FieldHub[];
  readonly nodes: readonly FieldNode[];
  readonly roots: readonly FieldRoot[];
  readonly edges: readonly FieldEdge[];
}

export const FIELD_WIDTH = 800;
export const FIELD_HEIGHT = 480;

export function layoutField(
  inbox: DeliveryInboxV1,
  rows: readonly DeliveryInboxPullRequestV1[],
  projection: UmbrellaProjection,
  focusUmbrellaId: string | null,
): FieldLayout {
  const width = FIELD_WIDTH;
  const height = FIELD_HEIGHT;
  const cx = width / 2;
  const cy = height / 2;

  const focus =
    focusUmbrellaId === null
      ? null
      : projection.umbrellas.find((umbrella) => umbrella.id === focusUmbrellaId) ?? null;
  const visibleRows =
    focus === null
      ? rows
      : rows.filter((row) => focus.members.some((member) => member.id === row.id));

  const projectIds = [...new Set(visibleRows.map((row) => row.project_id))].sort();
  const labels = new Map(inbox.projects.map((project) => [project.project_id, project.label]));
  const ringRadius =
    projectIds.length <= 1 ? 0 : Math.min(width, height) * (focus === null ? 0.33 : 0.36);
  const hubs: FieldHub[] = projectIds.map((projectId, index) => {
    const angle = -Math.PI / 2 + (index / Math.max(1, projectIds.length)) * Math.PI * 2;
    const count = visibleRows.filter((row) => row.project_id === projectId).length;
    return {
      projectId,
      label: labels.get(projectId) ?? projectId,
      x: cx + Math.cos(angle) * ringRadius,
      y: cy + Math.sin(angle) * ringRadius,
      radius: 14 + Math.min(10, count * 2),
      rows: count,
    };
  });
  const hubById = new Map(hubs.map((hub) => [hub.projectId, hub]));

  const nodes: FieldNode[] = [];
  for (const hub of hubs) {
    const members = visibleRows.filter((row) => row.project_id === hub.projectId);
    const orbit = hub.radius + 34 + Math.min(30, members.length * 3);
    members.forEach((row, index) => {
      const angle = -Math.PI / 2 + (index / Math.max(1, members.length)) * Math.PI * 2 + 0.35;
      const active = activeAttention(row);
      nodes.push({
        id: row.id,
        row,
        projectId: hub.projectId,
        x: hub.x + Math.cos(angle) * orbit,
        y: hub.y + Math.sin(angle) * orbit,
        radius: 5 + Math.min(4, active),
        active,
      });
    });
  }
  const nodeById = new Map(nodes.map((node) => [node.id, node]));

  const umbrellas = focus === null ? projection.umbrellas : [focus];
  const roots: FieldRoot[] = [];
  const edges: FieldEdge[] = [];
  for (const umbrella of umbrellas) {
    const memberNodes = umbrella.members
      .map((member) => nodeById.get(member.id))
      .filter((node): node is FieldNode => node !== undefined);
    if (memberNodes.length === 0) continue;
    const root =
      focus !== null
        ? { x: cx, y: cy }
        : {
            x: memberNodes.reduce((sum, node) => sum + node.x, 0) / memberNodes.length,
            y: memberNodes.reduce((sum, node) => sum + node.y, 0) / memberNodes.length,
          };
    roots.push({
      umbrella,
      x: root.x,
      y: root.y,
      radius: focus !== null ? 46 : 10 + Math.min(8, memberNodes.length * 2),
    });
    for (const node of memberNodes) {
      edges.push({
        id: `${umbrella.id}->${node.id}`,
        umbrellaId: umbrella.id,
        grade: umbrella.grade,
        from: root,
        to: { x: node.x, y: node.y },
      });
    }
  }

  return { width, height, hubs: [...hubById.values()], nodes, roots, edges };
}

/** A quadratic path bowed toward the field centre, so bundled edges read as
 * arcs rather than a star of straight spokes. */
export function arcPath(
  from: { readonly x: number; readonly y: number },
  to: { readonly x: number; readonly y: number },
  center: { readonly x: number; readonly y: number },
): string {
  const mx = (from.x + to.x) / 2;
  const my = (from.y + to.y) / 2;
  const control = { x: mx + (center.x - mx) * 0.25, y: my + (center.y - my) * 0.25 };
  return `M ${from.x.toFixed(1)} ${from.y.toFixed(1)} Q ${control.x.toFixed(1)} ${control.y.toFixed(1)} ${to.x.toFixed(1)} ${to.y.toFixed(1)}`;
}
