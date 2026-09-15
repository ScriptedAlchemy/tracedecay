import { Glyph, gradeColor, gradeDash } from './WeaveField';
import { workstreamEvidence } from './OutcomeSummary';
import { loomPivotUrl } from './pivots';
import { useEffect, useMemo, useRef, useState } from 'react';
import { shortId } from '../data/pack';
import { SOURCE, SOURCE_ID, SOURCE_OPTIONS } from './source';
import { SessionNavigator } from './SessionNavigator';
import { GRADE_LABEL, KIND_LABEL, SPINE_SESSION_ID, BRANCH_PARENT_ID, SELECTED_MARKER, eventGrade, eventKind } from './packScenes';
import { LOOM_STATES, parseLoomState, type LoomStateId, type EventKind, type EvidenceGrade } from './types';
import { EVIDENCE_MODES, type EvidenceMode, type EvidenceLayout, BOUNDS, inGroup, EVENTS, EVENT_BY_ID, GROUPS, LEVELS, SESSIONS, SESSION_BY_ID, sessionSearch, branchIds, rootId, sessionWindow, journeyNodes, matchesEvidence, stamp, type ZoomLevel, type Workspace } from './journey';
import { JourneyDensity, JourneyMinimap } from './JourneyMinimap';
import { useDemo } from '../app/workspace';
import { ProximityLens } from './ProximityLens';
import { PROXIMITY_STRANDS } from './proximity';
import { JourneyField } from './JourneyField';
import { EvidenceWorkspace, FeedbackWorkspace, GapsWorkspace } from './Workspaces';
import './loom.css';
import './journey.css';

type Navigation = {
  lens: 'execution' | 'proximity'; encounter: string | null;
  feedbackAnchor: number | null; evidenceLayout: EvidenceLayout; evidenceMode: EvidenceMode; level: ZoomLevel; workspace: Workspace; cursor: number; follow: boolean; replaying: boolean;
  selected: string | null; focus: string | null; project: string | null; provider: string | null;
  from: number; to: number; expanded: string[]; pins: string[];
  kinds: EventKind[]; grades: EvidenceGrade[]; query: string; pinnedOnly: boolean; unresolvedOnly: boolean; highRiskOnly: boolean;
};
function initialNavigation(frame: LoomStateId): Navigation {
  const overview = frame === '04' || (SOURCE_ID === 'ubuntu' && frame !== '03');
  const design = SOURCE.design;
  const gap = design && frame === '07' ? EVENT_BY_ID.get('design:event:ambiguous-attribution') : null;
  const focus = gap?.sessionId ?? (design && ['01', '02'].includes(frame) ? 'design:session:agent-095' : frame === '03' && design ? 'design:session:agent-091' : frame === '03' ? (SESSION_BY_ID.has(BRANCH_PARENT_ID) ? BRANCH_PARENT_ID : SESSIONS.find(s => SESSIONS.some(c => c.parentId === s.id))?.id ?? null) : null);
  const [from, to] = SOURCE.page === 'tail' && design ? [design.windows.tail[0], BOUNDS[1]] : SOURCE.page === 'morning' && design ? design.windows.tail : design ? (overview || ['05', '06', '07'].includes(frame) ? design.windows.dense : design.windows.tail) : overview ? BOUNDS : focus ? sessionWindow(branchIds(focus)) : [sessionWindow(new Set([SPINE_SESSION_ID]))[0], BOUNDS[1]];
  const selected = gap?.id ?? (design && EVENT_BY_ID.has(design.selectedEventId) && (frame === '05' || frame === '06') ? design.selectedEventId : (frame === '05' || frame === '06') && EVENT_BY_ID.has(SELECTED_MARKER.id) ? SELECTED_MARKER.id : null);
  return {
    lens: 'execution', encounter: null, feedbackAnchor: null, evidenceLayout: {tab:'source', focus:false, width:48}, evidenceMode: 'evidence',
    level: overview ? 'workstream' : frame === '03' ? 'agent' : frame === '05' || frame === '06' ? 'event' : 'episode',
    workspace: frame === '05' ? 'evidence' : frame === '06' ? 'feedback' : frame === '07' ? 'gaps' : 'weave',
    cursor: selected ? EVENT_BY_ID.get(selected)!.ts! : frame === '02' ? (from + to) / 2 : to,
    replaying: frame === '02', follow: to === BOUNDS[1] && !['02','05','06','07'].includes(frame), selected, focus,
    project: design ? null : overview ? null : focus ? SESSION_BY_ID.get(focus)?.project ?? null : 'tracedecay',
    from, to, provider: null, expanded: focus ? [...branchIds(focus)] : [], pins: [], kinds: [], grades: [], query: '', pinnedOnly: false, unresolvedOnly: false, highRiskOnly: false,
  };
}
function readNavigation(frame: LoomStateId): Navigation {
  const p = new URLSearchParams(location.search), n = initialNavigation(frame);
  n.lens = p.get('loom_lens') === 'proximity' ? 'proximity' : 'execution'; n.encounter = p.get('loom_encounter');
  n.query = p.get('loom_query') ?? ''; n.pinnedOnly = p.get('loom_pinned_only') === '1'; n.unresolvedOnly = p.get('loom_unresolved_only') === '1'; n.highRiskOnly = p.get('loom_high_risk') === '1';
  const level = p.get('loom_zoom');
  if (LEVELS.includes(level as ZoomLevel)) n.level = level as ZoomLevel;
  const evidenceMode = p.get('loom_evidence_mode');
  if (evidenceMode && Object.hasOwn(EVIDENCE_MODES,evidenceMode)) n.evidenceMode = evidenceMode as EvidenceMode;
  const evidenceTab = p.get('loom_evidence_tab');
  if (evidenceTab === 'source' || evidenceTab === 'context' || evidenceTab === 'links') n.evidenceLayout.tab = evidenceTab;
  n.evidenceLayout.focus = p.get('loom_evidence_focus') === '1';
  const evidenceWidth = p.get('loom_evidence_width');
  if (evidenceWidth?.trim() && Number.isFinite(Number(evidenceWidth))) n.evidenceLayout.width = Math.max(35,Math.min(65,Number(evidenceWidth)));
  const mode = p.get('loom_workspace');
  if (['weave', 'evidence', 'feedback', 'gaps'].includes(mode ?? '')) n.workspace = mode as Workspace;
  const number = (key: string, fallback: number) => {
    const value = p.get(key); const parsed = value === null ? NaN : Number(value);
    return Number.isFinite(parsed) ? Math.max(BOUNDS[0], Math.min(BOUNDS[1], parsed)) : fallback;
  };
  n.from = number('loom_from', n.from); n.to = number('loom_to', n.to);
  if (n.to <= n.from) [n.from, n.to] = BOUNDS;
  n.cursor = number('loom_time', n.cursor);
  if (p.has('loom_replay')) n.replaying = p.get('loom_replay') === '1';
  if (p.has('loom_follow')) n.follow = p.get('loom_follow') === '1';
  if (p.has('loom_event')) n.selected = EVENT_BY_ID.has(p.get('loom_event')!) ? p.get('loom_event') : null;
  if (p.has('loom_session')) n.focus = SESSION_BY_ID.has(p.get('loom_session')!) ? p.get('loom_session') : null;
  if (p.has('loom_provider')) n.provider = SESSIONS.some(s => s.provider === p.get('loom_provider')) ? p.get('loom_provider') : null;
  if (p.has('loom_project')) n.project = GROUPS.some(g => g.name === p.get('loom_project')) ? p.get('loom_project') : null;
  for (const key of ['expanded', 'pins'] as const) if (p.has(`loom_${key}`)) n[key] = p.get(`loom_${key}`)!.split(',').filter(id => SESSION_BY_ID.has(id));
  if (p.has('loom_kinds')) n.kinds = p.get('loom_kinds')!.split(',').filter(k => k in KIND_LABEL) as EventKind[];
  if (p.has('loom_grades')) n.grades = p.get('loom_grades')!.split(',').filter(g => g in GRADE_LABEL) as EvidenceGrade[];
  if (n.replaying) {
    n.follow = false;
    const event = n.selected ? EVENT_BY_ID.get(n.selected) : null;
    if (event?.ts == null || event.ts > n.cursor) n.selected = null;
  }
  const anchor = p.get('loom_feedback_line'), line = Number(anchor);
  const diff = SOURCE.design?.details[n.selected ?? '']?.diff;
  if (anchor?.trim() && Number.isInteger(line) && line >= 0 && diff?.split('\n')[line] !== undefined) n.feedbackAnchor = line;
  if (n.to < BOUNDS[1]) n.follow = false;
  if (n.follow) n.cursor = BOUNDS[1];
  return n;
}
function frameFor(n: Navigation): LoomStateId {
  if (n.workspace === 'evidence') return '05';
  if (n.workspace === 'feedback') return '06';
  if (n.workspace === 'gaps') return '07';
  if (n.replaying) return '02';
  if (n.level === 'outcome' || n.level === 'workstream') return '04';
  return n.level === 'agent' ? '03' : '01';
}
function saveNavigation(n: Navigation, push = false) {
  const url = new URL(location.href);
  if (SOURCE.page) url.searchParams.set('loom_page', SOURCE.page);
  url.searchParams.set('surface', 'loom'); url.searchParams.set('state', frameFor(n));
  for (const [key, value] of Object.entries({ lens: n.lens, encounter: n.encounter ?? '', feedback_line: n.feedbackAnchor ?? '', evidence_tab: n.evidenceLayout.tab, evidence_width: n.evidenceLayout.width, evidence_focus: n.evidenceLayout.focus ? 1 : 0, evidence_mode: n.evidenceMode, high_risk: n.highRiskOnly ? 1 : 0, query: n.query, pinned_only: n.pinnedOnly ? 1 : 0, unresolved_only: n.unresolvedOnly ? 1 : 0, zoom: n.level, workspace: n.workspace, time: n.cursor, follow: n.follow ? 1 : 0, replay: n.replaying ? 1 : 0, event: n.selected ?? '', session: n.focus ?? '', project: n.project ?? '', provider: n.provider ?? '', from: n.from, to: n.to, expanded: n.expanded.join(','), pins: n.pins.join(','), kinds: n.kinds.join(','), grades: n.grades.join(',') })) url.searchParams.set(`loom_${key}`, String(value));
  if (url.href !== location.href) history[push ? 'pushState' : 'replaceState'](null, '', url);
}

export function LoomPage(props: { state?: LoomStateId; onState?: (id: LoomStateId) => void } = {}) {
  const {navigate} = useDemo();
  const [nav, setNav] = useState(() => readNavigation(props.state ?? parseLoomState(new URLSearchParams(location.search).get('state'))));
  const frame = frameFor(nav), lastFrame = useRef(props.state);
  const [playing, setPlaying] = useState(false), [speed, setSpeed] = useState(1);
  const [filtersOpen, setFiltersOpen] = useState(false), [branchesOpen, setBranchesOpen] = useState<boolean | null>(null);
  const query = nav.query, onlyPinned = nav.pinnedOnly, onlyUnresolved = nav.unresolvedOnly;
  const setQuery = (query: string) => change({query}, false);
  const setOnlyPinned = (pinnedOnly: boolean) => change({pinnedOnly});
  const setOnlyUnresolved = (unresolvedOnly: boolean) => change({unresolvedOnly});
  const [tableOpen, setTableOpen] = useState(false), [treeOpen, setTreeOpen] = useState(false);
  const [treePage, setTreePage] = useState(0);
  const tableRef = useRef<HTMLDetailsElement>(null);
  function change(patch: Partial<Navigation>, push = true) {
    setNav(current => {
      const next = { ...current, ...patch };
      if (next.selected !== current.selected) next.feedbackAnchor = null;
      if (push) saveNavigation(next, true);
      return next;
    });
  }
  useEffect(() => {
    if (props.state && props.state !== lastFrame.current) {
      lastFrame.current = props.state;
      const id = props.state;
      // Gallery frame chips change presentation; they do not swap the source dataset.
      setNav(n => ({ ...n, workspace: id === '05' ? 'evidence' : id === '06' ? 'feedback' : id === '07' ? 'gaps' : 'weave',
        ...(id === '01' ? {from: Math.max(BOUNDS[0], BOUNDS[1] - (n.to - n.from)), to: BOUNDS[1], cursor: BOUNDS[1]} : {}),
        level: id === '04' ? 'workstream' : id === '03' ? 'agent' : n.level,
        follow: id === '01' ? true : id === '02' ? false : n.follow, replaying: id === '02' ? true : id === '01' ? false : n.replaying,
        selected: (id === '05' || id === '06') && !n.selected ? EVENTS.find(e => e.sessionId === n.focus && e.ts !== null && (n.follow || e.ts <= n.cursor))?.id ?? null : n.selected,
      }));
      setPlaying(false);
    }
  }, [props.state]);
  useEffect(() => {
    saveNavigation(nav);
    lastFrame.current = frame;
    if (props.state !== frame) props.onState?.(frame);
  }, [nav]);
  useEffect(() => {
    const onPop = () => { const n = readNavigation(parseLoomState(new URLSearchParams(location.search).get('state'))); lastFrame.current = frameFor(n); setNav(n); setPlaying(false); };
    window.addEventListener('popstate', onPop);
    return () => window.removeEventListener('popstate', onPop);
  }, []);
  const cutoff = nav.replaying ? nav.cursor : null;
  const searchIndex = useMemo(() => sessionSearch(cutoff), [cutoff]);
  const observedSessions = new Set(EVENTS.filter(event => event.ts !== null && (cutoff === null || event.ts <= cutoff)).map(event => event.sessionId));
  const unresolved = (id: string) => { const session = SESSION_BY_ID.get(id); return session?.endedTs == null || (cutoff !== null && session.endedTs > cutoff) || !observedSessions.has(id); };
  const allNodes = useMemo(() => journeyNodes(nav.level, nav.kinds, nav.grades, cutoff), [nav.level, nav.kinds, nav.grades, cutoff]);
  const selectedEvent = nav.selected ? EVENT_BY_ID.get(nav.selected) ?? null : null;
  const eventContext = selectedEvent ? [SOURCE.design?.details[selectedEvent.id]?.task, SESSION_BY_ID.get(selectedEvent.sessionId)?.agentId ?? selectedEvent.sessionId, SOURCE.design?.details[selectedEvent.id]?.title ?? selectedEvent.tool ?? selectedEvent.kind].filter(Boolean).join(' › ') : null;
  const selectedSession = nav.focus ? SESSION_BY_ID.get(nav.focus) : null;
  const riskSessions = new Set(EVENTS.filter(event => SOURCE.design?.details[event.id]?.risk === 'high' && (cutoff === null || (event.ts !== null && event.ts <= cutoff))).map(event => event.sessionId));
  const proximityActive = nav.workspace === 'weave' && nav.lens === 'proximity';
  const scopeEvents = proximityActive ? EVENTS : EVENTS.filter(e => (!nav.highRiskOnly || riskSessions.has(e.sessionId)) && (searchIndex.get(e.sessionId) ?? '').includes(query.toLowerCase()) && (!onlyPinned || nav.pins.includes(e.sessionId)) && (!onlyUnresolved || unresolved(e.sessionId)) && inGroup(e.sessionId, nav.project) && (!nav.provider || SESSION_BY_ID.get(e.sessionId)?.provider === nav.provider) && (!nav.focus || e.sessionId === nav.focus || nav.expanded.includes(e.sessionId)) && (!nav.kinds.length || nav.kinds.includes(eventKind(e))) && matchesEvidence(e, nav.grades, cutoff));
  const tableEvents = scopeEvents.filter(e => e.ts !== null && e.ts >= nav.from && e.ts <= nav.to && (cutoff === null || e.ts <= cutoff));

  const outcomeEvent = useMemo(() => {
    if (!nav.focus || !SOURCE.design) return null;
    const visible = new Set(EVENTS.filter(event => event.ts !== null && (cutoff === null || event.ts <= cutoff)).map(event => event.id));
    const reached = new Set(EVENTS.filter(event => event.sessionId === nav.focus && visible.has(event.id)).map(event => event.id));
    for (let count = -1; count !== reached.size;) {
      count = reached.size;
      for (const relation of SOURCE.design.relations) if (['exact', 'explicit'].includes(relation.grade) && reached.has(relation.from) && visible.has(relation.to)) reached.add(relation.to);
    }
    return EVENTS.filter(event => reached.has(event.id) && ['result', 'commit', 'rejoin', 'pr'].includes(eventKind(event))).at(-1) ?? null;
  }, [nav.focus, cutoff]);
  function selectEvent(id: string) {
    const event = EVENT_BY_ID.get(id);
    if (!event || event.ts === null) return;
    setPlaying(false);
    change({ selected: id, feedbackAnchor: null, follow: false, cursor: event.ts, workspace: 'evidence' });
  }
  function seek(time: number) {
    setPlaying(false);
    change({ cursor: Math.max(BOUNDS[0], Math.min(BOUNDS[1], time)), follow: false, replaying: true,
      selected: selectedEvent?.ts !== null && selectedEvent?.ts !== undefined && selectedEvent.ts > time ? null : nav.selected }, false);
  }
  function step(direction: -1 | 1) {
    const scopeIds = new Set(scopeEvents.map(event => event.id));
    const rows = nav.level === 'episode'
      ? journeyNodes('episode',nav.kinds,nav.grades,null).filter(node => node.events.some(event => scopeIds.has(event.id))).map(node => ({id:node.id, ts:node.time, ids:node.events.map(event => event.id)}))
      : scopeEvents.filter(event => event.ts !== null).map(event => ({id:event.id, ts:event.ts!, ids:[event.id]}));
    const selectedIndex = selectedEvent?.ts === nav.cursor ? rows.findIndex(row => row.ids.includes(nav.selected!)) : -1;
    const target = selectedIndex >= 0 ? rows[selectedIndex + direction] : direction > 0 ? rows.find(row => row.ts > nav.cursor) : [...rows].reverse().find(row => row.ts < nav.cursor);
    if (!target) return;
    setPlaying(false);
    change({ cursor: target.ts, selected: target.id, feedbackAnchor:null, follow: false, replaying: true }, false);
  }
  function returnToTail() { setPlaying(false); change({ from: Math.max(BOUNDS[0], BOUNDS[1] - (nav.to - nav.from)), to: BOUNDS[1], cursor: BOUNDS[1], follow: true, replaying: false, workspace: 'weave' }); }
  function loadRemainingExample() {
    if (!SOURCE.design || SOURCE.page === 'full') return;
    const url = new URL(location.href), end = SOURCE.design.windows.dense[1];
    url.searchParams.set('loom_page', 'full');
    if (nav.follow) {
      url.searchParams.set('loom_from', String(end - (nav.to - nav.from)));
      url.searchParams.set('loom_to', String(end)); url.searchParams.set('loom_time', String(end));
    }
    location.assign(url.href);
  }
  function focusSession(id: string) {
    const ids = branchIds(id), [from, to] = sessionWindow(ids);
    change({ focus: id, project: SOURCE.design ? null : SESSION_BY_ID.get(id)?.project ?? null, provider: null, expanded: [...ids], level: 'agent', from, to, follow: false, workspace: 'weave' });
  }
  function chooseLevel(level: ZoomLevel) {
    setBranchesOpen(null);
    change({ level, workspace: 'weave' });
  }
  function fit() {
    const [from, to] = nav.level === 'outcome' || nav.level === 'workstream' || !nav.focus ? BOUNDS : sessionWindow(branchIds(nav.focus));
    change({ from, to });
  }
  function setWindow(from: number, to: number) {
    const duration = Math.max(30, Math.min(BOUNDS[1] - BOUNDS[0], to - from));
    const start = Math.max(BOUNDS[0], Math.min(BOUNDS[1] - duration, from));
    change({ from: start, to: start + duration, follow: false }, false);
  }
  function zoom(factor: number) {
    const center = nav.cursor >= nav.from && nav.cursor <= nav.to ? nav.cursor : (nav.from + nav.to) / 2;
    setWindow(center - (center - nav.from) * factor, center + (nav.to - center) * factor);
  }
  function showTable() { setTableOpen(true); requestAnimationFrame(() => tableRef.current?.scrollIntoView({ block: 'nearest' })); }
  useEffect(() => {
    if (!playing) return;
    let before = performance.now();
    const handle = window.setInterval(() => {
      const now = performance.now(), elapsed = (now - before) / 1000 * speed; before = now;
      setNav(n => ({ ...n, cursor: Math.min(BOUNDS[1], n.cursor + elapsed), follow: false, replaying: true }));
    }, window.matchMedia('(prefers-reduced-motion: reduce)').matches ? 1000 : 120);
    return () => clearInterval(handle);
  }, [playing, speed]);
  useEffect(() => { if (playing && nav.cursor >= BOUNDS[1]) setPlaying(false); }, [nav.cursor, playing]);
  useEffect(() => {
    function key(e: KeyboardEvent) {
      if ((e.target as HTMLElement).closest('input, textarea, select, button, a, [contenteditable=true]')) return;
      if (e.key === 'j' || e.key === 'ArrowRight') { e.preventDefault(); step(1); }
      else if (e.key === 'k' || e.key === 'ArrowLeft') { e.preventDefault(); step(-1); }
      else if (e.key === ' ') { e.preventDefault(); setPlaying(v => !v); change({ follow: false, replaying: true }, false); }
      else if (e.key === 'End') { e.preventDefault(); returnToTail(); }
      else if (e.key === 'Home') { e.preventDefault(); seek(nav.from); }
      else if (e.key === 'Escape') { change({ workspace: 'weave' }); }
      else if (e.key === '+' || e.key === '=') zoom(.8);
      else if (e.key === '-') zoom(1.25);
    }
    window.addEventListener('keydown', key); return () => window.removeEventListener('keydown', key);
  });
  const matchingEventSessions = new Set(allNodes.map(node => node.session));
  const navigationSessions = proximityActive ? SESSIONS : SESSIONS.filter(s => (!(nav.kinds.length || nav.grades.length) || matchingEventSessions.has(s.id)) && (!nav.highRiskOnly || riskSessions.has(s.id)) && inGroup(s.id, nav.project) && (!nav.provider || s.provider === nav.provider) && (cutoff === null || (s.startedTs !== null && s.startedTs <= cutoff)) && (!onlyPinned || nav.pins.includes(s.id)) && (!onlyUnresolved || unresolved(s.id)) && (searchIndex.get(s.id) ?? '').includes(query.toLowerCase()));
  const navigationIds = new Set(navigationSessions.map(s => s.id));
  const visibleAgentCount = new Set(SOURCE.agents.filter(agent => navigationIds.has(agent.sessionId)).map(agent => agent.agentId)).size;
  const visibleWorkstreamParticipations = SOURCE.design
    ? PROXIMITY_STRANDS.filter(strand => navigationIds.has(strand.sessionId)).length
    : navigationSessions.length;
  useEffect(() => setTreePage(0), [query, onlyPinned, onlyUnresolved, nav.project, nav.provider, cutoff]);
  const compact = nav.workspace !== 'weave';
  const broad = nav.level === 'outcome' || nav.level === 'workstream';
  const rangeDate = !broad || Math.floor(nav.from / 86400) !== Math.floor(nav.to / 86400);
  const showNavigator = nav.lens === 'execution' && !compact && (branchesOpen ?? (broad || nav.level === 'agent'));
  const showMoment = nav.lens === 'execution' && !compact && !nav.follow && !showNavigator;
  return <section className="aperture is-loom"><div data-workspace={nav.workspace} className={`loom journey ${nav.level === 'workstream' && !compact ? 'is-dense' : ''}`}>
    <div className="loom-toolbar">
      <label className="journey-source"><span className="visually-hidden">Loom data source</span><select aria-label="Loom data source" value={SOURCE_ID} onChange={e => {
        const url = new URL(location.href);
        for (const key of [...url.searchParams.keys()]) if (key.startsWith('loom_')) url.searchParams.delete(key);
        url.searchParams.set('data', e.target.value === 'design' ? 'fixture' : 'snapshot'); url.searchParams.set('loom_source', e.target.value); url.searchParams.set('state', e.target.value === 'mac' ? '01' : '04'); location.assign(url.href);
      }}>{SOURCE_OPTIONS.map(source => <option key={source.id} value={source.id}>{source.label}</option>)}</select></label>
      <button className={`loom-btn ${nav.follow ? 'is-live' : ''}`} aria-pressed={nav.follow} onClick={returnToTail}>{nav.follow ? '● FOLLOW LOADED TAIL' : 'RETURN TO LOADED TAIL'}</button>
      <button className={`loom-btn ${!nav.follow && !playing ? 'is-on' : ''}`} onClick={() => { setPlaying(false); change({ follow: false, replaying: true }); }}>Ⅱ {nav.follow || playing ? 'PAUSE' : 'PAUSED'}</button>
      {!compact ? <><button className="loom-btn" onClick={fit}>⛶ FIT</button>
      <button className={`loom-btn ${filtersOpen ? 'is-on' : ''}`} disabled={proximityActive} aria-expanded={filtersOpen} onClick={() => setFiltersOpen(!filtersOpen)}>▽ EVENT FILTERS</button>
      <button className={`loom-btn ${showNavigator ? 'is-on' : ''}`} disabled={proximityActive} aria-expanded={showNavigator} onClick={() => setBranchesOpen(!showNavigator)}>⑂ BRANCHES</button>
      <div className="loom-zoom"><button aria-label="Zoom out" onClick={() => zoom(1.25)}>−</button><span>{nav.level.toUpperCase()}</span><button aria-label="Zoom in" onClick={() => zoom(.8)}>+</button></div></> : null}
    </div>
    {filtersOpen && !proximityActive ? <div className="journey-filters"><fieldset><legend>Event kinds · empty means all</legend>{Object.entries(KIND_LABEL).map(([kind, label]) => <label key={kind}><input type="checkbox" checked={nav.kinds.includes(kind as EventKind)} onChange={() => change({ kinds: nav.kinds.includes(kind as EventKind) ? nav.kinds.filter(k => k !== kind) : [...nav.kinds, kind as EventKind] })} />{label}</label>)}</fieldset><fieldset><legend>Evidence · source rows and linked relations</legend>{Object.entries(GRADE_LABEL).map(([grade, label]) => <label key={grade}><input type="checkbox" checked={nav.grades.includes(grade as EvidenceGrade)} onChange={() => change({ grades: nav.grades.includes(grade as EvidenceGrade) ? nav.grades.filter(g => g !== grade) : [...nav.grades, grade as EvidenceGrade] })} />{label}</label>)}</fieldset><button className="loom-btn" onClick={() => change({ kinds: [], grades: [] })}>CLEAR FILTERS</button></div> : null}
    <div className="loom-head"><div className="loom-head-l"><b>LOOM / {nav.workspace === 'evidence' ? 'SELECTED EVENT' : nav.workspace === 'feedback' ? 'FEEDBACK CONTINUATION' : nav.workspace === 'gaps' ? 'EVIDENCE GAPS' : proximityActive ? 'CODE PROXIMITY' : nav.replaying ? 'TEMPORAL REPLAY' : broad ? nav.level === 'workstream' ? 'LIVE EXECUTION' : 'OUTCOME OVERVIEW' : nav.level === 'agent' ? 'BRANCHING EXECUTION' : nav.follow ? 'FOLLOW LOADED TAIL' : 'EXECUTION INSPECTION'}</b><em title={compact ? eventContext ?? undefined : undefined}>{compact && selectedEvent ? <>{SOURCE.design ? 'CONCEPT / SYNTHETIC DATA' : `${SOURCE_ID.toUpperCase()} SNAPSHOT`} · {eventContext}</> : <>{!broad ? `${nav.project ?? 'ALL PROJECTS'}${nav.provider ? ` / ${nav.provider}` : ''} · ` : ''}{nav.focus ? `SESSION ${shortId(nav.focus, 14)}` : `${visibleAgentCount} UNIQUE ${SOURCE.design ? 'AGENTS' : 'CAPTURED AGENT IDS'} · ${visibleWorkstreamParticipations} ${SOURCE.design ? 'WORKSTREAM PARTICIPATIONS' : 'SESSIONS'}`}{!broad ? ` · ${SOURCE.design ? 'CONCEPT / SYNTHETIC DATA' : `${SOURCE_ID.toUpperCase()} SNAPSHOT`}` : ''}</>}</em></div>
      {compact ? <button className="loom-btn" onClick={() => change({ workspace: 'weave' })}>BACK TO WEAVE</button> : <span className="journey-coverage">{stamp(nav.from, rangeDate)} → {stamp(nav.to, rangeDate)} UTC<small>{SOURCE.design ? `${SOURCE.page?.toUpperCase()} EXAMPLE PAGE · CONCEPT / SYNTHETIC DATA` : 'Loaded snapshot · bodies partial / unavailable'}</small>{SOURCE.page && SOURCE.page !== 'full' ? <button className="journey-load-page" onClick={loadRemainingExample}>Load remaining example events →</button> : null}</span>}
    </div>
    <div className={`journey-stage ${compact ? 'compact' : ''} ${showNavigator ? 'with-navigator' : showMoment ? 'with-moment' : ''}`}>
      {!compact && nav.lens === 'proximity' ? <ProximityLens from={nav.from} to={nav.to} cutoff={cutoff} selected={nav.encounter} onSelect={encounter=>change({encounter})} onSeek={seek} onWindow={setWindow}/> : <JourneyField grades={nav.grades} sessionIds={query || onlyPinned || onlyUnresolved || nav.highRiskOnly || nav.kinds.length || nav.grades.length ? navigationSessions.map(s => s.id) : undefined} from={nav.from} to={nav.to} cursor={nav.cursor} cutoff={cutoff} level={nav.level} focus={nav.focus} project={nav.project} provider={nav.provider} expanded={nav.expanded} nodes={allNodes} selected={nav.selected} workspace={nav.workspace} onSelect={selectEvent} onSession={focusSession} onProject={(project, provider) => change({ project, provider, focus: null, expanded: [], level: 'agent' })} onSeek={seek} onWindow={setWindow} />}
      {nav.workspace === 'evidence' ? <div className="journey-context-map"><JourneyMinimap proximity={!compact && nav.lens === 'proximity'} encounter={nav.encounter} from={nav.from} to={nav.to} cursor={nav.cursor} cutoff={cutoff} onWindow={setWindow} onSeek={seek}/></div> : null}
      {showNavigator ? <aside className="loom-side journey-navigator"><h3>BRANCH NAVIGATOR</h3><label className="loom-search"><span className="visually-hidden">Search branches</span><input type="search" placeholder="Search agent, session, project…" value={query} onChange={e => setQuery(e.target.value)} /></label>
        <div className="loom-chip-row"><button className={`loom-btn ${onlyPinned ? 'is-on' : ''}`} aria-pressed={onlyPinned} onClick={() => setOnlyPinned(!onlyPinned)}>PINNED</button><button className={`loom-btn ${onlyUnresolved ? 'is-on' : ''}`} aria-pressed={onlyUnresolved} onClick={() => setOnlyUnresolved(!onlyUnresolved)}>UNRESOLVED</button><button className={`loom-btn ${nav.highRiskOnly ? 'is-on' : ''}`} aria-pressed={nav.highRiskOnly} onClick={() => change({highRiskOnly:!nav.highRiskOnly})}>HIGH RISK</button></div>{nav.highRiskOnly && !SOURCE.design ? <p className="loom-note">Risk assessment is not included in this export.</p> : null}
        <button className="loom-btn" onClick={() => change({ project: null, provider: null, focus: null, expanded: [], level: 'workstream', from: BOUNDS[0], to: BOUNDS[1] })}>ALL WORKSTREAMS</button>
        <div className={`journey-nav-list ${broad ? '' : 'session-list'}`}>{broad ? GROUPS.filter(g => navigationSessions.some(s => inGroup(s.id, g.name))).map(g => {
          const members = g.sessions.filter(s => navigationIds.has(s.id));
          const {records, grades, risks} = workstreamEvidence(allNodes.filter(node => node.time >= nav.from && node.time <= nav.to), new Set(members.map(s => s.id)), nav.grades);
          const evidence = [...grades].map(([grade, count]) => `${count} ${GRADE_LABEL[grade as EvidenceGrade].toLowerCase()}`).join(' · ');
          return <button key={g.id} className="loom-nav-row" onClick={() => change({ project: g.name, focus: null, expanded: [], level: 'agent' })}>
            <b style={{ color: g.color }}>{g.name}</b><span>{members.length} {SOURCE.design ? 'illustrated participations' : 'session records · grouped by project'}</span>
            <span className={risks.size ? 'journey-risk-count' : ''}>{SOURCE.design ? `${risks.size} high-risk records · ${records.reduce((count, node) => count + node.events.length, 0)} events in window` : 'Risk assessment not exported'}</span>
            <span className="journey-evidence-count" title={evidence}>{SOURCE.design ? 'Relations: ' : 'Event rows: '}{evidence || 'unavailable'}</span>
          </button>;
        }) : <SessionNavigator nodes={allNodes.filter(node => node.time >= nav.from && node.time <= nav.to)} grades={nav.grades} sessions={navigationSessions} selected={nav.focus} pins={nav.pins} onSelect={focusSession} onPin={id => change({ pins: nav.pins.includes(id) ? nav.pins.filter(pin => pin !== id) : [...nav.pins, id] })} />}</div>
        {nav.focus ? <><button className="loom-btn" onClick={() => focusSession(rootId(nav.focus!))}>↑ PATH TO ROOT</button><button className="loom-btn" onClick={() => outcomeEvent ? selectEvent(outcomeEvent.id) : change({ workspace: 'gaps' })}>↓ PATH TO OUTCOME{outcomeEvent ? '' : ' · UNAVAILABLE'}</button><button className="loom-btn" onClick={() => change({ expanded: nav.expanded.length ? [] : [...branchIds(nav.focus!)] })}>{nav.expanded.length ? 'COLLAPSE' : 'EXPAND'} CHILDREN</button><nav className="loom-pivots" aria-label="Focused session destinations">{(['sessions', 'agents', 'work'] as const).map(surface => <a key={surface} href={loomPivotUrl(surface, nav.focus!)} onClick={event=>{event.preventDefault();navigate(surface,Object.fromEntries(new URL(loomPivotUrl(surface,nav.focus!),location.origin).searchParams));}}>{surface.toUpperCase()} ↗</a>)}</nav></> : null}
      </aside> : null}
      {showMoment ? <aside className="journey-moment"><h3>SELECTED MOMENT</h3><b>{stamp(nav.cursor)} UTC</b><p>{selectedEvent ? `${KIND_LABEL[eventKind(selectedEvent)]} · ${selectedEvent.role ?? selectedEvent.tool ?? ''}` : 'Seek or select an event to inspect its recorded source.'}</p><p>{tableEvents.length} revealed · future not revealed</p>{tableEvents.slice(-3).map(e => <button key={e.id} className="loom-nav-row" onClick={() => selectEvent(e.id)}><b>{KIND_LABEL[eventKind(e)]}</b><span>{stamp(e.ts!)} · {shortId(e.id, 15)}</span></button>)}</aside> : null}
      {nav.workspace === 'evidence' ? <EvidenceWorkspace layout={nav.evidenceLayout} onLayout={evidenceLayout => change({evidenceLayout}, evidenceLayout.width === nav.evidenceLayout.width)} mode={nav.evidenceMode} onMode={evidenceMode => change({evidenceMode})} event={selectedEvent} cutoff={cutoff} onSelect={selectEvent} onFeedback={feedbackAnchor => change({ workspace: 'feedback', feedbackAnchor: feedbackAnchor ?? null })} onGap={() => change({ workspace: 'gaps' })} onTable={showTable} /> : null}
      {nav.workspace === 'feedback' ? <FeedbackWorkspace anchor={nav.feedbackAnchor} onAnchor={feedbackAnchor => change({feedbackAnchor})} onEvidence={() => change({workspace: 'evidence'})} event={selectedEvent} cutoff={cutoff} onSelect={selectEvent} /> : null}
      {nav.workspace === 'gaps' ? <GapsWorkspace event={selectedEvent} cutoff={cutoff} onSelect={selectEvent} onFocus={id => {
        const event = EVENT_BY_ID.get(id);
        if (event?.ts == null || (cutoff !== null && event.ts > cutoff)) return;
        const [from, to] = sessionWindow(new Set([event.sessionId]));
        change({selected:id, focus:event.sessionId, expanded:[], from, to, follow:false, workspace:'gaps'});
      }} /> : null}
    </div>
    {!compact ? <><div className="journey-legend">LEGEND {Object.entries(GRADE_LABEL).map(([grade,label]) => <span key={grade}><svg width="26" height="10" aria-hidden="true"><path d="M0 5H26" stroke={gradeColor(grade as EvidenceGrade)} strokeDasharray={gradeDash(grade as EvidenceGrade)} /></svg>{label}</span>)}<button onClick={() => change({ workspace: 'gaps' })}>EVIDENCE GAPS ↗</button><button className="loom-btn" aria-pressed={nav.lens === 'proximity'} onClick={() => change({lens: nav.lens === 'proximity' ? 'execution' : 'proximity'})}>{nav.lens === 'proximity' ? 'EXECUTION LENS' : 'PROXIMITY LENS'}</button></div>
      <ul className="journey-symbol-key" aria-label="Event symbols">{Object.entries(KIND_LABEL).filter(([kind]) => tableEvents.some(event => eventKind(event) === kind)).map(([kind,label]) => <li key={kind}><svg width="16" height="16" viewBox="-8 -8 16 16" aria-hidden="true"><Glyph kind={kind as EventKind} color="currentColor" s={5}/></svg>{label}</li>)}</ul>
      {nav.level === 'workstream' ? <JourneyDensity from={nav.from} to={nav.to} cursor={nav.cursor} events={tableEvents} onSeek={seek}/> : <JourneyMinimap proximity={!compact && nav.lens === 'proximity'} encounter={nav.encounter} from={nav.from} to={nav.to} cursor={nav.cursor} cutoff={cutoff} onWindow={setWindow} onSeek={seek} />}</> : null}
    <div className="journey-bottom"><div className="journey-transport"><button aria-label="Previous event" onClick={() => step(-1)}>◀</button><button aria-label={playing ? 'Pause replay' : 'Play replay'} onClick={() => { setPlaying(!playing); change({ follow: false, replaying: true }, false); }}>{playing ? 'Ⅱ' : '▶'}</button><button aria-label="Next event" onClick={() => step(1)}>▶|</button><span>{stamp(nav.cursor)} UTC</span><span className="visually-hidden" role="status">{playing ? `Playing replay at ${speed} times speed` : nav.follow ? 'Following loaded tail' : `Paused at ${stamp(nav.cursor)} UTC`}</span><select aria-label="Replay speed" value={speed} onChange={e => setSpeed(Number(e.target.value))}>{[.5, 1, 2, 4].map(s => <option key={s} value={s}>{s}×</option>)}</select></div><details className="journey-semantic" open={!compact} role="group" aria-label="Semantic zoom"><summary>{nav.level.toUpperCase()} ▾</summary>{LEVELS.map(level => <button key={level} aria-pressed={nav.level === level} onClick={() => chooseLevel(level)}>{level.toUpperCase()}</button>)}</details>{nav.level === 'workstream' && !compact ? <JourneyMinimap proximity={!compact && nav.lens === 'proximity'} encounter={nav.encounter} from={nav.from} to={nav.to} cursor={nav.cursor} cutoff={cutoff} onWindow={setWindow} onSeek={seek}/> : null}</div>
    {!nav.follow ? <input className="loom-scrub-range" type="range" aria-label="Scrub loaded page" min={nav.from} max={nav.to} step="any" value={Math.max(nav.from, Math.min(nav.to, nav.cursor))} onChange={e => seek(Number(e.target.value))} /> : null}
    <div className="journey-fallbacks"><details ref={tableRef} className="loom-fallback" open={tableOpen} onToggle={e => setTableOpen(e.currentTarget.open)}><summary>{SOURCE_ID === 'ubuntu' ? 'Event spine not exported' : `Exact event table · ${tableEvents.length} recorded rows`}</summary><table className="loom-table"><caption>Same source, filters and replay timestamp as the weave. Undated records are listed separately below.</caption><thead><tr><th>Time UTC</th><th>Kind</th><th>Session</th><th>Source</th><th>Grade</th></tr></thead><tbody>{tableEvents.map(e => <tr key={e.id} className={e.id === nav.selected ? 'is-sel' : ''}><td><button onClick={() => selectEvent(e.id)}>{stamp(e.ts!, true)}</button></td><td>{KIND_LABEL[eventKind(e)]}</td><td>{shortId(e.sessionId, 17)}</td><td>{e.tool ?? e.role ?? e.kind}</td><td>{GRADE_LABEL[eventGrade(e)]}</td></tr>)}</tbody></table>{!tableEvents.length ? <p>{SOURCE_ID === 'ubuntu' ? 'This export contains session identities only. Event rows and transcript bodies are unavailable.' : 'No events match this scope and replay time.'}</p> : null}<p>Undated source records: {scopeEvents.filter(e => e.ts === null).length}. They are not placed on the time axis.</p></details>
    <details className="loom-fallback" open={treeOpen} onToggle={e => setTreeOpen(e.currentTarget.open)}><summary>Exact branch tree · {navigationSessions.length} sessions</summary><table className="loom-table"><thead><tr><th>Session</th><th>Recorded parent</th><th>Parent evidence</th><th>Start</th><th>Coverage</th></tr></thead><tbody>{navigationSessions.slice(treePage * 50, treePage * 50 + 50).map(s => <tr key={s.id} aria-selected={s.id === (nav.workspace === 'weave' ? nav.focus : selectedEvent?.sessionId ?? nav.focus)} className={s.id === (nav.workspace === 'weave' ? nav.focus : selectedEvent?.sessionId ?? nav.focus) ? 'is-sel' : ''}><td><button onClick={() => focusSession(s.id)}>{s.id}</button></td><td>{s.parentId ?? 'No parent recorded'}</td><td>{s.parentId && SESSIONS.some(parent => parent.id === s.parentId && (cutoff === null || (parent.startedTs !== null && parent.startedTs <= cutoff))) ? <span className="loom-grade g-exact">EXACT</span> : <span className="loom-grade g-unavailable">UNAVAILABLE</span>}</td><td>{s.startedAt ?? 'Undated'}</td><td>{s.coverage}</td></tr>)}</tbody></table><div className="journey-tree-pages"><button disabled={treePage === 0} onClick={() => setTreePage(page => page - 1)}>PREVIOUS SESSIONS</button><span>{Math.min(treePage * 50 + 1, navigationSessions.length)}–{Math.min((treePage + 1) * 50, navigationSessions.length)} / {navigationSessions.length}</span><button disabled={(treePage + 1) * 50 >= navigationSessions.length} onClick={() => setTreePage(page => page + 1)}>NEXT SESSIONS</button></div></details></div>
    {selectedSession && nav.level === 'outcome' ? <p className="loom-note">Outcome links are not exported for {selectedSession.project}. Project membership is exact; no successful outcome is inferred.</p> : null}
  </div></section>;
}
export { LOOM_STATES, parseLoomState };
export type { LoomStateId };
