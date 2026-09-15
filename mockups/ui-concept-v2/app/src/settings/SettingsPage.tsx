import { Fragment, useEffect, useMemo, useRef, type KeyboardEvent as ReactKeyboardEvent } from "react";
import { useDemo, useWorkspaceState } from "../app/workspace";
import { PACK, PROFILE } from "../data/pack";
import "./settings.css";

const SECTIONS = ["General", "Code Indexing", "Ingestion", "Analysis", "Memory", "Retrieval", "Automation", "Delivery", "Observability", "Security", "Integrations", "Advanced"] as const;
type Section = (typeof SECTIONS)[number];
type Layer = "built-in" | "organization" | "system" | "profile" | "project" | "worktree/session" | "runtime";
type Impact = "immediate" | "restart required" | "reindex required" | "reconnect required";

const LAYERS: Layer[] = ["built-in", "organization", "system", "profile", "project", "worktree/session", "runtime"];
const SECTION_PREFIX: Partial<Record<Section, string[]>> = {
  "Code Indexing": ["code.index."], Ingestion: ["ingest."], Analysis: ["analysis."], Memory: ["memory."], Retrieval: ["retrieval."],
  Delivery: ["delivery."], Observability: ["observability."], Security: ["security."], Integrations: ["remote."],
};

type KeyRow = { key: string; value: string; provenance: string; origin: string };
type FixtureDefinition = KeyRow & {
  section: Section;
  winner: Layer;
  layers: Partial<Record<Layer, string>>;
  impact: Impact;
  affected: { label: string; surface: string }[];
};
type Receipt = {
  validation: "not run" | "valid" | "invalid";
  persistence: "not attempted" | "persisted" | "conflicted · stale revision";
  readback: "not run" | "confirmed";
  adoption: "not started" | "adopted" | Impact;
};

const IDLE_RECEIPT: Receipt = { validation: "not run", persistence: "not attempted", readback: "not run", adoption: "not started" };
const GRAPH_HEADS = PACK.projects.reduce((n, p) => n + (p.graphVerifiedHeads ?? 0), 0);
const ANCHORS = PACK.projects.reduce((n, p) => n + (p.retrievalAnchors ?? 0), 0);
const FACTS = PACK.projects.some((p) => p.factsTable && p.factsTable !== "absent") ? "present" : "absent";

const SNAPSHOT_ROWS: KeyRow[] = [
  { key: "profile.id", value: PROFILE.profileId, provenance: "snapshot", origin: "profile-pack" },
  { key: "brain.id", value: PROFILE.brainId, provenance: "snapshot", origin: "profile-pack" },
  { key: "daemon", value: PROFILE.daemon, provenance: "snapshot", origin: "profile-pack" },
  { key: "captured_at", value: PACK.capturedAt, provenance: "snapshot", origin: "profile-pack" },
  { key: "projects.enrolled", value: String(PACK.projects.length), provenance: "snapshot", origin: "profile-pack" },
  { key: "analysis.model", value: "unavailable", provenance: "unserved", origin: "—" },
  { key: "analysis.max_concurrency", value: "unavailable", provenance: "unserved", origin: "—" },
  { key: "code.index.sealed", value: "false", provenance: "snapshot", origin: "profile-pack" },
  { key: "code.index.graph_verified_heads", value: String(GRAPH_HEADS), provenance: "snapshot", origin: "profile-pack" },
  { key: "ingest.rate.limit", value: "unavailable", provenance: "unserved", origin: "—" },
  { key: "retrieval.anchors", value: String(ANCHORS), provenance: "snapshot", origin: "profile-pack" },
  { key: "retrieval.top_k", value: "unavailable", provenance: "unserved", origin: "—" },
  { key: "memory.facts.table", value: FACTS, provenance: "snapshot", origin: "profile-pack" },
  { key: "memory.ttl.default", value: "unavailable", provenance: "unserved", origin: "—" },
  { key: "delivery.provider.inbox", value: "not_published", provenance: "snapshot", origin: "profile-pack" },
  { key: "observability.metrics.level", value: "unavailable", provenance: "unserved", origin: "—" },
  { key: "security.redact.secrets", value: "unavailable", provenance: "unserved", origin: "—" },
];

const FIXTURE_DEFINITIONS: FixtureDefinition[] = [
  { key: "analysis.similarity_threshold", value: "0.82", provenance: "project", origin: "project:tracedecay", section: "Analysis", winner: "project", layers: { "built-in": "0.60", organization: "0.70", profile: "0.76", project: "0.82" }, impact: "immediate", affected: [{ label: "retrieval", surface: "explorer" }, { label: "analysis", surface: "code" }] },
  { key: "analysis.model", value: "trace-decay-3.1", provenance: "profile", origin: "profile:default", section: "Analysis", winner: "profile", layers: { "built-in": "trace-decay-3.0", organization: "trace-decay-3.0", profile: "trace-decay-3.1" }, impact: "restart required", affected: [{ label: "daemon", surface: "observatory" }, { label: "workflows", surface: "workflows" }] },
  { key: "code.index.refresh_interval", value: "15m", provenance: "project", origin: "project:tracedecay", section: "Code Indexing", winner: "project", layers: { "built-in": "30m", system: "20m", project: "15m" }, impact: "reindex required", affected: [{ label: "code index", surface: "code" }, { label: "retrieval", surface: "explorer" }] },
  { key: "retrieval.top_k", value: "25", provenance: "project", origin: "project:tracedecay", section: "Retrieval", winner: "project", layers: { "built-in": "10", organization: "20", project: "25" }, impact: "immediate", affected: [{ label: "retrieval", surface: "explorer" }] },
  { key: "remote.brain.endpoint", value: "brain-fixture.local", provenance: "profile", origin: "profile:default", section: "Integrations", winner: "profile", layers: { "built-in": "disabled", profile: "brain-fixture.local" }, impact: "reconnect required", affected: [{ label: "Remote Brain", surface: "settings" }, { label: "daemon", surface: "observatory" }] },
  { key: "security.redact.secrets", value: "true", provenance: "organization", origin: "org:security-policy", section: "Security", winner: "organization", layers: { "built-in": "true", organization: "true" }, impact: "immediate", affected: [{ label: "ingestion", surface: "sessions" }, { label: "delivery", surface: "delivery" }] },
];

function Kv({ l, r, abs }: { l: string; r: string; abs?: boolean }) {
  return <div className="st-kv"><span>{l}</span><b className={abs ? "abs" : undefined}>{r}</b></div>;
}

function validate(definition: FixtureDefinition, value: string) {
  if (!value.trim()) return false;
  if (definition.key.endsWith("threshold")) return Number(value) >= 0 && Number(value) <= 1;
  if (definition.key === "retrieval.top_k") return Number.isInteger(Number(value)) && Number(value) > 0 && Number(value) <= 100;
  if (definition.key.endsWith("refresh_interval")) return /^\d+[smh]$/.test(value);
  if (definition.key === "security.redact.secrets") return value === "true" || value === "false";
  return true;
}

function StrataView({ row, definition, proposal, receipt, changed, onNavigate }: {
  row: KeyRow | undefined; definition: FixtureDefinition | undefined; proposal: string; receipt: Receipt; changed: boolean;
  onNavigate: (surface: string) => void;
}) {
  return (
    <section className="st-structure" aria-label="Configuration precedence and proposal impact">
      <div className={`st-strata${definition ? " has-path" : " is-unserved"}`}>
        <header><b>PRECEDENCE STRATA</b><span>{row?.key ?? "select a key"}</span></header>
        <div className="st-layer-list">
          {LAYERS.map((layer) => {
            const served = definition?.layers[layer];
            const winner = definition?.winner === layer;
            const effective = winner && row ? row.value : served;
            return <div key={layer} className={`st-layer${winner ? " is-winner" : served ? " is-overridden" : " is-empty"}`}><span>{layer}</span><b>{effective ?? "unserved"}</b><em>{winner ? "winning path" : served ? "overridden" : "no served value"}</em></div>;
          })}
        </div>
      </div>
      <div className="st-impact">
        <header><b>PROPOSAL IMPACT</b><span>{definition ? definition.impact : "write metadata unserved"}</span></header>
        <div className="st-stations">
          <span className={changed ? "is-preview" : ""}><i>1</i>PROPOSAL<small>{changed ? proposal : "none"}</small></span>
          <span className={receipt.validation === "valid" ? "is-done" : receipt.validation === "invalid" ? "is-error" : ""}><i>2</i>VALIDATION<small>{receipt.validation}</small></span>
          <span className={receipt.persistence === "persisted" ? "is-done" : receipt.persistence.startsWith("conflicted") ? "is-error" : ""}><i>3</i>PERSISTED<small>{receipt.persistence}</small></span>
          <span className={receipt.readback === "confirmed" ? "is-done" : ""}><i>4</i>READ-BACK<small>{receipt.readback}</small></span>
          <span className={receipt.adoption === "adopted" ? "is-done" : receipt.adoption.endsWith("required") ? "is-wait" : ""}><i>5</i>RUNTIME<small>{receipt.adoption}</small></span>
        </div>
        <div className="st-affected" aria-label="Affected components">{(definition?.affected ?? []).map((item) => <button type="button" key={item.label} onClick={() => onNavigate(item.surface)}>{item.label} →</button>)}{!definition && <span>Impact edges remain unavailable until served.</span>}</div>
      </div>
    </section>
  );
}

function ReviewEditor({ fixture, revision, row, definition, proposal, setProposal, receipt, stale, setStale, onApply, onCancel, onAdopt }: {
  fixture: boolean; row: KeyRow; definition?: FixtureDefinition; proposal: string; setProposal: (value: string) => void; receipt: Receipt;
  revision: number;
  stale: boolean; setStale: (value: boolean) => void; onApply: () => void; onCancel: () => void; onAdopt: () => void;
}) {
  const changed = fixture && proposal !== row.value;
  return (
    <div className="st-apply">
      <div className="st-applybox">
        <div className="cell"><div className="st-cap">PROPOSED VALUE</div>{fixture ? <input aria-label={`Proposed value for ${row.key}`} value={proposal} onChange={(event) => setProposal(event.target.value)} /> : <span className="abs">none · snapshot is read-only</span>}</div>
        <div className="cell"><div className="st-cap dim">CURRENT REVISION</div><span className={fixture ? undefined : "abs"}>{fixture ? `fixture-r${revision}` : "CAS not offered"}</span></div>
        <div className="cell"><div className="st-cap dim">TARGET / IMPACT</div><span className={fixture ? undefined : "abs"}>{fixture ? `${definition?.winner} · ${definition?.impact}` : "n/a"}</span></div>
      </div>
      <div className="st-btns">
        {fixture && <button type="button" className={`st-stale${stale ? " is-on" : ""}`} aria-pressed={stale} onClick={() => setStale(!stale)}>STALE REVISION</button>}
        <button type="button" className="st-btn cy" disabled={!changed || receipt.validation !== "valid"} onClick={onApply}>{fixture ? "APPLY LOCAL FIXTURE" : <>APPLY <small>UNAVAILABLE</small></>}</button>
        <button type="button" className="st-btn am" disabled={!fixture || !changed} onClick={onCancel}>CANCEL</button>
        {fixture && receipt.adoption.endsWith("required") && <button type="button" className="st-btn cy" onClick={onAdopt}>MARK ADOPTED</button>}
      </div>
    </div>
  );
}

export function SettingsPage(_props: { onInspect?: unknown; state?: string; onState?: (id: string) => void } = {}) {
  const { mode, navigate } = useDemo();
  const fixture = mode === "fixture";
  const initialValues = Object.fromEntries(FIXTURE_DEFINITIONS.map((row) => [row.key, row.value]));
  const [fixtureValues, setFixtureValues] = useWorkspaceState<Record<string, string>>("settings:values", initialValues);
  const [section, setSection] = useWorkspaceState<Section>("settings:section", "General");
  const [q, setQ] = useWorkspaceState("settings:query", "");
  const [sel, setSel] = useWorkspaceState("settings:selected", fixture ? FIXTURE_DEFINITIONS[0].key : "memory.facts.table");
  const [proposal, setProposalState] = useWorkspaceState<{ key: string; value: string }>("settings:proposal", { key: "", value: "" });
  const [receipt, setReceipt] = useWorkspaceState<Receipt>("settings:receipt", IDLE_RECEIPT);
  const [stale, setStale] = useWorkspaceState("settings:stale", false);
  const [revision, setRevision] = useWorkspaceState("settings:revision", 12);
  const searchRef = useRef<HTMLInputElement>(null);
  const sectionRefs = useRef<Array<HTMLButtonElement | null>>([]);
  const rowRefs = useRef<Array<HTMLButtonElement | null>>([]);
  const allRows: KeyRow[] = fixture ? FIXTURE_DEFINITIONS.map((row) => ({ ...row, value: fixtureValues[row.key] ?? row.value })) : SNAPSHOT_ROWS;
  const rows = useMemo(() => {
    const prefixes = SECTION_PREFIX[section];
    const needle = q.trim().toLowerCase();
    return allRows.filter((row) => {
      if (section !== "General" && (!prefixes || !prefixes.some((prefix) => row.key.startsWith(prefix)))) return false;
      return !needle || `${row.key} ${row.value} ${row.provenance} ${row.origin}`.toLowerCase().includes(needle);
    });
  }, [allRows, section, q]);
  const selected = rows.find((row) => row.key === sel) ?? rows[0];
  const definition = fixture ? FIXTURE_DEFINITIONS.find((row) => row.key === selected?.key) : undefined;
  const proposalValue = proposal.key === selected?.key ? proposal.value : selected?.value ?? "";
  const changed = fixture && Boolean(selected) && proposalValue !== selected.value;

  useEffect(() => {
    const shortcut = (event: KeyboardEvent) => { if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "k") { event.preventDefault(); searchRef.current?.focus(); } };
    window.addEventListener("keydown", shortcut);
    return () => window.removeEventListener("keydown", shortcut);
  }, []);

  const selectRow = (row: KeyRow) => { setSel(row.key); setProposalState({ key: row.key, value: row.value }); setReceipt(IDLE_RECEIPT); setStale(false); };
  const moveSection = (current: Section, delta: number) => { const next = (SECTIONS.indexOf(current) + delta + SECTIONS.length) % SECTIONS.length; setSection(SECTIONS[next]); sectionRefs.current[next]?.focus(); };
  const moveRow = (event: ReactKeyboardEvent<HTMLButtonElement>, index: number, delta: number) => { event.preventDefault(); const next = Math.max(0, Math.min(rows.length - 1, index + delta)); selectRow(rows[next]); rowRefs.current[next]?.focus(); };
  const returnFocus = () => requestAnimationFrame(() => rowRefs.current[rows.findIndex((row) => row.key === selected?.key)]?.focus());
  const apply = () => {
    if (!definition || !selected || !changed) return;
    if (!validate(definition, proposalValue)) { setReceipt({ ...IDLE_RECEIPT, validation: "invalid" }); returnFocus(); return; }
    if (stale) { setReceipt({ validation: "valid", persistence: "conflicted · stale revision", readback: "not run", adoption: "not started" }); returnFocus(); return; }
    setFixtureValues((values) => ({ ...values, [selected.key]: proposalValue }));
    setRevision((value) => value + 1);
    setReceipt({ validation: "valid", persistence: "persisted", readback: "confirmed", adoption: definition.impact === "immediate" ? "adopted" : definition.impact });
    returnFocus();
  };
  const cancel = () => { if (!selected) return; setProposalState({ key: selected.key, value: selected.value }); setReceipt(IDLE_RECEIPT); setStale(false); returnFocus(); };

  return (
    <div className="st-root">
      <aside className="st-nav" aria-label="Settings sections">
        <div className="st-cap">SECTIONS</div>
        {SECTIONS.map((item, index) => <button key={item} type="button" className={item === section ? "st-item is-on" : "st-item"} onClick={() => setSection(item)} onKeyDown={(event) => { if (event.key === "ArrowDown" || event.key === "ArrowUp") { event.preventDefault(); moveSection(item, event.key === "ArrowDown" ? 1 : -1); } }} ref={(node) => { sectionRefs.current[index] = node; }}>{item}</button>)}
        <label className="st-section-menu"><span>SECTION</span><select value={section} onChange={(event) => setSection(event.target.value as Section)}>{SECTIONS.map((item) => <option key={item}>{item}</option>)}</select></label>
      </aside>

      <main className="st-main">
        <div className="st-search">
          <div className="st-field"><svg viewBox="0 0 16 16" aria-hidden="true"><circle cx="7" cy="7" r="4.4" fill="none" stroke="currentColor" strokeWidth="1.4" /><line x1="10.4" y1="10.4" x2="14" y2="14" stroke="currentColor" strokeWidth="1.4" /></svg><input ref={searchRef} type="search" value={q} onChange={(event) => setQ(event.target.value)} placeholder="Search effective settings (key or value)" aria-label="Search effective settings" onKeyDown={(event) => { if (event.key === "Escape" && q) { event.preventDefault(); setQ(""); } }} /><kbd>⌘K</kbd></div>
          <div className="st-scope"><div><span>SCOPE</span><b>project: all</b></div><div><span>MODE</span><b>{fixture ? "fixture-local" : "effective-only"}</b></div></div>
        </div>
        <StrataView row={selected} definition={definition} proposal={proposalValue} receipt={receipt} changed={changed} onNavigate={(surface) => navigate(surface, { source: "settings", key: selected?.key ?? "" })} />
        <section className="st-well" aria-label="Effective settings">
          <div className="st-cap st-well-cap">EFFECTIVE SETTINGS (KEY / VALUE)</div>
          <div className="st-scroll">
            <div className="st-gridrow st-head"><span>KEY</span><span>VALUE</span><span>PROVENANCE</span><span>ORIGIN</span><span>WRITE</span><span>APPLY</span></div>
            {rows.length === 0 ? <p className="st-noserve">No effective keys are served for {section} in this {fixture ? "fixture" : "snapshot"}. Missing layers stay unserved.</p> : rows.map((row, index) => (
              <Fragment key={row.key}>
                <button type="button" className={`st-gridrow st-row${row.key === selected?.key ? " is-sel" : ""}`} onClick={() => selectRow(row)} onKeyDown={(event) => { if (event.key === "ArrowDown" || event.key === "ArrowUp") moveRow(event, index, event.key === "ArrowDown" ? 1 : -1); }} ref={(node) => { rowRefs.current[index] = node; }}><span className="k" title={row.key}>{row.key}</span><span className={row.provenance === "unserved" ? "v abs" : "v"} title={row.value}>{row.value}</span><span className={`p p-${row.provenance}`} title={row.provenance}><i>{row.provenance}</i></span><span className="o" title={row.origin}>{row.origin}</span><span className={fixture ? "w" : "w abs"} title={fixture ? "local fixture only" : "unserved"}>{fixture ? "local only" : "unserved"}</span><span className={fixture ? "a" : "a abs"} title={fixture ? FIXTURE_DEFINITIONS.find((item) => item.key === row.key)?.impact : "unserved"}>{fixture ? FIXTURE_DEFINITIONS.find((item) => item.key === row.key)?.impact : "unserved"}</span></button>
                {row.key === selected?.key && <ReviewEditor fixture={fixture} revision={revision} row={row} definition={definition} proposal={proposalValue} setProposal={(value) => { setProposalState({ key: row.key, value }); setReceipt({ ...IDLE_RECEIPT, validation: definition && validate(definition, value) ? "valid" : "invalid" }); }} receipt={receipt} stale={stale} setStale={setStale} onApply={apply} onCancel={cancel} onAdopt={() => { setReceipt((prior) => ({ ...prior, adoption: "adopted" })); returnFocus(); }} />}
              </Fragment>
            ))}
          </div>
        </section>
      </main>

      <aside className="st-side" aria-label="Provenance review">
        <div className="st-card"><h2 className="ink">PROVENANCE / REVIEW</h2><div className="st-pair"><span>CONFIG SOURCE</span><b>{fixture ? "fixture-local" : "effective-only"}</b></div><div className="st-pair"><span>EVALUATED AT</span><b>{fixture ? "2026-09-08T14:30:00Z" : PACK.capturedAt}</b></div><div className="st-pair"><span>CONFIG REVISION (CAS)</span><b className={fixture ? undefined : "abs"}>{fixture ? `fixture-r${revision}` : "not offered"}</b></div><div className="st-pair"><span>SCOPE</span><b>project: <em>all</em></b></div><hr /><p>{fixture ? "Local fixture writes use revision checks and read back from session state. They never change host configuration." : "No CAS revision is served by this snapshot, so edits cannot be validated or persisted."}</p></div>
        <div className="st-card"><h2>MULTI-ROOT CAPABILITY</h2><Kv l="projects.enrolled" r={String(PACK.projects.length)} /><Kv l="roots.listed" r={String(PACK.projects.filter((project) => project.root).length)} /><Kv l="roots.configured" r="unavailable" abs /><Kv l="roots.health" r="unavailable" abs /><button type="button" className="st-link" onClick={() => navigate("brain", { source: "settings" })}>view roots →</button></div>
        <div className="st-card"><h2>REMOTE BRAIN <i>(UNSERVED)</i></h2><Kv l="daemon" r={PROFILE.daemon} /><Kv l="connected" r="unavailable" abs /><Kv l="observed" r="unavailable" abs /><Kv l="health" r="unavailable" abs /><button type="button" className="st-link" onClick={() => navigate("observatory", { source: "settings" })}>view status →</button></div>
      </aside>
      <footer className="st-foot">{fixture ? "FIXTURE / LOCAL SESSION ONLY · proposals become effective only after persisted read-back; runtime adoption remains separate." : "Showing effective snapshot values only. Unserved provenance and write authority are never inferred."}</footer>
    </div>
  );
}
