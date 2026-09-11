import { useWorkspaceState } from "../app/workspace";
import { useEffect, useState } from "react";
import {
  CROSS_REPO_RAILS,
  GAP_TYPES,
  JOURNEY_LANES,
  PARTIAL_LANES,
  REPLAY_LANES,
  WORKSTREAMS,
  type JourneyLane,
} from "./data";
import { EpisodeDetails, EpisodeTable, DenseFanout, JourneyCanvas, JourneyMinimap, LaneLinks, LaneRow, PhaseRow, TimeRuler, hash32, mulberry, type Connector } from "./field";
import { JourneyWeave } from "./weave";
import { GradeMark } from "./marks";

function LaneNav(props: { title: string; extra?: string; partial?: boolean }) {
  const [query, setQuery] = useState("");
  const [unavailable, setUnavailable] = useState("");
  const prs = ["#707 feat: add ingest retry backoff", "#18337 feat: persistent caching v2", "#18315 fix: tree-shake side effects", "#5628 docs: improve build guide", "#3241 perf: reduce bundle size", "#8125 fix: chunk graph leak"];
  return (
    <aside className="dl-pane">
      <h3>
        SELECT PR / JOURNEY <span>{props.extra ?? "23 PRs"}</span>
      </h3>
      <div className="dl-scroll">
        <input className="dl-search" placeholder="Search PRs…" value={query} onChange={(event) => setQuery(event.target.value)} />
        {props.partial ? (
          <div className="dl-fbox">
            <div className="fr">
              <span className="fl mono">#709: feat: honest partial journ…</span>
            </div>
            <div className="fr">
              <span className="fl amber">partial · 53% covered</span>
            </div>
          </div>
        ) : (
          <>
            {prs.filter((pr) => pr.toLowerCase().includes(query.toLowerCase())).map((pr, i) => <button key={pr} className={`dl-navitem dl-textbutton${pr.startsWith("#707 ") ? " is-on" : ""}`} onClick={() => pr.startsWith("#707 ") ? window.location.assign("?data=fixture&surface=delivery&state=04&pr=707") : setUnavailable(pr)}><b>{pr}</b><em>{["12h", "2h", "4h", "1d", "1d", "2d"][i]}</em></button>)}
            {unavailable ? <p role="status" className="dl-local-notice">{unavailable}: full journey source is unavailable in this authored snapshot.</p> : null}
          </>
        )}
        <div className="dl-fbox" style={{ marginTop: 8 }}>
          <div className="fk">JOURNEY OVERVIEW</div>
          {(props.partial
            ? [
                ["Elapsed", "10h 41m"],
                ["Started", "2025-05-09 11:18:42"],
                ["Last update", "2025-05-09 22:01:17"],
                ["Coverage (time)", "53%"],
                ["Exact episodes", "18"],
                ["Inferred segments", "6"],
                ["Gap intervals", "7"],
                ["Attribution", "Mixed source grades"],
                ["Commits", "6"],
                ["Code files", "32"],
                ["Tests passed", "31"],
                ["Agents involved", "5"],
                ["Evidence items", "76"],
              ]
            : [
                ["Elapsed", "10h 42m"],
                ["Started", "2025-05-09 11:18:42"],
                ["Merged", "2025-05-09 21:59:31"],
                ["Commits", "6"],
                ["Code files", "142"],
                ["Tests", "38"],
                ["Agents", "7"],
                ["Evidence items", "184"],
              ]
          ).map(([k, v]) => (
            <div className="fr" key={k}>
              <span className="fl">{k}</span>
              <b>{v}</b>
            </div>
          ))}
        </div>
        <div className="dl-fbox">
          <div className="fk">BRANCHES (COLLAPSED)</div>
          {[
            ["Module Federation", "+3"],
            ["Rspack", "+2"],
            ["Rsbuild / Rslib", "+2"],
            ["Unrelated Activity", "+12"],
          ].map(([k, v]) => (
            <div className="fr" key={k}>
              <span className="fl amber">{k}</span>
              <b>{v}</b>
            </div>
          ))}
        </div>
        {props.partial ? (
          <div className="dl-fbox">
            <div className="fr">
              <span className="fl" style={{ color: "var(--state-danger)" }}>
                UNRESOLVED GAPS
              </span>
              <b>7</b>
            </div>
          </div>
        ) : null}
      </div>
    </aside>
  );
}

const LEGEND_ITEMS = [
  { icon: "〜", title: "Main delivery spine", sub: "Milestone path" },
  { icon: "◎", title: "Agent lane", sub: "Work by agent" },
  { icon: "∴", title: "Subagent work", sub: "Tasks & activities" },
  { icon: "⇢", title: "Handoff / merge", sub: "Context handoff" },
  { icon: "〰", title: "Cross-repo rail", sub: "External PR flow" },
];

const COVERAGE_ITEMS = [
  {icon:"◑", title:"Authored episodes", lines:[String([...JOURNEY_LANES,...CROSS_REPO_RAILS].flatMap((lane)=>lane.events).filter((event)=>event.kind!=="ghost").length),"Named records in the example"]},
  {icon:"◎", title:"Authored lanes", lines:[String(JOURNEY_LANES.length),"Separate source labels"]},
  {icon:"⇄", title:"Cross-repository rails", lines:[String(CROSS_REPO_RAILS.length),"No inferred release join"]},
  {icon:"◇", title:"Source coverage", lines:["Episode metadata only","Exact source bodies unavailable"]},
];

export function JourneyOverview() {
  return (
    <div className="dl-stage is-j">
      <LaneNav title="overview" extra="All PRs ⌄" />
      <section className="dl-pane">
        <h3>
          08 DELIVERY — PR JOURNEY OVERVIEW <span>Semantic density · Level 3</span>
        </h3>
        <TimeRuler />
        <JourneyWeave lanes={JOURNEY_LANES.filter((l) => l.id !== "human")} rails={CROSS_REPO_RAILS} />
        <EpisodeTable lanes={[...JOURNEY_LANES, ...CROSS_REPO_RAILS]} />
        <a className="dl-btn" href="?data=fixture&surface=delivery&state=09&example=compiler-cache-8127&fromExample=journey-707">Explore PR8127 review example →</a>
        <JourneyMinimap
          lanes={[...JOURNEY_LANES, ...CROSS_REPO_RAILS]}
          window={[0.24, 0.78]}
          hours={["08:00", "12:00", "16:00", "20:11"]}
        />
      </section>
      <aside className="dl-pane">
        <h3>
          JOURNEY LEGEND <span />
        </h3>
        <div className="dl-scroll">
          <div className="dl-fbox">
            {LEGEND_ITEMS.map((l) => (
              <div className="dl-legenditem" key={l.title}>
                <i>{l.icon}</i>
                <div>
                  <b>{l.title}</b>
                  <em>{l.sub}</em>
                </div>
              </div>
            ))}
          </div>
          <div className="dl-fbox">
            <div className="fk">JOURNEY COVERAGE</div>
            {COVERAGE_ITEMS.map((c) => (
              <div className="dl-legenditem" key={c.title}>
                <i>{c.icon}</i>
                <div>
                  <b>{c.title}</b>
                  {c.lines.map((x) => (
                    <em key={x}>{x}</em>
                  ))}
                </div>
              </div>
            ))}
          </div>
          <div className="dl-fbox">
            <div className="fk">DATA SCOPE</div>
            <div className="dl-legenditem">
              <i>🗄</i>
              <div>
                <b>Evidence</b>
                <em>Authored episode metadata</em>
              </div>
            </div>
            <div className="dl-legenditem">
              <i>⟳</i>
              <div>
                <b>History</b>
                <em>Loaded fixture only</em>
              </div>
            </div>
          </div>
        </div>
      </aside>
    </div>
  );
}

const REPLAY_HOURS = ["08:00:00", "09:11:46", "10:23:32", "11:35:18", "12:47:05"];

/** Spawn cascade — the vertical trunk linking lane starts (recorded handoffs only). */
const REPLAY_CONNECTORS: Connector[] = [
  { from: "r-human|spawn", to: "r-lynx|plan", color: "#60a5fa" },
  { from: "r-lynx|assign", to: "r-rsbuild|codegen", color: "#c084fc", sweep: -6 },
  { from: "r-rsbuild|build", to: "r-mf|validate", color: "#9be15d", sweep: 8 },
  { from: "r-mf|simulate", to: "r-td|analyze", color: "#5ee7ff", sweep: -6 },
  { from: "r-rsbuild|test", to: "r-gha|ci: start", color: "#f0b429", sweep: 8 },
];

const replaySeconds = (value: string) => value.split(":").map(Number).reduce((total, part) => total * 60 + part, 0) * (value.split(":").length === 2 ? 60 : 1);
const replayStart = replaySeconds("08:00");
const replaySource = REPLAY_LANES.map((lane) => ({ ...lane, burst: 0, events: lane.events.filter((ev) => ev.kind !== "ghost" && ev.time).map((ev) => ({ ...ev, time: ev.label === "ci: test fail" ? "12:47:05" : ev.time! })) }));
const replayTail = Math.max(...replaySource.flatMap((lane) => lane.events.map((ev) => replaySeconds(ev.time))));
const replayDuration = replayTail - replayStart;
const replayLanes = replaySource.map((lane) => ({ ...lane, events: lane.events.map((ev) => ({ ...ev, t: (replaySeconds(ev.time) - replayStart) / replayDuration })) }));
const replayClock = (seconds: number) => new Date(seconds * 1000).toISOString().slice(11, 19);

export function TemporalReplay() {
  const [cursor, setCursor] = useWorkspaceState("delivery.fixture.replay.cursor", 1);
  const [playing, setPlaying] = useState(false);
  const [speed, setSpeed] = useWorkspaceState("delivery.fixture.replay.speed", 1);
  const [expanded, setExpanded] = useWorkspaceState("delivery.fixture.replay.expanded", true);
  const events = replayLanes.flatMap((lane) => lane.events.map((ev) => ({ ...ev, lane: lane.label }))).sort((a, b) => a.t - b.t);
  const selected = events.filter((ev) => ev.t <= cursor).at(-1);
  const admitted = replayLanes.map((lane) => ({ ...lane, events: lane.events.filter((ev) => ev.t <= cursor) }));
  const time = replayClock(replayStart + Math.floor(cursor * replayDuration));
  useEffect(() => {
    if (!playing) return;
    const timer = window.setInterval(() => setCursor((value) => Math.min(1, value + speed / replayDuration)), 1000);
    return () => window.clearInterval(timer);
  }, [playing, speed]);
  useEffect(() => { if (cursor >= 1) setPlaying(false); }, [cursor]);
  const step = (direction: number) => {
    setPlaying(false);
    setCursor(direction > 0 ? events.find((ev) => ev.t > cursor + 0.0001)?.t ?? 1 : events.filter((ev) => ev.t < cursor - 0.0001).at(-1)?.t ?? 0);
  };
  return (
    <div className="dl-stage is-r">
      <section className="dl-pane">
        <h3>
          08 DELIVERY — TEMPORAL REPLAY{" "}
          <span>REPLAY MODE · PRESENTATION OVER RECORDED / LOADED EVIDENCE — CANNOT INVENT MISSING HISTORY</span>
        </h3>
        <div className="dl-replayhead">
          <b>PR #18337 — Add bundle size guard to CI</b>
          <span className="chip">state: review</span>
          <span className="chip">replay: {playing ? "playing loaded page" : "paused"}</span>
          <span className="grow" />
          <span className="prog mono">
            progress <b>{Math.round(cursor * 100)}%</b>
          </span>
          <span className="dl-bar" style={{ width: 120, margin: 0 }}>
            <i style={{ width: `${cursor * 100}%` }} />
          </span>
          <span className="mono dim">{time} / {replayClock(replayTail)}</span>
        </div>
        <div className="dl-transport">
          <button className="chip" onClick={() => step(-1)}>‹ Previous event</button>
          <button className="chip on" onClick={() => setPlaying(!playing)}>{playing ? "Ⅱ Pause" : "▶ Play"}</button>
          <button className="chip" onClick={() => step(1)}>Next event ›</button>
          <input type="range" aria-label="Replay position" min="0" max="1000" value={Math.round(cursor * 1000)} onChange={(event) => { setPlaying(false); setCursor(Number(event.target.value) / 1000); }} />
          <span className="mono">{time} / {replayClock(replayTail)}</span>
          {[0.5, 1, 2, 4].map((value) => <button key={value} className={value === speed ? "chip on" : "chip"} aria-pressed={value === speed} onClick={() => setSpeed(value)}>{value}x</button>)}
          <button className="chip" onClick={() => { setCursor(1); setPlaying(false); }}>Follow loaded tail</button>
          <button className="chip" onClick={() => { setCursor(1); setPlaying(false); }}>Return to loaded tail</button>
        </div>
        <div className="dl-transport sub">
          <button className="chip" onClick={() => setExpanded(false)}>⧉ Collapse labels</button>
          <button className="chip" onClick={() => setExpanded(true)}>‹ Expand labels</button>
          <span className="mono dim">Loaded example only · future episodes remain unrevealed</span>
        </div>
        <TimeRuler hours={REPLAY_HOURS} chips />
        <JourneyCanvas
          lanes={admitted}
          phases={[]}
          hideRuler
          showLabels={expanded}
          tall
          cursor={cursor}
          cursorTime={time}
          cursorPct={`${Math.round(cursor * 100)}%`}
          maskFuture
          selected={selected?.label}
          link
          connectors={REPLAY_CONNECTORS}
        />
        <JourneyMinimap
          title="MINIMAP — ADMITTED SOURCE EVENTS"
          admittedThrough={cursor}
          lanes={admitted}
          window={[Math.max(0, cursor - 0.08), Math.min(1, cursor + 0.08)]}
          hours={REPLAY_HOURS}
          legend={admitted.filter((lane) => lane.events.length).map((lane) => ({ label: lane.label, color: lane.color }))}
        />
        <EpisodeTable lanes={replayLanes.map((lane) => ({ ...lane, events: lane.events.filter((ev) => ev.t <= cursor && ev.kind !== "ghost") }))} />
      </section>
      <aside className="dl-pane">
        <h3>
          SELECTED EVENT <span>{admitted.flatMap((lane) => lane.events).length} admitted</span>
        </h3>
        <div className="dl-scroll">
          <div className="dl-umbtitle">
            <i className="dot" style={{ background: "var(--activity-amber)", boxShadow: "0 0 8px var(--activity-amber)" }} />
            <b style={{ color: "var(--activity-amber)" }}>{selected?.label ?? "No episode admitted yet"}</b>
          </div>
          {selected?.label !== "ci: test fail" ? <div className="dl-local-notice">{selected ? `${selected.lane} · ${selected.grade} · ${selected.time ?? time}. Source detail is not attached to this loaded example.` : "Scrub forward to admit the first loaded episode."}</div> : <>
          <div className="dl-stat">
            <span>at</span>
            <b>12:47:05 UTC · 2025-05-09</b>
          </div>
          <div className="dl-fbox">
            <div className="fk">OUTCOME</div>
            <span className="dl-att tone-danger">failed</span>
          </div>
          <div className="dl-fbox">
            <div className="fk">TEST / CHECK</div>
            <div className="fr">
              <span className="fl mono">vitest</span>
            </div>
            <div className="fr">
              <span className="fl mono">apps/web / src/utils/size.test.ts</span>
            </div>
            <div className="fr">
              <span className="fl mono">expect(bundleSize).toBeLessThan(250000)</span>
            </div>
            <div className="fr">
              <span className="fl mono">expected &lt; 250,000</span>
              <b style={{ color: "var(--state-danger)" }}>received 287,341</b>
            </div>
          </div>
          <div className="dl-fbox">
            <div className="fk">CHANGED CODE (EXCERPT)</div>
            <div className="fr">
              <span className="fl mono">src/utils/bundleSize.ts</span>
              <GradeMark grade="EXACT" />
            </div>
            <div className="dl-minidiff mono">
              <div className="ln">124  export function calculate() {"{"}</div>
              <div className="ln del">125  −  return size;</div>
              <div className="ln add">125  +  return size + overhead;</div>
              <div className="ln">126  {"}"}</div>
            </div>
          </div>
          <div className="dl-fbox">
            <div className="fk">TASK</div>
            <div className="fr">
              <span className="fl mono">ci: run tests</span>
            </div>
            <div className="fk" style={{ marginTop: 6 }}>
              AGENT
            </div>
            <div className="fr">
              <span className="fl mono">github-actions / test-runner</span>
            </div>
            <div className="fk" style={{ marginTop: 6 }}>
              WORKTREE / COMMIT
            </div>
            <div className="fr">
              <span className="fl mono">wt: pr-18337-8f3a</span>
            </div>
            <div className="fr">
              <span className="fl mono">c: a1b2c3d (push) ⧉</span>
            </div>
          </div>
          <div className="dl-fbox">
            <div className="fk">REVIEW CONSEQUENCE</div>
            <div className="fr">
              <span className="fl" style={{ color: "var(--state-danger)" }}>
                Changes requested
              </span>
            </div>
            <div className="fr">
              <span className="fl">Blocking merge until tests pass</span>
            </div>
          </div>
          </>}
          <p className="dl-hint">This is a replay of recorded / loaded evidence. It cannot invent missing history.</p>
        </div>
      </aside>
    </div>
  );
}

/* ---- State 06: expanded agent branches ---- */

const BRANCH_PHASES = [
  { t: 0.03, label: "Human Objective", time: "11:18" },
  { t: 0.1, label: "Planning", time: "11:27" },
  { t: 0.19, label: "Investigation", time: "12:01" },
  { t: 0.28, label: "Decision", time: "13:11" },
  { t: 0.47, label: "Implementation", time: "14:02 → 19:43" },
  { t: 0.7, label: "Verification", time: "19:44" },
  { t: 0.82, label: "Review / Revision", time: "20:03" },
  { t: 0.92, label: "CI", time: "21:15" },
  { t: 0.985, label: "Delivery", time: "21:59" },
];

const BRANCH_HOURS = ["11:00", "12:00", "13:00", "14:00", "15:00", "16:00", "17:00", "18:00", "19:00", "20:00", "21:00", "22:00"];

const B_HUMAN: JourneyLane = {
  id: "b-human",
  label: "Human",
  role: "author",
  color: "#5ee7ff",
  quiet: true,
  events: [
    { t: 0.027, label: "task", time: "11:18", kind: "task", grade: "EXACT" },
    { t: 0.06, label: "session", time: "11:28", kind: "task", grade: "EXACT" },
    { t: 0.277, label: "worktree", time: "feat/retry-backoff · 14:03", kind: "commit", grade: "EXACT" },
    { t: 0.38, label: "commit", time: "a1b2c3d · 15:12", kind: "commit", grade: "EXACT" },
    { t: 0.456, label: "code", time: "42 files · 16:02", kind: "commit", grade: "EXACT" },
    { t: 0.53, label: "handoff", time: "→ Module Fed. · 16:08", kind: "handoff", grade: "EXACT" },
  ],
};

const B_GEMINI: JourneyLane = {
  id: "b-gemini",
  label: "Gemini-CLI",
  role: "@investigator",
  color: "#f0b429",
  quiet: true,
  events: [
    { t: 0.094, label: "spawn", time: "12:02", kind: "spawn", grade: "EXACT" },
    { t: 0.145, label: "search codebase", time: "12:15", kind: "task", grade: "EXACT" },
    { t: 0.2, label: "analysis", time: "12:34", kind: "task", grade: "EXPLICIT" },
  ],
};

const B_CODEGPT: JourneyLane = {
  id: "b-codegpt",
  label: "CodeGPT",
  role: "@architect",
  color: "#9be15d",
  quiet: true,
  events: [
    { t: 0.198, label: "spawn", time: "13:11", kind: "spawn", grade: "EXACT" },
    { t: 0.235, label: "design retry policy", time: "13:22", kind: "task", grade: "EXPLICIT" },
    { t: 0.29, label: "spec", time: "13:47", kind: "task", grade: "EXPLICIT" },
  ],
};

const B_SUBS: JourneyLane[] = [
  {
    id: "b-core",
    label: "CodeGPT:core",
    role: "@design-core",
    color: "#9be15d",
    quiet: true,
    events: [
      { t: 0.3, label: "spawn", time: "14:03", kind: "spawn", grade: "EXACT" },
      { t: 0.36, label: "implement core logic", time: "14:10", kind: "task", grade: "EXACT" },
      { t: 0.45, label: "edit retry.ts", time: "14:22", kind: "commit", grade: "EXACT" },
      { t: 0.54, label: "edit backoff.ts", time: "14:41", kind: "commit", grade: "EXACT" },
      { t: 0.64, label: "commit d4e5f6a", time: "14:56", kind: "commit", grade: "EXACT" },
      { t: 0.74, label: "tests · 8 passed", time: "15:05", kind: "test", grade: "EXACT" },
      { t: 0.84, label: "handoff → tests", time: "15:06", kind: "handoff", grade: "EXACT" },
    ],
  },
  {
    id: "b-policy",
    label: "CodeGPT:policy",
    role: "@design-policy",
    color: "#67e8f9",
    quiet: true,
    events: [
      { t: 0.31, label: "spawn", time: "14:04", kind: "spawn", grade: "EXACT" },
      { t: 0.38, label: "implement policy cfg", time: "14:11", kind: "task", grade: "EXACT" },
      { t: 0.47, label: "edit policy.ts", time: "14:25", kind: "commit", grade: "EXACT" },
      { t: 0.57, label: "edit config.ts", time: "15:38", kind: "commit", grade: "EXACT" },
      { t: 0.67, label: "commit 41b2c8f", time: "15:56", kind: "commit", grade: "EXACT" },
      { t: 0.76, label: "tests · 8 passed", time: "15:00", kind: "test", grade: "EXACT" },
      { t: 0.85, label: "handoff → tests", time: "15:01", kind: "handoff", grade: "EXACT" },
    ],
  },
  {
    id: "b-utils",
    label: "CodeGPT:utils",
    role: "@design-utils",
    color: "#c084fc",
    quiet: true,
    events: [
      { t: 0.32, label: "spawn", time: "14:05", kind: "spawn", grade: "EXACT" },
      { t: 0.4, label: "implement helpers", time: "14:12", kind: "task", grade: "EXACT" },
      { t: 0.5, label: "edit jitter.ts", time: "14:27", kind: "commit", grade: "EXACT" },
      { t: 0.6, label: "commit 9f8e7d6", time: "14:45", kind: "commit", grade: "EXACT" },
      { t: 0.72, label: "tests · 7 passed", time: "14:55", kind: "test", grade: "EXACT" },
      { t: 0.83, label: "handoff → tests", time: "14:56", kind: "handoff", grade: "EXACT" },
    ],
  },
];

const B_REVIEWER: JourneyLane = {
  id: "b-reviewer",
  label: "Human",
  role: "@reviewer",
  color: "#c084fc",
  quiet: true,
  events: [
    { t: 0.823, label: "review", time: "20:03", kind: "review", grade: "EXACT" },
    { t: 0.87, label: "comment", time: "20:18", kind: "review", grade: "EXACT" },
    { t: 0.92, label: "approval", time: "20:52", kind: "review", grade: "EXACT" },
  ],
};

const B_RAILS: JourneyLane[] = [
  {
    id: "b-rspack",
    label: "Rspack",
    role: "#18337",
    color: "#38cfe8",
    quiet: true,
    events: [
      { t: 0.21, label: "branch", time: "13:18", kind: "commit", grade: "EXACT" },
      { t: 0.3, label: "commit d4w5f6a", time: "14:20", kind: "commit", grade: "EXACT" },
      { t: 0.4, label: "tests · 12 passed", time: "14:05", kind: "test", grade: "EXACT" },
      { t: 0.5, label: "ci", time: "15:42", kind: "ci", grade: "EXACT" },
      { t: 0.6, label: "PR open #18337", time: "16:48", kind: "commit", grade: "EXACT" },
      { t: 0.7, label: "review", time: "16:48", kind: "review", grade: "EXACT" },
      { t: 0.8, label: "merged", time: "17:21", kind: "commit", grade: "EXACT" },
    ],
  },
  {
    id: "b-rslib",
    label: "Rsbuild / Rslib",
    role: "#8187",
    color: "#f0b429",
    quiet: true,
    events: [
      { t: 0.22, label: "branch", time: "13:25", kind: "commit", grade: "EXACT" },
      { t: 0.32, label: "commit e7f8a9b", time: "14:35", kind: "commit", grade: "EXACT" },
      { t: 0.42, label: "tests · 8 passed", time: "14:35", kind: "test", grade: "EXACT" },
      { t: 0.52, label: "ci", time: "15:58", kind: "ci", grade: "EXACT" },
      { t: 0.62, label: "PR open #8187", time: "16:55", kind: "commit", grade: "EXACT" },
      { t: 0.72, label: "review", time: "16:55", kind: "review", grade: "EXACT" },
      { t: 0.82, label: "merged", time: "17:33", kind: "commit", grade: "EXACT" },
    ],
  },
];

const BRANCH_CONNECTORS: Connector[] = [
  { from: "b-human|session", to: "b-gemini|spawn", color: "#f0b429", sweep: -8 },
  { from: "b-gemini|analysis", to: "b-codegpt|spawn", color: "#9be15d", sweep: -6 },
  { from: "b-codegpt|spec", to: "b-core|spawn", color: "#9be15d", sweep: 6 },
  { from: "b-codegpt|spec", to: "b-policy|spawn", color: "#67e8f9", sweep: -10 },
  { from: "b-codegpt|spec", to: "b-utils|spawn", color: "#c084fc", sweep: -18 },
  { from: "b-core|handoff → tests", to: "results|node", color: "#9be15d", sweep: 10 },
  { from: "b-policy|handoff → tests", to: "results|node", color: "#67e8f9", sweep: 8 },
  { from: "b-utils|handoff → tests", to: "results|node", color: "#c084fc", sweep: 6 },
  { from: "results|node", to: "phase|Delivery", color: "#9be15d", sweep: 42 },
  { from: "b-reviewer|approval", to: "phase|Delivery", color: "#c084fc", sweep: 26 },
  { from: "b-rspack|merged", to: "phase|Delivery", color: "#38cfe8", sweep: 34, dash: true },
  { from: "b-rslib|merged", to: "phase|Delivery", color: "#f0b429", sweep: 44, dash: true },
];

const BRANCH_ALL_LANES = [B_HUMAN, B_GEMINI, B_CODEGPT, ...B_SUBS, B_REVIEWER, ...B_RAILS];

const BRANCH_TOOL_ICONS = [
  "spawn",
  "task",
  "session",
  "search",
  "analysis",
  "design",
  "spec",
  "worktree",
  "commit",
  "code",
  "edit",
  "ci",
  "symbol",
  "test",
  "review",
  "handoff",
  "results",
  "branch",
  "pr",
  "merge",
];

export function AgentBranches() {
  const [expanded, setExpanded] = useWorkspaceState("delivery.fixture.branches.expanded", true);
  const [filter, setFilter] = useWorkspaceState("delivery.fixture.branches.filter", "");
  const [selection,setSelection] = useWorkspaceState<{lane:string;label:string;t:number}|null>("delivery.fixture.branches.selection",null);
  const selectedLane = BRANCH_ALL_LANES.find((lane)=>lane.id===selection?.lane);
  const selectedEvent = selectedLane?.events.find((event)=>event.label===selection?.label && event.t===selection.t);
  const inspect = (lane:JourneyLane,event:JourneyLane["events"][number])=>setSelection({lane:lane.id,label:event.label,t:event.t});
  return (
    <div className="dl-stage is-j">
      <LaneNav title="branches" extra="#707" />
      <section className="dl-pane">
        <h3>
          DELIVERY / PR #707 CAUSAL JOURNEY <span>PRIMARY TIMELINE ⌃</span>
        </h3>
        <TimeRuler hours={BRANCH_HOURS} chips />
        <div className="dl-jbody">
          <div className="grid" aria-hidden="true">
            {BRANCH_PHASES.map((p) => (
              <i key={p.label} style={{ left: `${p.t * 100}%` }} />
            ))}
          </div>
          <LaneLinks lanes={BRANCH_ALL_LANES} connectors={BRANCH_CONNECTORS} />
          <div className="inner">
            <PhaseRow phases={BRANCH_PHASES} />
            <div className="lanes">
              <LaneRow onInspect={inspect} query={filter} lane={B_HUMAN} showLabels tall noSpine rings />
              <LaneRow onInspect={inspect} query={filter} lane={B_GEMINI} showLabels tall noSpine rings />
              <LaneRow onInspect={inspect} query={filter} lane={B_CODEGPT} showLabels tall noSpine rings />
              <div className="dl-branchgroup" hidden={!expanded}>
                {B_SUBS.map((lane) => (
                  <LaneRow onInspect={inspect} query={filter} key={lane.id} lane={lane} showLabels tall noSpine rings />
                ))}
                <div className="collapsed mono">CodeGPT:math @helper-jitter · spawn 14:29 · impl jitter fn 14:33 · edit math.ts 14:37 · commit ab12cd3 14:40 · return 14:41</div>
                <span className="results" data-evkey="results|node">
                  <i>⛁</i> results (3) · 15:06
                </span>
              </div>
              <LaneRow onInspect={inspect} query={filter} lane={B_REVIEWER} showLabels tall noSpine rings />
            </div>
            <div className="dl-lanesec">
              CROSS-REPO PR RAILS <span className="chip">Collapse all 2</span>
            </div>
            <div className="lanes rails">
              {B_RAILS.map((lane) => (
                <LaneRow onInspect={inspect} query={filter} key={lane.id} lane={lane} showLabels tall noSpine rings />
              ))}
            </div>
            <div className="dl-jtoolbar">
              <button className="chip" onClick={() => setExpanded(false)}>⧉ Collapse branch</button>
              <button className="chip" onClick={() => setExpanded(true)}>⌄ Expand all</button>
              <input className="dl-search" aria-label="Filter branch agents tasks or files" placeholder="Agent, task or file…" value={filter} onChange={(event) => setFilter(event.target.value)} />
              <button className="chip" onClick={() => { setExpanded(true); setFilter(""); }}>Show complete branch</button>
            </div>
            <div className="dl-jicons mono">
              {BRANCH_TOOL_ICONS.map((x) => (
                <span key={x}>
                  <i>◦</i>
                  {x}
                </span>
              ))}
            </div>
            <div className="dl-jfoot mono">Select a node for loaded episode metadata. Expand a branch or use the time ruler to zoom.</div>
          </div>
        </div>
        <EpisodeTable lanes={BRANCH_ALL_LANES} />
      </section>
      <aside className="dl-pane" aria-label="Selected represented branch"><h3>SELECTED BRANCH / EVENT</h3><div className="dl-scroll dl-branch-inspector">{selectedLane && selectedEvent ? <><h2>{selectedLane.label}</h2><p>{selectedLane.role}</p><EpisodeDetails lane={selectedLane.id} ev={selectedEvent}/><h3>REPRESENTED BRANCH EVENTS</h3>{selectedLane.events.filter((event)=>event.kind!=="ghost").map((event,index)=><button className="dl-btn" key={index} aria-pressed={event===selectedEvent} onClick={()=>inspect(selectedLane,event)}>{event.label} · {event.time??"time not authored"}</button>)}<p>Task, session, worktree and source bodies are not attached to this event record. No other branch’s pictured artifacts are substituted.</p></> : <p>Select a named episode to inspect its represented branch and event. Task, worktree and exact source fields remain unavailable until selected metadata supplies them.</p>}</div></aside>
    </div>
  );
}

/* ---- State 07: honest partial / unknown ---- */

const PARTIAL_PHASES = [
  { t: 0.03, label: "Human Objective", time: "11:18" },
  { t: 0.09, label: "Planning", time: "11:27" },
  { t: 0.17, label: "Investigation", time: "12:02" },
  { t: 0.26, label: "Decision", time: "13:11" },
  { t: 0.42, label: "Implementation", time: "14:02" },
  { t: 0.6, label: "Verification", time: "18:24" },
  { t: 0.8, label: "Review / Revision", time: "20:03" },
  { t: 0.9, label: "CI", time: "21:15" },
  { t: 0.98, label: "Delivery", time: "21:59" },
];

const PARTIAL_HOURS = ["11:00", "12:00", "13:00", "14:00", "15:00", "16:00", "17:00", "18:00", "19:00", "20:00", "21:00", "22:00"];

/** Recorded handoffs and returns only — gaps stay absent, nothing is bridged. */
const PARTIAL_CONNECTORS: Connector[] = [
  { from: "human-p|session", to: "gemini-p|spawn", color: "#f0b429", sweep: -6 },
  { from: "gemini-p|search codebase", to: "codegpt-p|spawn", color: "#9be15d", sweep: -8 },
  { from: "reviewer-p|approval", to: "human-p|delivery", color: "#c084fc", sweep: 20 },
  { from: "ci-p|merged", to: "human-p|delivery", color: "#5ee7ff", dash: true, sweep: 46 },
];

export function HonestPartial() {
  const [acknowledged, setAcknowledged] = useWorkspaceState("delivery.fixture.partial.acknowledged", false);
  const [notice, setNotice] = useWorkspaceState("delivery.fixture.partial.notice", "");
  const [gapsOnly, setGapsOnly] = useWorkspaceState("delivery.fixture.partial.gapsOnly", false);
  const [annotation, setAnnotation] = useWorkspaceState("delivery.fixture.partial.annotation", "");
  return (
    <div className="dl-stage is-j">
      <LaneNav title="partial" extra="#709 · 7 gaps" partial />
      <section className="dl-pane">
        <h3>
          DELIVERY / PR #709 HONEST PARTIAL JOURNEY <span>PRIMARY TIMELINE · gaps retained — nothing invented</span>
        </h3>
        <TimeRuler hours={PARTIAL_HOURS} chips />
        <JourneyCanvas lanes={gapsOnly ? PARTIAL_LANES.map((lane) => ({ ...lane, events: lane.events.filter((ev) => ev.kind === "gap"), burst: 0 })) : PARTIAL_LANES} phases={PARTIAL_PHASES} hideRuler showLabels tall link rings connectors={PARTIAL_CONNECTORS}
          legend={
            <div className="dl-jlegendrow mono">
              <span>
                <i className="solid" /> exact episode
              </span>
              <span>
                <i className="dashed" /> inferred / partially observed
              </span>
              <span>
                <i className="hatch" /> gap / unavailable
              </span>
              <span className="dim">click any node to inspect evidence and provenance</span>
            </div>
          }
        />
        <EpisodeTable lanes={PARTIAL_LANES} />
      </section>
      <aside className="dl-pane">
        <h3>
          GAP INSPECTOR <span>7 of 7 gaps ‹ ›</span>
        </h3>
        <div className="dl-scroll">
          <div className="dl-umbtitle">
            <b style={{ color: "var(--state-violet)" }}>private reasoning unavailable</b>
          </div>
          <div className="dl-chiprow">
            <span className="dl-honest tone-violet">GAP</span>
            <span className="dl-honest tone-amber">{acknowledged ? "ACKNOWLEDGED LOCALLY" : "UNREVIEWED"}</span>
            <span className="mono" style={{ fontSize: 9, color: "var(--ink-muted)" }}>
              11:28 – 12:02
            </span>
          </div>
          <div className="dl-fbox">
            <div className="fk">WHY MISSING</div>
            <p className="dl-microbody">Provider does not retain private model reasoning.</p>
          </div>
          <div className="dl-fbox">
            <div className="fk">LAST OBSERVED</div>
            <div className="fr">
              <span className="fl mono">11:28:14 UTC</span>
            </div>
          </div>
          <div className="dl-fbox">
            <div className="fk">CORRELATION CONFIDENCE</div>
            <div className="fr">
              <span className="fl">INFERRED · basis required</span>
            </div>
            <div className="dl-bar">
              <i style={{ width: "20%", background: "var(--state-violet)" }} />
            </div>
          </div>
          <div className="dl-fbox">
            <div className="fk">AFFECTED REVIEW COVERAGE</div>
            <div className="fr">
              <span className="fl">−1 review lane · 34m interval</span>
            </div>
          </div>
          <div className="dl-fbox">
            <div className="fk">AVAILABLE RECOVERY</div>
            {["Request transcript (if retained)", "Ask provider (limited window)", "Mark acknowledged", "Add external evidence"].map((x) => (
              <button type="button" className="dl-btn ghost block" key={x} onClick={() => { if (x === "Mark acknowledged") { setAcknowledged(true); setNotice("Acknowledged in this browser session; source evidence remains missing."); } else if (x === "Add external evidence") setNotice("Add a local annotation below. It does not become source evidence."); else setNotice("Provider retrieval is unavailable in this static snapshot. No request was sent; the gap remains unavailable."); }} style={{ minHeight: 26, padding: "4px 8px" }}>
                {x}
              </button>
            ))}
          </div>
          {notice ? <p role="status" className="dl-local-notice">{notice}</p> : null}
          {notice.startsWith("Add") ? <label className="dl-local-notice">Local annotation<textarea aria-label="Local gap annotation" value={annotation} onChange={(event) => setAnnotation(event.target.value)} /><span>{annotation ? "Draft retained for this browser session only" : "No annotation supplied"}</span></label> : null}
          <div className="dl-fbox">
            <div className="fk">GAP TYPES (7)</div>
            {GAP_TYPES.map((g) => (
              <div className="fr" key={g.label}>
                <span className="fl">{g.label}</span>
                <b>
                  {g.count} · {g.grade}
                </b>
              </div>
            ))}
          </div>
          <div className="dl-chiprow">
            <button type="button" className="dl-btn ghost" aria-pressed={gapsOnly} onClick={() => setGapsOnly(!gapsOnly)} style={{ minHeight: 26, padding: "4px 8px" }}>
              {gapsOnly ? "Show full timeline" : "Show all gaps in timeline"}
            </button>
            <a className="dl-btn ghost" download="delivery-gap-snapshot.json" href={`data:application/json;charset=utf-8,${encodeURIComponent(JSON.stringify({ source: "authored design snapshot", gaps: PARTIAL_LANES.flatMap((lane) => lane.events.filter((ev) => ev.kind === "gap").map((ev) => ({ lane: lane.id, ...ev }))), acknowledged, localAnnotation: annotation }, null, 2))}`}>
              Export gap report (JSON)
            </a>
          </div>
          <p className="dl-hint">
            Adjudication appends a local record. It never rewrites source facts or fabricates attribution. Decimal
            confidence theatre is not shown.
          </p>
        </div>
      </aside>
    </div>
  );
}

/* ---- State 12: dense fan-out ---- */

const MINI_NODE_COLORS = ["#f0b429", "#5ee7ff", "#9be15d", "#c084fc", "#38cfe8"];

/** Mini-lane episode graph inside the state-12 amber envelope: discrete nodes, times, short luminous links. */
function MiniEpisodeGraph(props: { seed: string; start: string; end: string; unresolved: number }) {
  const rnd = mulberry(hash32(props.seed));
  const laneA = Array.from({ length: 8 }, (_, i) => ({
    x: 22 + i * 66 + (rnd() - 0.5) * 10,
    c: MINI_NODE_COLORS[Math.floor(rnd() * MINI_NODE_COLORS.length)],
    bad: false,
  }));
  const laneB = Array.from({ length: 5 }, (_, i) => ({
    x: 120 + i * 78 + (rnd() - 0.5) * 12,
    c: MINI_NODE_COLORS[Math.floor(rnd() * MINI_NODE_COLORS.length)],
    bad: false,
  }));
  for (let i = 0; i < props.unresolved; i++) {
    const pick = Math.floor(rnd() * laneB.length);
    laneB[pick].bad = true;
  }
  const yA = 17;
  const yB = 41;
  const spawnIdx = 1 + Math.floor(rnd() * 2);
  return (
    <svg className="dl-minilane" viewBox="0 0 520 52" preserveAspectRatio="none" aria-hidden="true">
      <line x1={laneA[0].x} x2={laneA[laneA.length - 1].x} y1={yA} y2={yA} stroke="#f0b429" strokeOpacity="0.55" strokeWidth="1.3" />
      <line x1={laneB[0].x} x2={laneB[laneB.length - 1].x} y1={yB} y2={yB} stroke="#f0b429" strokeOpacity="0.35" strokeWidth="1.1" strokeDasharray="1 0" />
      <path
        d={`M ${laneA[spawnIdx].x} ${yA + 4} C ${laneA[spawnIdx].x} ${yA + 16}, ${laneB[0].x} ${yB - 16}, ${laneB[0].x} ${yB - 4}`}
        fill="none"
        stroke="#f0b429"
        strokeOpacity="0.5"
        strokeWidth="1"
      />
      <path
        d={`M ${laneB[laneB.length - 1].x} ${yB - 4} C ${laneB[laneB.length - 1].x} ${yB - 16}, ${laneA[laneA.length - 1].x} ${yA + 16}, ${laneA[laneA.length - 1].x} ${yA + 4}`}
        fill="none"
        stroke="#f0b429"
        strokeOpacity="0.5"
        strokeWidth="1"
        strokeDasharray="3 3"
      />
      {laneA.map((n, i) => (
        <g key={`a${i}`}>
          <circle cx={n.x} cy={yA} r={7} fill={n.c} opacity="0.16" />
          <circle cx={n.x} cy={yA} r={3.8} fill="#0a0f18" stroke={n.c} strokeWidth="1.3" />
          <circle cx={n.x} cy={yA} r={1.3} fill={n.c} />
        </g>
      ))}
      {laneB.map((n, i) => (
        <g key={`b${i}`}>
          <circle cx={n.x} cy={yB} r={7} fill={n.bad ? "#ff5f56" : n.c} opacity="0.16" />
          <circle cx={n.x} cy={yB} r={3.8} fill="#0a0f18" stroke={n.bad ? "#ff5f56" : n.c} strokeWidth="1.3" />
          <circle cx={n.x} cy={yB} r={1.3} fill={n.bad ? "#ff5f56" : n.c} />
        </g>
      ))}
      <text x={laneA[0].x - 4} y={7} className="mt">
        {props.start}
      </text>
      <text x={laneA[laneA.length - 1].x + 4} y={7} textAnchor="end" className="mt">
        {props.end}
      </text>
    </svg>
  );
}

export function DenseFanoutState() {
  const [query, setQuery] = useWorkspaceState("delivery.fixture.dense.query", "");
  const [selectedId, setSelectedId] = useWorkspaceState("delivery.fixture.dense.selectedId", "guards");
  const [unresolvedOnly, setUnresolvedOnly] = useWorkspaceState("delivery.fixture.dense.unresolvedOnly", false);
  const [testRiskOnly, setTestRiskOnly] = useWorkspaceState("delivery.fixture.dense.testRiskOnly", false);
  const [zoom, setZoom] = useWorkspaceState("delivery.fixture.dense.zoom", "workstream");
  const [pins, setPins] = useWorkspaceState<string[]>("delivery.fixture.dense.pins", ["guards", "evidence", "review"]);
  const [boundary, setBoundary] = useWorkspaceState("delivery.fixture.dense.boundary", "");
  const selected = WORKSTREAMS.find((stream) => stream.id === selectedId) ?? WORKSTREAMS[1];
  const visible = WORKSTREAMS.filter((stream) => stream.label.toLowerCase().includes(query.toLowerCase()) && (!unresolvedOnly || stream.unresolved > 0) && (!testRiskOnly || stream.test_risk > 0));
  return (
    <div className="dl-stage is-dense">
      <aside className="dl-pane">
        <h3>
          BRANCH NAVIGATOR <span>Saved selection</span>
        </h3>
        <div className="dl-scroll">
          <input className="dl-search" placeholder="Search branches, clusters…" value={query} onChange={(event) => setQuery(event.target.value)} />
          <div className="dl-chiprow">
            <button className="dl-att tone-danger" aria-pressed={unresolvedOnly} onClick={() => setUnresolvedOnly(!unresolvedOnly)}>Unresolved only</button>
            <button className="dl-att tone-amber" aria-pressed={testRiskOnly} onClick={() => setTestRiskOnly(!testRiskOnly)}>test_risk</button>
            <button className="dl-att tone-violet" onClick={() => setBoundary("Per-workstream weak-evidence source counts are not attached. No filter result is invented.")}>Weak evidence</button>
            <button className="dl-att tone-quiet" onClick={() => setBoundary("Revision-specific review coverage is available only in the separate review fixture, not these workstream summaries.")}>Unreviewed</button>
          </div>
          <div className="dl-fbox">
            <div className="fk">
              PINNED BUNDLES <button className="dl-textbutton" onClick={() => setPins(pins.includes(selectedId) ? pins.filter((id) => id !== selectedId) : [...pins, selectedId])}>{pins.includes(selectedId) ? "Unpin selected" : "Pin selected"}</button>
            </div>
            {visible.filter((w) => pins.includes(w.id)).map((w) => (
              <div className={selectedId === w.id ? "fr is-on" : "fr"} key={w.id} role="button" tabIndex={0} aria-label={`Select workstream ${w.label}`} onClick={() => setSelectedId(w.id)} onKeyDown={(event) => { if (event.key === "Enter") setSelectedId(w.id); }}>
                <span className="fl">
                  <i className="mark" style={{ background: w.color }} />
                  {w.label}
                </span>
                <b>{w.agents} agents</b>
              </div>
            ))}
          </div>
          <div className="dl-fbox">
            <div className="fk">BREAKDOWN · PR #8127 › delivery › workflow guards</div>
            <div className="dl-chiprow">
              <button className="dl-chip" onClick={() => setBoundary("Task parentage is not attached to these summary records. No causal path to root can be established.")}>PATH TO ROOT</button>
              <button className="dl-chip" onClick={() => setBoundary("No recorded integration result joins this workstream to outcome. Its evidence port remains open.")}>PATH TO OUTCOME</button>
            </div>
          </div>
          <div className="dl-fbox">
            <div className="fk">WORKSTREAMS (6) · Agents · Status</div>
            {visible.map((w) => (
              <div className={selectedId === w.id ? "fr is-on" : "fr"} key={w.id} role="button" tabIndex={0} aria-label={`Select workstream ${w.label}`} onClick={() => setSelectedId(w.id)} onKeyDown={(event) => { if (event.key === "Enter") setSelectedId(w.id); }}>
                <span className="fl">
                  <i className="mark" style={{ background: w.color }} />
                  {w.label}
                </span>
                <b>
                  {w.agents} · U{w.unresolved} / T{w.test_risk}
                </b>
              </div>
            ))}
          </div>
          <div className="dl-fbox">
            <div className="fk">SELECTED CLUSTER</div>
            <div className="fr">
              <span className="fl amber">{selected.label}</span>
            </div>
            <div className="fr">
              <span className="fl">{selected.agents} agents / {selected.episodes} episodes / {selected.unresolved} unresolved</span>
            </div>
            <div className="fr">
              <span className="fl mono">{selectedId === "guards" ? "11:27 → 16:02 (authored span)" : "Time span not attached"}</span>
            </div>
            <div className="fr">
              <span className="fl">Collapsed descendants</span>
              <b>{selectedId === "guards" ? "156 authored" : "Not specified"}</b>
            </div>
            <div className="fr">
              <span className="fl">test_risk</span>
              <b style={{ color: "var(--activity-amber)" }}>{selected.test_risk} named findings</b>
            </div>
            <div className="fr">
              <span className="fl">Evidence</span>
              <b>Authored summary only</b>
            </div>
            <div className="fr">
              <span className="fl">Review coverage</span>
              <b>{selectedId === "guards" ? "58% fixture revision" : "Not specified"}</b>
            </div>
            <div className="dl-chiprow" style={{ marginTop: 4 }}>
              {["guards", "policy", "tests", "retry", "checks"].map((x) => (
                <span className="dl-chip" key={x}>
                  {x}
                </span>
              ))}
            </div>
          </div>
          <div className="dl-stat">
            <span>unique agents</span>
            <b>128</b>
          </div>
          <div className="dl-stat">
            <span>episodes</span>
            <b>469 authored episodes</b>
          </div>
          <p className="dl-hint">⊙ Focus+Context enabled. 128 agents is a concept population, not a hard product ceiling or a production count.</p>
        </div>
      </aside>
      <section className="dl-pane">
        <h3>
          DELIVERY · DENSE FAN-OUT · 128 AGENTS{" "}
          <span>Authored workstreams · no completed integration inferred · inspect selected source</span>
        </h3>
        <div className="dl-jtoolbar" style={{ borderTop: 0, paddingBottom: 0 }}>
          <span className="mono dim">SEMANTIC ZOOM</span>
          <button className={zoom === "outcome" ? "chip on" : "chip"} onClick={() => setZoom("outcome")}>OUTCOME</button>
          <button className={zoom === "workstream" ? "chip on" : "chip"} onClick={() => setZoom("workstream")}>WORKSTREAM</button>
          <span className="mono dim">Agent/event source pages not attached to snapshot</span>
          <button className="chip" onClick={() => { setZoom("workstream"); setSelectedId("guards"); setQuery(""); setUnresolvedOnly(false); setTestRiskOnly(false); }}>Reset focus</button>
          <a className="chip" href="?data=fixture&surface=delivery&state=05">Replay loaded example →</a>
          <span className="mono dim">08:31 → 20:11 UTC · LOADED EXAMPLE</span>
          <span className="grow" />
          <span className="mono dim">
            REVIEW COVERAGE <b style={{ color: "var(--signal-cyan)" }}>58%</b>
          </span>
          <span className="chip">≋ COMPRESSED</span>
        </div>
        <TimeRuler hours={["08:00", "09:00", "10:00", "11:00", "12:00", "13:00", "14:00", "15:00", "16:00", "17:00", "18:00", "19:00", "20:11"]} />
        <div className="dl-phaserow" style={{ margin: "0 8px" }}>
          <i className="spine" />
          {[
            { t: 0.05, label: "HUMAN OBJECTIVE", time: "08:31" },
            { t: 0.15, label: "PLANNING", time: "09:21" },
            { t: 0.28, label: "INVESTIGATION", time: "10:36" },
            { t: 0.41, label: "DECISION", time: "11:52" },
            { t: 0.55, label: "IMPLEMENTATION", time: "13:15" },
            { t: 0.69, label: "VERIFICATION", time: "15:02" },
            { t: 0.8, label: "REVIEW", time: "16:48" },
            { t: 0.88, label: "CI", time: "18:36" },
            { t: 0.955, label: "PLANNED OUTCOME", time: "20:11" },
          ].map((p) => (
            <span className="ms" key={p.label} style={{ left: `${p.t * 100}%` }}>
              <b>{p.label}</b>
              <i className="ring" />
              <em>{p.time}</em>
            </span>
          ))}
        </div>
        {boundary && <p role="status" className="dl-local-notice">{boundary}<button className="dl-textbutton" onClick={() => setBoundary("")}>Dismiss</button></p>}
        <div className="dl-densewrap">
          <DenseFanout streams={WORKSTREAMS} focus={zoom === "outcome" ? undefined : selectedId} onSelect={setSelectedId} />
          <div className="dl-densebox" hidden={zoom === "outcome"}>
            {selectedId !== "guards" ? <><h4>{selected.label}</h4><p>{selected.agents} agents · {selected.episodes} episodes · {selected.unresolved} unresolved · test_risk {selected.test_risk}</p><p className="dl-local-notice">The workstream summary is loaded. Exact agent/episode source pages are not attached to this authored snapshot.</p></> : <>
            <h4>
              workflow guards · 27 agents · 91 episodes · 4 unresolved
              <span className="tags">
                <span className="dl-att tone-danger">4 UNRESOLVED</span>
                <span className="dl-att tone-amber">2 TEST_RISK</span>
                <span className="dl-att tone-ready">37 EXAMPLE EVIDENCE</span>
                <span className="dl-att tone-quiet">58% REVIEW COVERAGE</span>
              </span>
            </h4>
            {[
              { name: "guard policy engine", a: "11 agents", e: "42 episodes", start: "12:15", end: "13:48", span: "12:15 → 13:48 (1h 33m)", chips: ["unresolved 2", "high risk 1", "18 evidence"], desc: 48, bad: 2 },
              { name: "retry + backoff logic", a: "9 agents", e: "28 episodes", start: "11:41", end: "14:02", span: "11:41 → 14:02 (2h 21m)", chips: ["unresolved 1", "high risk 1", "13 evidence"], desc: 37, bad: 1 },
              { name: "guard tests + fixtures", a: "7 agents", e: "21 episodes", start: "11:50", end: "14:18", span: "11:50 → 14:18 (2h 28m)", chips: ["unresolved 1", "7 evidence"], desc: 31, bad: 1 },
            ].map((row) => (
              <div className="sub" key={row.name}>
                <div className="l1">
                  <b>⛨ {row.name}</b>
                  <span className="mono">
                    {row.a} · {row.e}
                  </span>
                  <span className="chips">
                    {row.chips.map((c) => (
                      <em key={c}>{c}</em>
                    ))}
                  </span>
                </div>
                <MiniEpisodeGraph seed={row.name} start={row.start} end={row.end} unresolved={row.bad} />
                <div className="l2 mono">
                  {row.span} · Collapsed descendants {row.desc}
                </div>
              </div>
            ))}
            </>}
          </div>
        </div>
        <details className="dl-exact-fallback"><summary>Exact workstream table · 128 agents</summary><table><thead><tr><th>Workstream</th><th>Agents</th><th>Episodes</th><th>Unresolved</th><th>test_risk</th></tr></thead><tbody>{WORKSTREAMS.map((stream) => <tr key={stream.id}><td>{stream.label}</td><td>{stream.agents}</td><td>{stream.episodes}</td><td>{stream.unresolved}</td><td>{stream.test_risk}</td></tr>)}</tbody></table></details>
        <div className="dl-jicons mono">
          {[
            ["▣", "Task"],
            ["◇", "Decision"],
            ["✎", "Edit"],
            ["✓", "Test"],
            ["⬡", "Commit"],
            ["◉", "Review"],
            ["🗨", "Feedback"],
            ["✳", "Spawn"],
            ["⇢", "Handoff"],
            ["⭯", "Rejoin"],
            ["●", "Exact/Evidence"],
            ["◐", "Active / Decision"],
            ["◌", "Inferred / Ambiguous"],
            ["▨", "Missing / Broken"],
            ["✔", "Verified"],
          ].map(([i, x]) => (
            <span key={x}>
              <i>{i}</i>
              {x}
            </span>
          ))}
        </div>
        <JourneyMinimap
          lanes={WORKSTREAMS.map((w) => ({
            id: `mm-${w.id}`,
            label: w.label,
            role: "",
            color: w.color,
            burst: 110,
            events: [],
          }))}
          window={[0.3, 0.62]}
          hours={["◀ PAST (COMPRESSED)", "FUTURE (COMPRESSED) ▶"]}
        />
        <div className="dl-jtoolbar">
          <span className="mono dim">OVERVIEW MODE</span>
          <span className="chip on">Causal Loom</span>
          <button className="chip" onClick={(e) => { const details = e.currentTarget.closest("section")?.querySelector("details"); if(details) { details.open = !details.open; details.scrollIntoView({block:"nearest"}); } }}>Exact Table</button>
          <button className="chip" onClick={() => setBoundary("Tree source projection is not attached to this authored fixture.")}>Tree</button>
          <button className="chip" onClick={() => setBoundary("Transcript source projection is not attached to this authored fixture.")}>Transcript</button>
          <span className="mono dim">FILTERS</span>
          <button className="chip" onClick={() => setBoundary("Providers: all source projection is not attached to this authored fixture.")}>Providers: all ⌄</button>
          <button className="chip" onClick={() => setBoundary("Evidence: all source projection is not attached to this authored fixture.")}>Evidence: all ⌄</button>
          <button className="chip" onClick={() => setBoundary("Risk: all source projection is not attached to this authored fixture.")}>Risk: all ⌄</button>
          <button className="chip" onClick={() => setBoundary("Reviewed: any source projection is not attached to this authored fixture.")}>Reviewed: any ⌄</button>
          <span className="grow" />
          <span className="chip chip-violet">🔒 PROVIDER BOUNDARY read-only</span>
        </div>
      </section>
    </div>
  );
}
