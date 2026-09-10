import snapshot from './snapshot.json';
export type AtlasNode = {
    id: string;
    path: string;
    name: string;
    kind: 'directory' | 'file' | 'crate';
    parent: string | null;
    files: number;
    bytes: number;
    removed?: boolean;
};
export type AtlasEdge = {
    source: string;
    target: string;
    kind: 'manifest-dependency';
    scope: string;
    manifest: string;
    dependency: string;
    optional: boolean;
};
export type AtlasChange = {
    path: string;
    status: string;
    previousPath?: string;
};
export const atlasData = snapshot as Omit<typeof snapshot, 'nodes' | 'edges' | 'changes'> & {
    nodes: AtlasNode[];
    edges: AtlasEdge[];
    changes: AtlasChange[];
};
export type Rect = {
    x: number;
    y: number;
    w: number;
    h: number;
    depth: number;
    node: AtlasNode;
};
export const nodeById = new Map(atlasData.nodes.map(n => [n.id, n]));
export const children = new Map<string, AtlasNode[]>();
for (const n of atlasData.nodes)
    if (n.parent !== null) {
        const group = children.get(n.parent) ?? [];
        group.push(n);
        children.set(n.parent, group);
    }
for (const group of children.values())
    group.sort((a, b) => a.path.localeCompare(b.path));
export const bounds = new Map<string, Rect>();
function partition(items: AtlasNode[], x: number, y: number, w: number, h: number, depth: number) {
    if (!items.length)
        return;
    if (items.length === 1) {
        layout(items[0], x, y, w, h, depth);
        return;
    }
    const total = items.reduce((s, n) => s + Math.max(1, n.files), 0);
    let sum = 0;
    let split = 1;
    for (let i = 0; i < items.length - 1; i++) {
        sum += Math.max(1, items[i].files);
        split = i + 1;
        if (sum >= total / 2)
            break;
    }
    const ratio = sum / total;
    if (w > h) {
        partition(items.slice(0, split), x, y, w * ratio, h, depth);
        partition(items.slice(split), x + w * ratio, y, w * (1 - ratio), h, depth);
    }
    else {
        partition(items.slice(0, split), x, y, w, h * ratio, depth);
        partition(items.slice(split), x, y + h * ratio, w, h * (1 - ratio), depth);
    }
}
function layout(node: AtlasNode, x: number, y: number, w: number, h: number, depth: number) {
    bounds.set(node.id, { x, y, w, h, depth, node });
    const gap = Math.min(2, w * .02, h * .02), header = Math.min(18, h * .1);
    partition(children.get(node.id) ?? [], x + gap, y + header, w - gap * 2, h - header - gap, depth + 1);
}
layout(nodeById.get('')!, 0, 0, 1600, 1100, 0);
export function owningCrate(id: string): string | undefined { let node = nodeById.get(id); while (node) {
    if (node.kind === 'crate')
        return node.id;
    node = node.parent === null ? undefined : nodeById.get(node.parent);
} return undefined; }
export function containsChange(id: string) { return atlasData.changes.some(c => id === '' || c.path === id || c.path.startsWith(id + '/')); }
// SCC membership on declared production path dependencies; dev-only edges excluded.
const productionEdges = atlasData.edges.filter(e => !e.scope.includes('dev-dependencies'));
export function cyclicGroups() {
    const nodes = atlasData.nodes.filter(n => n.kind === 'crate').map(n => n.id);
    const adjacency = new Map(nodes.map(n => [n, productionEdges.filter(e => e.source === n).map(e => e.target)]));
    let index = 0;
    const indices = new Map<string, number>();
    const low = new Map<string, number>();
    const stack: string[] = [];
    const active = new Set<string>();
    const groups: string[][] = [];
    function visit(v: string) { indices.set(v, index); low.set(v, index++); stack.push(v); active.add(v); for (const w of adjacency.get(v) ?? []) {
        if (!indices.has(w)) {
            visit(w);
            low.set(v, Math.min(low.get(v)!, low.get(w)!));
        }
        else if (active.has(w))
            low.set(v, Math.min(low.get(v)!, indices.get(w)!));
    } if (low.get(v) === indices.get(v)) {
        const group: string[] = [];
        let w: string;
        do {
            w = stack.pop()!;
            active.delete(w);
            group.push(w);
        } while (w !== v);
        if (group.length > 1 || (adjacency.get(v) ?? []).includes(v))
            groups.push(group);
    } }
    for (const n of nodes)
        if (!indices.has(n))
            visit(n);
    return groups;
}
export const cycleGroups = cyclicGroups();

export type ChurnReading = { touches: number; lastTouchedAt: number; lastCommit: string };
export const churnByPath = new Map<string, ChurnReading>();
for (const file of atlasData.churn.files) {
    const parts = file.path.split('/');
    for (let depth = 0; depth <= parts.length; depth++) {
        const path = parts.slice(0, depth).join('/');
        const prior = churnByPath.get(path) ?? { touches: 0, lastTouchedAt: 0, lastCommit: '' };
        const latest = file.lastTouchedAt > prior.lastTouchedAt ? file : prior;
        churnByPath.set(path, { touches: prior.touches + file.touches, lastTouchedAt: latest.lastTouchedAt, lastCommit: latest.lastCommit });
    }
}
export const maximumFileTouches = Math.max(1, ...atlasData.churn.files.map(file => file.touches));
export const duplicatePathCount = new Map<string, number>();
for (const group of atlasData.duplicateFiles.groups) {
    for (const file of group.paths) {
        const parts = file.split('/');
        for (let depth = 0; depth <= parts.length; depth++) {
            const path = parts.slice(0, depth).join('/');
            duplicatePathCount.set(path, (duplicatePathCount.get(path) ?? 0) + 1);
        }
    }
}
