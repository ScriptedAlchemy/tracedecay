import { useEffect, useMemo, useRef, useState, type CSSProperties } from 'react';
import { Glyph, gradeColor, gradeDash } from './WeaveField';
import { BOUNDS, EVENT_BY_ID, GROUPS, inGroup, rootId, SESSIONS, SESSION_BY_ID, stamp, timeAxis, fiberPosition, branchLane, designLaneColor, WEAVE_COLORS, type JourneyNode, type ZoomLevel, type Workspace } from './journey';
import type { EvidenceGrade } from './types';
import { OutcomeSummary } from './OutcomeSummary';
import { SOURCE } from './source';
import { shortId } from '../data/pack';

type Props = {
  sessionIds?: string[]; grades: EvidenceGrade[];
  from: number; to: number; cursor: number; cutoff: number | null;
  level: ZoomLevel; focus: string | null; expanded: string[]; nodes: JourneyNode[];
  selected: string | null; workspace: Workspace; project: string | null; provider: string | null;
  onSelect: (id: string) => void; onSession: (id: string) => void;
  onProject: (name: string, provider: string | null) => void; onSeek: (t: number) => void;
  onWindow: (from: number, to: number) => void;
};

type Point = { key: string; group?: string; node: JourneyNode; x: number; y: number; color: string; label: boolean };
export function JourneyField(p: Props) {
  const compact = p.workspace !== 'weave';
  const investigation = !!SOURCE.design && (p.workspace === 'feedback' || p.workspace === 'gaps');
  const ref = useRef<HTMLDivElement>(null), canvas = useRef<HTMLCanvasElement>(null);
  const [size, setSize] = useState({ w: 1000, h: 460 });
  const [highlightGroup, setHighlightGroup] = useState<string | null>(SOURCE.design ? 'design:dashboard' : null);
  const [portKind, setPortKind] = useState<'start'|'tail'|null>(null);
  const portPopover = useRef<HTMLDivElement>(null);
  const [previewKey, setPreviewKey] = useState<string | null>(null);
  const [clusterKey, setClusterKey] = useState<string | null>(null);
  const clusterPopover = useRef<HTMLDivElement>(null);
  const drag = useRef<{ x: number; moved: boolean } | null>(null);
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const observer = new ResizeObserver(([entry]) => setSize({ w: entry.contentRect.width, h: entry.contentRect.height }));
    observer.observe(el);
    return () => observer.disconnect();
  }, []);
  const temporalAxis = useMemo(() => timeAxis(p.from, p.to), [p.from, p.to]);
  const { w, h } = size;
  const dense = p.level === 'workstream' && !compact;
  const spineId = dense && SOURCE.design ? EVENT_BY_ID.get(SOURCE.design.selectedEventId)?.sessionId : null;
  const episodes = !compact && p.focus ? SOURCE.design?.episodes.filter(episode=>episode.eventIds.every(id=>{const event=EVENT_BY_ID.get(id);return event?.sessionId===p.focus && event.ts!==null && event.ts < p.from;})) ?? [] : [];
  const episodeContext = episodes.length > 0 && w > 600;
  const left = investigation ? 72 : compact ? 24 : dense ? 90 : episodeContext ? 190 : Math.min(180, w * .15), right = compact && p.workspace === 'evidence' && w>900 ? 208 : investigation && p.workspace === 'gaps' ? 200 : investigation ? 60 : dense ? 80 : 34, top = investigation ? 42 : compact ? 22 : dense ? 22 : episodeContext ? 32 : 52, bottom = dense ? 12 : compact ? 24 : episodeContext ? 76 : 96;
  const width = Math.max(1, w - left - right), height = Math.max(1, h - top - bottom);
  let overview = (!compact || !p.selected) && (p.level === 'outcome' || p.level === 'workstream');
  const allowed = p.sessionIds ? new Set(p.sessionIds) : null;
  const layoutSessions = SESSIONS.filter(s => (!allowed || allowed.has(s.id) || (compact && s.id === EVENT_BY_ID.get(p.selected ?? '')?.sessionId)) && s.startedTs !== null && s.startedTs <= p.to && (s.endedTs ?? p.to) >= p.from);
  const visibleSessions = layoutSessions.filter(s => p.cutoff === null || s.startedTs! <= p.cutoff);
  const labelPosition = (index: number) => SOURCE.design && dense && groups.length===6 ? [.06,.23,.39,.64,.76,.91][index] : (index+.5)/groups.length;
  const groups = GROUPS.flatMap<{ id: string; name: string; project: string; provider: string | null; color: string; members: typeof SESSIONS }>(group => {
    const members = layoutSessions.filter(s => group.sessions.some(member => member.id === s.id));
    if (!members.length) return [];
    const providers = [...new Set(members.map(s => s.provider))].sort();
    if (GROUPS.length === 1 && p.level !== 'outcome' && providers.length > 1) return providers.map((provider, i) => ({
      id: `${group.id}:${provider}`, name: provider, project: group.name, provider,
      color: ['#59bded', '#68d5cb', '#d5ac64'][i % 3], members: members.filter(s => s.provider === provider),
    }));
    return [{ ...group, project: group.name, provider: null, members }];
  });
  const focused = new Set(p.expanded);
  if (p.focus) focused.add(p.focus);
  const continuationSessions = new Map<string, number>();
  const continuationEvents = new Set<string>();
  const selectedEvent = p.selected ? EVENT_BY_ID.get(p.selected) : null;
  if (compact && selectedEvent) {
    focused.clear(); focused.add(selectedEvent.sessionId);
    const parent = SESSION_BY_ID.get(selectedEvent.sessionId)?.parentId;
    if (parent) focused.add(parent);
    for (const relation of SOURCE.design?.relations ?? []) {
      if (relation.from !== selectedEvent.id && relation.to !== selectedEvent.id) continue;
      const neighbor = EVENT_BY_ID.get(relation.from === selectedEvent.id ? relation.to : relation.from);
      if (neighbor && (p.cutoff === null || (neighbor.ts !== null && neighbor.ts <= p.cutoff))) focused.add(neighbor.sessionId);
    }
    if (p.workspace === 'evidence' && SOURCE.design) {
      const workstream = SOURCE.design.groups.find(group => group.name === SOURCE.design?.details[selectedEvent.id]?.task);
      for (const id of workstream?.sessionIds ?? []) {
        if (focused.size >= 5) break;
        focused.add(id);
      }
    }
  }
  const feedback = p.workspace === 'feedback' && selectedEvent ? SOURCE.design?.feedback.find(note => note.targetEventId === selectedEvent.id || note.lifecycle.some(step => step.eventId === selectedEvent.id)) : null;
  const storyIds = new Set<string>();
  if (feedback) {
    storyIds.add(feedback.targetEventId);
    for (const relation of SOURCE.design?.relations ?? []) {
      if (relation.to === feedback.targetEventId && EVENT_BY_ID.get(relation.from)?.kind === 'decision' && ['exact', 'explicit'].includes(relation.grade)) storyIds.add(relation.from);
    }
    for (const step of feedback.lifecycle) {
      storyIds.add(step.eventId);
      if (step.eventId !== feedback.sourceEventId) continuationEvents.add(step.eventId);
      const event = EVENT_BY_ID.get(step.eventId);
      if (event?.ts == null || event.sessionId === selectedEvent?.sessionId) continue;
      focused.add(event.sessionId);
      continuationSessions.set(event.sessionId, Math.min(continuationSessions.get(event.sessionId) ?? Infinity, event.ts));
    }
  }
  if (investigation && p.workspace === 'gaps' && selectedEvent) {
    for (const candidate of SOURCE.design?.details[selectedEvent.id]?.candidates ?? []) focused.add(candidate.sessionId);
  }
  const layoutDetail = layoutSessions.filter(s => (compact || inGroup(s.id, p.project)) && (compact || !p.provider || s.provider === p.provider) && (!focused.size || focused.has(s.id)));
  const detailSessions = layoutDetail.filter(s => p.cutoff === null || (s.startedTs! <= p.cutoff && (continuationSessions.get(s.id) ?? -Infinity) <= p.cutoff));
  const aggregated = (!compact || !p.selected) && !overview && layoutDetail.length > Math.max(6, Math.floor(height / 48));
  overview ||= aggregated;
  const layoutIds = new Set(layoutDetail.map(session => session.id));
  const axis = temporalAxis;
  const gapCallouts = investigation && p.workspace === 'gaps' ? SOURCE.events.filter(event => layoutIds.has(event.sessionId) && event.ts !== null && event.ts >= p.from && event.ts <= p.to && event.kind === 'gap' && !SOURCE.design?.coverageGaps.some(gap => gap.eventId === event.id)) : [];
  const calloutY = (id: string) => 42 + Math.max(0,gapCallouts.findIndex(event => event.id === id)) * Math.min(46, (h-70) / Math.max(1,gapCallouts.length-1));
  const x = (t: number) => left + axis.position(t) * width;
  const detail = overview ? visibleSessions : detailSessions;
  const row = new Map(layoutDetail.map((s, i) => [s.id, top + height * (i + 1) / (layoutDetail.length + 1)]));
  if (investigation && p.workspace === 'gaps' && selectedEvent) {
    row.set(selectedEvent.sessionId, top + height * .58);
    layoutDetail.filter(session => session.id !== selectedEvent.sessionId).forEach((session, i, others) => row.set(session.id, top + height * .2 * (i + 1) / others.length));
  }
  if (SOURCE.design && !compact && !investigation && p.focus && layoutDetail.length <= 6) {
    row.set(p.focus, top + height * branchLane(-1,episodeContext));
    const children = layoutDetail.filter(s => s.id !== p.focus);
    children.forEach((s, i) => row.set(s.id, top + height * branchLane(i,episodeContext)));
  }
  if (SOURCE.design && compact && p.workspace === 'evidence' && selectedEvent) {
    row.set(selectedEvent.sessionId,top+height/2);
    layoutDetail.filter(session=>session.id!==selectedEvent.sessionId).forEach((session,index)=>row.set(session.id,top+height*([.08,.28,.72,.92][index] ?? .5)));
  }
  const detailIds = new Set(detail.map(s => s.id));
  const collapsed = new Map<string, typeof SESSIONS>();
  if (p.level === 'agent' && !overview && !compact && p.focus) {
    for (const session of visibleSessions) {
      if (detailIds.has(session.id)) continue;
      const root = rootId(session.id), members = collapsed.get(root) ?? [];
      members.push(session); collapsed.set(root, members);
    }
  }
  const collapsedEntries = [...collapsed].sort(([a], [b]) => Number(b === p.focus) - Number(a === p.focus)).slice(0, 2);
  const revealedIds = new Set(p.nodes.flatMap(node => node.events.map(event => event.id)));
  const geometry = new Map<string, { center: number; offset: number; color: string; phase: number }>();
  groups.forEach((group, gi) => {
    const members = group.members;
    const band = height / groups.length;
    members.forEach((s, i) => geometry.set(`${group.id}/${s.id}`, {
      center: top + (gi + .5) * band,
      offset: ((i + .5) / members.length - .5) * band * .86,
      color: group.color, phase: i * 2.399,
    }));
  });
  const primaryGeometry = new Map<string, { center: number; offset: number; color: string; phase: number }>();
  for (const [key, lane] of geometry) { const id = key.slice(key.indexOf('/') + 1); if (!primaryGeometry.has(id)) primaryGeometry.set(id, lane); }
  const firstLane = (id: string) => primaryGeometry.get(id);
  const colorOf = (id: string) => !overview && SOURCE.design ? designLaneColor(id) : firstLane(id)?.color ?? '#8096a8';
  const y = (id: string, u: number, groupId?: string) => {
    if (!overview) return row.get(id) ?? top + height / 2;
    const lane = groupId ? geometry.get(`${groupId}/${id}`) : firstLane(id);
    if (!lane) return top + height / 2;
    if (spineId === id) return top+height*.515;
    const time=p.from+u*(p.to-p.from);
    return top+height*fiberPosition(time,(lane.center+lane.offset-top)/height,(lane.center-top)/height,SOURCE.design?.windows.dense ?? BOUNDS);
  };
  const lastObservedBySession = new Map<string,number>();
  for (const node of p.nodes) for (const event of node.events) if (event.ts !== null) lastObservedBySession.set(event.sessionId,Math.max(lastObservedBySession.get(event.sessionId) ?? -Infinity,event.ts));
  const threads = overview ? groups.flatMap(group => group.members.filter(s => detailIds.has(s.id)).map(session => ({session, group}))) : detail.map(session => ({session, group: null}));
  const points: Point[] = [];
  const failed = (node: JourneyNode) => node.events.some(event => SOURCE.design?.details[event.id]?.outcome === 'failed');
  for (const node of p.nodes) {
    if (node.time < p.from || node.time > p.to || !detailIds.has(node.session)) continue;
    if (feedback && !node.events.some(event => storyIds.has(event.id))) continue;
    const nx = x(node.time), ny = feedback ? top + height * (node.events.some(event => continuationEvents.has(event.id)) ? .84 : .12) : y(node.session, axis.position(node.time));
    const label = !overview && !compact;
    if (overview) {
      for (const group of groups.filter(g => g.members.some(s => s.id === node.session))) {
        points.push({ key: `${group.id}/${node.id}`, group: group.name, node, x: nx, y: y(node.session, axis.position(node.time), group.id), color: failed(node) ? '#ed735a' : group.color, label: false });
      }
    } else points.push({ key: node.id, node, x: nx, y: ny, color: failed(node) ? '#ed735a' : colorOf(node.session), label });
  }
  // Callouts use readable slots; leaders retain exact recorded-time anchors.
  const storyCardX = (point: Point) => {
    const response = continuationEvents.has(point.node.id);
    const events = SOURCE.events.filter(event => storyIds.has(event.id) && continuationEvents.has(event.id) === response).sort((a,b)=>a.ts!-b.ts!);
    return left + width * (events.findIndex(event=>event.id===point.node.id)+.5)/Math.max(1,events.length);
  };
  const preview = points.find(point=>point.key === previewKey || (!previewKey && point.node.id === p.selected));
  const denseEvents=SOURCE.design ? SOURCE.events.filter(event=>event.ts!==null && event.ts>=SOURCE.design!.windows.dense[0] && event.ts<=SOURCE.design!.windows.dense[1]) : [];
  const startTime=Math.min(...denseEvents.filter(event=>event.kind==='session').map(event=>event.ts!));
  const tailTime=Math.max(...denseEvents.filter(event=>event.kind==='message').map(event=>event.ts!));
  const portRecords=(kind:'start'|'tail')=>p.nodes.flatMap(node=>node.events).filter(event=>detailIds.has(event.sessionId) && event.ts===(kind==='start'?startTime:tailTime) && event.kind===(kind==='start'?'session':'message'));
  const ports=dense && SOURCE.design ? (['start','tail'] as const).map(kind=>({kind,time:kind==='start'?startTime:tailTime,records:portRecords(kind)})).filter(port=>port.records.length && port.time>=p.from && port.time<=p.to) : [];
  const highRisk = (point: Point) => point.node.events.some(event => SOURCE.design?.details[event.id]?.risk === 'high');
  const detailGlyphs = new Map<string, Point[]>();
  if (!overview && !compact) {
    const anchors: Point[] = [];
    for (const point of [...points.filter(point => point.node.id === p.selected), ...points.filter(point => point.node.id !== p.selected)]) {
      const anchor = anchors.find(other => other.node.session === point.node.session && Math.abs(other.x - point.x) < 26);
      if (anchor) detailGlyphs.get(anchor.key)!.push(point);
      else { anchors.push(point); detailGlyphs.set(point.key, [point]); }
    }
  }
  const cluster = clusterKey ? detailGlyphs.get(clusterKey) ?? [] : [];
  const compactGlyphs = new Set<string>();
  if (compact && p.workspace !== 'evidence') {
    const occupied = points.filter(point => point.node.id === p.selected);
    const priority = (point: Point) => point.node.events.some(event => continuationEvents.has(event.id));
    const ordered = [...points.filter(priority), ...points.filter(point => !priority(point))];
    if (investigation) ordered.sort((a,b) => Number(b.node.kind === 'gap') - Number(a.node.kind === 'gap'));
    for (const point of ordered) {
      if (!feedback && !(investigation && point.node.kind === 'gap') && occupied.some(other => investigation ? Math.abs(other.x - point.x) < 52 && Math.abs(other.y - point.y) < 28 : Math.hypot(other.x - point.x, other.y - point.y) < 28)) continue;
      occupied.push(point); compactGlyphs.add(point.key);
    }
  }
  const denseGlyphs = new Set<string>();
  if (overview && SOURCE.design) {
    const occupied: Point[] = [];
    const lanes = new Map<string, Point[]>();
    for (const point of points) {
      if (['session', 'message'].includes(point.node.kind)) continue;
      const key = point.key.slice(0, point.key.lastIndexOf('/')) + point.node.session;
      const candidates = lanes.get(key) ?? []; candidates.push(point); lanes.set(key, candidates);
    }
    for (const [id, candidates] of lanes) {
      const hash = [...id].reduce((value, char) => (value * 31 + char.charCodeAt(0)) >>> 0, 0);
      const choices = dense ? candidates.filter(point => axis.position(point.node.time) > .26) : candidates;
      if (!choices.length) continue;
      const point = choices[hash % choices.length];
      if (occupied.some(other => Math.hypot(other.x - point.x, other.y - point.y) < 25)) continue;
      occupied.push(point); denseGlyphs.add(point.key);
    }
  }
  useEffect(() => {
    const el = canvas.current;
    if (!el) return;
    const ctx = el.getContext('2d');
    if (!ctx) return;
    const dpr = Math.min(2, window.devicePixelRatio || 1);
    el.width = Math.max(1, Math.round(w * dpr)); el.height = Math.max(1, Math.round(h * dpr));
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, w, h);
    ctx.save(); ctx.beginPath(); ctx.rect(dense ? 0 : left, top - 14, dense ? w : width, height + 26); ctx.clip();
    const path = (draw: () => void, color: string, opacity: number, grade: EvidenceGrade = 'exact') => {
      ctx.setLineDash(gradeDash(grade)?.split(' ').map(Number) ?? []);
      if (overview) {
        ctx.globalCompositeOperation='lighter';
        ctx.strokeStyle=color; ctx.globalAlpha=opacity * .065; ctx.lineWidth=3; ctx.shadowColor=color; ctx.shadowBlur=16;
        ctx.beginPath(); draw(); ctx.stroke();
        ctx.globalCompositeOperation='source-over';
      }
      ctx.strokeStyle = color; ctx.globalAlpha = opacity * .2; ctx.lineWidth = overview ? 1.5 : 3;
      ctx.shadowColor = color; ctx.shadowBlur = overview ? 4 : 8; ctx.beginPath(); draw(); ctx.stroke();
      ctx.shadowBlur = 0; ctx.globalAlpha = opacity; ctx.lineWidth = overview ? color === '#b9efff' ? 1.35 : .6 : .75; ctx.beginPath(); draw(); ctx.stroke();
    }
    for (const { session: s, group } of threads) {
      const lastObserved = lastObservedBySession.get(s.id) ?? s.startedTs!;
      const ended = s.endedTs !== null && (p.cutoff === null || s.endedTs <= p.cutoff);
      const start = Math.max(p.from, s.startedTs!), end = Math.min(p.to, p.cutoff ?? Infinity, s.endedTs ?? lastObserved);
      const x0 = x(start), x1 = x(end);
      const groupIndex = group ? GROUPS.findIndex(item => item.id === group.id) : -1;
      const col = s.id === spineId ? '#b9efff' : dense && SOURCE.design && groupIndex >= 0 ? WEAVE_COLORS[groupIndex % WEAVE_COLORS.length] : group?.color ?? colorOf(s.id), active = inGroup(s.id, p.project);
      const alpha = s.id === spineId ? 1 : p.focus && s.id !== p.focus && !focused.has(s.id) ? .28 : active ? overview ? .25 + (Math.sin(firstLane(s.id)?.phase ?? 0) + 1) * .13 : .8 : .14;
      if ((!SOURCE.design || overview) && x1>=x0) {
        const drawStrand = () => {
        for (let k = 0; k <= 70; k++) {
          const px = x0 + (x1 - x0) * k / 70, py = y(s.id, (px - left) / width, group?.id);
          if (!k) ctx.moveTo(px, py); else ctx.lineTo(px, py);
        }
        };
        path(drawStrand, ended ? col : '#597180', ended ? alpha : alpha * .28, ended ? 'exact' : 'unavailable');

      }
      if (!SOURCE.design && s.parentId && row.has(s.parentId) && !overview) {
        const sx = x(s.startedTs!), sy = y(s.parentId, axis.position(s.startedTs!)), ey = y(s.id, axis.position(s.startedTs!));
        // Parent identity is recorded. Timing is the child's recorded start, not an invented spawn hook.
        path(() => { ctx.moveTo(sx - 20, sy); ctx.bezierCurveTo(sx + 12, sy, sx - 12, ey, sx + 20, ey); }, col, .7, 'unavailable');
      }
      for (const t of feedback || overview ? [] : [s.startedTs, s.endedTs]) {
        if (t === null || t < p.from || t > p.to || (p.cutoff !== null && t > p.cutoff)) continue;
        ctx.globalAlpha = alpha; ctx.strokeStyle = col; ctx.fillStyle = '#03080c'; ctx.lineWidth = .8;
        ctx.beginPath(); ctx.arc(x(t), y(s.id, axis.position(t), group?.id), overview ? 1 : 4, 0, Math.PI * 2); ctx.fill(); ctx.stroke();
      }
    }
    if (SOURCE.design) {
      const byId = new Map(points.flatMap(point => point.node.events.map(event => [event.id, point] as const)));
      for (const relation of SOURCE.design.relations) {
        if (p.grades.length && !p.grades.includes(relation.grade)) continue;
        const a = byId.get(relation.from), b = byId.get(relation.to);
        if (!a || !b || a === b || (overview && a.node.session === b.node.session)) continue;
        const color = gradeColor(relation.grade, b.color);
        const bend = Math.max(18, (b.x - a.x) * .5);
        path(() => { ctx.moveTo(a.x, a.y); if (a.y!==b.y && (relation.kind==='handoff' || relation.kind==='rejoin') && b.x-a.x>60) { const turn=b.x-48; ctx.lineTo(turn,a.y); ctx.bezierCurveTo(turn+30,a.y,b.x-25,b.y,b.x,b.y); } else ctx.bezierCurveTo(a.x + bend, a.y, b.x - bend, b.y, b.x, b.y); }, color, .9, relation.grade);
      }
    }
    for (const point of points) {
      const selected = point.node.id === p.selected;
      ctx.globalAlpha = selected ? 1 : .85; ctx.fillStyle = point.color;
      ctx.shadowColor = point.color; ctx.shadowBlur = selected ? 10 : 5;
      ctx.beginPath(); ctx.arc(point.x, point.y, selected ? 2.6 : overview ? ['session','message'].includes(point.node.kind) ? .42 : 1.25 : 1.1, 0, Math.PI * 2); ctx.fill();
      if (dense) { ctx.shadowBlur = 0; ctx.fillStyle = '#d0eff6'; ctx.globalAlpha = .75; ctx.beginPath(); ctx.arc(point.x, point.y, .45, 0, Math.PI * 2); ctx.fill(); }
    }
    ctx.restore();
  });
  function pick(clientX: number, clientY: number) {
    const box = ref.current?.getBoundingClientRect();
    if (!box) return null;
    const px = clientX - box.left, py = clientY - box.top;
    let nearest: { id: string; event: boolean; distance: number } | null = null;
    for (const point of points) {
      const distance = Math.hypot(point.x - px, point.y - py);
      if (distance < 9 && (!nearest || distance < nearest.distance)) nearest = { id: point.node.id, event: true, distance };
    }
    if (nearest) return nearest;
    for (const session of detail) {
      const start = x(Math.max(p.from, session.startedTs!));
      const end = x(Math.min(p.to, p.cutoff ?? Infinity, session.endedTs ?? lastObservedBySession.get(session.id) ?? session.startedTs!));
      if (px < start - 4 || px > end + 4) continue;
      const distance = Math.abs(y(session.id, (px - left) / width) - py);
      if (distance < 5 && (!nearest || distance < nearest.distance)) nearest = { id: session.id, event: false, distance };
    }
    return nearest;
  }
  const tickCount = Math.max(2, Math.floor(width / 130));
  return <div ref={ref} className={`journey-field ${w < 800 && !compact ? 'is-condensed' : ''}`} data-level={p.level} style={{'--caption-font':w < 900 ? '7.5px' : '9px'} as CSSProperties}
    onWheel={e => {
      const box = e.currentTarget.getBoundingClientRect();
      const anchor = axis.time((e.clientX - box.left - left) / width);
      const factor = e.deltaY > 0 ? 1.2 : .8;
      p.onWindow(anchor - (anchor - p.from) * factor, anchor + (p.to - anchor) * factor);
    }}
    onPointerDown={e => { if ((e.target as HTMLElement).closest('button')) return; drag.current = { x: e.clientX, moved: false }; e.currentTarget.setPointerCapture(e.pointerId); }}
    onPointerMove={e => {
      if (!drag.current || Math.abs(e.clientX - drag.current.x) < 5) return;
      const box = e.currentTarget.getBoundingClientRect();
      const delta = axis.time((drag.current.x - box.left - left) / width) - axis.time((e.clientX - box.left - left) / width);
      drag.current = { x: e.clientX, moved: true }; p.onWindow(p.from + delta, p.to + delta);
    }}
    onPointerUp={e => {
      if (drag.current && !drag.current.moved) {
        const hit = pick(e.clientX, e.clientY);
        if (hit) { if (hit.event) p.onSelect(hit.id); else p.onSession(hit.id); }
        else { const box = e.currentTarget.getBoundingClientRect(); p.onSeek(axis.time((e.clientX - box.left - left) / width)); }
      }
      drag.current = null;
    }}>
    <canvas ref={canvas} aria-hidden="true" />
    {p.level === 'outcome' && !compact ? <OutcomeSummary grades={p.grades} nodes={p.nodes.filter(node => node.time >= p.from && node.time <= p.to)} sessionIds={new Set(visibleSessions.map(session => session.id))} onGroup={p.onProject} onSelect={p.onSelect} /> : null}
    {ports.map(port=><button key={port.kind} type="button" className={`journey-boundary-port ${port.records.some(event=>event.id===p.selected)?'selected':''}`} data-boundary={port.kind} data-time={port.time} style={{left:x(port.time),top:top+height*.515}} popoverTarget="loom-boundary-records" aria-label={`Inspect ${port.records.length} ${port.kind==='start'?'session start':'loaded tail'} records at ${stamp(port.time)}`} onClick={()=>setPortKind(port.kind)}><span><b>{port.records.length} {port.kind==='start'?'START RECORDS':'TAIL RECORDS'}</b><small>{stamp(port.time)} UTC</small></span><i aria-hidden="true">⋮</i></button>)}
    <div className="journey-cluster journey-boundary-records" id="loom-boundary-records" popover="auto" ref={portPopover} aria-label="Co-timed source records"><h3>CO-TIMED {portKind==='start'?'SESSION START':'LOADED TAIL'} RECORDS</h3><p>This is a time aggregate, not evidence of a shared parent or a rejoin. Each row opens its own source record.</p>{portKind ? portRecords(portKind).map(event=><button type="button" key={event.id} aria-current={event.id===p.selected?'true':undefined} onClick={()=>{portPopover.current?.hidePopover();p.onSelect(event.id);}}>{SESSION_BY_ID.get(event.sessionId)?.agentId ?? event.sessionId}<small>{stamp(event.ts!)} · {event.id}</small></button>):null}</div>
    {spineId ? <span className="journey-spine-label" style={{top:top+height*.515+9,left:left+5}}>SOURCE STRAND · {SESSION_BY_ID.get(spineId)?.agentId}</span> : null}
    {aggregated ? <span className="journey-density-note">{detailSessions.length} sessions bundled · select a session in the navigator to expand</span> : null}
    <svg width={w} height={h} className="journey-ruler" aria-hidden="true">
      <defs><pattern id="loom-capture-hatch" width="7" height="7" patternUnits="userSpaceOnUse"><path d="M-1 1L1-1M0 7L7 0M6 8L8 6" stroke="#9e78b7" strokeWidth=".5" opacity=".25" /></pattern></defs>
      {!overview && !feedback ? SOURCE.design?.coverageGaps.filter(gap => detailIds.has(gap.sessionId) && gap.to >= p.from && gap.from <= p.to && (p.cutoff === null || gap.to <= p.cutoff)).map(gap => {
        const gx=x(Math.max(p.from,gap.from)), end=x(Math.min(p.to,gap.to)), cy=row.get(gap.sessionId) ?? top;
        return <g key={gap.eventId}><rect x={gx} y={Math.max(32,cy-28)} width={Math.max(0,end-gx)} height={investigation ? 70 : compact ? 62 : 56} fill="url(#loom-capture-hatch)" stroke="#a580c0" strokeDasharray="3 3" strokeWidth=".7" /><text x={gx+5} y={compact ? Math.min(h-12,cy+30) : Math.max(43,cy-34)} style={{fill:'#b493c9',fontSize:8}}>{end-gx>175 ? gap.label : 'TRANSCRIPT GAP'}</text></g>;
      }) : null}
      {feedback ? <>
        <text x="8" y={top + height * .12 - 23}>ARTIFACT · CALLOUTS LINK TO RECORDED TIME</text>
        {points.map(point=><line key={point.key} x1={point.x} y1={point.y} x2={storyCardX(point)} y2={point.y} stroke={point.color} strokeWidth=".7" strokeDasharray="2 3"/>)}
        {[...continuationEvents].some(id => revealedIds.has(id)) ? <text x="8" y={top + height * .84 - 8}>RESPONSE →</text> : null}
      </> : investigation ? detail.map(session => <g key={session.id}>
        <line x1={left} x2={w-right} y1={row.get(session.id)} y2={row.get(session.id)} stroke="#263342" strokeWidth=".5" />
        <text x="5" y={(row.get(session.id) ?? top)-10}>{session.agentId?.replace('design:', '') ?? shortId(session.id, 10)}</text>
        {SOURCE.design?.details[selectedEvent?.id ?? '']?.candidates?.some(candidate => candidate.sessionId === session.id) ? <text x="5" y={(row.get(session.id) ?? top)+3} style={{fontSize:7,fill:'#c5a76d'}}>CANDIDATE</text> : null}
      </g>) : null}
      {gapCallouts.filter(event => revealedIds.has(event.id)).map(event => {
        const point = points.find(point => point.node.events.some(source => source.id === event.id));
        return point ? <path key={event.id} d={`M${point.x} ${point.y}C${w-right+15} ${point.y} ${w-right+5} ${calloutY(event.id)} ${w-178} ${calloutY(event.id)}`} fill="none" stroke="#708696" strokeWidth=".6" strokeDasharray="2 4" opacity=".45" /> : null;
      })}
      {Array.from({ length: tickCount + 1 }, (_, i) => {
        const t = axis.time(i / tickCount), tx = x(t);
        return <g key={i}><line x1={tx} x2={tx} y1="25" y2="30" stroke="#4a555d" /><text x={tx} y="17" textAnchor="middle">{stamp(t, p.to - p.from > 86400)}</text></g>;
      })}
      {p.cutoff !== null && p.cursor >= p.from && p.cursor <= p.to ? <>
        <rect x={x(p.cursor)} y="32" width={Math.max(0, w - right - x(p.cursor))} height={h - 64} fill="#101a21" opacity=".32" />
        <line x1={x(p.cursor)} x2={x(p.cursor)} y1="30" y2={h - 23} stroke="#6bdaf4" />
        {w - right - x(p.cursor) > 160 ? <text x={x(p.cursor) + 20} y="49">FUTURE / UNREVEALED</text> : null}
      </> : null}
    </svg>
    {!compact && overview ? groups.map((group, i) => group.members.some(s => p.cutoff === null || s.startedTs! <= p.cutoff) ? <button type="button" key={group.id} className={`journey-group ${dense && highlightGroup===group.id ? 'highlighted' : ''}`} onPointerEnter={()=>setHighlightGroup(group.id)} onFocus={()=>setHighlightGroup(group.id)} style={{ top: top + height * labelPosition(i) - 18, left: dense ? left+43 : 8, width: dense ? width*.28-43 : undefined, color: group.color }} onClick={() => p.onProject(group.project, group.provider)}>
      <span>{group.name}</span><small>{group.members.filter(s => p.cutoff === null || s.startedTs! <= p.cutoff).length} {SOURCE.design ? 'participations' : 'session records'}</small>
    </button> : null) : null}
    {!compact && !overview && !episodeContext ? detail.filter(s => !collapsed.size || s.id === p.focus).map(s => <button type="button" key={s.id} className={`journey-group ${p.focus === s.id ? 'selected' : ''}`} style={{ top: (row.get(s.id) ?? top) - 15, width:left-22, color: colorOf(s.id) }} onClick={() => p.onSession(s.id)}>
      {s.title ? s.title : shortId(s.agentId ?? s.id, 15)}<small>{s.isSubagent ? 'SUBAGENT' : s.provider} · {SOURCE.design ? `${p.nodes.filter(node => node.session === s.id).reduce((count, node) => count + node.events.length, 0)} illustrative events` : p.cutoff === null ? s.messages === null ? 'count unavailable' : `${s.messages} messages` : `${p.nodes.filter(n => n.session === s.id).reduce((sum, n) => sum + n.events.length, 0)} revealed events`}</small>
    </button>) : null}
    {episodeContext ? <div className="journey-episodes" style={{top:(row.get(p.focus!) ?? top)-92,width:left-12}}>
      <svg className="journey-episode-spine" viewBox={`0 0 ${left} 180`} aria-hidden="true"><path d={`M0 92H${left}`} stroke="#54d6f0" fill="none"/></svg>
      {episodes.map((episode,index)=>{
        const records=episode.eventIds.flatMap(id=>{const event=EVENT_BY_ID.get(id);return event?.ts!==null && event?.ts!==undefined && (p.cutoff===null || event.ts<=p.cutoff)?[event]:[];});
        if (!records.length) return null;
        const tint=index===1?'#dba955':'#52cee9';
        return <button type="button" key={episode.id} className="journey-episode" style={{color:tint}} aria-label={`Expand episode ${episode.title}, ${records.length} revealed events`} onClick={()=>p.onWindow(episode.at-30,Math.max(...records.map(event=>event.ts!))+30)}>
          <b>EP. {episode.id.split(':').at(-1)}</b><span>{episode.title}</span><small>{stamp(episode.at).slice(0,5)}</small>
          <svg viewBox="0 0 56 150" aria-hidden="true">{records.map((event,i)=><g key={event.id}><path d={`M0 60C${10+i*2} ${8+i*9} ${43-i*2} ${110-i*8} 56 60`} fill="none" stroke="currentColor" strokeWidth=".55" opacity=".45"/><path d={`M${12+i*5} 8V126`} stroke="currentColor" strokeWidth=".4" strokeDasharray="1 7" opacity=".5"/><circle cx={10+i*7} cy={48+(i%3)*12} r="1.5" fill="currentColor"/></g>)}</svg>
          <em>{records.length} events</em>
        </button>;
      })}
    </div> : null}
    {collapsedEntries.map(([root, members], i) => {
      const ids = new Set(members.map(session => session.id));
      const nodes = p.nodes.filter(node => ids.has(node.session) && node.time >= p.from && node.time <= p.to);
      const nodeIds = new Set(nodes.flatMap(node => node.events.map(event => event.id)));
      const relationGrades = SOURCE.design?.relations.filter(relation => revealedIds.has(relation.from) && revealedIds.has(relation.to) && (nodeIds.has(relation.from) || nodeIds.has(relation.to))).map(relation => relation.grade) ?? [];
      const grades = [...new Set([...nodes.map(node => node.grade), ...relationGrades])].join(' / ');
      return <button type="button" key={root} className="journey-collapsed" style={{top:top + height * (collapsedEntries.length === 1 && root === p.focus ? .73 : i ? .73 : .02)}} onClick={() => p.onSession(root)}>
        <b>{root === p.focus ? 'Collapsed children' : SESSION_BY_ID.get(root)?.title ?? shortId(root, 15)}</b>
        <small>{members.length} sessions · {nodes.reduce((count, node) => count + node.events.length, 0)} revealed events{p.cutoff !== null ? ' · future not revealed' : ''}</small>
        <svg viewBox="0 0 140 26" aria-hidden="true">
          {nodes.map(node => <circle key={node.id} cx={4 + axis.position(node.time) * 132} cy={4 + members.findIndex(session => session.id === node.session) / Math.max(1, members.length - 1) * 18} r="1" fill={colorOf(node.session)} />)}
        </svg>
        <small>{grades || 'No revealed event evidence'} · expand ↗</small>
      </button>;
    })}
    {collapsed.size > collapsedEntries.length ? <span className="journey-context-count">+{collapsed.size - collapsedEntries.length} branches in navigator</span> : null}
    {points.filter(point => !ports.some(port=>port.records.some(event=>event.id===point.node.id))).filter(point => detailGlyphs.has(point.key) || point.node.id === p.selected || compactGlyphs.has(point.key) || denseGlyphs.has(point.key) || (overview && highRisk(point))).map(point => {
      const members = detailGlyphs.get(point.key) ?? [point];
      const multiple = members.length > 1 && point.node.id !== p.selected;
      const neighbors = point.label ? points.filter(other => other.node.session === point.node.session && other.key !== point.key) : [];
      const labelWidth = Math.max(28, Math.min(84, ...neighbors.map(other => Math.abs(other.x - point.x) - 5)));
      const detail = SOURCE.design?.details[point.node.events[0]?.id];
      const gapCallout = gapCallouts.some(event => event.id === point.node.id);
      const storyCard = (compact && p.workspace==='evidence' && point.node.id===p.selected) || (investigation && ((feedback && w >= 760) || gapCallout));
      const incomingGrade = point.node.kind === 'gap' ? SOURCE.design?.relations.find(relation => relation.to === point.node.id)?.grade ?? 'unavailable' : point.node.grade;
      return <button key={point.key} type="button"
      onFocus={()=>setPreviewKey(point.key)} onBlur={()=>setPreviewKey(null)} onPointerEnter={()=>setPreviewKey(point.key)} onPointerLeave={()=>setPreviewKey(null)}
      data-workstream={point.group} data-event-id={point.node.id} data-session-id={point.node.session} data-time={point.node.time}
      className={`journey-event ${storyCard ? 'journey-story-event' : ''} ${gapCallout ? 'journey-gap-callout' : ''} ${point.node.id === p.selected ? 'selected' : ''} ${highRisk(point) ? 'high-risk' : ''} ${failed(point.node) ? 'failed' : ''}`}
      style={{ left: gapCallout ? w-93 : storyCard && feedback ? storyCardX(point) : point.x, top: gapCallout ? calloutY(point.node.id) : point.y, color: failed(point.node) ? '#ed735a' : highRisk(point) ? '#eeb564' : storyCard ? gradeColor(incomingGrade,point.color) : point.color, '--label-width': `${labelWidth}px` } as CSSProperties}
      popoverTarget={multiple ? 'loom-event-cluster' : undefined}
      title={`${point.group ? point.group+' · ' : ''}${point.node.title} · ${stamp(point.node.time)} · ${point.node.detail}${detail?.file ? ` · ${detail.file}` : ''}`} aria-label={multiple ? `Inspect ${members.reduce((count, member) => count + member.node.events.length, 0)} nearby events at ${stamp(point.node.time)}` : `${point.node.title} ${stamp(point.node.time)} ${point.node.detail}${detail?.file ? ` · ${detail.file}` : ''}${highRisk(point) ? ' · HIGH RISK' : ''}`} onClick={() => multiple ? setClusterKey(point.key) : p.onSelect(point.node.id)}>
      <svg width="22" height="22" viewBox="-11 -11 22 22" aria-hidden="true">{failed(point.node) ? <><polygon points="0,-9 8,-4.5 8,4.5 0,9 -8,4.5 -8,-4.5" fill="#1c0b0a" stroke="currentColor" /><path d="M-3-3L3 3M-3 3L3-3" stroke="currentColor" /></> : <>{dense ? <rect x="-7" y="-7" width="14" height="14" rx="2" fill="#040a0f" stroke="currentColor" strokeWidth=".6"/> : <circle r="9" fill="#040a0f" stroke="currentColor" strokeWidth=".8" />}<Glyph kind={point.node.kind} color="currentColor" s={dense ? 5.2 : 4.4} /></>}</svg>
      {multiple ? <sup>{members.reduce((count, member) => count + member.node.events.length, 0)}</sup> : null}
      {storyCard ? <strong>{detail?.title ?? point.node.title}<small>{stamp(point.node.time)} · {SESSION_BY_ID.get(point.node.session)?.agentId?.replace('design:', '') ?? shortId(point.node.session, 10)}</small></strong> : null}
      {point.label ? <span><svg className="journey-event-kind" width={labelWidth} height="12" aria-hidden="true"><text x={labelWidth/2} y="9" textAnchor="middle" textLength={Math.min(labelWidth,point.node.title.length*5)} lengthAdjust="spacingAndGlyphs">{point.node.title}</text></svg><small title={`${stamp(point.node.time)} UTC`}>{point.node.time % 60 === 0 ? stamp(point.node.time).slice(0,5) : stamp(point.node.time)}</small><small className="journey-event-detail">{detail?.file ?? point.node.detail}</small></span> : null}
    </button>; })}
    <div id="loom-event-cluster" className="journey-cluster" popover="auto" ref={clusterPopover} aria-label="Nearby recorded events">
      <h3>NEARBY RECORDED EVENTS</h3><p>Distinct records share this small time range. Select a source or zoom in.</p>
      {[...cluster].sort((a,b) => a.node.time - b.node.time).flatMap(point => point.node.events.map(event => <button type="button" key={event.id} onClick={() => {clusterPopover.current?.hidePopover(); p.onSelect(event.id);}}>{stamp(event.ts!)} · {event.kind}<small>{SOURCE.design?.details[event.id]?.title ?? event.tool ?? event.role ?? event.id}</small></button>))}
    </div>
    {!detail.length ? <p className="journey-empty">{p.sessionIds ? 'No sessions match these filters in the visible time window.' : 'No session records in this time window. Fit to restore the loaded page.'}</p> : null}
    {w < 800 && !compact && preview ? <div className="journey-point-preview"><b>{preview.node.title} · {stamp(preview.node.time)} UTC</b><span>{preview.node.detail}</span></div> : null}
    <span className="journey-axis-note">{'RECORDED TIME → · SEPARATE STRAND ENDPOINTS'}{SOURCE.design ? ' · SYNTHETIC DESIGN EXAMPLE' : overview ? ' · GROUPED RECORDS · DASHED = END UNAVAILABLE' : ' · PARENT LINKS DASHED'}</span>
  </div>;
}
