import { useEffect, useRef, useState } from 'react';
import { useDemo, useWorkspaceState } from '../app/workspace';
import { atlasData, bounds, children, containsChange, cycleGroups, nodeById, owningCrate, churnByPath, maximumFileTouches, duplicatePathCount, type AtlasNode, type Rect } from './model';
import './atlas.css';
type Camera = {
    x: number;
    y: number;
    scale: number;
};
export const ATLAS_LAYERS = ['structure', 'comparison', 'dependencies', 'changes', 'cycles', 'forwarding', 'churn', 'duplicates', 'coverage'] as const;
export type AtlasLayer = typeof ATLAS_LAYERS[number];
function isCamera(value: unknown): value is Camera {
    if (!value || typeof value !== 'object') return false;
    const candidate = value as Partial<Camera>;
    return typeof candidate.x === 'number' && Number.isFinite(candidate.x)
        && typeof candidate.y === 'number' && Number.isFinite(candidate.y)
        && typeof candidate.scale === 'number' && Number.isFinite(candidate.scale) && candidate.scale > 0;
}
export type AtlasViewState = {
    selected: string;
    camera: Camera | null;
    layer: AtlasLayer;
    comparison: 'before' | 'after';
};
export type RepositoryAtlasProps = {
    context?: 'brain' | 'code' | 'explorer' | 'delivery';
    initialSelection?: string;
    onSelect?: (node: AtlasNode) => void;
    compact?: boolean;
};
const colors = ['#258ba0', '#236b86', '#245b73', '#327b89', '#325e79'];
const short = (s: string) => s.slice(0, 8);
function churnColor(node: AtlasNode) {
    const touches = churnByPath.get(node.id)?.touches ?? 0;
    const intensity = Math.log1p(touches / Math.max(1,node.files)) / Math.log1p(maximumFileTouches);
    const amount = touches ? .1 + intensity*.55 : 0;
    return `rgb(${Math.round(6+229*amount)},${Math.round(18+121*amount)},${Math.round(27+37*amount)})`;
}
export function RepositoryAtlas({ context = 'brain', initialSelection, onSelect, compact = false }: RepositoryAtlasProps) {
    const { navigate } = useDemo();
    const [restoredState, setState] = useWorkspaceState<AtlasViewState>(context === 'delivery' ? 'repository-atlas:delivery' : 'repository-atlas', { selected: initialSelection ?? '', camera: null, layer: 'structure', comparison: 'after' });
    const state: AtlasViewState = {
        selected: typeof restoredState.selected === 'string' && nodeById.has(restoredState.selected) ? restoredState.selected : '',
        camera: isCamera(restoredState.camera) ? restoredState.camera : null,
        layer: ATLAS_LAYERS.includes(restoredState.layer) ? restoredState.layer : 'structure',
        comparison: restoredState.comparison === 'before' ? 'before' : 'after',
    };
    useEffect(() => {
        if (state.selected !== restoredState.selected || state.camera !== restoredState.camera
            || state.layer !== restoredState.layer || state.comparison !== restoredState.comparison) setState(state);
    }, [restoredState, setState]);
    const [duplicateId, setDuplicateId] = useWorkspaceState<string>(context === 'delivery' ? 'atlas-duplicate:delivery' : 'atlas-duplicate', '');
    const [query, setQuery] = useState('');
    const [exact, setExact] = useState(false);
    const [page, setPage] = useState(0);
    const [size, setSize] = useState({ w: 1000, h: 650 });
    const canvas = useRef<HTMLCanvasElement>(null);
    const minimap = useRef<HTMLCanvasElement>(null);
    const inspector = useRef<HTMLElement>(null);
    const drawn = useRef<Rect[]>([]);
    const drag = useRef<{
        x: number;
        y: number;
        camera: Camera;
        moved: boolean;
    } | null>(null);
    const selected = nodeById.get(state.selected) ?? nodeById.get('')!;
    useEffect(() => { if (inspector.current) inspector.current.scrollTop = 0; }, [selected.id]);
    const crate = owningCrate(selected.id);
    const churnReading = churnByPath.get(selected.id);
    const selectedDuplicate = atlasData.duplicateFiles.groups.find(group => group.paths.includes(selected.id)) ?? atlasData.duplicateFiles.groups.find(group => group.blob === duplicateId);
    const duplicateScopeCount = duplicatePathCount.get(selected.id) ?? 0;
    const churnLeaders = atlasData.churn.files.filter(file => selected.kind === 'file' ? file.path === selected.id : selected.id === '' || file.path.startsWith(selected.id + '/')).sort((a,b) => b.touches-a.touches || a.path.localeCompare(b.path)).slice(0,8);
    const narrow = compact || matchMedia('(max-width: 850px)').matches;
    const viewWidth = Math.max(80, size.w - (narrow ? 28 : 270));
    const viewHeight = Math.max(40, size.h - 90);
    const defaultScale = Math.min(viewWidth / 1600, viewHeight / 1100);
    const camera = state.camera ?? { x: 14 + (viewWidth - 1600 * defaultScale) / 2, y: 45 + (viewHeight - 1100 * defaultScale) / 2, scale: defaultScale };
    const selectedEdges = atlasData.edges.filter(e => e.source === crate || e.target === crate);
    const results = query ? atlasData.nodes.filter(n => n.path.toLowerCase().includes(query.toLowerCase())) : atlasData.nodes;
    const change = atlasData.changes.find(c => c.path === selected.path);
    const comparisonScope = bounds.get(selected.id) ?? bounds.get('')!;
    const comparisonRects = [...bounds.values()].filter(rect => {
        const inScope = selected.id === '' || rect.node.id === selected.id || rect.node.id.startsWith(selected.id + '/');
        return inScope && (rect.depth <= comparisonScope.depth + 2 || containsChange(rect.node.id));
    });
    const sourceRevision = selected.removed ? atlasData.baselineRevision : change && (state.layer === 'changes' || state.layer === 'forwarding') ? (state.comparison === 'before' ? atlasData.baselineRevision : atlasData.changeRevision) : atlasData.revision;
    const href = `https://github.com/${atlasData.repository}/${selected.kind === 'file' ? 'blob' : 'tree'}/${sourceRevision}/${selected.path}`;
    function fitDuplicateGroup() {
        if (!selectedDuplicate) return;
        const regions = selectedDuplicate.paths.map(path => bounds.get(path)).filter((rect): rect is Rect => !!rect);
        if (!regions.length) return;
        const x = Math.min(...regions.map(r => r.x)), y = Math.min(...regions.map(r => r.y));
        const w = Math.max(...regions.map(r => r.x+r.w))-x, h = Math.max(...regions.map(r => r.y+r.h))-y;
        fitRect({x,y,w,h});
    }
    function updateCamera(c: Camera) { setState(s => ({ ...s, camera: c })); }
    function fitRect(rect: Pick<Rect, 'x' | 'y' | 'w' | 'h'>) {
        // Padding is in screen pixels; world-unit padding swallows small file cells.
        const scale = Math.max(.1, Math.min(viewWidth / rect.w, viewHeight / rect.h, 100000));
        updateCamera({x:14+(viewWidth-rect.w*scale)/2-rect.x*scale, y:45+(viewHeight-rect.h*scale)/2-rect.y*scale, scale});
    }
    function fit(id = '') { const rect = bounds.get(id); if (rect) fitRect(rect); }
    function select(node: AtlasNode, zoom = false) { setState(s => ({ ...s, selected: node.id })); onSelect?.(node);
        if(context==='brain') { const url=new URL(location.href);url.searchParams.set('node',node.id);url.searchParams.set('path',node.id);history.replaceState(null,'',url); } if (zoom)
        fit(node.id); }
    function moveSelection(delta: number) {
        const siblings = children.get(selected.parent ?? '') ?? [];
        const index = siblings.findIndex(node => node.id === selected.id);
        const next = siblings[(index + delta + siblings.length) % siblings.length];
        if (next) select(next);
    }
    function zoom(factor: number, x = 14 + viewWidth / 2, y = size.h / 2) { const scale = Math.max(.1, Math.min(100000, camera.scale * factor)); updateCamera({ x: x - (x - camera.x) * scale / camera.scale, y: y - (y - camera.y) * scale / camera.scale, scale }); }
    useEffect(() => { if (initialSelection && nodeById.has(initialSelection))
        setState(s => ({ ...s, selected: initialSelection })); }, [initialSelection, setState]);
    useEffect(() => { const el = canvas.current; if (!el)
        return; const wheel = (e: WheelEvent) => { e.preventDefault(); const box = el.getBoundingClientRect(); zoom(e.deltaY < 0 ? 1.15 : 1 / 1.15, e.clientX - box.left, e.clientY - box.top); }; el.addEventListener('wheel', wheel, { passive: false }); return () => el.removeEventListener('wheel', wheel); }, [camera.x, camera.y, camera.scale]);
    useEffect(() => { const el = canvas.current; if (!el)
        return; const observer = new ResizeObserver(() => { setSize({ w: el.clientWidth, h: el.clientHeight }); }); observer.observe(el); return () => observer.disconnect(); }, []);
    useEffect(() => {
        const el = canvas.current;
        if (!el)
            return;
        const ctx = el.getContext('2d');
        if (!ctx)
            return;
        const dpr = Math.min(devicePixelRatio, 2);
        el.width = size.w * dpr;
        el.height = size.h * dpr;
        ctx.scale(dpr, dpr);
        ctx.fillStyle = '#040b12';
        ctx.fillRect(0, 0, size.w, size.h);
        // Geometry is frozen to the union of the head tree and two removed source files.
        ctx.save();
        ctx.translate(camera.x, camera.y);
        ctx.scale(camera.scale, camera.scale);
        drawn.current = [];
        const changes = state.layer === 'changes';
        const draw = (id: string) => {
            const r = bounds.get(id);
            if (!r)
                return;
            const { x, y, w, h, node, depth } = r;
            const px = w * camera.scale, py = h * camera.scale;
            if (px < 2 || py < 2 || camera.x + (x + w) * camera.scale < 0 || camera.y + (y + h) * camera.scale < 0 || camera.x + x * camera.scale > size.w || camera.y + y * camera.scale > size.h)
                return;
            if (node.removed && node.id !== selected.id && !(['changes','forwarding'].includes(state.layer) && state.comparison === 'before'))
                return;
            const active = node.id === selected.id;
            const changed = containsChange(id);
            const cycled = cycleGroups.some(g => g.includes(id));
            ctx.fillStyle = depth === 0 ? '#07121b' : colors[depth % colors.length] + (node.kind === 'file' ? '22' : '19');
            if (state.layer === 'churn') {
                ctx.fillStyle = churnColor(node);
            }
            const inset = Math.min(1 / camera.scale, w / 10, h / 10);
            ctx.fillRect(x + inset, y + inset, w - 2 * inset, h - 2 * inset);
            ctx.strokeStyle = active ? '#8bfcff' : changes && changed ? '#e8aa64' : state.layer === 'cycles' && cycled ? '#d28ef8' : colors[depth % colors.length];
            ctx.lineWidth = (active ? 2 : node.kind === 'crate' ? 1.1 : .45) / camera.scale;
            ctx.shadowColor = active ? '#4fefff' : node.kind === 'crate' ? '#126c84' : 'transparent';
            ctx.shadowBlur = active ? 14 : node.kind === 'crate' ? 4 : 0;
            if (node.removed) ctx.setLineDash([5 / camera.scale, 4 / camera.scale]);
            ctx.strokeRect(x + inset, y + inset, w - 2 * inset, h - 2 * inset);
            ctx.setLineDash([]);
            ctx.shadowBlur = 0;
            const groupContains = selectedDuplicate?.paths.some(path => path === id || path.startsWith(id + '/'));
            if (state.layer === 'duplicates' && (duplicatePathCount.get(id) ?? 0) > 0 && px>16 && py>16) {
                ctx.strokeStyle = groupContains ? '#dec6ff' : '#75608c';
                ctx.lineWidth = (groupContains ? 2 : 1)/camera.scale;
                // Paired corner stripes mark byte-identity groups; not call edges.
                for (let i=0;i<3;i++) { ctx.beginPath();ctx.moveTo(x+(5+i*4)/camera.scale,y+5/camera.scale);ctx.lineTo(x+(10+i*4)/camera.scale,y+11/camera.scale);ctx.stroke(); }
            }
            drawn.current.push(r);
            if (px > 95 && py > 32) {
                ctx.font = `${Math.min(12, Math.max(10, px / 30)) / camera.scale}px ui-monospace,monospace`;
                ctx.fillStyle = active ? '#e7ffff' : node.kind === 'crate' ? '#8ad6e9' : '#779fad';
                const label = node.name.replace(/^tracedecay-/, '') + (node.removed ? ' · baseline ghost' : '');
                const count = Math.floor(px / 7);
                ctx.fillText(label.length > count ? label.slice(0, count - 1) + '…' : label, x + 5 / camera.scale, y + 13 / camera.scale, Math.max(0, w - 10 / camera.scale));
            }
            // Subdivision is independent of label legibility. A narrow selected
            // directory can still contain visible before/after file cells.
            if (px > 6 && py > 6)
                for (const child of children.get(id) ?? [])
                    draw(child.id);
        };
        draw('');
        if (state.layer === 'dependencies' && crate) {
            for (const edge of selectedEdges) {
                const a = bounds.get(edge.source), b = bounds.get(edge.target);
                if (!a || !b)
                    continue;
                const ax = a.x + a.w / 2, ay = a.y + a.h / 2, bx = b.x + b.w / 2, by = b.y + b.h / 2;
                ctx.beginPath();
                ctx.setLineDash(edge.scope.includes('dev-dependencies') ? [4 / camera.scale, 3 / camera.scale] : []);
                ctx.strokeStyle = edge.source === crate ? '#ffc780aa' : '#64d7faaa';
                ctx.lineWidth = 1.25 / camera.scale;
                ctx.moveTo(ax, ay);
                ctx.quadraticCurveTo((ax + bx) / 2, Math.min(ay, by) - 35 / camera.scale, bx, by);
                ctx.stroke();
                ctx.beginPath();
                ctx.arc(bx, by, 3 / camera.scale, 0, Math.PI * 2);
                ctx.fillStyle = '#9ceeff';
                ctx.fill();
            }
            ctx.setLineDash([]);
        }
        if (state.layer === 'duplicates' && selectedDuplicate) {
            for (const path of selectedDuplicate.paths) {
                const region = bounds.get(path); if (!region) continue;
                const x=region.x+region.w/2,y=region.y+region.h/2;
                ctx.strokeStyle='#dcc0ff';ctx.lineWidth=1.5/camera.scale;
                ctx.beginPath();ctx.arc(x,y,7/camera.scale,0,Math.PI*2);ctx.stroke();
                ctx.beginPath();ctx.arc(x,y,3/camera.scale,0,Math.PI*2);ctx.stroke();
            }
        }
        ctx.restore();
        const mini = minimap.current;
        const m = mini?.getContext('2d');
        if (mini && m) {
            mini.width = 156;
            mini.height = 107;
            m.fillStyle = '#061019';
            m.fillRect(0, 0, 156, 107);
            for (const r of bounds.values())
                if (r.node.kind === 'crate') {
                    m.fillStyle = state.layer==='churn' ? churnColor(r.node) : state.layer==='duplicates' && duplicatePathCount.has(r.node.id) ? '#76528b' : containsChange(r.node.id) && changes ? '#ad7f49' : '#205667';
                    m.fillRect(r.x / 1600 * 156, r.y / 1100 * 107, Math.max(1, r.w / 1600 * 156), Math.max(1, r.h / 1100 * 107));
                }
            m.strokeStyle = '#91f4ff';
            m.strokeRect(-camera.x / camera.scale / 1600 * 156, -camera.y / camera.scale / 1100 * 107, size.w / camera.scale / 1600 * 156, size.h / camera.scale / 1100 * 107);
        }
    }, [camera.x, camera.y, camera.scale, size, selected.id, state.layer, state.comparison, crate, selectedDuplicate?.blob]);
    function pick(x: number, y: number) {
        if (state.layer==='duplicates' && selectedDuplicate) {
            const matching=selectedDuplicate.paths.find(path=>{const r=bounds.get(path);return r && Math.hypot(camera.x+(r.x+r.w/2)*camera.scale-x,camera.y+(r.y+r.h/2)*camera.scale-y)<=10;});
            if(matching) return nodeById.get(matching);
        }
 const wx = (x - camera.x) / camera.scale, wy = (y - camera.y) / camera.scale; return [...drawn.current].reverse().find(r => wx >= r.x && wx <= r.x + r.w && wy >= r.y && wy <= r.y + r.h)?.node; }
    const label = state.layer === 'coverage' ? 'Tracked files + Cargo measured · symbol and call coverage unavailable' : state.layer === 'comparison' ? 'Focused pinned refactor · shared union layout · baseline-wide measures unavailable' : state.layer === 'forwarding' ? 'Focused source diff · forwarding path before / after · not runtime reachability' : state.layer === 'churn' ? `Amber = log-scaled commit/file touches per file · ${atlasData.churn.days} days · not defects` : state.layer === 'duplicates' ? 'Violet stripes = byte-identical file candidates · rings = selected matching blob' : state.layer === 'dependencies' ? 'Declared Cargo paths · orange outgoing / cyan incoming · dashed dev' : state.layer === 'changes' ? 'Amber = paths touched by the focused refactor · positions remain frozen' : state.layer === 'cycles' ? `${cycleGroups.length} cyclic groups · declared non-dev path dependencies; not call cycles` : 'Area = tracked file count (+2 baseline ghosts) · boundaries = directories';
    return <section className="repository-atlas" aria-label="Measured repository atlas" data-context={context} data-compact={compact}>
  <div className="atlas-tools"><strong className="atlas-title">TRACEDECAY / STRUCTURAL ATLAS</strong><input aria-label="Find atlas path" placeholder="Locate a crate, directory, file…" value={query} onChange={e => { setQuery(e.target.value); setPage(0); }}/><button onClick={() => setExact(true)}>Exact paths</button></div>
  <div className="atlas-tools">{ATLAS_LAYERS.map(layer => <button key={layer} aria-pressed={state.layer === layer} onClick={() => setState(s => ({ ...s, layer }))}>{layer[0].toUpperCase() + layer.slice(1)}</button>)}<span className="atlas-revision">Git {short(atlasData.revision)} · {atlasData.coverage.trackedFiles.toLocaleString()} tracked files</span></div>
  <div className="atlas-stage">
   <canvas ref={canvas} aria-label="Repository containment map. Arrow keys pan, plus and minus zoom, Home fits, Enter zooms selection, Escape returns to parent." tabIndex={0} onPointerDown={e => { if (e.button !== 0)
        return; e.currentTarget.setPointerCapture(e.pointerId); drag.current = { x: e.clientX, y: e.clientY, camera, moved: false }; }} onPointerMove={e => { const d = drag.current; if (!d)
        return; const dx = e.clientX - d.x, dy = e.clientY - d.y; d.moved = d.moved || Math.hypot(dx, dy) > 4; if (d.moved)
        updateCamera({ ...d.camera, x: d.camera.x + dx, y: d.camera.y + dy }); }} onPointerUp={e => { const d = drag.current; drag.current = null; if (d && !d.moved) {
        const box = e.currentTarget.getBoundingClientRect();
        const n = pick(e.clientX - box.left, e.clientY - box.top);
        if (n)
            select(n);
    } }} onPointerCancel={() => { drag.current = null; }} onDoubleClick={e => { const box = e.currentTarget.getBoundingClientRect(); const n = pick(e.clientX - box.left, e.clientY - box.top); if (n)
        select(n, true); }} onKeyDown={e => { if (['ArrowLeft', 'ArrowRight', 'ArrowUp', 'ArrowDown'].includes(e.key)) {
        e.preventDefault();
        updateCamera({ ...camera, x: camera.x + (e.key === 'ArrowLeft' ? 45 : e.key === 'ArrowRight' ? -45 : 0), y: camera.y + (e.key === 'ArrowUp' ? 45 : e.key === 'ArrowDown' ? -45 : 0) });
    }
    else if (e.key === '+' || e.key === '=') {
        e.preventDefault();
        zoom(1.3);
    }
    else if (e.key === '-') {
        e.preventDefault();
        zoom(1 / 1.3);
    }
    else if (e.key === 'Home') {
        e.preventDefault();
        fit();
    }
    else if (e.key === 'Enter') {
        e.preventDefault();
        fit(selected.id);
    }
    else if (e.key === 'Escape') {
        const n = nodeById.get(selected.parent ?? '')!;
        select(n, true);
    } }}/>
   {compact && <button className="atlas-compact-fit" onClick={() => fit()}>Fit map</button>}
   <div className="atlas-caption"><strong>{label}</strong><br />Frozen anchors · no force drift · no inferred symbol or call topology</div>
   {query && !exact && <div className="atlas-results" aria-label="Matching atlas paths"><button onClick={() => setQuery('')}>Close results</button><p>{results.length} matching paths</p>{results.slice(0, 30).map(n => <button key={n.id} onClick={() => { select(n, true); setQuery(''); }}>{n.path || '/'} · {n.files} files</button>)}{results.length > 30 && <button onClick={() => setExact(true)}>Browse all matches</button>}</div>}
   <aside ref={inspector} className="atlas-inspect" aria-label="Atlas selected source" tabIndex={0}><h3>{selected.path || 'Repository / tracedecay'}</h3><p>{selected.kind} · {selected.files.toLocaleString()} tracked {selected.files === 1 ? 'file' : 'files'}{selected.removed ? ' · baseline ghost' : ''}</p><p className="atlas-revision">Head map {short(atlasData.revision)}{selected.removed ? ' · baseline ghost' : ''}<br />{selected.bytes.toLocaleString()} blob bytes</p><button onClick={() => fit(selected.id)}>Zoom to selection</button>{selected.parent !== null && <button onClick={() => select(nodeById.get(selected.parent!)!, true)}>↑ Parent</button>}<button onClick={() => navigate(context === 'code' ? 'brain' : 'code', { path: selected.id, node: selected.id, lens: 'cortex' })}>Open same place in {context === 'code' ? 'Brain' : 'Code'}</button><p><a href={href} target="_blank" rel="noreferrer">Open source {short(sourceRevision)} ↗</a></p>{change && <p>Focused diff: {change.status}<br />{short(atlasData.baselineRevision)} → {short(atlasData.changeRevision)}</p>}
   {state.layer === 'dependencies' && <><p>{crate ? `${selectedEdges.length} declared incident edges · ${crate}` : 'Select a crate or its contents to reveal declared dependencies.'}</p>{selectedEdges.map((e, i) => <div className="atlas-dependency-row" key={i}><button onClick={() => select(nodeById.get(e.source === crate ? e.target : e.source)!, true)}>{e.source === crate ? '→' : '←'} {(e.source === crate ? e.target : e.source).replace('crates/tracedecay-', '')}<br />{e.scope}{e.optional ? ' · optional' : ''}</button><a href={`https://github.com/${atlasData.repository}/blob/${atlasData.revision}/${e.manifest}`} target="_blank" rel="noreferrer">Cargo source ↗</a></div>)}</>}
   {state.layer === 'churn' && <>
     <p><strong>{churnReading?.touches ?? 0} commit/file touches</strong> in this region.</p>
     <p className="atlas-revision">{atlasData.churn.start.replace('T',' ')}<br/>→ {atlasData.churn.end.replace('T',' ')}<br/>Pinned-head commit time · {atlasData.churn.days} days</p>
     <p>{atlasData.churn.commitCount} non-merge commits examined; {atlasData.churn.touchedFiles} current paths touched; {atlasData.churn.pathTouches} touches repository-wide.</p>
     <p>{atlasData.churn.basis}. {atlasData.churn.excludedHistoricalPaths} historical paths outside the current tree excluded.</p>
     {churnReading && <p><a href={`https://github.com/${atlasData.repository}/commit/${churnReading.lastCommit}`} target="_blank" rel="noreferrer">Latest counted touch {short(churnReading.lastCommit)} ↗</a><br/>{new Date(churnReading.lastTouchedAt*1000).toISOString()}</p>}
     <p>Most-touched current paths in this region:</p>
     {churnLeaders.map(file=><button key={file.path} onClick={()=>select(nodeById.get(file.path)!,true)}>{file.touches} touches · {file.path}</button>)}
   </>}
   {state.layer === 'duplicates' && <>
     <p><strong>{atlasData.duplicateFiles.groups.length} exact-blob groups</strong> across {atlasData.duplicateFiles.sourceFiles} source-code files; {duplicateScopeCount} matching paths in this region.</p>
     <p>{atlasData.duplicateFiles.basis}</p><details><summary>Source-file coverage</summary><p>{atlasData.duplicateFiles.sourceExtensions.join(", ")} · {atlasData.duplicateFiles.emptyFilesExcluded} empty files excluded</p></details>{duplicateScopeCount===0&&<p>No byte-identical candidates in this selected region. Groups below cover the repository.</p>}
     <label>Choose a byte-identical group<select aria-label="Byte-identical file group" value={selectedDuplicate?.blob ?? ''} onChange={event=>{setDuplicateId(event.target.value);const group=atlasData.duplicateFiles.groups.find(g=>g.blob===event.target.value);const node=group&&nodeById.get(group.paths[0]);if(node)select(node);}}><option value="">Select a group</option>{atlasData.duplicateFiles.groups.map(group=><option key={group.blob} value={group.blob}>{group.paths.length} paths · {group.bytes} bytes · {short(group.blob)}</option>)}</select></label>
     {!atlasData.duplicateFiles.groups.length && <p>No nonempty byte-identical source files in this scope.</p>}
     {selectedDuplicate && <><p>Blob {selectedDuplicate.blob}<br/>{selectedDuplicate.bytes} bytes per file</p><button onClick={fitDuplicateGroup}>Fit matching files</button>{selectedDuplicate.paths.map(path=><div key={path}><button onClick={()=>select(nodeById.get(path)!,true)}>Inspect matching path · {path}</button><a href={`https://github.com/${atlasData.repository}/blob/${atlasData.revision}/${path}`} target="_blank" rel="noreferrer">Pinned matching source ↗</a></div>)}</>}
   </>}
   {state.layer === 'cycles' && <p>{cycleGroups.length ? cycleGroups.map(g => g.join(' ↔ ')).join('; ') : 'No cycles in this declared non-dev path graph. This says nothing about symbol cycles or runtime reachability.'}</p>}
   {state.layer === 'changes' && <><p>Focused wrapper removal: {atlasData.changes.length} paths, {atlasData.changes.filter(c => c.status === 'removed').length} removed, {atlasData.changes.filter(c => c.status === 'relocated').length} relocated (Git -M). Not whole-PR churn.</p>{atlasData.changes.map(c => <button key={c.path} onClick={() => select(nodeById.get(c.path)!, true)}>{c.status} · {c.path.split('/').slice(-2).join('/')}</button>)}</>}
   {(state.layer === 'coverage' || state.layer === 'forwarding') && <p className="atlas-no-data">Measured: all tracked files (including tests, generated files and assets); {atlasData.coverage.packages} Cargo packages; {atlasData.edges.length} declared local dependency edges.<br /><br />Measured health layers: 7-day Git path touches and exact-blob duplicate-file candidates. Unmeasured: calls, runtime reachability, semantic duplication, complexity, review quality, and history outside that window. No empty success is implied.</p>}
   {!['comparison', 'dependencies', 'changes', 'coverage', 'forwarding', 'cycles', 'churn', 'duplicates'].includes(state.layer) && <p>Select a region, then Enter or double-click to enter. Directory boundaries stay in place across lenses.</p>}
   </aside>
	   <canvas className="atlas-minimap" ref={minimap} aria-label="Atlas minimap. Click to recenter." onClick={e => { const b = e.currentTarget.getBoundingClientRect(); const x = (e.clientX - b.left) / b.width * 1600, y = (e.clientY - b.top) / b.height * 1100; updateCamera({ ...camera, x: 14 + viewWidth / 2 - x * camera.scale, y: size.h / 2 - y * camera.scale }); }}/>
	   {state.layer === 'comparison' && <div className="atlas-comparison" aria-label="Focused structural comparison. Arrow keys move between sibling regions, Home selects the repository, and Escape selects the parent region." tabIndex={0} onKeyDown={event => {
	    if (event.key === 'ArrowRight' || event.key === 'ArrowDown') { moveSelection(1); event.preventDefault(); }
	    else if (event.key === 'ArrowLeft' || event.key === 'ArrowUp') { moveSelection(-1); event.preventDefault(); }
	    else if (event.key === 'Home') { select(nodeById.get('')!); event.preventDefault(); }
	    else if (event.key === 'Escape' && selected.parent !== null) { select(nodeById.get(selected.parent)!); event.preventDefault(); }
	   }}>
	    <header><div><h3>PINNED STRUCTURAL COMPARISON</h3><p>{selected.path || 'Repository'} · identical scope and region placement</p><p className="atlas-revision">Union layout from the current tree plus {atlasData.coverage.ghostFiles} baseline ghosts. Region positions and selection are shared. Arrow keys move selection across both panes.</p></div><strong>{atlasData.changes.length} recorded paths</strong></header>
	    <div className="atlas-comparison-panes">
	     {([['Before', atlasData.baselineRevision, true], ['After', atlasData.changeRevision, false]] as const).map(([name, revision, before]) => <article key={name}>
	      <h4>{name} · <a href={`https://github.com/${atlasData.repository}/tree/${revision}/${selected.path}`} target="_blank" rel="noreferrer">{short(revision)} ↗</a></h4>
	      <svg viewBox={`${comparisonScope.x} ${comparisonScope.y} ${Math.max(1, comparisonScope.w)} ${Math.max(1, comparisonScope.h)}`} role="img" aria-label={`${name} union-layout map of ${selected.path || 'repository'}`} preserveAspectRatio="xMidYMid meet">
	       {comparisonRects.map(rect => {
	        const changed = containsChange(rect.node.id);
	        const deleted = !!rect.node.removed;
	        return <g key={rect.node.id} className={`${changed ? 'changed' : ''} ${deleted ? 'deleted' : ''} ${rect.node.id === selected.id ? 'selected' : ''} ${!before && deleted ? 'absent' : ''}`} onClick={() => select(rect.node)}>
	         <rect x={rect.x + .5} y={rect.y + .5} width={Math.max(0, rect.w - 1)} height={Math.max(0, rect.h - 1)} />
	         {rect.w / comparisonScope.w > .16 && rect.h / comparisonScope.h > .06 && <text x={rect.x + 6} y={rect.y + 15}>{rect.node.name.replace(/^tracedecay-/, '')}</text>}
	        </g>;
	       })}
	      </svg>
	      <p>{before ? `${atlasData.forwarding.removed.length} named forwarding elements evidenced` : '0 named forwarding elements retained'} · {before ? `${atlasData.coverage.ghostFiles} later-deleted files shown` : `${atlasData.coverage.ghostFiles} deleted-file positions disclosed`}</p>
	     </article>)}
	    </div>
	    <table><caption>Focused comparison measures</caption><thead><tr><th>Measure</th><th>Before · {short(atlasData.baselineRevision)}</th><th>After · {short(atlasData.changeRevision)}</th></tr></thead><tbody>
	     <tr><th>Named forwarding elements</th><td>{atlasData.forwarding.removed.length}</td><td>0 retained</td></tr>
	     <tr><th>Recorded changed paths</th><td colSpan={2}>{atlasData.changes.length} shared diff records · {atlasData.changes.filter(c => c.status === 'removed').length} removed</td></tr>
	     <tr><th>Tracked files</th><td>Unavailable</td><td>Unavailable</td></tr>
	     <tr><th>Declared local dependency edges</th><td>Unavailable</td><td>Unavailable</td></tr>
	    </tbody></table>
	    <p className="atlas-revision">Layout reference only · current capture {short(atlasData.revision)} measures {atlasData.coverage.trackedFiles.toLocaleString()} tracked files and {atlasData.edges.length} declared local dependency edges; these counts do not belong to either pinned revision.</p>
	    <p>{atlasData.forwarding.basis}. This focused comparison does not claim semantic duplication, runtime reachability, or repository-wide structural change.</p>
	    <div className="atlas-comparison-sources">{atlasData.forwarding.sourcePaths.map(path => <button key={path} onClick={() => { const node = nodeById.get(path); if (node) select(node, true); }}>Inspect exact source · {path}</button>)}</div>
	   </div>}
	   {state.layer === 'forwarding' && <div className="atlas-flow"><h3>{atlasData.forwarding.title}</h3><p>{short(atlasData.baselineRevision)} → {short(atlasData.changeRevision)} · inspected source diff</p><div className="atlas-flow-chain"><article>{atlasData.forwarding.consumer}</article><span>↓</span>{state.comparison === 'before' && <><article className="removed">{atlasData.forwarding.removed.map((name,index)=><div key={name}>{index>0?"→ ":""}{name}</div>)}</article><span>↓</span></>}<article>{atlasData.forwarding.owner}</article></div><p>{state.comparison === 'before' ? 'Live forwarding layers, not unreachable code.' : 'Direct concrete ownership. The intentional store-runtime crate boundary remains.'}</p><p>{atlasData.forwarding.basis}</p>{atlasData.forwarding.sourcePaths.map(path=><button key={path} onClick={()=>{const node=nodeById.get(path);if(node)select(node,true);}}>Inspect {path.split("/").slice(-2).join("/")}</button>)}</div>}
   {exact && <div className="atlas-exact" role="dialog" aria-label="Exact tracked paths" aria-modal="false"><button onClick={() => setExact(false)}>Close exact paths</button><input aria-label="Filter exact paths" value={query} onChange={e => { setQuery(e.target.value); setPage(0); }}/><p>{results.length.toLocaleString()} paths · page {page + 1} / {Math.max(1, Math.ceil(results.length / 60))} · directories include two baseline ghost files</p><table><thead><tr><th>Path</th><th>Kind</th><th>Files</th><th>Git source</th></tr></thead><tbody>{results.slice(page * 60, (page + 1) * 60).map(n => <tr key={n.id}><td><button onClick={() => { select(n, true); setExact(false); setQuery(''); }}>{n.path || '/'}</button></td><td>{n.kind}{n.removed ? ' · removed' : ''}</td><td>{n.files}</td><td>{short(n.removed ? atlasData.baselineRevision : atlasData.revision)}</td></tr>)}</tbody></table><button disabled={!page} onClick={() => setPage(p => p - 1)}>Previous</button><button disabled={(page + 1) * 60 >= results.length} onClick={() => setPage(p => p + 1)}>Next</button></div>}
  </div>
  <div className="atlas-footer"><span className="atlas-controls-hint">Drag to pan · wheel / + − zoom · Enter enters · Escape ascends</span>{['changes', 'forwarding'].includes(state.layer) && <><button aria-pressed={state.comparison === 'before'} onClick={() => setState(s => ({ ...s, comparison: 'before' }))}>Before {short(atlasData.baselineRevision)}</button><button aria-pressed={state.comparison === 'after'} onClick={() => setState(s => ({ ...s, comparison: 'after' }))}>After {short(atlasData.changeRevision)}</button></>}<button onClick={() => zoom(1 / 1.3)} aria-label="Zoom out">−</button><output>{Math.round(camera.scale * 100)}%</output><button onClick={() => zoom(1.3)} aria-label="Zoom in">+</button><button onClick={() => fit()}>Fit repository</button></div>
 </section>;
}
