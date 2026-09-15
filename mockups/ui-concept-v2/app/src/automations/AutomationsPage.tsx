import { useEffect, useState, type CSSProperties, type KeyboardEvent, type ReactNode } from "react";
import { Corners } from "../app/shell/Corners";
import { useDemo, useWorkspaceState } from "../app/workspace";
import { AUTOMATIONS_ATTENTION } from "./attention";
import "./automations.css";

type Tone = "success" | "partial" | "skipped" | "denied" | "failed" | "verified" | "mismatch" | "quiet" | "running" | "scheduled";

type Run = {
  id: string;
  start: string;
  job: string;
  duration: string;
  outcome: Tone;
  receipt: string;
  artifacts: string;
  integrity: Tone;
  targets: string;
};

const JOBS = [
  ["ingest:delta", "*/5 min", "14:36:12", "success", "14:40:00", "running", "success"],
  ["graph:prune", "15 min", "14:30:03", "success", "14:45:00", "scheduled", "success"],
  ["embeddings:refresh", "30 min", "14:15:44", "success", "14:45:00", "scheduled", "success"],
  ["policies:reconcile", "1 h", "14:02:10", "skipped", "15:02:10", "scheduled", "skipped"],
  ["index:optimize", "2 h", "13:47:22", "success", "15:47:22", "scheduled", "success"],
] as const;

const SKILLS = [
  ["ingest", "ingestion", "cyan"],
  ["graph", "graph", "cyan"],
  ["index", "index", "green"],
  ["policy", "policy", "green"],
  ["telemetry", "telemetry", "amber"],
] as const;

const OUTCOMES = [
  ["14:36:12", "ingest:delta", "success", "+1,248 nodes, +3,912 edges", "targets: 7"],
  ["14:30:03", "graph:prune", "success", "−18,441 edges, −2.1% density", "targets: 7"],
  ["14:22:05", "embeddings:refresh", "partial", "73/98 sources updated", "targets: 4"],
  ["14:15:44", "policies:reconcile", "skipped", "window skip (no policy changes)", "targets: 7"],
  ["14:02:10", "index:optimize", "success", "segments merged: 4 → 3", "targets: 7"],
] as const;

const RUNS: Run[] = [
  { id: "run-143612", start: "2025-05-09 14:36:12.341", job: "ingest:delta", duration: "00:00:18.731", outcome: "success", receipt: "fr-9f3b1c2e", artifacts: "3", integrity: "verified", targets: "7" },
  { id: "run-143003", start: "2025-05-09 14:30:03.118", job: "graph:prune", duration: "00:00:12.447", outcome: "success", receipt: "fr-3a8c7b1d", artifacts: "2", integrity: "verified", targets: "7" },
  { id: "run-142205", start: "2025-05-09 14:22:05.009", job: "embeddings:refresh", duration: "00:01:04.226", outcome: "partial", receipt: "fr-6d2e1f90", artifacts: "2", integrity: "mismatch", targets: "4" },
  { id: "run-141544", start: "2025-05-09 14:15:44.882", job: "policies:reconcile", duration: "00:00:03.551", outcome: "skipped", receipt: "—", artifacts: "0", integrity: "quiet", targets: "7" },
  { id: "run-140210", start: "2025-05-09 14:02:10.557", job: "index:optimize", duration: "00:02:31.984", outcome: "success", receipt: "fr-b1d7a2e4", artifacts: "4", integrity: "verified", targets: "7" },
  { id: "run-134722", start: "2025-05-09 13:47:22.441", job: "graph:prune", duration: "00:00:11.995", outcome: "success", receipt: "fr-2c9a4d11", artifacts: "2", integrity: "verified", targets: "7" },
  { id: "run-133207", start: "2025-05-09 13:32:07.223", job: "ingest:delta", duration: "00:00:19.402", outcome: "success", receipt: "fr-7e4b1a9c", artifacts: "3", integrity: "verified", targets: "7" },
  { id: "run-130000", start: "2025-05-09 13:00:00.000", job: "embeddings:refresh", duration: "00:01:02.118", outcome: "partial", receipt: "fr-5f6e2b77", artifacts: "2", integrity: "denied", targets: "4" },
];

type Attempt = {
  id: string;
  runId: string;
  order: number;
  outcome: Extract<Tone, "success" | "partial" | "skipped" | "failed">;
  recordedAt: string;
  detail: string;
};

type AutomationTrace = {
  job: string;
  trigger: string;
  duePosition: number;
  activityPosition: number;
  queue: string;
  waitReason: string;
  attempts: Attempt[];
  receipt: string;
  attentionId?: string;
};

/* Stable authored fixture topology. A run owns its attempts; a retry adds an
   attempt and never changes the durable failed attempt beneath it. */
const AUTOMATION_TRACES: AutomationTrace[] = [
  {
    job: "ingest:delta",
    trigger: "schedule */5 min · due 14:35:00",
    duePosition: 8,
    activityPosition: 14,
    queue: "admitted to ingest lane",
    waitReason: "retry backoff 30s · elapsed",
    attempts: [
      { id: "attempt-143532", runId: "run-143612", order: 1, outcome: "failed", recordedAt: "14:35:32.104", detail: "provider lease expired" },
      { id: "attempt-143612", runId: "run-143612", order: 2, outcome: "success", recordedAt: "14:36:12.341", detail: "receipt committed" },
    ],
    receipt: "fr-9f3b1c2e",
  },
  {
    job: "graph:prune",
    trigger: "schedule 15 min · due 14:30:00",
    duePosition: 16,
    activityPosition: 10,
    queue: "admitted to graph lane",
    waitReason: "none · queue age 3s",
    attempts: [{ id: "attempt-143003", runId: "run-143003", order: 1, outcome: "success", recordedAt: "14:30:03.118", detail: "receipt committed" }],
    receipt: "fr-3a8c7b1d",
  },
  {
    job: "embeddings:refresh",
    trigger: "schedule 30 min · due 14:22:00",
    duePosition: 16,
    activityPosition: 7,
    queue: "admitted to embeddings lane",
    waitReason: "source quorum wait · 14s",
    attempts: [{ id: "attempt-142205", runId: "run-142205", order: 1, outcome: "partial", recordedAt: "14:22:05.009", detail: "73/98 sources updated" }],
    receipt: "fr-6d2e1f90",
    attentionId: "automation:run-142205:integrity",
  },
  {
    job: "policies:reconcile",
    trigger: "schedule 1 h · due 14:02:10",
    duePosition: 36,
    activityPosition: 4,
    queue: "admission evaluated",
    waitReason: "not queued · no policy changes",
    attempts: [{ id: "attempt-141544", runId: "run-141544", order: 1, outcome: "skipped", recordedAt: "14:15:44.882", detail: "window skip" }],
    receipt: "no receipt",
  },
  {
    job: "index:optimize",
    trigger: "schedule 2 h · due 13:47:00",
    duePosition: 82,
    activityPosition: 2,
    queue: "admitted to index lane",
    waitReason: "none · queue age 22s",
    attempts: [{ id: "attempt-140210", runId: "run-140210", order: 1, outcome: "success", recordedAt: "14:02:10.557", detail: "receipt committed" }],
    receipt: "fr-b1d7a2e4",
  },
];

type Artifact = { name: string; size: string; availability: string; integrity: Tone };

const EMBEDDINGS_ARTIFACTS: Artifact[] = [
  { name: "vectors-20250509T142205.zst", size: "412.7 MB", availability: "present", integrity: "mismatch" as Tone },
  { name: "manifest.json", size: "2.1 KB", availability: "present", integrity: "verified" as Tone },
];

function artifactsFor(run: Run): Artifact[] {
  if (run.job === "embeddings:refresh") return EMBEDDINGS_ARTIFACTS;
  const count = Number(run.artifacts);
  if (!count) return [{ name: "No artifact recorded", size: "—", availability: "absent", integrity: "quiet" }];
  return Array.from({ length: count }, (_, index) => ({
    name: index === 0 ? `${run.job.replace(":", "-")}-receipt.json` : `${run.job.replace(":", "-")}-artifact-${index + 1}.json`,
    size: "2.1 KB",
    availability: "present",
    integrity: "verified" as Tone,
  }));
}

function onRowKey(event: KeyboardEvent<HTMLElement>, activate: () => void) {
  if (event.key === "ArrowDown" || event.key === "ArrowUp") {
    event.preventDefault();
    const sibling = (event.key === "ArrowDown" ? event.currentTarget.nextElementSibling : event.currentTarget.previousElementSibling) as HTMLElement | null;
    sibling?.focus();
    sibling?.click();
    return;
  }
  if (event.key === "Enter" || event.key === " ") {
    event.preventDefault();
    activate();
  }
}

function ToneText({ children }: { children: string }) {
  return <span className={`am-tone is-${children.replaceAll(" ", "-")}`}>{children}</span>;
}

function Row({ l, r, tone }: { l: string; r: string; tone?: Tone }) {
  return <div className="am-row"><span className="lab">{l}</span><span className={`r ${tone ? `is-${tone}` : ""}`}>{r}</span></div>;
}

function Kpi({ lab, value, note }: { lab: string; value: string; note: string }) {
  return <div className="am-kpi"><span className="k">{lab}</span><div className="v"><b>{value}</b><small>{note}</small></div></div>;
}

function Well({ title, note, className, footL, footR, children }: {
  title: string; note?: string; className?: string; footL?: string; footR?: string; children: ReactNode;
}) {
  return <section className={`am-well ${className ?? ""}`.trim()}><header><b>{title}</b>{note && <span>{note}</span>}</header>{children}{footL && <footer><span>{footL}</span><span>{footR}</span></footer>}</section>;
}

function UnavailableRow({ columns, children }: { columns: number; children: string }) {
  return <tr className="am-empty-row"><td colSpan={columns}><span className="note">{children}</span></td></tr>;
}

function traceForRun(runId: string) {
  return AUTOMATION_TRACES.find((trace) => trace.attempts.some((attempt) => attempt.runId === runId));
}

function TraceNode({ className, label, detail, children }: { className: string; label: string; detail: string; children?: ReactNode }) {
  return <div className={`am-trace-node ${className}`}><span>{label}</span><b>{detail}</b>{children}</div>;
}

function AttemptNode({ attempt, active, onSelect }: { attempt: Attempt; active: boolean; onSelect: () => void }) {
  return <button type="button" className={`am-trace-node is-attempt is-attempt-${attempt.order} is-${attempt.outcome}${active ? " is-active" : ""}`} aria-pressed={active} onClick={onSelect}>
    <span>ATTEMPT {attempt.order}</span><b>{attempt.outcome}</b><small>{attempt.detail}</small>
  </button>;
}

function FleetLanes({ selectedJob, onSelect }: { selectedJob: string; onSelect: (trace: AutomationTrace) => void }) {
  return <div className="am-fleet" aria-label="Stable fleet time lanes">
    <div className="am-fleet-head"><span>FLEET LANES · NEXT DUE WINDOW</span><span>14:39</span><span>15:02</span><span>15:47 UTC</span></div>
    {AUTOMATION_TRACES.map((trace) => <button type="button" key={trace.job} className={selectedJob === trace.job ? "selected" : ""} onClick={() => onSelect(trace)} style={{ "--due": `${trace.duePosition}%`, "--activity": `${trace.activityPosition}%` } as CSSProperties}>
      <span>{trace.job}</span><i><b /><em /></i>
    </button>)}
  </div>;
}

function AutomationCanvas({ trace, focusedAttemptId, onAttempt, onAttention, onTrace }: {
  trace: AutomationTrace;
  focusedAttemptId: string;
  onAttempt: (attempt: Attempt) => void;
  onAttention: () => void;
  onTrace: (trace: AutomationTrace) => void;
}) {
  const retry = trace.attempts.length > 1;
  const current = trace.attempts[trace.attempts.length - 1];
  const failed = trace.attempts[0];
  const attention = trace.attentionId ? AUTOMATIONS_ATTENTION.find((item) => item.id === trace.attentionId) : undefined;
  return <section className="am-canvas" aria-labelledby="am-canvas-title">
    <header><div><b id="am-canvas-title">TRIGGER → QUEUE → RUN LINEAGE</b><span>FIXED LANES · SELECT A DURABLE ATTEMPT</span></div><span className="am-canvas-source">AUTHORED FIXTURE</span></header>
    <FleetLanes selectedJob={trace.job} onSelect={onTrace} />
    <div className={`am-trace-field${retry ? " has-retry" : ""}`} aria-label={`Run structure for ${trace.job}`}>
      <svg viewBox="0 0 100 100" preserveAspectRatio="none" aria-hidden="true">
        {retry ? <><path d="M12 53H33H48V70H63L77 39H91" /><path className="am-trace-retry-path" d="M63 70L77 39" /></> : <path d="M12 53H33H63H91" />}
        <circle cx="12" cy="53" r="1.1" /><circle cx="33" cy="53" r="1.1" />
        <circle cx={retry ? "48" : "63"} cy={retry ? "70" : "53"} r="1.1" />
        {retry && <><circle cx="63" cy="70" r="1.1" /><circle cx="77" cy="39" r="1.1" /></>}
        <circle cx="91" cy={retry ? "39" : "53"} r="1.1" />
      </svg>
      <div className="am-trace-lanes" aria-hidden="true"><span>TRIGGER</span><span>QUEUE</span><span>RUN / ATTEMPT</span><span>RECEIPT</span></div>
      <TraceNode className="is-trigger" label="TRIGGER" detail={trace.trigger} />
      <TraceNode className="is-queue" label="QUEUE" detail={trace.queue} />
      {retry ? <>
        <AttemptNode attempt={failed} active={focusedAttemptId === failed.id} onSelect={() => onAttempt(failed)} />
        <TraceNode className="is-retry-wait" label="RETRY WAIT" detail={trace.waitReason} />
        <AttemptNode attempt={current} active={focusedAttemptId === current.id} onSelect={() => onAttempt(current)} />
      </> : <AttemptNode attempt={current} active={focusedAttemptId === current.id} onSelect={() => onAttempt(current)} />}
      <TraceNode className="is-receipt" label="RECEIPT" detail={trace.receipt} />
    </div>
    <div className="am-trace-evidence">
      <div><span>QUEUE WAIT REASON</span><b>{trace.waitReason}</b></div>
      {attention ? <button type="button" onClick={onAttention}><span>ATTENTION EVIDENCE · {attention.evidence}</span><b>{attention.title}</b><small>{attention.owner} · {attention.observedAt}</small></button> : <div><span>ATTENTION EVIDENCE</span><b>none in this fixture lineage</b></div>}
    </div>
    <details className="am-canvas-fallback"><summary>Exact lineage text</summary><ol><li>Trigger — {trace.trigger}</li><li>Queue — {trace.queue}; wait reason: {trace.waitReason}</li>{trace.attempts.map((attempt) => <li key={attempt.id}>Attempt {attempt.order} · {attempt.id} · {attempt.outcome} · {attempt.recordedAt} · {attempt.detail}</li>)}<li>Receipt — {trace.receipt}</li></ol></details>
  </section>;
}

function SnapshotCanvas() {
  return <section className="am-canvas is-unavailable" aria-labelledby="am-canvas-title">
    <header><div><b id="am-canvas-title">TRIGGER → QUEUE → RUN LINEAGE</b><span>STRUCTURE RESERVED · NO INFERRED EDGES</span></div><span className="am-canvas-source">SNAPSHOT</span></header>
    <div className="am-snapshot-field"><div><span>TRIGGER</span><b>scheduler configuration unavailable</b></div><i aria-hidden="true" /><div><span>QUEUE</span><b>queue authority unavailable</b></div><i aria-hidden="true" /><div><span>RUN / ATTEMPT</span><b>run ledger unavailable</b></div><i aria-hidden="true" /><div><span>RECEIPT</span><b>no evidence served</b></div></div>
    <p className="am-snapshot-note">The snapshot contains no scheduler read authority. No health, failure, queue, retry, or receipt state is inferred from its absence.</p>
  </section>;
}

function UnservedRunCanvas({ run }: { run: Run }) {
  return <section className="am-canvas is-unavailable" aria-labelledby="am-canvas-title">
    <header><div><b id="am-canvas-title">TRIGGER → QUEUE → RUN LINEAGE</b><span>FIXTURE ROW · NO INFERRED EDGES</span></div><span className="am-canvas-source">LINEAGE UNSERVED</span></header>
    <div className="am-snapshot-field is-disconnected"><div><span>TRIGGER</span><b>not authored for this run</b></div><div><span>QUEUE</span><b>not authored for this run</b></div><div><span>RUN / ATTEMPT</span><b>{run.id} · attempt lineage unserved</b></div><div><span>RECEIPT</span><b>{run.receipt === "—" ? "no receipt recorded" : run.receipt}</b></div></div>
    <p className="am-snapshot-note">This exact ledger row has no authored queue or attempt topology. A newer run for the same job is never substituted.</p>
  </section>;
}

function Inspector({ run, trace, focusedAttemptId, onAttempt, onReceipt }: { run: Run | null; trace?: AutomationTrace; focusedAttemptId?: string; onAttempt?: (attempt: Attempt) => void; onReceipt: () => void }) {
  const [artifactName, setArtifactName] = useState(EMBEDDINGS_ARTIFACTS[0].name);
  if (!run) return <aside className="am-inspect" aria-label="Run inspector">
    <Corners />
    <div className="am-inspect-body am-inspect-empty"><div className="am-inspect-head"><h2>RUN INSPECTOR</h2><span>NO RUN SELECTED</span></div><div className="am-box"><div className="k">SNAPSHOT READ MODEL</div><p>No scheduler run, attempt, receipt, or artifact authority is served by this snapshot.</p><p>Use the ledger fallback when a named authority supplies exact rows.</p></div></div>
    <div className="am-truth">SNAPSHOT · SCHEDULER DATA UNAVAILABLE</div>
  </aside>;
  const artifacts = artifactsFor(run);
  const artifact = artifacts.find((item) => item.name === artifactName) ?? artifacts[0];
  const artifactPresent = artifact.availability === "present";
  const integrityDetail = artifact.integrity === "mismatch" ? "digest mismatch" : artifact.integrity === "quiet" ? "no artifact manifest" : "digest verified";
  return (
    <aside className="am-inspect" aria-label="Run inspector">
      <Corners />
      <div className="am-inspect-body">
        <div className="am-inspect-head"><h2>RUN INSPECTOR</h2><span>SELECTED RUN</span></div>
        <div className="am-run-box"><b>{run.start} UTC</b><div><span>{run.job}</span><ToneText>{run.outcome}</ToneText></div></div>
        <div className="am-open-rows"><Row l="DURATION" r={run.duration} /><Row l="FACT RECEIPT" r={run.receipt} /><Row l="SKILL / AUTH" r="index / index" /><Row l="TARGETS" r={run.targets} /><Row l="OUTCOME" r={run.outcome} tone={run.outcome} /><Row l="INTEGRITY" r={run.integrity} tone={run.integrity} /></div>
        {trace && <div className="am-box am-lineage"><div className="k">ATTEMPT LINEAGE (DURABLE)</div>{trace.attempts.map((attempt) => <button type="button" key={attempt.id} className={focusedAttemptId === attempt.id ? "selected" : ""} onClick={() => onAttempt?.(attempt)}><span>#{attempt.order} · {attempt.id}</span><ToneText>{attempt.outcome}</ToneText><small>{attempt.detail}</small></button>)}</div>}
        <div className="am-box">
          <div className="k">ARTIFACTS ({run.artifacts})</div>
          <table className="am-mini"><thead><tr><th>ARTIFACT</th><th>SIZE</th><th>AVAIL</th><th>INTEGRITY</th></tr></thead><tbody>
            {artifacts.map((item) => <tr key={item.name} data-artifact-id={item.name} className={artifact.name === item.name ? "selected" : ""} tabIndex={0} onClick={() => setArtifactName(item.name)} onKeyDown={(event) => onRowKey(event, () => setArtifactName(item.name))}><td>{item.name}</td><td>{item.size}</td><td><i className="am-dot" aria-label={item.availability} /></td><td><ToneText>{item.integrity}</ToneText></td></tr>)}
          </tbody></table>
        </div>
        <div className="am-box"><div className="k">SELECTED ARTIFACT PAYLOAD</div><div className="am-file">{artifact.name}</div><Row l="AVAILABILITY" r={artifact.availability} tone={artifactPresent ? "success" : "quiet"} /><Row l="STORED AT" r={artifactPresent ? "fixture://trace-archive/automations/" : "—"} /><Row l="SIZE" r={artifact.size} /><Row l="SHA256" r={artifactPresent ? "9c1b2e6f5d4a7e8b3c2d1f0a6e7b8c9d5e2f…" : "—"} /></div>
        <div className="am-box"><div className="k am-box-head">INTEGRITY VERDICT (DAEMON)<ToneText>{artifact.integrity}</ToneText></div><Row l="CHECKED AT" r={artifactPresent ? "2025-05-09 14:22:07 UTC" : "—"} /><Row l="ALGORITHM" r={artifactPresent ? "sha256" : "—"} /><Row l="EXPECTED" r={artifactPresent ? "9c1b2e6f5d4a7e8b3c2d1f0a6e7b8c9d…" : "—"} /><Row l="COMPUTED" r={artifactPresent ? artifact.integrity === "mismatch" ? "7a3f1d4c9b2e6f8a1d3c4b5e6f7a8b9c…" : "9c1b2e6f5d4a7e8b3c2d1f0a6e7b8c9d…" : "—"} /><Row l="DETAILS" r={integrityDetail} tone={artifact.integrity} /></div>
        <div className="am-box"><div className="k am-box-head">FACT RECEIPT<button type="button" className="am-view" onClick={onReceipt}>▣ view</button></div><div className="am-file">{run.receipt}</div><Row l="EMITTED AT" r="2025-05-09 14:22:06 UTC" /><Row l="SIGNED BY" r="trace-sched@node-7" /></div>
      </div>
      <div className="am-truth">AUTHORED FIXTURE · SYNTHETIC DATA</div>
    </aside>
  );
}

export function AutomationsPage({ state, onState }: { onInspect?: unknown; state?: string; onState?: (id: string) => void } = {}) {
  const { mode, navigate } = useDemo();
  const fixture = mode === "fixture";
  const [rememberedRun, setRememberedRun] = useWorkspaceState("automations:selected-run", "run-143612");
  const [rememberedAttempt, setRememberedAttempt] = useWorkspaceState("automations:selected-attempt", "attempt-143612");
  const [receiptOpen, setReceiptOpen] = useState(false);
  const routedRun = RUNS.find((run) => run.id === state);
  const selected = fixture ? routedRun ?? RUNS.find((run) => run.id === rememberedRun) ?? RUNS[0] : null;
  const trace = selected ? traceForRun(selected.id) : undefined;
  const focusedAttemptId = trace?.attempts.some((attempt) => attempt.id === rememberedAttempt) ? rememberedAttempt : trace?.attempts[trace.attempts.length - 1].id;
  useEffect(() => {
    if (!fixture || !routedRun || rememberedRun === routedRun.id) return;
    setRememberedRun(routedRun.id);
    const attempts = traceForRun(routedRun.id)?.attempts ?? [];
    setRememberedAttempt(attempts[attempts.length - 1]?.id ?? "");
  }, [fixture, rememberedRun, routedRun, setRememberedAttempt, setRememberedRun]);
  const selectRun = (run: Run, attemptId?: string) => {
    const nextTrace = traceForRun(run.id);
    setRememberedRun(run.id);
    setRememberedAttempt(attemptId ?? nextTrace?.attempts[nextTrace.attempts.length - 1]?.id ?? "");
    onState?.(run.id);
  };
  const selectJob = (job: string) => selectRun(RUNS.find((run) => run.job === job) ?? RUNS[0]);
  const selectAttempt = (attempt: Attempt) => selectRun(RUNS.find((run) => run.id === attempt.runId) ?? RUNS[0], attempt.id);
  const openAttention = () => {
    const attention = trace?.attentionId ? AUTOMATIONS_ATTENTION.find((item) => item.id === trace.attentionId) : undefined;
    if (attention) navigate(attention.target.surface, attention.target.params);
  };

  return (
    <div className="am-root">
      <div className="am-main">
        <div className="am-status">
          <div className="am-status-card"><span className="k">SCHEDULER STATUS</span>{fixture ? <><div className="am-status-line"><i className="am-dot is-running" /><ToneText>running</ToneText><small>authored at 2025-05-09 14:37:11 UTC</small></div><small className="am-daemon">illustrated authority: trace-sched@node-7</small></> : <><div className="am-status-line"><i className="am-dot is-off" /><ToneText>unavailable</ToneText><small>no observed scheduler authority</small></div><small className="am-daemon">configuration cannot establish runtime health</small></>}</div>
          <div className="am-actions"><button type="button" className="am-ctrl is-primary" disabled title="Synthetic plate: no scheduler mutation authority">Ⅱ Pause</button><button type="button" className="am-ctrl" disabled title="Synthetic plate: no scheduler mutation authority">▶ Resume</button></div>
          <div className="am-status-card am-next"><div className="am-due-head"><span className="k">NEXT DUE WINDOW</span><b>{fixture ? "in 02:14" : "—"}</b></div><Row l="earliest due" r={fixture ? "2025-05-09 14:39:30 UTC" : "unavailable"} /><Row l="latest due" r={fixture ? "2025-05-09 14:44:30 UTC" : "unavailable"} /></div>
        </div>
        <div className="am-kpis"><Kpi lab="DUE" value={fixture ? "26" : "—"} note={fixture ? "next 02:14" : "not served"} /><Kpi lab="OVERDUE" value={fixture ? "0" : "—"} note="" /><Kpi lab="RUNNING" value={fixture ? "3" : "—"} note="" /><Kpi lab="SKIPPED (WINDOW)" value={fixture ? "4" : "—"} note={fixture ? "last 15m" : "not served"} /><Kpi lab="SUCCESS (LAST 24H)" value={fixture ? "182" : "—"} note={fixture ? "92.4%" : "not served"} /><Kpi lab="FAILED (LAST 24H)" value={fixture ? "15" : "—"} note={fixture ? "7.6%" : "not served"} /></div>
        {fixture && selected ? trace ? <AutomationCanvas trace={trace} focusedAttemptId={focusedAttemptId ?? ""} onAttempt={selectAttempt} onAttention={openAttention} onTrace={(nextTrace) => selectRun(RUNS.find((run) => run.id === nextTrace.attempts[nextTrace.attempts.length - 1].runId) ?? RUNS[0])} /> : <UnservedRunCanvas run={selected} /> : <SnapshotCanvas />}
        <div className="am-mid">
          <Well title="MANAGED JOBS" className="am-jobs"><div className="am-table-scroll"><table><thead><tr><th>JOB</th><th>SCHEDULE</th><th>LAST RUN</th><th>LAST OUTCOME</th><th>NEXT DUE</th><th>STATE</th></tr></thead><tbody>{fixture ? JOBS.map((job) => <tr key={job[0]} data-job-id={job[0]} className={selected?.job === job[0] ? "is-related" : ""} tabIndex={0} onClick={() => selectJob(job[0])} onKeyDown={(event) => onRowKey(event, () => selectJob(job[0]))}><td><i className={`am-dot is-${job[6]}`} />{job[0]}</td><td>{job[1]}</td><td>{job[2]}</td><td><ToneText>{job[3]}</ToneText></td><td>{job[4]}</td><td><ToneText>{job[5]}</ToneText></td></tr>) : <UnavailableRow columns={6}>No managed-job authority in this snapshot.</UnavailableRow>}</tbody></table></div></Well>
          <Well title="SKILLS" className="am-skills"><div className="am-table-scroll"><table><thead><tr><th>SKILL</th><th>AUTHORITY</th></tr></thead><tbody>{fixture ? SKILLS.map((skill) => <tr key={skill[0]}><td><i className={`am-dot is-${skill[2]}`} />{skill[0]}</td><td className="is-cyan">{skill[1]}</td></tr>) : <UnavailableRow columns={2}>No skill authority in this snapshot.</UnavailableRow>}</tbody></table></div></Well>
        </div>
        <Well title="AUTOMATIC FACT OUTCOMES" note={fixture ? "(LAST 10)" : "(UNAVAILABLE)"} className="am-facts"><div className="am-table-scroll"><table><tbody>{fixture ? OUTCOMES.map((row) => <tr key={row[0]} tabIndex={0} onClick={() => selectJob(row[1])} onKeyDown={(event) => onRowKey(event, () => selectJob(row[1]))}><td>{row[0]}</td><td>▣</td><td>{row[1]}</td><td><ToneText>{row[2]}</ToneText></td><td>{row[3]}</td><td>{row[4]}</td><td>ⓘ　◉</td></tr>) : <UnavailableRow columns={7}>No fact-outcome authority in this snapshot.</UnavailableRow>}</tbody></table></div></Well>
        <Well title="RUN LEDGER" note={fixture ? "(LATEST FIRST)" : "(UNAVAILABLE)"} className="am-ledger" footL={fixture ? "fixture sample: 8 authored runs" : "ledger window: unavailable"} footR={fixture ? "showing all 8 loaded rows" : "no ledger rows served"}><div className="am-table-scroll"><table><thead><tr><th>START (UTC)</th><th>JOB</th><th>DURATION</th><th>OUTCOME</th><th>FACT RECEIPT</th><th>ARTIFACTS</th><th>INTEGRITY</th></tr></thead><tbody>{fixture ? RUNS.map((run) => <tr key={run.id} data-run-id={run.id} className={selected?.id === run.id ? "selected" : ""} tabIndex={0} aria-selected={selected?.id === run.id} onClick={() => selectRun(run)} onKeyDown={(event) => onRowKey(event, () => selectRun(run))}><td><i className={`am-run-dot is-${run.integrity}`} />{run.start}</td><td>{run.job}</td><td>{run.duration}</td><td><ToneText>{run.outcome}</ToneText></td><td>{run.receipt}</td><td>{run.artifacts}</td><td><ToneText>{run.integrity}</ToneText></td></tr>) : <UnavailableRow columns={7}>No durable run ledger authority in this snapshot.</UnavailableRow>}</tbody></table></div></Well>
      </div>
      <Inspector run={selected} trace={trace} focusedAttemptId={focusedAttemptId} onAttempt={selectAttempt} onReceipt={() => setReceiptOpen(true)} />
      {receiptOpen && selected && <div className="am-modal-backdrop" role="presentation" onMouseDown={() => setReceiptOpen(false)}><section className="am-modal" role="dialog" aria-modal="true" aria-labelledby="am-receipt-title" onKeyDown={(event) => event.key === "Escape" && setReceiptOpen(false)} onMouseDown={(event) => event.stopPropagation()}><Corners /><h2 id="am-receipt-title">FACT RECEIPT · {selected.receipt}</h2><p>Read-only authored example. No scheduler request was sent.</p><Row l="RUN" r={selected.id} /><Row l="OUTCOME" r={selected.outcome} tone={selected.outcome} /><Row l="SIGNED BY" r="trace-sched@node-7" /><button type="button" autoFocus onClick={() => setReceiptOpen(false)}>Close</button></section></div>}
    </div>
  );
}
