import { gradeColor, gradeDash } from './WeaveField';
import { useEffect, useId, useMemo, useRef, useState } from 'react';
import { BOUNDS, EVENTS, EVENT_BY_ID, GROUPS, SESSIONS, rootId, stamp, timeAxis, fiberPosition, branchLane, designLaneColor, WEAVE_COLORS } from './journey';
import type { PackLoomEvent } from '../data/pack';
import { PROXIMITY_EXAMPLES, PROXIMITY_STRANDS, proximityGroupForEncounter, proximityGroupForEvent, proximityTone, proximityY } from './proximity';
import { SOURCE } from './source';

/** Scene and full-page scopes are explicit; neither changes selection or replay. */
export function JourneyMinimap({ from, to, cursor, cutoff, onWindow, onSeek, proximity = false }: {
  from: number; to: number; cursor: number; cutoff: number | null; proximity?: boolean; encounter?: string | null;
  onWindow: (from: number, to: number) => void; onSeek: (time: number) => void;
}) {
  const denseRange=SOURCE.page==='full' ? SOURCE.design?.windows.dense : undefined;
  const defaultScope=denseRange && from>=denseRange[0] ? 'scene' : 'page';
  const readScope=()=>new URLSearchParams(location.search).get('loom_map_scope')==='page' ? 'page' : new URLSearchParams(location.search).get('loom_map_scope')==='scene' && denseRange ? 'scene' : defaultScope;
  const [mapScope,setMapScope]=useState<'scene'|'page'>(readScope);
  const mapBounds=mapScope==='scene' && denseRange ? [Math.min(from,denseRange[0]),Math.max(to,denseRange[1])] : BOUNDS;
  const axis=useMemo(()=>timeAxis(mapBounds[0],mapBounds[1]),[mapBounds[0],mapBounds[1]]);
  const changeScope=()=>{const next=mapScope==='scene'?'page':'scene';setMapScope(next);const url=new URL(location.href);url.searchParams.set('loom_map_scope',next);history.pushState(null,'',url);};
  useEffect(()=>{const sync=()=>setMapScope(readScope());window.addEventListener('popstate',sync);return()=>window.removeEventListener('popstate',sync);},[from,denseRange]);
  const ref = useRef<HTMLDivElement>(null), glow = useId();
  const [size, setSize] = useState({width:300,height:88});
  useEffect(() => {
    if (!ref.current) return;
    const observer = new ResizeObserver(([entry]) => setSize({width:entry.contentRect.width,height:entry.contentRect.height}));
    observer.observe(ref.current);
    return () => observer.disconnect();
  }, []);
  const drag = useRef<{ origin: number; from: number; to: number; edge: string | undefined; bounds:[number,number] } | null>(null);
  const {width, height} = size, top = 22, plotHeight = Math.max(1,height-34);
  const x = (time: number) => 10 + axis.position(time) * (width-20);
  const groups = GROUPS.filter(group => group.sessions.length);
  const positions = new Map<string, {position:number; phase:number; group:number; color:string}>();
  groups.forEach((group, gi) => group.sessions.forEach((session, i) => {
    if (!positions.has(session.id)) positions.set(session.id, {position:(gi+.5+((i+.5)/group.sessions.length-.5)*.86)/groups.length, phase:i*2.399, group:(gi+.5)/groups.length, color:SOURCE.design ? WEAVE_COLORS[gi % WEAVE_COLORS.length] : group.color});
  }));
  const spineId = SOURCE.design ? EVENT_BY_ID.get(SOURCE.design.selectedEventId)?.sessionId : null;
  const roots = SOURCE.design ? [...new Set(SESSIONS.filter(session=>session.startedTs !== null && session.startedTs < SOURCE.design!.windows.dense[0]).map(session=>rootId(session.id)))] : [];
  const branches = new Map(roots.map(id=>[id,SESSIONS.filter(session=>rootId(session.id)===id && session.id!==id)]));
  const observedEncounters = SOURCE.design ? PROXIMITY_EXAMPLES.filter(item=>item.sourceEventIds.every(id=>EVENT_BY_ID.has(id)) && (cutoff===null || item.observedAt<=cutoff)) : [];
  const y = (id:string, time:number, groupId?:string) => {
    if (proximity) return top+plotHeight*proximityY(id,time,observedEncounters,groupId);
    const lane=positions.get(id);
    if (!lane) return top+plotHeight/2;
    if (SOURCE.design && time >= SOURCE.design.windows.dense[0]) {
      return top+plotHeight*(id===spineId ? .515 : fiberPosition(time,lane.position,lane.group,SOURCE.design.windows.dense));
    }
    if (SOURCE.design) {
      const root=rootId(id), children=branches.get(root) ?? [];
      const history=SOURCE.design.episodes.some(episode=>EVENT_BY_ID.get(episode.eventIds[0])?.sessionId===root);
      const band=(roots.indexOf(root)+(branchLane(children.findIndex(session=>session.id===id),history) ?? .5)*.86+.07)/roots.length;
      return top+plotHeight*band;
    }
    return top+plotHeight*fiberPosition(time,lane.position,lane.group,BOUNDS);
  };
  const color = (id:string, time:number) => SOURCE.design && time < SOURCE.design.windows.dense[0] ? designLaneColor(id) : id===spineId ? '#b9efff' : positions.get(id)?.color ?? '#8096a8';
  const visibleEvents = EVENTS.filter(event => event.ts !== null && event.ts>=mapBounds[0] && event.ts<=mapBounds[1] && (cutoff === null || event.ts <= cutoff));
  const proximityPath = (id:string,groupId:string|undefined,start:number,end:number,steps=32) => Array.from({length:steps+1},(_,step)=>{
    const time=start+(end-start)*step/steps;
    return `${step?'L':'M'}${x(time)} ${y(id,time,groupId)}`;
  }).join(' ');
  const lastObserved = new Map<string, number>();
  for (const event of visibleEvents) lastObserved.set(event.sessionId, event.ts!);
  function pointerTime(clientX: number) {
    const box = ref.current!.getBoundingClientRect();
    return (drag.current ? timeAxis(...drag.current.bounds) : axis).time((clientX-box.left-10)/(box.width-20));
  }
  function move(delta:number, start:number, end:number, edge?:string) {
    if (edge==='from') onWindow(Math.max(BOUNDS[0],Math.min(end-30,start+delta)),end);
    else if (edge==='to') onWindow(start,Math.min(BOUNDS[1],Math.max(start+30,end+delta)));
    else onWindow(start+delta,end+delta);
  }
  return <div className="journey-minimap" ref={ref} role="group" aria-label="Loaded snapshot minimap"
    onPointerDown={event => {
      if ((event.target as Element).closest('input,.journey-minimap-heading')) return;
      const time = pointerTime(event.clientX), edge = (event.target as HTMLElement).dataset.edge;
      const start = edge || (time >= from && time <= to) ? from : time - (to - from) / 2;
      drag.current = { origin: time, from: start, to: start + to - from, edge, bounds:[mapBounds[0],mapBounds[1]] };
      if (!edge) onWindow(start, start + to - from);
      event.currentTarget.setPointerCapture(event.pointerId);
    }}
    onPointerMove={event => { if (drag.current) move(pointerTime(event.clientX)-drag.current.origin,drag.current.from,drag.current.to,drag.current.edge); }}
    onPointerUp={() => { drag.current = null; }} onPointerCancel={() => { drag.current = null; }}>
    <div className="journey-minimap-heading"><span>MINIMAP</span><span title={mapScope==='scene'?'Loaded scene and visible time window':'Full currently loaded page'}>{stamp(mapBounds[0])}–{stamp(mapBounds[1])} UTC · {mapScope==='scene'?'SCENE':'PAGE'}</span>{denseRange ? <button type="button" onClick={changeScope} aria-label={mapScope==='scene'?'Show full loaded page in minimap':'Show current scene in minimap'}>{mapScope==='scene'?'FULL PAGE':'SCENE'}</button>:null}</div>
    <svg viewBox={`0 0 ${width} ${height}`} preserveAspectRatio="none" aria-hidden="true">
      <defs><filter id={glow} x="-10%" y="-50%" width="120%" height="200%"><feGaussianBlur stdDeviation="1"/><feMerge><feMergeNode/><feMergeNode in="SourceGraphic"/></feMerge></filter>
        {proximity ? observedEncounters.map((item,index)=><linearGradient key={item.id} id={`${glow}-proximity-${index}`} gradientUnits="userSpaceOnUse" x1={x(item.from)} x2={x(item.to)}><stop offset="0" stopColor={proximityTone(item)} stopOpacity="0"/><stop offset="28%" stopColor={proximityTone(item)}/><stop offset="72%" stopColor={proximityTone(item)}/><stop offset="100%" stopColor={proximityTone(item)} stopOpacity="0"/></linearGradient>) : null}
      </defs>
      <g filter={`url(#${glow})`}>
      {proximity ? PROXIMITY_STRANDS.map(strand=><path key={strand.key} data-proximity-session={strand.sessionId} d={proximityPath(strand.sessionId,strand.groupId,mapBounds[0],Math.min(mapBounds[1],cutoff ?? Infinity))} stroke={strand.color} strokeOpacity=".24" fill="none" strokeWidth=".3"/>) : !SOURCE.design ? groups.flatMap(group => group.sessions.map(session => {
        if (session.startedTs === null || (cutoff !== null && session.startedTs > cutoff)) return null;
        const start = session.startedTs, end = Math.min(session.endedTs ?? lastObserved.get(session.id) ?? start, cutoff ?? Infinity);
        const path = Array.from({ length: 25 }, (_, step) => { const t = start + (end - start) * step / 24; return `${step ? 'L' : 'M'}${x(t)},${y(session.id,t)}`; }).join(' ');
        return <path key={`${group.id}/${session.id}`} d={path} stroke={group.color} strokeOpacity=".4" strokeWidth=".65" strokeDasharray={session.endedTs === null || (cutoff !== null && session.endedTs > cutoff) ? '2 3' : undefined} fill="none" />;
      })) : SOURCE.design.relations.map(relation => {
        const a = EVENT_BY_ID.get(relation.from), b = EVENT_BY_ID.get(relation.to);
        if (a?.ts == null || b?.ts == null || Math.min(a.ts,b.ts)<mapBounds[0] || Math.max(a.ts,b.ts)>mapBounds[1] || (cutoff !== null && Math.max(a.ts, b.ts) > cutoff)) return null;
        const ax = x(a.ts), bx = x(b.ts), ay = y(a.sessionId,a.ts), by = y(b.sessionId,b.ts), bend = (bx-ax)/2;
        const dense = a.sessionId===b.sessionId && a.ts>=SOURCE.design!.windows.dense[0];
        const path = dense ? Array.from({length:17},(_,step)=>{const t=a.ts!+(b.ts!-a.ts!)*step/16;return `${step?'L':'M'}${x(t)},${y(a.sessionId,t)}`;}).join(' ') : `M${ax},${ay} C${ax+bend},${ay} ${bx-bend},${by} ${bx},${by}`;
        return <path key={relation.id} d={path} stroke={gradeColor(relation.grade,color(b.sessionId,b.ts))} strokeOpacity={dense ? '.3' : '.65'} strokeWidth=".65" strokeDasharray={gradeDash(relation.grade)} fill="none" />;
      })}
      {proximity ? observedEncounters.flatMap((item,index)=>item.agentSessions.map(id=><path key={`proximity/${item.id}/${id}`} data-proximity-encounter={item.id} d={proximityPath(id,proximityGroupForEncounter(item,id),item.from,item.to,24)} fill="none" stroke={`url(#${glow}-proximity-${index})`} strokeWidth="1.2"/>)) : null}
      {visibleEvents.map(event => <circle className="journey-map-event" data-event-id={event.id} data-time={event.ts} key={event.id} cx={x(event.ts!)} cy={y(event.sessionId,event.ts!,proximity ? proximityGroupForEvent(event.id) : undefined)} r={proximity ? .4 : width<500 ? .55 : 1.1} fill="#c3edf4" fillOpacity={proximity ? '.38' : '.85'} stroke={color(event.sessionId,event.ts!)} strokeWidth={proximity ? '.15' : width<500 ? .2 : .5} />)}
      </g>
      {!proximity && SOURCE.design?.coverageGaps.filter(gap => gap.from>=mapBounds[0] && gap.to<=mapBounds[1] && (cutoff === null || gap.to <= cutoff)).map(gap => <rect key={gap.eventId} x={x(gap.from)} y={y(gap.sessionId,(gap.from+gap.to)/2)-4} width={x(gap.to)-x(gap.from)} height="8" fill="#a981bb" fillOpacity=".06" stroke="#a981bb" strokeDasharray="2 3" strokeWidth=".65"/>)}
      <rect className="journey-map-viewport" x={x(from)} y="18" width={Math.max(2,x(to)-x(from))} height={height-24} stroke="#c3d5db" strokeWidth=".8" fill="#a3d9ef" fillOpacity=".035"/>
      <line x1={x(cursor)} x2={x(cursor)} y1="18" y2={height-6} stroke="#8ee5ff" strokeWidth=".8"/>
    </svg>
    <button className="journey-window" style={{left:`${x(from)}px`,width:`${Math.max(2,x(to)-x(from))}px`}} aria-label="Move visible time window; use left and right arrows" onKeyDown={event => {
      if (event.key !== 'ArrowLeft' && event.key !== 'ArrowRight') return;
      event.preventDefault(); move((to-from)*.1*(event.key==='ArrowLeft'?-1:1),from,to);
    }}/>
    {(['from','to'] as const).map(edge=><button key={edge} type="button" data-edge={edge} className="journey-map-handle" style={{left:`${x(edge==='from'?from:to)}px`}} aria-label={`Resize visible window ${edge==='from'?'start':'end'}; use left and right arrows`} onKeyDown={event=>{
      if (event.key!=='ArrowLeft' && event.key!=='ArrowRight') return;
      event.preventDefault(); move((to-from)*.1*(event.key==='ArrowLeft'?-1:1),from,to,edge);
    }}/>)}
    <input type="range" aria-label="Seek loaded snapshot" aria-valuetext={`${stamp(cursor)} UTC${cursor<mapBounds[0] || cursor>mapBounds[1] ? ' · outside minimap scope' : ''}`} min={mapBounds[0]} max={mapBounds[1]} step="1" value={Math.max(mapBounds[0],Math.min(mapBounds[1],cursor))} onChange={event => onSeek(Number(event.target.value))} />
  </div>;
}

/** The time strip counts revealed records; the separate minimap pans the loaded page. */
export function JourneyDensity({from, to, cursor, events, onSeek}: {
  from:number; to:number; cursor:number; events:PackLoomEvent[]; onSeek:(time:number)=>void;
}) {
  const axis = useMemo(() => timeAxis(from,to),[from,to]);
  const densityGlow = useId(), densityGrid = useId();
  const tickStep = to-from > 3600 ? 900 : 300;
  const ticks = [from, ...Array.from({length:Math.max(0,Math.ceil(to/tickStep)-Math.ceil(from/tickStep))},(_,i)=>(Math.ceil(from/tickStep)+i)*tickStep).filter(time=>time>from+60 && time<to-60), to];
  const inWindow = events.filter(event=>event.ts !== null && event.ts >= from && event.ts <= to);
  const groups = GROUPS.filter(group=>group.sessions.length);
  const rows = new Map<string,number>();
  groups.forEach((group,index)=>group.sessions.forEach(session=>{if(!rows.has(session.id)) rows.set(session.id,index);}));
  const rowHeight = Math.min(4,24 / Math.max(1,groups.length));
  const bins = new Map<string,{column:number; row:number; count:number}>();
  for (const event of inWindow) {
    const row=rows.get(event.sessionId);
    if(row === undefined) continue;
    const column=Math.min(179,Math.floor(axis.position(event.ts!)*180)), key=`${row}/${column}`;
    const bin=bins.get(key);
    if(bin) bin.count++; else bins.set(key,{column,row,count:1});
  }
  const x=(time:number)=>12+axis.position(time)*976;
  return <div className="journey-density" role="group" aria-label={`Event density: ${inWindow.length} revealed records in the visible window`}>
    <svg viewBox="0 0 1000 80" preserveAspectRatio="none" aria-hidden="true">
      <defs><filter id={densityGlow}><feGaussianBlur stdDeviation="1.5"/><feMerge><feMergeNode/><feMergeNode in="SourceGraphic"/></feMerge></filter><pattern id={densityGrid} width="8" height="5" patternUnits="userSpaceOnUse"><circle cx="1" cy="1" r=".4" fill="#647b8c" opacity=".55"/></pattern></defs>
      <rect x="12" y="21" width="976" height="26" fill={`url(#${densityGrid})`}/>
      {ticks.map((time,index)=><text key={time} x={x(time)} y="12" textAnchor={index===0?'start':index===ticks.length-1?'end':'middle'}>{stamp(time).slice(0,5)}</text>)}
      {groups.map((group,index)=><line key={group.id} x1="12" x2="988" y1={24+index*rowHeight} y2={24+index*rowHeight} stroke="#263643" strokeWidth=".5"/>)}
      <g filter={`url(#${densityGlow})`}>{[...bins.values()].map(bin=><line key={`${bin.row}/${bin.column}`} x1={12+bin.column/180*976} x2={12+bin.column/180*976} y1={22+bin.row*rowHeight} y2={22+bin.row*rowHeight+Math.max(1,rowHeight-1)} stroke={groups[bin.row].color} strokeWidth={bin.count>1?1.8:1.2} opacity={Math.min(1,.55+bin.count*.15)}/>)}</g>
      {cursor>=from && cursor<=to ? <><line x1={x(cursor)} x2={x(cursor)} y1="17" y2="57" stroke="#9cdcf4"/><circle cx={x(cursor)} cy="57" r="2" fill="#c1ecff"/><text x={Math.max(60,Math.min(940,x(cursor)))} y="72" textAnchor="middle" className="journey-density-time">{stamp(cursor)} UTC</text></> : null}
      {!inWindow.length ? <text x="500" y="39" textAnchor="middle">{SOURCE.events.length ? 'No revealed event records in this window' : 'Event spine unavailable · session metadata only'}</text> : null}
    </svg>
    <input type="range" aria-label="Seek visible event density" min={from} max={to} step="any" value={Math.max(from,Math.min(to,cursor))} onChange={event=>onSeek(Number(event.target.value))}/>
  </div>;
}
