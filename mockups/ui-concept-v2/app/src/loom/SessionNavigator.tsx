import { useEffect, useRef, useState } from 'react';
import { shortId } from '../data/pack';
import {workstreamEvidence} from './OutcomeSummary';
import type {EvidenceGrade} from './types';
import { stamp, type JourneyNode } from './journey';
import { SOURCE, type JourneySession } from './source';

const ROW_HEIGHT = 88;
export function SessionNavigator({ nodes, grades, sessions, selected, pins, onSelect, onPin }: {
  nodes: JourneyNode[]; grades: EvidenceGrade[];
  sessions: JourneySession[]; selected: string | null; pins: string[];
  onSelect: (id: string) => void; onPin: (id: string) => void;
}) {
  const viewport = useRef<HTMLDivElement>(null);
  const [scroll, setScroll] = useState(0), [height, setHeight] = useState(400), [active, setActive] = useState(0);
  const previousSessions = useRef(sessions);
  const identity = sessions.map(s => s.id).join(',');
  useEffect(() => {
    const activeId = previousSessions.current[active]?.id;
    setActive(Math.max(0, sessions.findIndex(session => session.id === activeId)));
    const nextScroll = Math.min(viewport.current?.scrollTop ?? 0, Math.max(0, sessions.length * ROW_HEIGHT - height));
    viewport.current?.scrollTo({ top: nextScroll }); setScroll(nextScroll);
    previousSessions.current = sessions;
  }, [identity]);
  useEffect(() => {
    if (!viewport.current) return;
    const observer = new ResizeObserver(([entry]) => setHeight(entry.contentRect.height));
    observer.observe(viewport.current);
    return () => observer.disconnect();
  }, []);
  const start = Math.max(0, Math.floor(scroll / ROW_HEIGHT) - 2);
  const stop = Math.min(sessions.length, start + Math.ceil(height / ROW_HEIGHT) + 5);
  function move(index: number) {
    const next = Math.max(0, Math.min(sessions.length - 1, index));
    const el = viewport.current;
    setActive(next);
    if (el) {
      const top = next * ROW_HEIGHT;
      if (top < el.scrollTop || top + ROW_HEIGHT > el.scrollTop + height) {
        el.scrollTop = Math.max(0, top - height / 2); setScroll(el.scrollTop);
      }
      requestAnimationFrame(() => el.querySelector<HTMLButtonElement>(`[data-row="${next}"]`)?.focus());
    }
  }
  return <div className="journey-session-window" ref={viewport} role="group" aria-label="Recorded sessions"
    onScroll={e => setScroll(e.currentTarget.scrollTop)}
    onKeyDown={e => {
      const delta = e.key === 'ArrowDown' ? 1 : e.key === 'ArrowUp' ? -1 : e.key === 'PageDown' ? 5 : e.key === 'PageUp' ? -5 : 0;
      if (delta || e.key === 'Home' || e.key === 'End') {
        e.preventDefault(); e.stopPropagation(); move(e.key === 'Home' ? 0 : e.key === 'End' ? sessions.length - 1 : active + delta);
      }
    }}>
    {!sessions.length ? <p className="loom-note">No sessions match these filters.</p> : <div style={{ height: sessions.length * ROW_HEIGHT, position: 'relative' }}>
      {sessions.slice(start, stop).map((session, offset) => {
        const i = start + offset;
        const evidence = workstreamEvidence(nodes,new Set([session.id]),grades);
        const summary = `${evidence.records.reduce((sum,node)=>sum+node.events.length,0)} revealed · ${SOURCE.design ? 'relations' : 'rows'}: ${[...evidence.grades.keys()].join(' / ') || 'unavailable'}`;
        return <div key={session.id} className="journey-nav-entry" style={{ position: 'absolute', top: i * ROW_HEIGHT, left: 0, right: 0, height: ROW_HEIGHT }}>
          <button type="button" data-row={i} tabIndex={i === active ? 0 : -1} className={`loom-nav-row ${selected === session.id ? 'is-on' : ''}`}
            onFocus={() => setActive(i)} onClick={() => onSelect(session.id)} aria-label={`${session.title ?? session.id}, ${session.provider}, session ${i + 1} of ${sessions.length}, ${summary}`}>
            <b>{session.title ?? shortId(session.agentId ?? session.id, 21)}</b>
            <span>{session.provider} · {session.isSubagent ? 'subagent' : 'root session'}</span>
            <span>{session.startedTs === null ? 'Undated' : stamp(session.startedTs, true)} · {session.coverage}</span>
            <span className="journey-session-evidence" title={summary}>{summary}</span>
          </button>
          <button type="button" className="journey-pin" tabIndex={i === active ? 0 : -1} aria-label={`${pins.includes(session.id) ? 'Unpin' : 'Pin'} session ${session.id}`} aria-pressed={pins.includes(session.id)} onClick={() => onPin(session.id)}>⌖</button>
        </div>;
      })}
    </div>}
  </div>;
}
