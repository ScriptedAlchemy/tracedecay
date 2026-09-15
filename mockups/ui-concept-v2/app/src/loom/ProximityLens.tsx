import { useId } from 'react';
import { useDemo } from '../app/workspace';
import { SOURCE } from './source';
import { EVENT_BY_ID, SESSION_BY_ID, stamp } from './journey';
import { PROXIMITY_EXAMPLES, PROXIMITY_STRANDS, proximityGroupForEncounter, proximityGroupForEvent, proximityTone, proximityY, type ProximityExample } from './proximity';
import { loomPivotUrl } from './pivots';
import './proximity.css';

const VIEW = { left: 116, right: 988, top: 24, bottom: 532 };

function status(encounter: ProximityExample, stale: boolean) {
  if (stale) return 'STALE · observation expired, not resolved';
  if (encounter.kind === 'conflict') return 'ILLUSTRATED CONFIRMED CONTENT CONFLICT';
  if (encounter.kind === 'neighborhood') return 'CODE NEIGHBORHOOD · no collision claim';
  return 'CANDIDATE · no conflict confirmed';
}

export function ProximityLens({from,to,cutoff,selected,onSelect,onSeek,onWindow}:{from:number;to:number;cutoff:number|null;selected:string|null;onSelect:(id:string|null)=>void;onSeek:(time:number)=>void;onWindow:(from:number,to:number)=>void}) {
  const { navigate } = useDemo(), gradientPrefix = useId().replaceAll(':','');
  const pivot = (surface:'code'|'work', session:string,event:string) => navigate(surface, Object.fromEntries(new URL(loomPivotUrl(surface,session,event),location.origin).searchParams));
  if (!SOURCE.design) return <section className="proximity-lens"><h3>PROXIMITY / SOURCE UNAVAILABLE</h3><p>This snapshot does not export current edited-path or code-neighborhood findings. No overlap, cross-worktree prediction or conflict has been inferred from session timing.</p></section>;

  const examples = PROXIMITY_EXAMPLES.filter(item => item.sourceEventIds.every(id => EVENT_BY_ID.has(id)) && (cutoff === null || item.observedAt <= cutoff));
  const encounter = examples.find(item => item.id === selected);
  const now = cutoff ?? to;
  const x = (time:number) => VIEW.left + (time-from)/Math.max(1,to-from)*(VIEW.right-VIEW.left);
  const y = (sessionId:string,time:number,groupId?:string) => VIEW.top + proximityY(sessionId,time,examples,groupId)*(VIEW.bottom-VIEW.top);
  const path = (sessionId:string,groupId:string|undefined,start=from,end=to,steps=64) => Array.from({length:steps+1},(_,step)=>{
    const time=start+(end-start)*step/steps;
    return `${step?'L':'M'}${x(time).toFixed(2)} ${y(sessionId,time,groupId).toFixed(2)}`;
  }).join(' ');
  const groups = SOURCE.design.groups.map(group => {
    const strands = PROXIMITY_STRANDS.filter(strand => strand.groupId === group.id);
    const ys = strands.map(strand => y(strand.sessionId,(from+to)/2,strand.groupId));
    return {...group, count:strands.length, center:(Math.min(...ys)+Math.max(...ys))/2};
  }).filter(group => group.count);

  if (encounter) {
    const stale = now > encounter.expiresAt, color = proximityTone(encounter);
    return <section className="proximity-lens proximity-detail" aria-label="Code proximity encounter detail">
      <header><b>PROXIMITY · RECORDED TIME</b><small>AUTHORED FIXTURE · proximity is not delegation or rejoin</small></header>
      <button className="loom-btn proximity-back" onClick={()=>onSelect(null)}>← BACK TO OVERVIEW</button>
      <div className="proximity-description"><b style={{color}}>{status(encounter,stale)}</b><span>{encounter.worktrees[0]===encounter.worktrees[1]?'SAME WORKTREE':'SEPARATE WORKTREES · no on-disk overwrite'}</span><h3>{encounter.label}</h3><p>{encounter.basis}</p><p>Source activity interval {stamp(encounter.from)} → {stamp(encounter.to)} UTC</p><p>{encounter.method} · observed {stamp(encounter.observedAt)} · expires {stamp(encounter.expiresAt)} UTC</p><button className="loom-btn" onClick={()=>onSeek(encounter.observedAt)}>REPLAY OBSERVATION</button> <button className="loom-btn" onClick={()=>onWindow(encounter.from-60,encounter.to+60)}>FOCUS LOCAL INTERVAL</button></div>
      <div className="proximity-pair">{encounter.agentSessions.map((session,i)=><article key={`${session}-${i}`}><b>{SESSION_BY_ID.get(session)?.agentId} · {encounter.access[i].toUpperCase()}</b><p>{stamp(EVENT_BY_ID.get(encounter.sourceEventIds[i])!.ts!)} UTC · source event</p><p>{encounter.worktrees[i]}<br/>{encounter.heads[i]}</p><code>{encounter.paths[i]}<br/>{encounter.ranges[i]}</code>{encounter.intent ? <p>{encounter.intent[i]}</p>:null}<a onClick={e=>{e.preventDefault();pivot('code',session,encounter.sourceEventIds[i]);}} href={loomPivotUrl('code',session,encounter.sourceEventIds[i])}>OPEN EXACT SOURCE IN CODE ↗</a><a onClick={e=>{e.preventDefault();pivot('work',session,encounter.sourceEventIds[i]);}} href={loomPivotUrl('work',session,encounter.sourceEventIds[i])}>OPEN TASK CONTEXT ↗</a></article>)}</div>
      <div className="proximity-unavailable">PRIVATE REASONING · UNAVAILABLE<br/>No intent, coordination, conflict, or duplicate-work claim is inferred from proximity alone.</div>
      {encounter.mergeBase ? <p>COMMON BASE · <code>{encounter.mergeBase}</code></p>:null}<p>{encounter.tests.join(' · ')}</p><small>{encounter.sourceRef}</small>
    </section>;
  }

  return <section className="proximity-lens" aria-label="Code proximity lens"><header><b>PROXIMITY · {SOURCE.agents.length} AGENTS · {PROXIMITY_STRANDS.length} WORKSTREAM PARTICIPATIONS</b><small>CONCEPT / SYNTHETIC DATA · private reasoning unavailable</small></header>
    <div className="proximity-overview">
      <div className="proximity-field-shell">
        <svg viewBox="0 0 1000 556" role="img" aria-label="Agent strands grouped by workstream with revealed proximity observations" className="proximity-field" preserveAspectRatio="none">
          <defs>
            <clipPath id={`${gradientPrefix}-window`}><rect x={VIEW.left} y="0" width={Math.max(0,Math.min(VIEW.right-VIEW.left,x(now)-VIEW.left))} height="556"/></clipPath>
            {examples.map(item=><linearGradient key={item.id} id={`${gradientPrefix}-${item.id}`} gradientUnits="userSpaceOnUse" x1={x(item.from)} x2={x(item.to)}>
              <stop offset="0" stopColor={proximityTone(item)} stopOpacity="0"/><stop offset="28%" stopColor={proximityTone(item)}/><stop offset="72%" stopColor={proximityTone(item)}/><stop offset="100%" stopColor={proximityTone(item)} stopOpacity="0"/>
            </linearGradient>)}
          </defs>
          {groups.map(group=><g key={group.id}><path className="proximity-bracket" d={`M98 ${group.center-18}h-7v36h7`}/><text className="proximity-group" x="6" y={group.center-3}>{group.name}</text><text className="proximity-count" x="6" y={group.center+11}>{group.count} agents</text></g>)}
          <g clipPath={`url(#${gradientPrefix}-window)`}>
            {PROXIMITY_STRANDS.map(strand=><path className="proximity-strand" key={strand.key} data-session-id={strand.sessionId} d={path(strand.sessionId,strand.groupId)} stroke={strand.color}><title>{SESSION_BY_ID.get(strand.sessionId)?.agentId ?? strand.sessionId} · {strand.groupName}</title></path>)}
            {examples.flatMap(item=>item.agentSessions.map(sessionId=><path className="proximity-signal" key={`${item.id}/${sessionId}`} data-encounter-id={item.id} d={path(sessionId,proximityGroupForEncounter(item,sessionId),item.from,item.to,24)} stroke={`url(#${gradientPrefix}-${item.id})`}/>))}
            {SOURCE.events.filter(event=>event.ts!==null && event.ts>=from && event.ts<=to && event.ts<=now).map(event=><circle className="proximity-event" key={event.id} data-event-id={event.id} cx={x(event.ts!)} cy={y(event.sessionId,event.ts!,proximityGroupForEvent(event.id))} r={1.2}><title>{event.kind} · {stamp(event.ts!)} UTC · {event.id}</title></circle>)}
            {examples.map(item=>{
              const time=(item.from+item.to)/2, cx=x(time), cy=(y(item.agentSessions[0],time,proximityGroupForEncounter(item,item.agentSessions[0]))+y(item.agentSessions[1],time,proximityGroupForEncounter(item,item.agentSessions[1])))/2;
              return <g key={`marker/${item.id}`} className="proximity-marker" role="button" tabIndex={0} aria-label={`Inspect ${item.label}`} onClick={()=>onSelect(item.id)} onKeyDown={event=>{if(event.key==='Enter'||event.key===' '){event.preventDefault();onSelect(item.id);}}}>
                <ellipse cx={cx} cy={cy} rx="18" ry="12"/><circle cx={cx} cy={cy} r="3.2" fill={proximityTone(item)}/><title>{item.label}</title>
              </g>;
            })}
          </g>
          <text className="proximity-time" x={VIEW.left} y="550">{stamp(from)} UTC</text><text className="proximity-time" x={VIEW.right} y="550" textAnchor="end">{stamp(to)} UTC</text>
        </svg>
      </div>
      <aside className="proximity-encounters"><h3>OBSERVED ENCOUNTERS · {examples.length}</h3>{examples.length ? examples.map(item=><button key={item.id} onClick={()=>onSelect(item.id)}><b style={{color:proximityTone(item)}}>{item.kind==='conflict'?'●':'▲'} {item.label.replace('Authored fixture · ','')}</b><span>{SESSION_BY_ID.get(item.agentSessions[0])?.agentId} + {SESSION_BY_ID.get(item.agentSessions[1])?.agentId}</span><span>{stamp(item.from)}–{stamp(item.to)} UTC · {item.worktrees[0]===item.worktrees[1]?'same worktree':'separate worktrees'}</span></button>) : <p>No proximity observations revealed at this replay time.</p>}<div className="proximity-unavailable">MISSING COVERAGE · UNKNOWN<br/>No proximity signal does not establish safe separation.</div></aside>
    </div>
  </section>;
}
