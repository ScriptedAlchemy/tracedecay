import { useEffect, useState, type CSSProperties, type KeyboardEvent as ReactKeyboardEvent } from "react";
import { Corners } from "../app/shell/Corners";
import { useDemo, useWorkspaceState } from "../app/workspace";
import { PACK, PROFILE, providerCounts } from "../data/pack";
import sparksRaw from "../data/session-sparks.json";
import "./costs.css";

/*
 * Costs keeps the profile snapshot honest while offering a clearly marked,
 * session-local fixture for the complete usage-to-outcome interaction. The
 * snapshot never manufactures token or price values. Fixture totals derive
 * only from the exact records below and never escape browser workspace state.
 */

const HUES: Record<string, string> = {
  claude: "#f0b429",
  cursor: "#5ee7ff",
  codex: "#c084fc",
};

const NULL_CELL = "—";

type ProviderStat = {
  name: string;
  hue: string;
  sessions: number;
  projects: number;
  messages: number;
  models: number | null;
  tokens: number | null;
  spend: number | null;
};

type ProviderSelection = {
  selected: string | null;
  onSelect: (name: string | null) => void;
};

type CostFlow = {
  id: string;
  event: string;
  run: string;
  provider: string;
  model: string;
  project: string;
  outcome: string;
  tokens: number | null;
  spend: number | null;
  topology: "direct" | "agent" | null;
  state: "priced" | "unpriced" | "gap" | "unavailable";
  source: string;
};

const FIXTURE_FLOWS: CostFlow[] = [
  { id: "evt-91c2", event: "MODEL CALL · 14:31", run: "run-707-review", provider: "OpenAI", model: "gpt-5.2-codex", project: "tracedecay", outcome: "PR #707 · review", tokens: 2_840_000, spend: 18.72, topology: "direct", state: "priced", source: "fixture/usage-ledger/evt-91c2" },
  { id: "evt-a044", event: "MODEL CALL · 14:38", run: "run-707-tests", provider: "Anthropic", model: "claude-sonnet-4", project: "tracedecay", outcome: "PR #707 · checks", tokens: 1_960_000, spend: null, topology: "agent", state: "unpriced", source: "fixture/usage-ledger/evt-a044" },
  { id: "evt-b781", event: "TOOL LOOP · 14:44", run: "run-batch-19", provider: "OpenAI", model: "gpt-5.2-codex", project: "identity unresolved", outcome: "attribution gap", tokens: 780_000, spend: 5.14, topology: "direct", state: "gap", source: "fixture/usage-ledger/evt-b781" },
];

function providerStats(): ProviderStat[] {
  return providerCounts().map(([name, sessions]) => {
    const own = PACK.sessions.filter((s) => s.provider === name);
    return {
      name,
      hue: HUES[name] ?? "#8b99a8",
      sessions,
      projects: new Set(own.map((s) => s.project)).size,
      messages: own.reduce((a, s) => a + s.messages, 0),
      models: null,
      tokens: null,
      spend: null,
    };
  });
}

function fixtureProviderStats(): ProviderStat[] {
  return [
    { name: "OpenAI", hue: "#5ee7ff", sessions: 2, projects: 1, messages: 2, models: 1, tokens: 3_620_000, spend: 23.86 },
    { name: "Anthropic", hue: "#f0b429", sessions: 1, projects: 1, messages: 1, models: 1, tokens: 1_960_000, spend: null },
  ];
}

function snapshotFlows(): CostFlow[] {
  return providerStats().map((provider) => ({
    id: `snapshot-${provider.name}`,
    event: `${provider.messages.toLocaleString("en-US")} spine messages`,
    run: `${provider.sessions} recorded sessions`,
    provider: provider.name,
    model: "model identity unserved",
    project: `${provider.projects} observed projects`,
    outcome: "deliverable attribution unavailable",
    tokens: null,
    spend: null,
    topology: null,
    state: "unavailable",
    source: "profile-pack/message-spine",
  }));
}

function compactTokens(value: number | null) {
  if (value === null) return "token width unknown";
  return value >= 1_000_000 ? `${(value / 1_000_000).toFixed(2)}M tokens` : `${Math.round(value / 1000)}k tokens`;
}

function FlowCanvas(props: { flows: CostFlow[]; selected: string | null; selectedProvider: string | null; onSelect: (flow: CostFlow) => void; fixture: boolean }) {
  const maxTokens = Math.max(1, ...props.flows.map((flow) => flow.tokens ?? 0));
  return (
    <section className="cs-well cs-flow" aria-label="Usage to outcome cost flow">
      <header>
        <div>
          <b>USAGE → PROVIDER / MODEL → PROJECT → OUTCOME</b>
          <span>stable landmarks · width is measured tokens only</span>
        </div>
        <div className="cs-flow-key" aria-label="Flow encoding key">
          <span><i className="priced" /> priced band</span>
          <span><i className="unpriced" /> unpriced hatch</span>
          <span><i className="gap" /> attribution gap</span>
        </div>
      </header>
      <div className="cs-flow-stage" aria-hidden="true">
        <span>MEASURED USAGE / RUN</span><i /><span>PROVIDER / MODEL</span><i /><span>PROJECT / REPOSITORY</span><i /><span>OUTCOME</span>
      </div>
      <div className={`cs-budget-boundary${props.fixture ? "" : " is-unserved"}`}>
        <b>{props.fixture ? "BUDGET BOUNDARY · project: tracedecay · SEP" : "BUDGET BOUNDARY · unserved"}</b>
        <span>{props.fixture ? "$60.00 cap · $18.72 attributed priced · $41.28 remaining · 59% project-token pricing coverage" : "amount / period / freshness unavailable"}</span>
      </div>
      <div className="cs-flow-list">
        {props.flows.map((flow) => {
          const width = flow.tokens === null ? 3 : 5 + Math.round((flow.tokens / maxTokens) * 13);
          return (
            <button
              type="button"
              key={flow.id}
              className={`cs-flow-row is-${flow.state}${props.selected === flow.id ? " is-selected" : ""}${props.selectedProvider && props.selectedProvider !== flow.provider ? " is-dim" : ""}`}
              style={{ "--flow-width": `${width}px` } as CSSProperties}
              aria-pressed={props.selected === flow.id}
              onClick={() => props.onSelect(flow)}
            >
              <span className="cs-flow-node"><b>{flow.event}</b><small>{compactTokens(flow.tokens)} · {flow.run}</small></span>
              <i className="cs-flow-link" />
              <span className="cs-flow-node"><b>{flow.provider}</b><small>{flow.model}</small></span>
              <i className="cs-flow-link" />
              <span className="cs-flow-node"><b>{flow.project}</b><small>{flow.state === "gap" ? "exact join missing" : "attribution served"}</small></span>
              <i className="cs-flow-link" />
              <span className="cs-flow-node"><b>{flow.outcome}</b><small>{flow.spend === null ? (flow.state === "unavailable" ? "spend unavailable" : "spend unpriced") : `$${flow.spend.toFixed(2)} priced spend`}</small></span>
            </button>
          );
        })}
      </div>
      <footer>{props.fixture ? "FIXTURE / SYNTHETIC · exact IDs are local demonstration records" : "PROFILE SNAPSHOT · event counts served; token width, pricing, and outcome attribution remain unavailable"}</footer>
    </section>
  );
}

/** [unixTs, messageCount, sessionIndex] rows from profile-pack message-spine timestamps. */
const SPARK_POINTS = (sparksRaw as unknown as { points?: [number, number, number][] }).points ?? [];

type EventWindow = { days: number[]; timestamped: number; startDayTs: number };

/**
 * Message events per UTC day across the snapshot window, aggregated from the
 * spine's message timestamps (session-sparks points) — not session starts,
 * not spend. Messages without a timestamp cannot be placed on the axis.
 */
function dailyMessageEvents(): EventWindow {
  const byDay = new Map<string, number>();
  let min = Infinity;
  let max = -Infinity;
  let timestamped = 0;
  for (const [ts, count] of SPARK_POINTS) {
    if (!ts || !count) continue;
    if (ts < min) min = ts;
    if (ts > max) max = ts;
    timestamped += count;
    const d = new Date(ts * 1000).toISOString().slice(0, 10);
    byDay.set(d, (byDay.get(d) ?? 0) + count);
  }
  if (!Number.isFinite(min)) return { days: [], timestamped: 0, startDayTs: 0 };
  const startDayTs = min - (min % 86400);
  const days: number[] = [];
  for (let t = startDayTs; t <= max; t += 86400) {
    days.push(byDay.get(new Date(t * 1000).toISOString().slice(0, 10)) ?? 0);
  }
  return { days, timestamped, startDayTs };
}

const MONTHS = ["JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC"];

function dayLabel(startDayTs: number, dayIndex: number): string {
  const d = new Date((startDayTs + dayIndex * 86400) * 1000);
  return `${MONTHS[d.getUTCMonth()]} ${String(d.getUTCDate()).padStart(2, "0")}`;
}

/** Keep the capture-time UTC offset visible; never silently UTC-wash it. */
function snapshotStamp(iso: string): string {
  const m = iso.match(/^(\d{4}-\d{2}-\d{2})T(\d{2}:\d{2}).*(Z|[+-]\d{2}:\d{2})$/);
  if (!m) return iso;
  return `${m[1]} ${m[2]} ${m[3] === "Z" ? "UTC" : m[3]}`;
}

function Spark(props: { values: number[]; cap?: string; hue?: string }) {
  const peak = Math.max(1, ...props.values);
  return (
    <div className="cs-spark" aria-hidden="true">
      {props.values.map((v, i) => (
        <i
          key={i}
          style={{
            height: `${Math.max(v > 0 ? 14 : 6, (v / peak) * 100)}%`,
            background: v > 0 ? (props.hue ?? "var(--signal-cyan)") : "rgba(90, 120, 140, 0.35)",
          }}
        />
      ))}
      {props.cap ? <b className="cap">{props.cap}</b> : null}
    </div>
  );
}

function SparkAbsent(props: { label: string }) {
  return (
    <div className="cs-spark cs-spark-absent" aria-hidden="true">
      <span>{props.label}</span>
    </div>
  );
}

function Kpi(props: {
  label: string;
  value: string;
  unit: string;
  note: string;
  absent?: boolean;
  spark?: number[];
  sparkCap?: string;
  sparkAbsent?: string;
}) {
  return (
    <div className="cs-kpi">
      <span className="k">{props.label}</span>
      <div className="cs-kpi-line">
        <b className={props.absent ? "abs" : undefined}>{props.value}</b>
        <em>{props.unit}</em>
      </div>
      {props.spark ? (
        <Spark values={props.spark} cap={props.sparkCap} />
      ) : (
        <SparkAbsent label={props.sparkAbsent ?? "no stream"} />
      )}
      <small>{props.note}</small>
    </div>
  );
}

function nextProvider(providers: ProviderStat[], current: string, delta: number): string {
  const index = providers.findIndex((provider) => provider.name === current);
  return providers[(index + delta + providers.length) % providers.length].name;
}

function handleProviderRowKeyDown(
  event: ReactKeyboardEvent<HTMLTableRowElement>,
  providers: ProviderStat[],
  current: string,
  selected: string | null,
  onSelect: (name: string | null) => void,
) {
  if (event.key === "Enter" || event.key === " ") {
    event.preventDefault();
    onSelect(selected === current ? null : current);
    return;
  }
  if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
  event.preventDefault();
  const name = nextProvider(providers, current, event.key === "ArrowDown" ? 1 : -1);
  onSelect(name);
  event.currentTarget.closest("tbody")?.querySelector<HTMLTableRowElement>(`[data-provider="${name}"]`)?.focus();
}

function SpendChart(props: { providers: ProviderStat[]; events: EventWindow; fixture: boolean; rangeOpen: boolean; onRangeToggle: () => void } & ProviderSelection) {
  const yTicks = props.fixture ? ["$24", "$18", "$12", "$6", "$0"] : ["$420", "$315", "$210", "$105", "$0"];
  const dayCount = Math.max(props.events.days.length, 2);
  const tickEvery = 4;
  const xTicks: string[] = [];
  for (let i = 0; i < dayCount; i += tickEvery) {
    xTicks.push(dayLabel(props.events.startDayTs, i));
  }
  const rangeCap = props.fixture ? "SEP 08 14:30–14:45 UTC" : `${dayLabel(props.events.startDayTs, 0)} – ${dayLabel(props.events.startDayTs, dayCount - 1)}`;
  return (
    <section className="cs-well cs-chart">
      <header>
        <div>
          <b>ACTUAL PROVIDER SPEND</b>
          <span>{props.fixture ? "USD · cumulative priced spend" : "USD · not manufactured"}</span>
        </div>
        <div className="cs-chart-total">
          <b className={props.fixture ? undefined : "abs"}>{props.fixture ? "$23.86 priced" : "— total"}</b>
          <span>{props.fixture ? `${rangeCap} · exact events` : `${rangeCap} (UTC) · timestamped-event window`}</span>
          <button type="button" className="cs-range-control" aria-expanded={props.rangeOpen} aria-controls="cost-range-status" onClick={props.onRangeToggle}>RANGE · {props.fixture ? "15 MIN" : "SOURCE WINDOW"}</button>
        </div>
      </header>
      {props.rangeOpen && <div className="cs-range-status" id="cost-range-status" role="status"><b>ONLY SERVED WINDOW</b><span>{props.fixture ? `${rangeCap}. Alternate windows are not present in this local fixture.` : `${rangeCap} UTC. The snapshot does not serve alternate spend windows.`}</span></div>}
      <div className="cs-chart-body">
        <div className="cs-axis" aria-hidden="true">
          {yTicks.map((t) => (
            <span key={t}>{t}</span>
          ))}
        </div>
        <div className="cs-plot" role={props.fixture ? "img" : undefined} aria-label={props.fixture ? "Cumulative priced spend: OpenAI 18 dollars 72 cents at 14:31 UTC and 5 dollars 14 cents at 14:44 UTC. The Anthropic event at 14:38 UTC is unpriced and excluded from the dollar line." : undefined}>
          <div className="cs-grid" aria-hidden="true">
            {yTicks.map((t) => (
              <i key={t} />
            ))}
          </div>
          <div className="cs-plot-note" aria-hidden="true">
            <span className="cs-badge">{props.fixture ? "exact events" : "spend unpriced"}</span>
            <p>{props.fixture ? "cumulative priced dollars · hatched marker excluded from total" : "no ledger, no pricing table — tracks hold the $0 baseline, not a $0.00 reading"}</p>
          </div>
          {props.fixture && <>
            <svg className={`cs-fixture-series${props.selected && props.selected !== "OpenAI" ? " is-dim" : ""}`} viewBox="0 0 100 100" preserveAspectRatio="none" aria-hidden="true">
              <path d="M 0 100 H 7 V 22 H 53 V 22 H 93 V 1 H 100" />
              <circle cx="7" cy="22" r="1.3" vectorEffect="non-scaling-stroke" />
              <circle cx="93" cy="1" r="1.3" vectorEffect="non-scaling-stroke" />
            </svg>
            <div className={`cs-unpriced-marker${props.selected && props.selected !== "Anthropic" ? " is-dim" : ""}`} aria-hidden="true"><span>unpriced</span></div>
          </>}
          {/* All tracks sit ON the $0 gridline — any lift above it would encode
              positive spend. Providers are told apart horizontally: interleaved
              dash phases on the line, and one dot per provider per day cluster. */}
          <div className="cs-tracks" role="group" aria-label="Provider spend series">
            {!props.fixture && props.providers.map((p, i) => (
              <button
                key={p.name}
                type="button"
                className={`ln${props.selected === p.name ? " is-selected" : ""}${props.selected && props.selected !== p.name ? " is-dim" : ""}`}
                aria-label={`Select ${p.name} provider series`}
                aria-pressed={props.selected === p.name}
                onClick={() => props.onSelect(props.selected === p.name ? null : p.name)}
                onKeyDown={(event) => {
                  if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
                  event.preventDefault();
                  const name = nextProvider(props.providers, p.name, event.key === "ArrowDown" ? 1 : -1);
                  props.onSelect(name);
                  event.currentTarget.parentElement?.querySelector<HTMLButtonElement>(`[data-provider="${name}"]`)?.focus();
                }}
                data-provider={p.name}
                style={{
                  background: `repeating-linear-gradient(90deg, ${p.hue} 0 5px, transparent 5px 15px)`,
                  backgroundPosition: `${i * 5}px center`,
                  backgroundSize: "auto 1px",
                  backgroundRepeat: "no-repeat",
                }}
              />
            ))}
            {!props.fixture && <div className="cs-days" aria-hidden="true">
              {props.events.days.map((_, d) => (
                <span key={d} className="day">
                  {props.providers.map((p) => (
                    <i key={p.name} className="pt" style={{ background: p.hue, boxShadow: `0 0 4px ${p.hue}aa` }} />
                  ))}
                </span>
              ))}
            </div>}
          </div>
          <div className="cs-xticks" aria-hidden="true">
            {(props.fixture ? ["14:30", "14:35", "14:40", "14:45"] : xTicks).map((t) => (
              <span key={t}>{t}</span>
            ))}
          </div>
        </div>
        <div className="cs-legend">
          {props.providers.map((p) => (
            <button
              type="button"
              className={`cs-legend-row${props.selected === p.name ? " is-selected" : ""}${props.selected && props.selected !== p.name ? " is-dim" : ""}`}
              key={p.name}
              aria-pressed={props.selected === p.name}
              onClick={() => props.onSelect(props.selected === p.name ? null : p.name)}
              onKeyDown={(event) => {
                if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
                event.preventDefault();
                const name = nextProvider(props.providers, p.name, event.key === "ArrowDown" ? 1 : -1);
                props.onSelect(name);
                event.currentTarget.parentElement
                  ?.querySelector<HTMLButtonElement>(`[data-provider="${name}"]`)
                  ?.focus();
              }}
              data-provider={p.name}
            >
              <i style={{ background: p.hue, boxShadow: `0 0 6px ${p.hue}55` }} />
              <div>
                <b>{p.name}</b>
                <span>
                  <em className={p.spend === null ? "abs" : undefined}>{p.spend === null ? NULL_CELL : `$${p.spend.toFixed(2)}`}</em> · {p.spend === null ? "unpriced" : "priced"}
                </span>
              </div>
            </button>
          ))}
          <div className="cs-legend-note">{props.fixture ? "3 exact fixture events" : `${PACK.totals.sessions} sessions · spine-only`}</div>
        </div>
      </div>
    </section>
  );
}

function CoverageTable(props: { providers: ProviderStat[]; fixture: boolean } & ProviderSelection) {
  return (
    <section className="cs-well cs-mid-well">
      <header>
        <b>PROJECT / MODEL / SESSION COVERAGE</b>
        <span>{props.fixture ? "exact fixture events · token coverage" : "usage from spine · spend not priced"}</span>
      </header>
      <table className="cs-cov-table">
        <colgroup>
          <col style={{ width: "25%" }} />
          <col style={{ width: "20%" }} />
          <col style={{ width: "14%" }} />
          <col style={{ width: "15%" }} />
          <col style={{ width: "26%" }} />
        </colgroup>
        <thead>
          <tr>
            <th>PROVIDER</th>
            <th className="num">MODELS</th>
            <th className="num">PROJ</th>
            <th className="num">{props.fixture ? "EVENTS" : "SESS"}</th>
            <th className="num">COVERAGE</th>
          </tr>
        </thead>
        <tbody>
          {props.providers.map((p) => (
            <tr
              key={p.name}
              className={props.selected === p.name ? "is-selected" : props.selected ? "is-dim" : undefined}
              tabIndex={0}
              data-provider={p.name}
              aria-selected={props.selected === p.name}
              onClick={() => props.onSelect(props.selected === p.name ? null : p.name)}
              onKeyDown={(event) => handleProviderRowKeyDown(event, props.providers, p.name, props.selected, props.onSelect)}
            >
              <td>{p.name}</td>
              <td className={p.models === null ? "num nul" : "num"}>{p.models ?? NULL_CELL}</td>
              <td className="num">{p.projects}</td>
              <td className="num">{p.sessions}</td>
              <td className="num">{props.fixture ? (p.tokens === null ? "unknown" : "100% tokens") : <span className="abs">spine</span>}</td>
            </tr>
          ))}
          <tr className="total">
            <td>total</td>
            <td className={props.fixture ? "num" : "num nul"}>{props.fixture ? 2 : NULL_CELL}</td>
            <td className="num">{props.fixture ? 1 : PACK.projects.filter((p) => p.sessions > 0).length}</td>
            <td className="num">{props.fixture ? 3 : PACK.totals.sessions}</td>
            <td className="num">{props.fixture ? "100% tokens" : <span className="abs">spine</span>}</td>
          </tr>
        </tbody>
      </table>
    </section>
  );
}

function PricingTable(props: { providers: ProviderStat[]; fixture: boolean } & ProviderSelection) {
  return (
    <section className="cs-well cs-mid-well">
      <header>
        <b>CANONICAL COST &amp; LATENCY OBSERVATIONS</b>
        <span>{props.fixture ? "application observed · rate components unserved" : "pricing table absent"}</span>
      </header>
      <table>
        <colgroup>
          <col style={{ width: "29%" }} />
          <col style={{ width: "21%" }} />
          <col style={{ width: "24%" }} />
          <col style={{ width: "26%" }} />
        </colgroup>
        <thead>
          <tr>
            <th>PROVIDER</th>
            <th className="num">IN / 1M</th>
            <th className="num">OUT / 1M</th>
            <th className="num">P50 LAT</th>
          </tr>
        </thead>
        <tbody>
          {props.providers.map((p) => (
            <tr
              key={p.name}
              className={props.selected === p.name ? "is-selected" : props.selected ? "is-dim" : undefined}
              tabIndex={0}
              data-provider={p.name}
              aria-selected={props.selected === p.name}
              onClick={() => props.onSelect(props.selected === p.name ? null : p.name)}
              onKeyDown={(event) => handleProviderRowKeyDown(event, props.providers, p.name, props.selected, props.onSelect)}
            >
              <td>{p.name}</td>
              <td className="num nul">{NULL_CELL}</td>
              <td className="num nul">{NULL_CELL}</td>
              <td className="num nul">{NULL_CELL}</td>
            </tr>
          ))}
          <tr className="total">
            <td>{props.fixture ? "priced windows" : "observed"}</td>
            <td className="num nul" colSpan={3}>
              {props.fixture ? "1 applicable · components unserved" : "0 rows"}
            </td>
          </tr>
        </tbody>
      </table>
    </section>
  );
}

function TopologyTable(props: { direct: number; agent: number; fixture: boolean }) {
  const rows: [string, string][] = [
    ["direct", String(props.direct)],
    ["agent", String(props.agent)],
    ["function-call", NULL_CELL],
    ["tool", NULL_CELL],
    ["batch", NULL_CELL],
  ];
  return (
    <section className="cs-well cs-mid-well">
      <header>
        <b>EXECUTION TOPOLOGY ACCOUNTING</b>
        <span>{props.fixture ? "exact fixture events · spend share unavailable" : "spend share unavailable"}</span>
      </header>
      <table>
        <colgroup>
          <col style={{ width: "46%" }} />
          <col style={{ width: "17%" }} />
          <col style={{ width: "17%" }} />
          <col style={{ width: "20%" }} />
        </colgroup>
        <thead>
          <tr>
            <th>TOPOLOGY</th>
            <th className="num">SPEND</th>
            <th className="num">SHARE</th>
            <th className="num">{props.fixture ? "EVENTS" : "SESS"}</th>
          </tr>
        </thead>
        <tbody>
          {rows.map(([name, sessions]) => (
            <tr key={name}>
              <td>{name}</td>
              <td className="num nul">{NULL_CELL}</td>
              <td className="num nul">{NULL_CELL}</td>
              <td className={sessions === NULL_CELL ? "num nul" : "num"}>{sessions}</td>
            </tr>
          ))}
          <tr className="total">
            <td>total</td>
            <td className="num nul">{NULL_CELL}</td>
            <td className="num nul">{NULL_CELL}</td>
            <td className="num">{props.fixture ? 3 : PACK.totals.sessions}</td>
          </tr>
        </tbody>
      </table>
    </section>
  );
}

function DetailTable(props: { providers: ProviderStat[]; fixture: boolean } & ProviderSelection) {
  return (
    <section className="cs-well cs-detail">
      <header>
        <b>PROVIDER SPEND DETAIL (CANONICAL PRICING)</b>
        <span>{props.fixture ? "exact-event aggregate · no input/output split" : "no priced rows · tokens null in spine"}</span>
      </header>
      <table className="cs-detail-table">
        <colgroup>
          <col style={{ width: "13%" }} />
          <col style={{ width: "11%" }} />
          <col style={{ width: "7.5%" }} />
          <col style={{ width: "9.5%" }} />
          <col style={{ width: "9.5%" }} />
          <col style={{ width: "9.5%" }} />
          <col style={{ width: "9.5%" }} />
          <col style={{ width: "9%" }} />
          <col style={{ width: "10.5%" }} />
          <col style={{ width: "11%" }} />
        </colgroup>
        <thead>
          <tr className="grp">
            <th colSpan={3} />
            <th colSpan={3}>TOKEN USAGE</th>
            <th colSpan={2}>TOKEN SAVINGS</th>
            <th colSpan={2} />
          </tr>
          <tr>
            <th>PROVIDER</th>
            <th className="num">SPEND (USD)</th>
            <th className="num">SHARE</th>
            <th className="num">INPUT</th>
            <th className="num">OUTPUT</th>
            <th className="num">TOTAL</th>
            <th className="num">SAVED</th>
            <th className="num">SAVED %</th>
            <th className="num">SESSIONS</th>
            <th className="num">MODELS</th>
          </tr>
        </thead>
        <tbody>
          {props.providers.map((p) => (
            <tr
              key={p.name}
              className={props.selected === p.name ? "is-selected" : props.selected ? "is-dim" : undefined}
              tabIndex={0}
              data-provider={p.name}
              aria-selected={props.selected === p.name}
              onClick={() => props.onSelect(props.selected === p.name ? null : p.name)}
              onKeyDown={(event) => handleProviderRowKeyDown(event, props.providers, p.name, props.selected, props.onSelect)}
            >
              <td>{p.name}</td>
              <td className={p.spend === null ? "num nul" : "num"}>{p.spend === null ? NULL_CELL : `$${p.spend.toFixed(2)}`}</td>
              <td className={p.spend === null ? "num nul" : "num"}>{p.spend === null ? NULL_CELL : `${Math.round((p.spend / 23.86) * 100)}%`}</td>
              <td className="num nul">{NULL_CELL}</td>
              <td className="num nul">{NULL_CELL}</td>
              <td className={p.tokens === null ? "num nul" : "num"}>{p.tokens === null ? NULL_CELL : compactTokens(p.tokens).replace(" tokens", "")}</td>
              <td className="num nul">{NULL_CELL}</td>
              <td className="num nul">{NULL_CELL}</td>
              <td className="num">{p.sessions}</td>
              <td className={p.models === null ? "num nul" : "num"}>{p.models ?? NULL_CELL}</td>
            </tr>
          ))}
          <tr className="total">
            <td>TOTAL</td>
            <td className={props.fixture ? "num" : "num nul"}>{props.fixture ? "$23.86" : NULL_CELL}</td>
            <td className={props.fixture ? "num" : "num nul"}>{props.fixture ? "100% priced" : NULL_CELL}</td>
            <td className="num nul">{NULL_CELL}</td>
            <td className="num nul">{NULL_CELL}</td>
            <td className={props.fixture ? "num" : "num nul"}>{props.fixture ? "5.58M" : NULL_CELL}</td>
            <td className="num nul">{NULL_CELL}</td>
            <td className="num nul">{NULL_CELL}</td>
            <td className="num">{props.fixture ? 3 : PACK.totals.sessions}</td>
            <td className={props.fixture ? "num" : "num nul"}>{props.fixture ? 2 : NULL_CELL}</td>
          </tr>
        </tbody>
      </table>
      <footer>
        {props.fixture ? "priced spend $23.86 · pricing coverage 65% of measured tokens · 1 unpriced event · 1 attribution gap" : `total spend — · pricing coverage 0% disclosed · tokens null 71 / 71 · ${PACK.totals.sessions} sessions · ${PACK.projects.length} projects`}
      </footer>
    </section>
  );
}

function Inspector(props: { providers: ProviderStat[]; flow: CostFlow | null; fixture: boolean; onOpenRun: () => void }) {
  return (
    <aside className="cs-inspect" aria-label="Provider pricing authority">
      <Corners />
      <div className="cs-inspect-body">
        <div className="cs-flow-inspect">
          <h2>SELECTED FLOW</h2>
          {props.flow ? (
            <>
              <div className="cs-row"><span>event</span><span className="r">{props.flow.id}</span></div>
              <div className="cs-row"><span>run</span><span className="r">{props.flow.run}</span></div>
              <div className="cs-row"><span>source</span><span className="r">{props.flow.source}</span></div>
              <div className="cs-row"><span>tokens</span><span className="r">{compactTokens(props.flow.tokens)}</span></div>
              <div className="cs-row"><span>priced spend</span><span className={`r${props.flow.spend === null ? " abs" : ""}`}>{props.flow.spend === null ? (props.flow.state === "unavailable" ? "unavailable" : "unpriced") : `$${props.flow.spend.toFixed(2)}`}</span></div>
              {props.fixture && <button type="button" className="cs-open" onClick={props.onOpenRun}>OPEN EXACT RUN →</button>}
            </>
          ) : <p className="cs-copy">Select a flow to inspect its exact event, run, and source.</p>}
        </div>
        <h2>PROVIDER PRICING AUTHORITY</h2>
        <div className="cs-block">
          <div className="k">
            PRICED <em className={`cnt${props.fixture ? "" : " abs"}`}>{props.fixture ? 1 : 0}</em>
          </div>
          <p className="cs-copy">{props.fixture ? "OpenAI rate window is pinned to fixture pricing revision 3." : "Canonical pricing pages were not copied. Not a $0 book."}</p>
        </div>
        <div className="cs-block">
          <div className="k">
            UNPRICED <em className="cnt">{props.fixture ? 1 : props.providers.length}</em>
          </div>
          <ul className="cs-prov">
            {(props.fixture ? [{ name: "Anthropic", hue: HUES.claude }] : props.providers).map((p) => (
              <li key={p.name}>
                <i style={{ background: p.hue, boxShadow: `0 0 5px ${p.hue}66` }} />
                {p.name}
              </li>
            ))}
          </ul>
        </div>
        <div className="cs-block">
          <div className="k">
            NULL <em className="cnt abs">tokens</em>
          </div>
          <div className="cs-row">
            <span>null tokens</span>
            <span className="r abs">{props.fixture ? "0 / 3" : "71 / 71"}</span>
          </div>
          <div className="cs-row">
            <span>denied / stale</span>
            <span className="r abs">not measured</span>
          </div>
        </div>
        <div className="cs-block">
          <div className="k">
            UNAVAILABLE <em className="cnt abs">2</em>
          </div>
          <div className="cs-row">
            <span>{props.fixture ? "attribution join" : "usage-event ledger"}</span>
            <span className="r abs">{props.fixture ? "1 gap" : "absent"}</span>
          </div>
          <div className="cs-row">
            <span>{props.fixture ? "forecast" : "token stream"}</span>
            <span className="r abs">absent</span>
          </div>
        </div>
        <div className="cs-block">
          <div className="k">AUTHORITY SOURCE</div>
          <p className="cs-copy">{props.fixture ? "fixture/canonical-pricing/openai-r3" : "Canonical provider pricing pages — not in snapshot."}</p>
        </div>
        <div className="cs-block">
          <div className="k">LAST REFRESH</div>
          <div className="cs-row">
            <span>pricing</span>
            <span className={`r${props.fixture ? "" : " abs"}`}>{props.fixture ? "2026-09-08 14:30 UTC" : "unavailable"}</span>
          </div>
          <div className="cs-row">
            <span>snapshot</span>
            <span className="r">{snapshotStamp(PACK.capturedAt)}</span>
          </div>
          <div className="cs-row">
            <span>daemon</span>
            <span className="r">{PROFILE.daemon}</span>
          </div>
        </div>
      </div>
      <div className="cs-hint">{props.fixture ? "Priced spend covers 65% of measured tokens. One event is unpriced; one priced event has an attribution gap." : "Session presence is not spend. Pricing coverage is unavailable in this snapshot."}</div>
    </aside>
  );
}

export function CostsPage(_props: { onInspect?: unknown; state?: string; onState?: (id: string) => void } = {}) {
  const { mode, navigate } = useDemo();
  const fixture = mode === "fixture";
  const providers = fixture ? fixtureProviderStats() : providerStats();
  const events = dailyMessageEvents();
  const agentSessions = PACK.sessions.filter((s) => s.isSubagent).length;
  const directSessions = PACK.totals.sessions - agentSessions;
  const [selected, setSelected] = useWorkspaceState<string | null>("costs:provider", (() => {
    const provider = new URLSearchParams(window.location.search).get("provider");
    return providers.some((item) => item.name === provider) ? provider : null;
  })());
  const flows = fixture ? FIXTURE_FLOWS : snapshotFlows();
  const [selectedFlowId, setSelectedFlowId] = useWorkspaceState<string | null>("costs:flow", new URLSearchParams(window.location.search).get("flow"));
  const [rangeOpen, setRangeOpen] = useWorkspaceState("costs:range-open", false);
  const [, setRouteRevision] = useState(0);
  const routeParams = new URLSearchParams(window.location.search);
  const routeProviderParam = routeParams.get("provider");
  const routeFlowParam = routeParams.get("flow");
  const routeProvider = providers.find((provider) => provider.name === routeProviderParam)?.name ?? null;
  const routeFlowCandidate = flows.find((flow) => flow.id === routeFlowParam) ?? null;
  const routeFlow = routeFlowCandidate && (!routeProviderParam || routeFlowCandidate.provider === routeProviderParam) ? routeFlowCandidate : null;
  const rememberedFlow = flows.find((flow) => flow.id === selectedFlowId) ?? null;
  const selectedFlow = routeFlowParam !== null ? routeFlow : routeProviderParam !== null ? null : rememberedFlow;
  const activeProvider = routeFlowParam !== null ? (routeFlow?.provider ?? routeProvider) : routeProviderParam !== null ? routeProvider : selected;

  useEffect(() => {
    if (routeFlowParam !== null) {
      setSelectedFlowId(routeFlow?.id ?? null);
      setSelected(routeFlow?.provider ?? routeProvider);
    } else if (routeProviderParam !== null) {
      setSelectedFlowId(null);
      setSelected(routeProvider);
    }
  }, [mode]);
  const replaceRoute = (url: URL) => {
    history.replaceState(null, "", url);
    setRouteRevision((revision) => revision + 1);
  };
  const selectProvider = (provider: string | null) => {
    setSelected(provider);
    setSelectedFlowId(null);
    const url = new URL(window.location.href);
    if (provider) url.searchParams.set("provider", provider);
    else url.searchParams.delete("provider");
    url.searchParams.delete("flow");
    replaceRoute(url);
  };
  const selectFlow = (flow: CostFlow) => {
    setSelected(flow.provider);
    setSelectedFlowId(flow.id);
    const url = new URL(window.location.href);
    url.searchParams.set("provider", flow.provider);
    url.searchParams.set("flow", flow.id);
    replaceRoute(url);
  };
  return (
    <div className="cs-root" onKeyDown={(event) => {
      if (event.key !== "Escape") return;
      if (rangeOpen) {
        setRangeOpen(false);
      } else if (routeFlowParam !== null || selectedFlow) {
        setSelectedFlowId(null);
        const url = new URL(window.location.href);
        url.searchParams.delete("flow");
        replaceRoute(url);
      } else selectProvider(null);
    }}>
      <div className="cs-main">
        <FlowCanvas flows={flows} selected={selectedFlow?.id ?? null} selectedProvider={activeProvider} onSelect={selectFlow} fixture={fixture} />
        <div className="cs-top">
          <SpendChart providers={providers} events={events} fixture={fixture} rangeOpen={rangeOpen} onRangeToggle={() => setRangeOpen((open) => !open)} selected={activeProvider} onSelect={selectProvider} />
          <div className="cs-kpis">
            <Kpi
              label="USAGE EVENTS"
              value={fixture ? "3" : PACK.totals.messages.toLocaleString("en-US")}
              unit={fixture ? "exact events" : "spine events"}
              note={fixture ? "local synthetic usage ledger" : `spark: ${events.timestamped.toLocaleString("en-US")} timestamped msgs / day`}
              spark={fixture ? undefined : events.days}
              sparkCap={fixture ? undefined : "msgs / day"}
              sparkAbsent={fixture ? "no time series" : undefined}
            />
            <Kpi
              label="TOKENS CONSUMED"
              value={fixture ? "5.58M" : "—"}
              unit="tokens"
              note={fixture ? "width source · 3 / 3 exact events" : "null in spine · 71 / 71"}
              absent={!fixture}
              sparkAbsent="no token stream"
            />
            <Kpi
              label="SAVED-TOKEN WINDOWS"
              value="—"
              unit="—% of total"
              note="not measured · not a 0%"
              absent
              sparkAbsent="not measured"
            />
          </div>
        </div>

        <div className="cs-mid">
          <CoverageTable providers={providers} fixture={fixture} selected={activeProvider} onSelect={selectProvider} />
          <PricingTable providers={providers} fixture={fixture} selected={activeProvider} onSelect={selectProvider} />
          <TopologyTable direct={fixture ? flows.filter((flow) => flow.topology === "direct").length : directSessions} agent={fixture ? flows.filter((flow) => flow.topology === "agent").length : agentSessions} fixture={fixture} />
        </div>

        <DetailTable providers={providers} fixture={fixture} selected={activeProvider} onSelect={selectProvider} />
      </div>
      <Inspector
        providers={providers}
        flow={selectedFlow}
        fixture={fixture}
        onOpenRun={() => selectedFlow && navigate("sessions", { session: selectedFlow.run, event: selectedFlow.id })}
      />
    </div>
  );
}
