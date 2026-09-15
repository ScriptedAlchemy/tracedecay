import type { EvidenceGrade } from './types';
import { SOURCE } from './source';
import { GROUPS, type JourneyNode } from './journey';
import { GRADE_LABEL } from './packScenes';

export function workstreamEvidence(nodes: JourneyNode[], sessionIds: Set<string>, selectedGrades: EvidenceGrade[]) {
  const records = nodes.filter(node => sessionIds.has(node.session));
  const grades = new Map<string, number>();
  if (SOURCE.design) {
    const events = new Set(records.flatMap(node => node.events.map(event => event.id)));
    for (const relation of SOURCE.design.relations) if ((!selectedGrades.length || selectedGrades.includes(relation.grade)) && events.has(relation.from) && events.has(relation.to)) grades.set(relation.grade, (grades.get(relation.grade) ?? 0) + 1);
  } else for (const node of records) grades.set(node.grade, (grades.get(node.grade) ?? 0) + node.events.length);
  const risks = new Set(records.flatMap(node => node.events.filter(event => SOURCE.design?.details[event.id]?.risk === 'high').map(event => event.id)));
  return {records, grades, risks};
}

export function OutcomeSummary({ nodes, sessionIds, onGroup, onSelect, grades: selectedGrades }: {
  grades: EvidenceGrade[]; nodes: JourneyNode[]; sessionIds: Set<string>;
  onGroup: (name: string, provider: string | null) => void; onSelect: (id: string) => void;
}) {
  return <div className="journey-outcomes" aria-label="Workstream outcomes and coverage" onPointerDown={event => event.stopPropagation()} onWheel={event => event.stopPropagation()}>
    {GROUPS.map(group => {
      const members = group.sessions.filter(session => sessionIds.has(session.id));
      if (!members.length) return null;
      const ids = new Set(members.map(session => session.id));
      const {records, grades, risks} = workstreamEvidence(nodes, ids, selectedGrades);
      const frontier = records.filter(node => ['result', 'commit', 'pr', 'gap'].includes(node.kind)).slice(-1)[0];
      return <section key={group.id} className="journey-outcome" style={{borderTopColor:group.color}}>
        <button className="journey-outcome-title" onClick={() => onGroup(group.name, null)} style={{color:group.color}}>{group.name} ↗</button>
        <p>{new Set(members.map(session => session.agentId).filter(Boolean)).size} identified agents · {members.length} participations</p>
        <p className={risks.size ? 'warn' : ''}>{SOURCE.design ? `${risks.size} illustrated high-risk records in window` : 'Risk assessment not exported'}</p>
        <p>{SOURCE.design ? 'Relations: ' : 'Event rows: '}{grades.size ? [...grades].map(([grade, count]) => `${count} ${GRADE_LABEL[grade as keyof typeof GRADE_LABEL].toLowerCase()}`).join(' · ') : 'Event evidence unavailable'}</p>
        {frontier ? <button className="journey-outcome-frontier" onClick={() => onSelect(frontier.id)}><small>Latest result / delivery record</small>{frontier.detail}</button> : <p className="journey-outcome-unavailable">Delivery outcome unavailable</p>}
        <small>{SOURCE.design ? 'Authored example · source grades remain separate' : 'Session identities do not establish delivery success'}</small>
      </section>;
    })}
  </div>;
}
