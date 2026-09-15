import { useEffect, useMemo, useRef, useState } from 'react';
import { AUTOMATIONS_ATTENTION } from '../automations/attention';
import { DELIVERY_ATTENTION } from '../delivery/attention';
import { OBSERVATORY_ATTENTION } from '../observatory/attention';
import { WORKFLOW_ATTENTION } from '../workflows/attention';
import { PROXIMITY_EXAMPLES } from '../loom/proximity';
import { FIXTURE_GRAPH, blockingRelations, isValidWorkGraph, type WorkGraph } from '../work/model';
import { attentionSignature, useAttentionMarks, useDemo, useWorkspaceState, type AttentionItem, type AttentionMark, type DemoMode } from './workspace';
import './workspace.css';

const COVERAGE: AttentionItem[] = [
  { id:'coverage:knowledge', title:'Knowledge facts are not included in this export', detail:'The captured profile has no served fact table. Independent Git structure remains available; no conclusion about missing or false claims can be made.', source:'coverage', severity:'information', status:'active', owner:'system', evidence:'unavailable', mode:'snapshot', observedAt:null, sourceRef:'profile-pack / facts coverage', target:{surface:'knowledge',params:{knowledge_camera:'facts'}} },
  { id:'coverage:work', title:'Planned Work graph is unavailable', detail:'Recorded sessions and delegation are available separately. They do not establish task dependencies or readiness.', source:'coverage', severity:'information', status:'active', owner:'system', evidence:'unavailable', mode:'snapshot', observedAt:null, sourceRef:'profile-pack / canonical Work coverage', target:{surface:'work',params:{}} },
];
const LOOM_ATTENTION: AttentionItem[] = PROXIMITY_EXAMPLES.filter(example => example.kind === 'conflict').map(example => ({
  id: example.id, title: 'Fixture · Git conflict between worktrees',
  detail: `${example.basis} Observation validity ends ${new Date(example.expiresAt * 1000).toISOString()}; expiry does not establish resolution.`,
  source: 'proximity', severity: 'error', status: 'active', owner: 'you', evidence: 'explicit', mode: 'fixture',
  observedAt: new Date(example.observedAt * 1000).toISOString(), sourceRef: `${example.sourceRef} / ${example.id}`,
  target: {surface: 'loom', params: {state: '04', loom_lens: 'proximity', loom_encounter: example.id, loom_replay: '1', loom_time: String(example.observedAt + 15), loom_from: String(example.observedAt - 80), loom_to: String(example.expiresAt + 80)}},
}));
const ITEMS = [...DELIVERY_ATTENTION, ...AUTOMATIONS_ATTENTION, ...OBSERVATORY_ATTENTION, ...WORKFLOW_ATTENTION, ...LOOM_ATTENTION, ...COVERAGE];
const ORDER = {error:0,warning:1,information:2};

function workAttention(graph: WorkGraph): AttentionItem[] {
  return graph.tasks.filter(task=>['failed','blocked','waiting'].includes(task.status)).map(task=>{
    const blockers = blockingRelations(graph, task.id);
    const latest = graph.events.filter(event=>event.taskId===task.id).at(-1);
    return {
      id:`fixture:work:${task.id}`, title:`Fixture · ${task.status} task: ${task.title}`,
      detail:`${task.id} is ${task.status}. ${blockers.length ? `Gating predecessors: ${blockers.map(edge=>edge.from).join(', ')}.` : 'No incomplete gating predecessor.'} ${latest ? `Latest authored event: ${latest.event} at ${latest.at}; calendar date unavailable.` : 'No observed task event is included.'} This is the current local task condition; prior attempt evidence remains in Work.`,
      source:'work', severity:task.status==='failed'?'error':'warning', status:'active', owner:'unknown',
      evidence:'explicit', mode:'fixture', observedAt:null, sourceRef:`fixture:work:${task.id}`,
      target:{surface:'work',params:{work_task:task.id}},
    };
  });
}

export function useAttentionItems() {
  const { mode } = useDemo();
  const [storedWorkGraph] = useWorkspaceState<WorkGraph>('work.fixture.graph', FIXTURE_GRAPH);
  const workGraph = isValidWorkGraph(storedWorkGraph) ? storedWorkGraph : FIXTURE_GRAPH;
  return useMemo(()=>[...new Map([...ITEMS, ...(mode==='fixture' ? workAttention(workGraph) : [])]
    .filter(item=>item.mode===mode).map(item=>[item.id,item])).values()]
    .sort((a,b)=>ORDER[a.severity]-ORDER[b.severity] || a.id.localeCompare(b.id)),[mode,workGraph]);
}

export function WorkspaceControls() {
  const { mode, setMode, navigate } = useDemo();
  const items = useAttentionItems();
  const [marks,setMarks] = useAttentionMarks();
  const [open,setOpen] = useState(false);
  const [filter,setFilter] = useState('all');
  const [selected,setSelected] = useState<string|null>(null);
  const dialog = useRef<HTMLDialogElement>(null);
  const markFor = (item:AttentionItem): AttentionMark | undefined => marks[item.id]?.signature === attentionSignature(item) ? marks[item.id] : undefined;
  const active = items.filter(item=>item.status==='active');
  const unacknowledged = active.filter(item=>!markFor(item)?.acknowledged && !markFor(item)?.snoozed);
  const visible = items.filter(item=>filter==='all' || (filter==='snoozed' ? markFor(item)?.snoozed : item.owner===filter));
  const current = visible.find(item=>item.id===selected) ?? visible[0];
  const next = unacknowledged.find(item=>item.owner==='you' && !markFor(item)?.seen)
    ?? unacknowledged.find(item=>!markFor(item)?.seen)
    ?? unacknowledged[0];
  function mark(item:AttentionItem, patch:Partial<Omit<AttentionMark,'signature'>>) {
    setMarks(previous=>({ ...previous, [item.id]:{seen:false,acknowledged:false,snoozed:false,...(previous[item.id]?.signature===attentionSignature(item)?previous[item.id]:{}),...patch,signature:attentionSignature(item)} }));
  }
  function visit(item:AttentionItem) {
    mark(item,{seen:true});
    navigate(item.target.surface,{...item.target.params,attention:item.id});
  }
  useEffect(()=>{
    if(open && !dialog.current?.open) dialog.current?.showModal();
    if(!open && dialog.current?.open) dialog.current.close();
  },[open]);
  return <div className="workspace-controls">
    <label className="workspace-mode-label">Data source
      <select aria-label="Data source" value={mode} onChange={event=>setMode(event.target.value as DemoMode)}>
        <option value="snapshot">Recorded snapshots</option>
        <option value="fixture">Design fixtures</option>
      </select>
    </label>
    <button type="button" className="workspace-attention" onClick={()=>setOpen(true)} aria-label={`Attention: ${active.length} active sources`}>
      <span className="attention-dot" data-severity={active[0]?.severity} />
      <span className="workspace-control-label">Attention</span><b>{active.length}</b>
    </button>
    <button type="button" className="workspace-next" disabled={!next} onClick={()=>next&&visit(next)} title={next?.title ?? 'No source currently assigns an action to you'}>
      {next?.owner==='you'?'Next needs me':'Next to inspect'} <span aria-hidden="true">↗</span>
    </button>
    <dialog ref={dialog} className="attention-dialog" onClose={()=>setOpen(false)} aria-labelledby="attention-heading">
      <header><div><h2 id="attention-heading">Attention / source evidence</h2><span>{mode==='fixture'?'Authored examples':'Recorded snapshots'} · {active.length} distinct active sources · {unacknowledged.length} unacknowledged</span></div><button type="button" onClick={()=>setOpen(false)} aria-label="Close attention">×</button></header>
      <div className="attention-toolbar">
        <label>Data <select aria-label="Attention data source" value={mode} onChange={event=>setMode(event.target.value as DemoMode)}><option value="snapshot">Recorded snapshots</option><option value="fixture">Design fixtures</option></select></label>
        <label>Focus <select aria-label="Attention owner" value={filter} onChange={event=>setFilter(event.target.value)}>
          <option value="all">All sources</option><option value="you">Needs you</option><option value="agent">Agent</option><option value="external">External review</option><option value="unknown">Unassigned / candidates</option><option value="system">System / coverage</option><option value="snoozed">Snoozed</option>
        </select></label>
        <span>Seen and acknowledged do not mean resolved.</span>
      </div>
      <div className="attention-body">
        <div className="attention-list" aria-label="Attention sources">
          {visible.map(item=><button type="button" key={item.id} className={item.id===current?.id?'selected':''} aria-pressed={item.id===current?.id} onClick={()=>{setSelected(item.id);mark(item,{seen:true});}}>
            <span className="attention-dot" data-severity={item.severity}/><span><strong>{item.title}</strong><small>{item.source} · {item.evidence} · {markFor(item)?.snoozed?'snoozed':markFor(item)?.acknowledged?'acknowledged':markFor(item)?.seen?'seen':'unseen'}</small></span>
          </button>)}
          {!visible.length&&<p>No recorded sources in this focus.</p>}
        </div>
        {current&&<section className="attention-detail" aria-label="Selected attention evidence">
          <div className="attention-condition" data-severity={current.severity}>{current.status} / {current.severity}</div>
          <h3>{current.title}</h3><p>{current.detail}</p>
          <dl><dt>Awaited actor</dt><dd>{current.owner}</dd><dt>Evidence</dt><dd>{current.evidence}</dd><dt>Observed</dt><dd>{current.observedAt??'Exact observation time unavailable'}</dd><dt>Source identity</dt><dd>{current.sourceRef}</dd></dl>
          <div className="attention-actions">
            <button type="button" onClick={()=>visit(current)}>Show exact context ↗</button>
            <button type="button" onClick={()=>mark(current,{seen:true,acknowledged:!markFor(current)?.acknowledged})}>{markFor(current)?.acknowledged?'Unacknowledge':'Acknowledge'}</button>
            <button type="button" onClick={()=>mark(current,{snoozed:!markFor(current)?.snoozed})}>{markFor(current)?.snoozed?'Unsnooze':'Snooze until evidence changes'}</button>
          </div>
          <p className="attention-footnote">Local attention marks persist for this browser session. The source condition remains {current.status}; no provider mutation is sent.</p>
        </section>}
      </div>
    </dialog>
  </div>;
}
