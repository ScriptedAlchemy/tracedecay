import { useDemo, useWorkspaceState } from "../app/workspace";
import { EVIDENCE_MODES, type EvidenceMode, type EvidenceLayout } from './journey';
import { gradeColor, gradeDash } from './WeaveField';
import { useEffect, useRef, useState } from "react";
import type { CSSProperties, KeyboardEvent } from "react";
import type { EvidenceGrade } from "./types";
import { GAP_LEDGER, GRADE_LABEL, KIND_LABEL, eventGrade, eventKind } from "./packScenes";
import { PACK, shortId, type PackLoomEvent } from "../data/pack";
import "./workspaces.css";
import { SOURCE, SOURCE_ID } from "./source";
import { loomPivotUrl, PIVOT_SURFACES } from "./pivots";

function navigateTabs(event: KeyboardEvent<HTMLDivElement>) {
  if (!["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key)) return;
  const tabs = [...event.currentTarget.querySelectorAll<HTMLButtonElement>('[role="tab"]')];
  const index = tabs.indexOf(event.target as HTMLButtonElement);
  if (index < 0) return;
  const next = event.key === "Home" ? 0 : event.key === "End" ? tabs.length - 1 : (index + (event.key === "ArrowRight" ? 1 : -1) + tabs.length) % tabs.length;
  event.preventDefault(); tabs[next].focus(); tabs[next].click();
}

function Grade({ g }: { g: EvidenceGrade }) {
  return <span style={{color:gradeColor(g),borderColor:gradeColor(g)}} className={`loom-grade g-${g}`}>{GRADE_LABEL[g]}</span>;
}

function visible(event: PackLoomEvent, cutoff: number | null) {
  return cutoff === null || (event.ts !== null && event.ts <= cutoff);
}
function sequence(event: PackLoomEvent, cutoff: number | null) {
  return SOURCE.events.filter(e => e.sessionId === event.sessionId && visible(e, cutoff))
    .sort((a, b) => (a.ordinal ?? Infinity) - (b.ordinal ?? Infinity) || (a.ts ?? Infinity) - (b.ts ?? Infinity));
}
function EventButton({ event, onSelect }: { event: PackLoomEvent; onSelect: (id: string) => void }) {
  return <button type="button" className="loom-neighbor" onClick={() => onSelect(event.id)}>
    <b>{SOURCE.design?.details[event.id]?.title ?? event.tool ?? KIND_LABEL[eventKind(event)]}</b><span>{event.at ?? "Undated record"}</span>
    <small>ordinal {event.ordinal ?? "unavailable"} · <Grade g={SOURCE.design && event.kind === "decision" ? "explicit" : eventGrade(event)} /></small>
  </button>;
}

function DesignNeighborhood({ event, cutoff, onSelect, highlighted }: { event: PackLoomEvent; cutoff: number | null; onSelect: (id: string) => void; highlighted?: string | null }) {
  const design = SOURCE.design!;
  const related = design.relations.filter(r => (r.from === event.id || r.to === event.id) && [r.from, r.to].every(id => design.events.some(e => e.id === id && visible(e, cutoff))));
  const inbound = related.filter(r => r.to === event.id), outbound = related.filter(r => r.from === event.id);
  const height = Math.max(174, Math.max(inbound.length, outbound.length) * 31);
  const y = (i: number, count: number) => height * (i + .5) / count;
  const card = (relation: typeof related[number], incoming: boolean) => {
    const id = incoming ? relation.from : relation.to;
    const item = design.events.find(e => e.id === id)!;
    return <button key={relation.id} type="button" className={`loom-design-relation g-${relation.grade}${highlighted === id ? " is-inspected" : ""}`} aria-label={`${design.details[id].title} · ${item.at?.slice(11, 23)} · ${relation.grade}`} title={relation.sourceRef} onClick={() => onSelect(id)}>
      <b title={design.details[id].title}>{design.details[id].title.split(" · ")[0]}</b><span>{item.at?.slice(11, 23)}</span><small><Grade g={relation.grade} /></small>
    </button>;
  };
  return <><div className="loom-design-neighborhood" style={{ minHeight: height }} aria-label="Illustrated causal neighborhood">
    <svg viewBox={`0 0 600 ${height}`} preserveAspectRatio="none" aria-hidden="true"><defs><filter id="loomRelationGlow"><feGaussianBlur stdDeviation="2" /></filter>{Object.keys(GRADE_LABEL).map(grade => <marker key={grade} id={`loom-arrow-${grade}`} markerWidth="5" markerHeight="5" refX="4" refY="2.5" orient="auto" markerUnits="userSpaceOnUse"><path d="M0 0 L5 2.5 L0 5 Z" fill={gradeColor(grade as EvidenceGrade)} /></marker>)}</defs>
      {[...inbound.map((r, i) => ({ r, d: `M180 ${y(i, inbound.length)} C225 ${y(i, inbound.length)} 215 ${height / 2} 252 ${height / 2}` })), ...outbound.map((r, i) => ({ r, d: `M348 ${height / 2} C385 ${height / 2} 375 ${y(i, outbound.length)} 420 ${y(i, outbound.length)}` }))].map(({ r, d }) => <g key={r.id}><path d={d} stroke={gradeColor(r.grade)} strokeWidth="4" strokeDasharray={gradeDash(r.grade)} opacity=".13" fill="none" filter="url(#loomRelationGlow)" /><path d={d} stroke={gradeColor(r.grade)} strokeWidth="1" fill="none" strokeDasharray={gradeDash(r.grade)} markerEnd={`url(#loom-arrow-${r.grade})`} /></g>)}
    </svg>
    <div className="loom-design-side">{inbound.map(r => card(r, true))}</div><div className="loom-selected-record"><b>{design.details[event.id].title}</b><span>{event.at?.slice(11, 23)}</span><small>ILLUSTRATED EVENT</small></div><div className="loom-design-side">{outbound.map(r => card(r, false))}</div>
  </div>{related.length === 0 && <p className="loom-note">No relations revealed at this replay time.</p>}<p className="loom-note loom-relation-key">Authored relations · solid exact/explicit · dashed inferred or limited · not profile observations</p></>;
}

function DiffLines({ diff, onReview, selected = null }: { diff: string; onReview: (line: number) => void; selected?: number | null }) {
  return <pre className="loom-pre loom-design-diff" tabIndex={0}>{diff.split('\n').map((line, i) => <span key={i} className={`loom-diff-line ${line.startsWith('+') ? 'd-add' : line.startsWith('-') ? 'd-del' : 'd-ctx'}`}><button type="button" aria-label={`Review illustrated diff line ${i + 1}`} aria-pressed={selected === i} onClick={() => onReview(i)}>+</button>{line}{'\n'}</span>)}</pre>;
}

function WorkspacePivot({ surface, event }: { surface: typeof PIVOT_SURFACES[number]; event: PackLoomEvent }) {
  const { navigate } = useDemo();
  const href = loomPivotUrl(surface, event.sessionId, event.id);
  return <a href={href} onPointerDown={pointer => { pointer.currentTarget.href = loomPivotUrl(surface, event.sessionId, event.id); }} onFocus={focus => { focus.currentTarget.href = loomPivotUrl(surface, event.sessionId, event.id); }} onClick={click => {
    const currentHref = loomPivotUrl(surface, event.sessionId, event.id);
    click.currentTarget.href = currentHref;
    if (click.button !== 0 || click.metaKey || click.ctrlKey || click.shiftKey || click.altKey) return;
    click.preventDefault();
    navigate(surface, Object.fromEntries(new URL(currentHref, location.origin).searchParams));
  }}>OPEN {surface.toUpperCase()} ↗</a>;
}

function LinkedEvidence({ event, cutoff, inspected, onInspect, onSelect }: { event: PackLoomEvent; cutoff: number | null; inspected: string | null; onInspect: (id: string) => void; onSelect: (id: string) => void }) {
  const design = SOURCE.design!;
  const links = design.relations.filter(r => (r.from === event.id || r.to === event.id) && [r.from, r.to].every(id => design.events.some(e => e.id === id && visible(e, cutoff))));
  const related = links.flatMap(relation => {
    const source = design.events.find(e => e.id === (relation.from === event.id ? relation.to : relation.from));
    if (!source || !["decision", "file-edit", "test", "result"].includes(source.kind)) return [];
    return [{source, relation}];
  });
  const active = related.find(item => item.source.id === inspected);
  const excerpt = useRef<HTMLElement>(null);
  useEffect(() => { if (active) excerpt.current?.scrollIntoView({ block: "nearest", behavior: "instant" }); }, [active?.source.id]);
  return <section className="loom-linked-evidence" aria-label="Statement change and check evidence">
    <h4>STATEMENT · CHANGE · CHECK</h4><p className="loom-note">Inspect linked source while keeping the selected event and camera. Relationship grades are separate from source contents.</p>
    <div className="loom-linked-stages"><button type="button" aria-pressed={!active} onClick={() => onInspect(event.id)}>SELECTED SOURCE</button>{related.map(({source,relation}) => <button type="button" key={relation.id} aria-pressed={active?.source.id === source.id} onClick={() => onInspect(source.id)}><span>{source.kind === "decision" ? "STATEMENT" : source.kind === "file-edit" ? "CHANGE" : "CHECK"}</span><b>{design.details[source.id].title}</b><Grade g={relation.grade} /></button>)}</div>
    {active ? <article ref={excerpt} className="loom-linked-excerpt"><header><b>{design.details[active.source.id].title}</b><time>{active.source.at}</time><button type="button" onClick={() => onSelect(active.source.id)}>OPEN THIS EVENT</button></header><pre className="loom-pre" tabIndex={0}>{design.details[active.source.id].body ?? design.details[active.source.id].diff ?? "No authored source text supplied for this event."}</pre><details><summary>Relationship provenance</summary><p>{active.relation.kind} · <Grade g={active.relation.grade} /></p><code>{active.relation.sourceRef}</code><p className="loom-note">{active.relation.from} → {active.relation.to}</p></details></article> : <p className="loom-note">Selected source remains above. {related.length ? "Choose a linked statement or check to compare its exact authored contents." : "No revealed statement, change or check relation is supplied; chronology does not fill this gap."}</p>}
  </section>;
}

export function EvidenceWorkspace({ layout, onLayout, mode, onMode, event, cutoff, onSelect, onFeedback, onGap, onTable }: {
  layout: EvidenceLayout; onLayout: (layout: EvidenceLayout) => void;
  mode: EvidenceMode; onMode: (mode: EvidenceMode) => void;
  event: PackLoomEvent | null; cutoff: number | null; onSelect: (eventId: string) => void;
  onFeedback: (line?: number) => void; onGap: () => void; onTable: () => void;
}) {
  const {tab, focus, width:split} = layout;
  const [inspected, setInspected] = useWorkspaceState<string | null>(`loom:${SOURCE_ID}:${SOURCE.page ?? "snapshot"}:${event?.id ?? "none"}:linked-evidence`, null);
  if (!event || !visible(event, cutoff)) return <section className="loom-panel"><h3>SELECT AN EVENT</h3><p>Select a recorded event in the weave or exact event table to inspect its source.</p><button type="button" onClick={onTable}>OPEN EXACT EVENT TABLE</button></section>;
  const detail = SOURCE.design?.details[event.id];
  const session = SOURCE.sessions.find(s => s.id === event.sessionId);
  const rows = sequence(event, cutoff), index = rows.findIndex(e => e.id === event.id);
  const before = rows.slice(Math.max(0, index - 3), index), after = rows.slice(index + 1, index + 4);
  const marker = PACK.prMarkers.find(m => m.sessionId === event.sessionId && m.messageId === event.id);
  return <div data-mode={mode} className={`loom-evidence-workspace${detail ? " is-design" : ""}`}>
    <div className="loom-workspace-tools"><div className="loom-evidence-modes" role="group" aria-label="Evidence modes">{Object.entries(EVIDENCE_MODES).map(([key,label]) => <button key={key} type="button" aria-pressed={mode === key} onClick={() => onMode(key as EvidenceMode)}>{label}</button>)}<button type="button" onClick={() => onFeedback()}>FEEDBACK</button></div><button type="button" aria-pressed={focus} onClick={() => onLayout({...layout,focus:!focus})}>{focus ? "RESTORE SPLIT" : "FOCUS EXACT SOURCE"}</button>
      {!focus && <label>Pane width <input aria-label="Evidence pane width" type="range" min="35" max="65" value={split} onChange={e => onLayout({...layout,width:Number(e.target.value)})} /></label>}
      <button type="button" onClick={onGap}>EVIDENCE GAPS</button></div>
    <div className={`loom-ws loom-evidence-split${focus ? " source-focus" : ""}`} style={{ "--source-width": `${split}%` } as CSSProperties}>
      <section className="loom-panel">
        <h3>{mode === "story" ? "EVENT STORY & SOURCE" : mode === "code" ? "CODE & IMPACT" : "EVENT & SOURCE"}</h3><div className="loom-ev-head"><b className="loom-ev-title">{detail?.title ?? event.tool ?? event.kind}</b>{detail ? <span className="loom-design-boundary">DESIGN ILLUSTRATION</span> : <Grade g="exact" />}{detail && <span className="loom-evidence-id" title={event.id} aria-label={`Event ID ${event.id}`}>{event.id}</span>}</div>
        {detail ? <dl className="loom-kv wide loom-evidence-meta">
          <div><dt>TIME</dt><dd title={event.at ?? undefined} aria-label={event.at ?? 'Timestamp unavailable'}>{event.at?.slice(11, 23) ?? 'Unavailable'}</dd></div>
          <div><dt>AGENT</dt><dd title={session?.agentId ?? undefined}>{session?.agentId?.replace('design:', '') ?? 'Unavailable'}</dd></div>
          <div><dt>TASK</dt><dd title={detail.task}>{detail.task ?? 'No illustrated task'}</dd></div>
          <div><dt>SESSION</dt><dd title={event.sessionId} aria-label={event.sessionId}>{event.sessionId.replace('design:session:', '')}</dd></div>
          <div className="loom-evidence-file"><dt>FILE</dt><dd title={detail.file}>{detail.file ?? 'No illustrated file'}</dd></div>
        </dl> : <>        <dl className="loom-kv wide">
          <div><dt>EVENT ID</dt><dd>{event.id}</dd></div><div><dt>TIME / ORDINAL</dt><dd>{event.at ?? "Timestamp unavailable"} · {event.ordinal ?? "unavailable"}</dd></div>
          <div><dt>SESSION</dt><dd>{event.sessionId}</dd></div><div><dt>PROJECT / PROVIDER</dt><dd>{session ? `${session.project} · ${session.provider}` : "Session row unavailable"}</dd></div>
          <div><dt>AGENT / ROLE</dt><dd>{session?.agentId ?? "Agent identity unavailable"} · {event.role ?? "role unavailable"}</dd></div>
          <div><dt>TASK / WORKTREE</dt><dd>No event-to-task or worktree join exported</dd></div>
        </dl>
</>}
        {!detail && <><h4>EXACT EXPORTED EVENT ROW <Grade g="exact" /></h4><pre tabIndex={0} className="loom-pre" data-testid="loom-event-source">{JSON.stringify(event, null, 2)}</pre></>}
        {detail ? <><div className="loom-story-content"><h4>ILLUSTRATED TRANSCRIPT / SOURCE</h4>{detail.body ? <pre className="loom-pre loom-design-body" tabIndex={0}>{detail.body}</pre> : <div className="loom-absent"><b>No authored body for this illustrative event</b></div>}</div><div className="loom-code-content"><h4>ILLUSTRATED CODE &amp; IMPACT</h4>{detail.diff ? <DiffLines diff={detail.diff} onReview={onFeedback} /> : <div className="loom-absent"><b>No authored diff for this illustrative event</b><span>Source relationships are available in the neighborhood only where explicitly authored.</span></div>}</div></> : <><div className="loom-story-content"><h4>TRANSCRIPT EXCERPT <Grade g="unavailable" /></h4><div className="loom-absent"><b>MESSAGE BODY NOT IN SNAPSHOT</b><span>The export contains event metadata only. It does not contain transcript text, full tool payloads, or private reasoning.</span></div></div><div className="loom-code-content"><h4>CODE &amp; IMPACT <Grade g="unavailable" /></h4><div className="loom-absent"><b>DIFF, SYMBOL, TEST AND TASK LINKS UNAVAILABLE</b><span>{marker ? marker.note : "No source-backed event-to-code or outcome relationships were exported. A tool name alone does not prove its arguments, result, or affected file."}</span></div></div></>}
        {detail && mode !== "evidence" && <LinkedEvidence event={event} cutoff={cutoff} inspected={inspected} onInspect={setInspected} onSelect={onSelect} />}
        {detail && <details className="loom-design-source" open={focus}><summary>ILLUSTRATED EVENT RECORD</summary><pre tabIndex={0} className="loom-pre" data-testid="loom-event-source">{JSON.stringify(event, null, 2)}</pre></details>}
        <div className="loom-prevnext"><button type="button" disabled={index <= 0} onClick={() => onSelect(rows[index - 1].id)}>‹ PREVIOUS EVENT<span>{before.at(-1)?.at ?? "No earlier loaded event"}</span></button><button type="button" disabled={index < 0 || index >= rows.length - 1} onClick={() => onSelect(rows[index + 1].id)}>NEXT EVENT ›<span>{after[0]?.at ?? "No later revealed event"}</span></button></div>
      </section>
      {!focus && <section className="loom-panel">
        <h3>CAUSAL NEIGHBORHOOD {detail ? <span className="loom-design-boundary">ILLUSTRATED RELATIONS</span> : <Grade g="unavailable" />}</h3>{detail ? <DesignNeighborhood event={event} cutoff={cutoff} onSelect={onSelect} highlighted={mode !== "evidence" ? inspected : null} /> : <><p className="loom-note">Causal links were not exported. The recorded sequence below gives context; its dotted connectors mean order only.</p>
        <div className="loom-sequence-map" aria-label="Recorded session sequence, not causal relationships">
          <div>{before.map(e => <EventButton key={e.id} event={e} onSelect={onSelect} />)}</div><div className="loom-selected-record"><b>{event.tool ?? event.kind}</b><span>{event.at ?? "Undated record"}</span><small>SELECTED · EXACT ROW</small></div><div>{after.map(e => <EventButton key={e.id} event={e} onSelect={onSelect} />)}{after.length === 0 && <p className="loom-note">No later revealed records</p>}</div>
        </div>
        </>}<button type="button" className="loom-note warn loom-gap-link" onClick={onGap}>{detail ? "⚠ Illustrated attribution limits · VIEW GAP DETAILS" : "⚠ Attribution and downstream effects cannot be proven. VIEW GAP DETAILS"}</button>
        <div className="loom-source-grid"><div className="loom-source-tabs"><div className="tabs" role="tablist" onKeyDown={navigateTabs} aria-label="Event details">{(["source", "context", "links"] as const).map(t => <button key={t} type="button" role="tab" tabIndex={tab === t ? 0 : -1} aria-selected={tab === t} className={tab === t ? "is-on" : ""} onClick={() => onLayout({...layout,tab:t})}>{t.toUpperCase()}</button>)}</div>
          <div role="tabpanel">{tab === "source" ? <dl className="loom-kv"><div><dt>SOURCE</dt><dd>{detail ? detail.sourceLabel : "pack-index.json · loomEvents"}</dd></div><div><dt>IDENTITY</dt><dd>{event.id}</dd></div><div><dt>{detail ? "SOURCE BOUNDARY" : "CAPTURED"}</dt><dd>{detail ? "Authored example; not captured profile data" : SOURCE.capturedAt ?? "Exact capture timestamp unavailable"}</dd></div><div><dt>COVERAGE</dt><dd>{detail ? "Illustrated events, source excerpts and graded relations" : "Spine metadata only"}</dd></div></dl> : tab === "context" ? <dl className="loom-kv"><div><dt>SESSION</dt><dd>{event.sessionId}</dd></div><div><dt>PARENT ID</dt><dd>{session?.parentId ?? "No parent reference recorded"}</dd></div><div><dt>ROLE</dt><dd>{session?.isSubagent ? "Subagent session" : "Session"}</dd></div><div><dt>SEQUENCE</dt><dd>{rows.length} revealed records; {detail ? "authored relations shown separately" : "causal attribution unavailable"}</dd></div></dl> : <p className="loom-note">Event: {event.id}. Session: {event.sessionId}. {detail ? "Authored relationships and source excerpts belong only to this design illustration." : "No task, file, commit, symbol or provider outcome links exported."} Surface links retain this exact source and replay boundary. Return to Loom restores this view.</p>}</div>
        </div><div className="loom-pivots">{PIVOT_SURFACES.map(surface => <WorkspacePivot key={surface} surface={surface} event={event} />)}<button type="button" onClick={onTable}>EXACT EVENT TABLE</button></div></div>
      </section>}
    </div>
  </div>;
}

type LocalRecord = { id: string; target: string; author: string; at: string; kind: "feedback" | "lifecycle" | "adjudication"; text: string; marker: string; status: "open" | "acknowledged" | "acted-upon" | "contradicted"; parent: string | null; source: string | null; diffLine?: number };
const STORAGE_KEY = "td-loom-local-review";
function validDiffLine(target: string, line: unknown): boolean {
  if (line === undefined) return true;
  if (!Number.isSafeInteger(line) || (line as number) < 0) return false;
  // Notes from another source or unloaded page remain readable; validate its
  // line against the actual artifact once that target is in the loaded source.
  if (!SOURCE.events.some(event => event.id === target)) return true;
  const diff = SOURCE.design?.details[target]?.diff;
  return diff !== undefined && (line as number) < diff.split('\n').length;
}
function readRecords(): LocalRecord[] {
  const raw = localStorage.getItem(STORAGE_KEY);
  if (!raw) return [];
  const value: unknown = JSON.parse(raw);
  if (!Array.isArray(value) || value.some(r => !r || typeof r.id !== "string" || typeof r.target !== "string" || typeof r.text !== "string" || typeof r.author !== "string" || typeof r.at !== "string" || !Number.isFinite(Date.parse(r.at)) || !["feedback", "lifecycle", "adjudication"].includes(r.kind) || !["open", "acknowledged", "acted-upon", "contradicted"].includes(r.status) || typeof r.marker !== "string" || !validDiffLine(r.target, r.diffLine) || (r.parent !== null && typeof r.parent !== "string") || (r.source !== null && typeof r.source !== "string"))) throw new Error("Stored Loom notes could not be read. Existing storage has been left unchanged.");
  return value as LocalRecord[];
}
function useRecords() {
  const [initial] = useState(() => { try { return { records: readRecords(), error: "" }; } catch (error) { return { records: [] as LocalRecord[], error: String(error) }; } });
  const [records, setRecords] = useState(initial.records), [error, setError] = useState(initial.error);
  function append(record: Omit<LocalRecord, "id" | "at" | "author">) {
    try {
      const next = [...readRecords(), { ...record, id: crypto.randomUUID(), at: new Date().toISOString(), author: "Local reviewer" }];
      localStorage.setItem(STORAGE_KEY, JSON.stringify(next)); setRecords(next); setError(""); return true;
    } catch (cause) { setError(`Could not save locally: ${String(cause)}`); return false; }
  }
  return { records, error, append };
}

function IllustratedFeedback({ event, cutoff, onSelect }: { event: PackLoomEvent; cutoff: number | null; onSelect: (id: string) => void }) {
  const design = SOURCE.design!;
  const feedback = design.feedback.find(note => (note.targetEventId === event.id || note.lifecycle.some(step => step.eventId === event.id)) && design.events.some(e => e.id === note.sourceEventId && visible(e, cutoff)));
  if (!feedback) return <p className="loom-note" data-testid="illustrated-feedback-empty">No illustrated feedback is revealed for this selection at the current replay time.</p>;
  const steps = feedback.lifecycle.flatMap(step => {
    const source = design.events.find(e => e.id === step.eventId && visible(e, cutoff));
    const relation = step.relationId ? design.relations.find(r => r.id === step.relationId) : null;
    if (!source || (relation && !design.events.some(e => e.id === relation.from && visible(e, cutoff)))) return [];
    return [{ ...step, source, relation }];
  });
  const created = design.events.find(e => e.id === feedback.sourceEventId)!;
  return <section className="loom-illustrated-feedback" aria-label="Illustrated historical feedback">
    <div className="loom-illustrated-feedback-head"><span><b>{feedback.author}</b><small>Created {created.at?.slice(11, 23)}</small></span><b className="warn">{feedback.marker}</b><span className="loom-design-boundary" data-testid="illustrated-feedback-status">{steps.at(-1)?.status ?? 'open'} · ILLUSTRATED</span></div>
    <table className="loom-table" aria-label="Illustrated feedback source chronology"><thead><tr><th>Event / source</th><th>Time</th><th>Relationship</th><th>Lifecycle</th></tr></thead><tbody>{steps.map(step => <tr key={step.eventId}><td><button type="button" title={`${design.details[step.eventId].title} · ${step.source.sessionId}`} onClick={() => onSelect(step.eventId)}>{design.details[step.eventId].title}</button></td><td>{step.source.at?.slice(11, 23)}</td><td title={step.relation?.sourceRef}>{step.relation ? <Grade g={step.relation.grade} /> : 'Authored note'}</td><td>{step.status}</td></tr>)}</tbody></table>
    <p className="loom-note loom-feedback-lifecycle-boundary">Acknowledgment ≠ revision ≠ verification. Each step requires its own linked source.</p>
    <details className="loom-feedback-provenance"><summary>Historical path &amp; full source provenance</summary>
      <p className="loom-feedback-challenge">{feedback.text}</p><p className="loom-note">Authored example attached to <button type="button" onClick={() => onSelect(feedback.targetEventId)}>illustrated hunk L258</button>. Its lifecycle belongs only to this historical illustration.</p>
      <div className="loom-illustrated-path" aria-label="Illustrated feedback continuation">{steps.map(step => <button type="button" key={step.eventId} onClick={() => onSelect(step.eventId)}><span>{design.details[step.eventId].title}</span><small>{step.source.at?.slice(11, 23)}</small></button>)}</div>
      <pre className="loom-pre" tabIndex={0}>{JSON.stringify({ feedback: { id: feedback.id, targetEventId: feedback.targetEventId, sourceEventId: feedback.sourceEventId, author: feedback.author }, revealedLifecycle: steps.map(step => ({ status: step.status, event: step.source, relation: step.relation })) }, null, 2)}</pre>
      <p className="loom-note">Action requires the explicit revision source; time proximity alone does not set this lifecycle. Nothing was posted to a provider.</p>
    </details>
  </section>;
}

export function FeedbackWorkspace({ anchor, onAnchor, event, cutoff, onSelect, onEvidence }: { anchor: number | null; onAnchor: (anchor: number | null) => void; event: PackLoomEvent | null; cutoff: number | null; onSelect: (eventId: string) => void; onEvidence: () => void }) {
  const { records, error, append } = useRecords();
  const [text, setText] = useState(""), [marker, setMarker] = useState("needs clarification"), [parent, setParent] = useState("");
  const [status, setStatus] = useState<LocalRecord["status"]>("open"), [notice, setNotice] = useState("");
  if (!event || !visible(event, cutoff)) return <section className="loom-panel"><h3>SELECT A RECORDED EVENT TO ATTACH FEEDBACK</h3><p>Feedback needs an exact target. No note will be attached to a substitute event.</p></section>;
  const notes = records.filter(r => r.target === event.id && r.kind !== "adjudication");
  const selectedNote = notes.find(r => r.id === parent && r.kind === "feedback");
  const rows = sequence(event, cutoff);
  const designDetail = SOURCE.design?.details[event.id];
  const diffLines = designDetail?.diff?.split('\n');
  const targetLine = status === "open" ? anchor : selectedNote?.diffLine ?? null;
  function save() {
    if (!event || !text.trim()) { setNotice("Enter a comment or rationale before saving."); return; }
    if (status !== "open" && !notes.some(r => r.id === parent && r.kind === "feedback")) { setNotice("Choose the feedback record whose lifecycle you are updating."); return; }
    if (status === "acted-upon" || status === "contradicted") { setNotice("An effect requires a revealed event recorded after this feedback was created and an explicit source link to this local note. This snapshot contains no such link; chronological proximity cannot prove a response."); return; }
    if (targetLine !== null && (!diffLines || !validDiffLine(event.id, targetLine))) { setNotice("This illustrated diff line is unavailable. Select an event target or an existing line."); return; }
    if (append({ ...(targetLine === null ? {} : {diffLine: targetLine}), target: event.id, kind: status === "open" ? "feedback" : "lifecycle", text: text.trim(), marker, status, parent: status === "open" ? null : parent, source: null })) { setText(""); setNotice("Saved on this device. This is a local review record; source events remain unchanged."); }
  }
  const illustratedNote = SOURCE.design?.feedback.find(note => note.targetEventId === event.id && SOURCE.events.some(e => e.id === note.sourceEventId && visible(e, cutoff)));
  return <div className={`loom-fb${designDetail ? " is-design" : ""}`}><div className="loom-ws">
    <section className="loom-panel"><h3>VISIBLE ARTIFACT {designDetail ? <span className="loom-design-boundary">DESIGN ILLUSTRATION</span> : <Grade g="exact" />}</h3><div className="loom-ev-head"><b>{designDetail?.title ?? event.tool ?? event.kind}</b><span>{event.at}</span></div>
      {designDetail ? <><p className="loom-note">{designDetail.sourceLabel}</p>{designDetail.diff ? <DiffLines diff={designDetail.diff} selected={targetLine} onReview={line => { onAnchor(line); setStatus("open"); }} /> : <pre className="loom-pre loom-design-body" tabIndex={0}>{designDetail.body ?? 'No authored source body for this event.'}</pre>}{illustratedNote && <aside className="loom-feedback-annotation"><b>{illustratedNote.text}</b><small>Illustrated local reviewer · historical note on this hunk</small></aside>}<div className="loom-review-markers" role="group" aria-label="Local review markers">{[["understood","✓ MARK UNDERSTOOD"],["risky","⚠ MARK RISKY"],["needs clarification","? NEEDS CLARIFICATION"]].map(([value,label]) => <button key={value} type="button" aria-pressed={marker === value} onClick={() => setMarker(value)}>{label}</button>)}</div><details className="loom-design-source"><summary>ILLUSTRATED EVENT RECORD</summary><pre className="loom-pre">{JSON.stringify(event, null, 2)}</pre></details></> : <pre className="loom-pre" tabIndex={0}>{JSON.stringify(event, null, 2)}</pre>}
      <p className="loom-note">Target: {event.id}. {designDetail ? "Authored illustration, not a recorded profile event. New feedback is a real local note on this example only." : "Exact event metadata; code hunks, persisted decisions and task joins are not in this export."}</p><details className="loom-feedback-source-sequence" open={!designDetail}><summary>REVEALED SESSION SEQUENCE</summary><div className="loom-feedback-sequence">{rows.slice(Math.max(0, rows.findIndex(e => e.id === event.id) - 1), rows.findIndex(e => e.id === event.id) + 5).map(e => <EventButton key={e.id} event={e} onSelect={onSelect} />)}</div><p className="loom-note">These are chronological neighbors. New present-day feedback has no evidenced response in this source.</p></details></section>
    <section className="loom-panel"><h3 className="loom-feedback-heading">FEEDBACK &amp; CONTINUATION <button type="button" onClick={onEvidence}>BACK TO EVIDENCE</button></h3>{SOURCE.design && <IllustratedFeedback event={event} cutoff={cutoff} onSelect={onSelect} />}<h4>PRESENT-DAY LOCAL NOTES</h4><p className="loom-note loom-local-boundary">{designDetail ? "On this device · separate from illustrated history · provider unchanged" : "Present-day annotations on this device, separate from replay. Lifecycle changes are attributed to the local reviewer, never to the provider."}</p>
      <div className="loom-feedback-controls">{diffLines && <div className="loom-feedback-target"><label className="loom-compose">Feedback target<select aria-label="Feedback target" value={targetLine ?? "event"} disabled={status !== "open"} onChange={e => onAnchor(e.target.value === "event" ? null : Number(e.target.value))}><option value="event">Whole event</option>{diffLines.map((line,i) => <option key={i} value={i}>Illustrated diff line {i+1}: {line}</option>)}</select></label>{targetLine !== null && <code tabIndex={0} data-testid="loom-feedback-target-line" title="Line position within the illustrated diff, not a repository line number">{diffLines[targetLine] ?? "Selected line unavailable"}</code>}</div>}<label className="loom-compose">Review marker<select value={marker} onChange={e => setMarker(e.target.value)}>{["understood", "risky", "needs clarification", "challenge"].map(m => <option key={m}>{m}</option>)}</select></label>
      <label className="loom-compose">Lifecycle<select value={status} onChange={e => setStatus(e.target.value as LocalRecord["status"])}>{["open", "acknowledged", "acted-upon", "contradicted"].map(s => <option key={s}>{s}</option>)}</select></label>
      </div>{status !== "open" && <label className="loom-compose">Feedback to update<select value={parent} onChange={e => setParent(e.target.value)}><option value="">Select a local note</option>{notes.filter(r => r.kind === "feedback").map(r => <option key={r.id} value={r.id}>{r.marker} · {r.text.slice(0, 70)}</option>)}</select></label>}
      {(status === "acted-upon" || status === "contradicted") && <p className="loom-note warn">No source explicitly links later work to this present-day local note. An action or contradiction cannot be recorded from chronology alone.</p>}
      {status === "acknowledged" && <p className="loom-note">Local reviewer acknowledgment records that the note was read. It does not establish a revision, a passing check, or a resolved gap.</p>}
      <div className="loom-comment-row"><label className="loom-compose">Comment / evidence rationale<textarea rows={designDetail ? 2 : 3} value={text} onChange={e => setText(e.target.value)} placeholder="What can you establish from this artifact?" /></label><button type="button" className="is-on" onClick={save}>SAVE LOCALLY</button></div>
      {error && <p role="alert" className="loom-note warn">{error}</p>}<p role="status" className="loom-note">{notice}</p>
      <details className="loom-local-history" open={notes.length > 0}><summary>Local review history · {notes.length} records</summary><table className="loom-table"><caption>Append-only local review history for this event</caption><thead><tr><th>Author / time</th><th>Marker / lifecycle</th><th>Record / evidence</th></tr></thead><tbody>{notes.map(r => <tr key={r.id}><td>{r.author}<small>{r.at}</small></td><td>{r.marker}<small>{r.status} · {r.kind === "lifecycle" ? "reviewer assessment" : "local note"}</small></td><td>{r.text}{r.diffLine !== undefined && <small className="loom-feedback-saved-line">Illustrated diff line {r.diffLine + 1}: <code>{diffLines?.[r.diffLine] ?? "Source line unavailable"}</code></small>}{r.source && SOURCE.events.some(e => e.id === r.source && visible(e, cutoff)) && <button type="button" onClick={() => onSelect(r.source!)}>OPEN SOURCE {shortId(r.source)}</button>}{r.parent && <small>Appends to {r.parent}</small>}</td></tr>)}</tbody></table>{notes.length === 0 && <p className="loom-note">No local feedback has been recorded for this event.</p>}</details>
    </section></div></div>;
}

export function GapsWorkspace({ event: selectedEvent = null, cutoff = null, onSelect, onFocus }: {
  event?: PackLoomEvent | null; cutoff?: number | null;
  onSelect?: (eventId: string) => void; onFocus?: (eventId: string) => void;
}) {
  const event = selectedEvent && visible(selectedEvent, cutoff) ? selectedEvent : null;
  const [selected, setSelected] = useWorkspaceState(`loom:${SOURCE_ID}:${SOURCE.page ?? "snapshot"}:gap-selected`, event?.id ?? "parent");
  const [view, setView] = useWorkspaceState<"source" | "candidates" | "path" | "coverage">(`loom:${SOURCE_ID}:${SOURCE.page ?? "snapshot"}:${event?.id ?? "none"}:gap-view`, event && SOURCE.design?.details[event.id]?.candidates?.length ? "candidates" : "source");
  const [direction, setDirection] = useWorkspaceState<"root" | "outcome">(`loom:${SOURCE_ID}:${SOURCE.page ?? "snapshot"}:gap-direction`, "root");
  const [rationale, setRationale] = useState(""), [reference, setReference] = useState(""), [notice, setNotice] = useState("");
  const { records, error, append } = useRecords();
  const revealedSessions = SOURCE.sessions.filter(s => cutoff === null || (s.startedTs !== null && s.startedTs <= cutoff));
  const children = revealedSessions.filter(s => s.parentId !== null && !SOURCE.sessions.some(parent => parent.id === s.parentId));
  const markers = SOURCE.events.filter(e => e.kind === "git_pull_request" && visible(e, cutoff));
  const profileLedger = GAP_LEDGER.filter(g => (SOURCE_ID === "mac" || g.id !== "index") && (g.id !== "parent" || children.length > 0) && (g.id !== "pr" || markers.length > 0)).map(g => g.id === "parent" ? { ...g, detail: `${children.length} revealed subagent sessions reference an absent parent row`, exists: `Exact parent_id fields in ${children.length} revealed child rows.` } : g.id === "pr" ? { ...g, detail: `${markers.length} revealed markers; no PR number, body, or inbox`, exists: "Exact revealed transcript markers with timestamps." } : g.id === "bodies" && SOURCE_ID === "ubuntu" ? { ...g, exists: "Session identities, native thread associations and parent references. No event spine was exported." } : g);
  const designLedger = SOURCE.design ? [
    ...SOURCE.design.events.filter(e => visible(e, cutoff) && (e.kind === "gap" || SOURCE.design!.relations.some(r => r.to === e.id && ["inferred", "ambiguous", "stale", "unavailable"].includes(r.grade) && SOURCE.events.some(from => from.id === r.from && visible(from, cutoff))))).map(e => {
      const relation = SOURCE.design!.relations.find(r => r.to === e.id && ["inferred", "ambiguous", "stale", "unavailable"].includes(r.grade));
      return { id: e.id, issue: SOURCE.design!.details[e.id].title, detail: SOURCE.design!.details[e.id].body ?? "Authored relation is not exact attribution.", exists: relation?.sourceRef ?? "Illustrative event only; no return source supplied.", grade: relation?.grade ?? "unavailable" as EvidenceGrade };
    }),
    { id: "illustration", issue: "Illustrated source boundary", detail: "This scenario is authored; it is not evidence from either profile.", exists: "Labeled illustrative events, source excerpts and typed relationships.", grade: "unavailable" as EvidenceGrade },
  ] : null;
  const ledger = designLedger ?? profileLedger;
  useEffect(() => {
    if (event && ledger.some(g => g.id === event.id)) {
      setSelected(event.id);
    }
  }, [event?.id]);
  const gap = ledger.find(g => g.id === selected) ?? ledger.find(g => g.id === event?.id) ?? ledger.find(g => g.grade === "ambiguous") ?? ledger[0];
  const gapEvent = SOURCE.events.find(e => e.id === gap.id && visible(e, cutoff)) ?? null;
  const detail = SOURCE.design?.details[gap.id];
  const commonCandidateClaim = detail?.candidates?.length && detail.candidates.every(c => c.claim === detail.candidates![0].claim) ? detail.candidates[0].claim : null;
  const target = `gap:${SOURCE_ID}:${gap.id}${!gapEvent && event ? `:${event.id}` : ""}`;
  const local = records.filter(r => r.target === target && r.kind === "adjudication");
  const relations = SOURCE.design?.relations.filter(r => [r.from, r.to].every(id => SOURCE.events.some(e => e.id === id && visible(e, cutoff)))) ?? [];
  const adjacent = gapEvent ? relations.filter(r => r.from === gapEvent.id || r.to === gapEvent.id) : [];
  const knownPath = new Set<string>();
  const visited = new Set<string>();
  function trace(id: string) {
    if (visited.has(id)) return;
    visited.add(id);
    for (const relation of relations.filter(r => direction === "root" ? r.to === id : r.from === id)) {
      knownPath.add(relation.id);
      trace(direction === "root" ? relation.from : relation.to);
    }
  }
  if (gapEvent && view === "path") trace(gapEvent.id);
  const pathRows = relations.filter(r => knownPath.has(r.id)).sort((a, b) => (SOURCE.events.find(e => e.id === a.to)?.ts ?? 0) - (SOURCE.events.find(e => e.id === b.to)?.ts ?? 0));
  function choose(id: string) {
    setSelected(id); setView(SOURCE.design?.details[id]?.candidates?.length ? "candidates" : "source"); setNotice("");
    if (SOURCE.events.some(e => e.id === id && visible(e, cutoff))) onFocus?.(id);
  }
  function save() {
    if (!rationale.trim() || !reference.trim()) { setNotice("Provide a rationale and source reference. A resolution cannot fill in missing evidence."); return; }
    if (append({ target, kind: "adjudication", text: rationale.trim(), source: reference.trim(), marker: "resolution assessment", status: "open", parent: local.at(-1)?.id ?? null })) { setRationale(""); setNotice("Local adjudication appended. The evidence gap and immutable source records are unchanged."); }
  }
  function acknowledgeGap() {
    if (append({ target, kind: "adjudication", text: "Reviewed locally. Missing source and limited attribution remain unresolved.", source: gapEvent?.id ?? gap.exists, marker: "attention acknowledged", status: "acknowledged", parent: local.at(-1)?.id ?? null })) setNotice("Gap acknowledged locally. No evidence was filled and the source grade is unchanged.");
  }
  const sourceButton = (id: string) => <button type="button" title={id} onClick={() => onSelect?.(id)} disabled={!onSelect}>{SOURCE.design?.details[id]?.title ?? shortId(id, 22)}</button>;
  return <div className="loom-ws three loom-gap-workspace">
    <section className="loom-panel loom-gap-ledger"><h3>GAPS &amp; AMBIGUITIES</h3><p className="loom-note">{SOURCE.design ? "Authored source limitations · CONCEPT / SYNTHETIC DATA" : "Unresolved limitations in the selected profile snapshot"}</p>
      <table className="loom-table gaps"><caption className="visually-hidden">{SOURCE.design ? "Illustrated source limitations" : "Snapshot source limitations"}</caption><thead><tr><th>Issue / affected source</th><th>Time UTC</th><th>Grade</th></tr></thead><tbody>{ledger.map(g => {
        const row = SOURCE.events.find(e => e.id === g.id);
        return <tr key={g.id} className={gap.id === g.id ? "is-sel" : ""}><td><button type="button" aria-pressed={gap.id === g.id} onClick={() => choose(g.id)}>{g.issue}</button><small title={g.detail}>{g.detail}</small></td><td>{row?.at?.slice(11, 23) ?? "Snapshot"}</td><td><Grade g={g.grade} /></td></tr>;
      })}</tbody></table>
      <div className="loom-gap-actions"><button type="button" disabled={!gapEvent || !onFocus} onClick={() => gapEvent && onFocus?.(gapEvent.id)}>◎ FOCUS GAP</button><button type="button" onClick={() => setView("path")}>⇢ SHOW KNOWN PATH</button><button type="button" onClick={() => setView("candidates")}>♧ COMPARE CANDIDATES</button></div>
      <p className="loom-note">Private reasoning is unavailable. Missing evidence is not proof that an event did not occur.</p>
    </section>
    <section className="loom-panel loom-gap-evidence"><h3>BEST AVAILABLE EVIDENCE</h3><div className="loom-gap-selection"><b>{gap.issue}</b><Grade g={gap.grade} /></div>
      <div className="loom-gap-tabs" role="tablist" onKeyDown={navigateTabs} aria-label="Gap evidence views">{["source", "candidates", "path", "coverage"].map(tab => <button type="button" role="tab" tabIndex={view === tab ? 0 : -1} aria-selected={view === tab} className={view === tab ? "is-on" : ""} key={tab} onClick={() => setView(tab as typeof view)}>{tab.toUpperCase()}</button>)}</div>
      <div role="tabpanel" className="loom-gap-content">
      {view === "source" && <><dl className="loom-gap-facts"><div><dt>RECORDS THAT EXIST</dt><dd>{gap.exists}</dd></div><div><dt>CLAIMS LIMITED</dt><dd>{gap.detail}</dd></div><div><dt>{SOURCE.design ? "SOURCE BOUNDARY" : "SOURCE CAPTURE"}</dt><dd>{SOURCE.design ? "Authored illustration; not a profile capture" : SOURCE.capturedAt ?? "Exact capture timestamp unavailable"}</dd></div></dl>
        {gapEvent && <><h4>AFFECTED EVENT</h4>{sourceButton(gapEvent.id)}<p className="loom-note">{gapEvent.at} · {gapEvent.sessionId}</p><details><summary>Exact {SOURCE.design ? "illustrative" : "exported"} source row</summary><pre className="loom-pre">{JSON.stringify(gapEvent, null, 2)}</pre></details></>}
        {adjacent.length > 0 && <><h4>NEIGHBORHOOD · TYPED RELATIONS</h4><div className="loom-gap-neighbors">{adjacent.map(r => <article key={r.id}><span>{r.kind}</span><Grade g={r.grade} />{sourceButton(r.from === gapEvent?.id ? r.to : r.from)}<small>{r.sourceRef}</small></article>)}</div></>}
        {gap.id === "parent" && <><h4>RECORDED PARENT REFERENCES</h4>{children.map(s => <details key={s.id}><summary>{shortId(s.id)} · {s.provider}</summary><pre className="loom-pre">{JSON.stringify({ id: s.id, parentId: s.parentId, project: s.project, startedAt: s.startedAt }, null, 2)}</pre></details>)}</>}
        <h4>{SOURCE.design ? "ILLUSTRATED EVIDENCE" : "WHAT REMAINS EXACT"}</h4><p className="loom-note">{SOURCE.design ? "Authored exact and explicit relations remain identified separately from this limitation. Adjudication cannot change their grade or manufacture a profile observation." : "Exported event metadata and parent IDs remain source facts. Missing transcript, code or parent records limit the joins they can support."}</p></>}
      {view === "candidates" && <><h4>CANDIDATE IDENTITIES · UNRESOLVED</h4>{commonCandidateClaim && <p className="loom-candidate-limit">{commonCandidateClaim}</p>}{detail?.candidates?.length ? detail.candidates.map(candidate => {
        const session = revealedSessions.find(s => s.id === candidate.sessionId);
        const rows = SOURCE.events.filter(e => e.sessionId === candidate.sessionId && e.id !== gapEvent?.id && visible(e, Math.min(cutoff ?? Infinity, gapEvent?.ts ?? Infinity))).slice(-2);
        return <article className="loom-gap-candidate" key={candidate.sessionId}><header title={candidate.sessionId} aria-label={`${candidate.label} · ${candidate.sessionId}`}><b>{candidate.label}</b><Grade g={candidate.grade} /></header>{!commonCandidateClaim && <p>{candidate.claim}</p>}{session ? <><p className="loom-note">{session.agentId ?? "Agent identity unavailable"} · revealed examples, not attribution proof</p>{rows.map(e => <div key={e.id}>{sourceButton(e.id)}<small>{e.at}</small></div>)}{rows.length === 0 && <p className="loom-note">No event evidence revealed for this candidate.</p>}</> : <p className="loom-note">Candidate session source not revealed at this replay time.</p>}</article>;
      }) : <p className="loom-note">No candidate identities are supplied by this source. Missing capture, retention or deletion are possible explanations, not identified candidates.</p>}<article className="loom-gap-candidate unknown"><b>OTHER POSSIBILITY · UNKNOWN</b><Grade g="unavailable" /><p>No supplied record excludes another explanation. Candidate comparison cannot resolve identity by time proximity.</p></article></>}
      {view === "path" && <><div className="loom-gap-actions"><button type="button" aria-pressed={direction === "root"} onClick={() => setDirection("root")}>↑ PATH TO ROOT</button><button type="button" aria-pressed={direction === "outcome"} onClick={() => setDirection("outcome")}>↓ PATH TO OUTCOME</button></div>{pathRows.length ? <ol className="loom-gap-path">{pathRows.map(r => <li key={r.id}>{sourceButton(r.from)}<span>{r.kind} → <Grade g={r.grade} /></span>{sourceButton(r.to)}<small>{r.sourceRef}</small></li>)}</ol> : <p className="loom-note">No {direction === "root" ? "upstream" : "downstream"} event relations are supplied for this selection. No return or outcome is invented.</p>}<p className="loom-note">Only revealed typed source relationships appear. An inferred or unavailable edge remains limited along the path.</p></>}
      {view === "coverage" && <><h4>EXACT COVERAGE / BRANCH ROWS</h4><table className="loom-table"><thead><tr><th>Session / parent</th><th>Coverage</th></tr></thead><tbody>{(gap.id === "parent" ? children : gapEvent ? revealedSessions.filter(s => s.id === gapEvent.sessionId) : revealedSessions).map(s => <tr key={s.id}><td>{s.id}<small>Parent: {s.parentId ?? "No reference recorded"}</small></td><td>{SOURCE.design ? "Illustrated source" : s.coverage}</td></tr>)}</tbody></table><details><summary>Source coverage notes</summary><ul>{SOURCE.notes.map((note, i) => <li key={i}>{note}</li>)}</ul></details></>}
      </div>
    </section>
    <aside className="loom-panel loom-gap-resolution"><div className={`loom-gap-unknown g-${gap.grade}`}><span aria-hidden="true">?</span><p>{gap.grade === "ambiguous" ? "Attribution remains unresolved between the supplied candidates." : "The available source does not establish the missing relation."}</p><Grade g={gap.grade} /></div><p className="loom-note loom-gap-boundary">Bounded source limitation · independent recorded events remain available.</p><button type="button" onClick={acknowledgeGap}>ACKNOWLEDGE GAP</button><h4>SOURCE ACTIONS</h4><button type="button" onClick={() => setView("source")}>OPEN SOURCE DETAILS</button><button type="button" onClick={() => setView("coverage")}>COVERAGE / BRANCH ROWS</button><button type="button" disabled title="No production refresh or re-ingest path is configured in this standalone demo">REFRESH · NOT CONFIGURED</button>
      <details className="loom-adjudication-form"><summary>RECORD RESOLUTION</summary><p className="loom-note">Append an assessment with provenance. Immutable source facts and attribution stay unchanged.</p><label className="loom-compose">Source reference<input value={reference} onChange={e => setReference(e.target.value)} placeholder="Event ID, source path, or URL" /></label><label className="loom-compose">Resolution rationale<textarea rows={3} value={rationale} onChange={e => setRationale(e.target.value)} /></label><button type="button" onClick={save}>RECORD RESOLUTION</button></details>
      {error && <p role="alert" className="loom-note warn">{error}</p>}<p role="status" className="loom-note">{notice}</p>{local.map(r => <article className="loom-card" key={r.id}><header>{r.author} · {r.at}</header><p>{r.text}</p><small>Source: {r.source}</small><small>{r.marker} · {r.status} · source grade unchanged</small></article>)}
    </aside>
  </div>;
}
