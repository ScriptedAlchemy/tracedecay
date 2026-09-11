import { useState, type ReactNode } from 'react';
import { SOURCE, SOURCE_ID } from './source';
import { loomPivotUrl, PIVOT_SURFACES, type LoomPivot } from './pivots';
import './source-destination.css';

/** Inspect the selected source without mounting an unrelated profile's canvas. */
export function SourceDestination({pivot, children}: {pivot:LoomPivot; children?:ReactNode}) {
  const [native, setNative] = useState(Boolean(children));
  if (pivot.kind === 'unavailable') return <section className="loom-destination"><header><h1>SOURCE CONTEXT UNAVAILABLE</h1><a href={pivot.returnUrl}>RETURN TO LOOM</a></header><p role="alert">{pivot.reason}</p></section>;
  const {surface, session, event, cutoff, returnUrl} = pivot;
  const detail = event ? SOURCE.design?.details[event.id] : null;
  const visible = (time:number|null) => cutoff === null || (time !== null && time <= cutoff);
  const rows = SOURCE.events.filter(row => row.sessionId === session.id && visible(row.ts));
  const agents = [...new Set(SOURCE.agents.filter(row => row.sessionId === session.id).map(row => row.agentId))];
  const parents = SOURCE.sessions.filter(row => (row.id === session.parentId || row.parentId === session.id) && visible(row.startedTs));
  const ended = session.endedTs !== null && visible(session.endedTs);
  return <section className="loom-destination">
    <header><div><h1>{surface.toUpperCase()} / LOOM SOURCE</h1><p>{SOURCE.label} · {SOURCE.design ? 'CONCEPT / SYNTHETIC DATA' : 'PROFILE SNAPSHOT'}{cutoff !== null ? ` · replay through ${new Date(cutoff*1000).toISOString()}` : ''}</p></div><a href={returnUrl}>RETURN TO LOOM</a></header>
    <div className="loom-destination-identity"><span>SESSION <b>{session.id}</b></span><span>EVENT <b>{event?.id ?? 'No event selected'}</b></span><span>TIME <b>{event?.at ?? 'Event timestamp unavailable'}</b></span></div>
    <nav aria-label="Selected source destinations">{PIVOT_SURFACES.map(target=><a key={target} aria-current={target===surface?'page':undefined} href={loomPivotUrl(target,session.id,event?.id)}>{target.toUpperCase()}</a>)}{children ? <button type="button" aria-pressed={!native} onClick={()=>setNative(value=>!value)}>{native?'SOURCE DETAILS':'SURFACE VIEW'}</button> : null}</nav>
    {native && children ? <div className="loom-destination-native">{children}</div> : <div className="loom-destination-details">
      <section className="loom-destination-record"><h2>{surface==='code'?'CODE & IMPACT':surface==='delivery'?'DELIVERY EVIDENCE':surface==='work'?'WORK & DELEGATION':surface==='agents'?'AGENT IDENTITIES':'SESSION & TRANSCRIPT'}</h2>
        <dl><div><dt>PROJECT</dt><dd>{session.project}</dd></div><div><dt>PROVIDER</dt><dd>{session.provider}</dd></div><div><dt>SESSION START</dt><dd>{session.startedAt ?? 'Unavailable'}</dd></div><div><dt>SESSION END</dt><dd>{ended?session.endedAt:session.endedTs===null?'Unavailable':'Not revealed at this replay cursor'}</dd></div></dl>
        {surface==='agents' ? <><h3>ASSOCIATED AGENT IDS</h3>{agents.length ? <ul>{agents.map(id=><li key={id}>{id}</li>)}</ul>:<p className="loom-destination-unavailable">No agent association was exported for this session.</p>}<p>These are source associations. They do not establish authorship of another event or expose private reasoning.</p></> : null}
        {surface==='sessions' ? <><h3>{SOURCE.design?'ILLUSTRATED SOURCE EXCERPT':'TRANSCRIPT COVERAGE'}</h3>{detail?.body?<pre>{detail.body}</pre>:<p className="loom-destination-unavailable">{SOURCE_ID==='ubuntu'?'This profile exports session identities only. No event spine or transcript body is available.':'The selected source has no transcript body for this event.'}</p>}</> : null}
        {surface==='work' ? <><h3>TASK REFERENCE</h3>{detail?.task?<p>{detail.task} · authored event field; no independent task record supplied.</p>:<p className="loom-destination-unavailable">No event-to-task record is exported. Session parentage is available separately below.</p>}</> : null}
        {surface==='code' ? <><h3>{detail?.file ?? 'FILE IDENTITY UNAVAILABLE'}</h3>{detail?.file?<p>File path supplied by the authored event. No repository filesystem or symbol index is loaded.</p>:<p className="loom-destination-unavailable">This event has no source-backed file, hunk, or symbol join.</p>}{detail?.diff?<pre className="loom-destination-diff" tabIndex={0}>{detail.diff.split('\n').map((line,i)=><span key={i} className={line.startsWith('+')?'added':line.startsWith('-')?'removed':''}>{line}{'\n'}</span>)}</pre>:<p className="loom-destination-unavailable">Diff / impact evidence unavailable in this source.</p>}</> : null}
        {surface==='delivery' ? <><h3>PROVIDER OUTCOME UNAVAILABLE</h3><p className="loom-destination-unavailable">No exact PR, review, deployment, or provider outcome record is linked by this source. A commit or transcript marker alone does not prove delivery.</p>{detail?.body?<pre>{detail.body}</pre>:null}</> : null}
        {['sessions','agents','work'].includes(surface)?<><h3>RECORDED SESSION PARENTAGE</h3><p>Parent reference: {session.parentId ?? 'None recorded'}</p>{parents.length?<ul>{parents.map(row=><li key={row.id}><a href={loomPivotUrl(surface, row.id)}>{row.id}</a> · {row.id===session.parentId?'parent':'child'}</li>)}</ul>:<p>No related session row is revealed in this source. A parent reference is not an evidenced handoff.</p>}</>:null}
        <details open><summary>{event?'EXACT EVENT SOURCE':'REVEALED SESSION FIELDS'}</summary><pre data-testid="loom-destination-source" tabIndex={0}>{JSON.stringify(event ?? {id:session.id,provider:session.provider,project:session.project,startedAt:session.startedAt,parentId:session.parentId,agentIds:agents,coverage:session.coverage},null,2)}</pre></details>
      </section>
      <aside><h2>REVEALED SESSION EVENTS</h2><p>{rows.length} records · same source and replay boundary</p>{rows.length?<ol>{rows.map(row=><li key={row.id}><a aria-current={row.id===event?.id?'true':undefined} href={loomPivotUrl(surface,session.id,row.id)}><b>{SOURCE.design?.details[row.id]?.title ?? row.tool ?? row.kind}</b><small>{row.at ?? 'Undated'}</small></a></li>)}</ol>:<p className="loom-destination-unavailable">No event rows available in this scope.</p>}</aside>
    </div>}
  </section>;
}
