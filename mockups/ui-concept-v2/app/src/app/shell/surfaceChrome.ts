import { CHANNELS, channelToSurface, surfaceToChannel, type ChannelName, type Surface } from "../../data/fixtures";
import { parseDeliveryState } from "../../delivery/data";
import { parseLoomState } from "../../loom/types";
import type { DemoMode } from '../workspace';
import workload from '../../data/tracked-workload';
import { PACK } from '../../data/pack';
import { atlasData } from '../../structure/model';

export type { ChannelName, Surface };
export { channelToSurface, surfaceToChannel, parseDeliveryState };
export type SurfaceSlug =
  | "explorer"
  | "loom"
  | "sessions"
  | "agents"
  | "code"
  | "knowledge"
  | "delivery"
  | "automations"
  | "observatory"
  | "costs"
  | "settings"
  | "work"
  | "workflows";
export type RouteSurface = "brain" | SurfaceSlug;
export type IconName =
  | "link"
  | "wifi"
  | "graph"
  | "scope"
  | "pulse"
  | "layers"
  | "lock"
  | "sliders"
  | "search"
  | "db"
  | "cloud"
  | "shield"
  | "camera"
  | "hex"
  | "page"
  | "target"
  | "inbox"
  | "eye";
export type StatusTone = "live" | "quiet" | "ready" | "scope" | "partial" | "running";

export type StatusCell = {
  icon: IconName;
  lab: string;
  val: string;
  sub?: string;
  tone: StatusTone;
  bar?: number;
};

export type InspectorRow = { label?: string; value: string };
export type InspectorSection = {
  k: string;
  rows?: InspectorRow[];
  text?: string;
  code?: string;
};

export type SurfaceInspectorSpec = {
  title: string;
  kind: string;
  id?: string;
  sections: InspectorSection[];
};

export type RegisterExtra =
  | { type: "query"; query: string; cancel: string; running: string; pct: number }
  | { type: "loom-follow" };

export type SurfaceChrome = {
  slug: SurfaceSlug;
  channel: ChannelName;
  kicker: string;
  extra?: RegisterExtra;
  inspector: SurfaceInspectorSpec | null;
  status: StatusCell[];
  stamp: string;
};

const STAMP = "CONCEPT / PROFILE SNAPSHOT" as const;

const SURFACE_SET = new Set<string>(
  CHANNELS.filter((c) => c !== "Brain").map((c) => c.toLowerCase()),
);

export function channelToSlug(name: string): RouteSurface {
  const s = name.toLowerCase();
  if (s === "brain" || !SURFACE_SET.has(s)) return "brain";
  return s as SurfaceSlug;
}

export function slugToChannel(slug: RouteSurface): ChannelName {
  if (slug === "brain") return "Brain";
  return (CHANNELS.find((c) => c.toLowerCase() === slug) ?? "Brain") as ChannelName;
}

export function parseSurfaceParam(raw: string | null): RouteSurface {
  if (!raw) return "brain";
  const s = raw.trim().toLowerCase();
  if (s === "brain" || !SURFACE_SET.has(s)) return "brain";
  return s as SurfaceSlug;
}

export function parseStateParam(surface: RouteSurface, raw: string | null): string {
  if (surface === "loom") return parseLoomState(raw);
  if (surface === "delivery") return parseDeliveryState(raw);
  if (surface === "automations" || surface === "workflows") return raw ?? '01';
  return "01";
}

export function surfaceNeedsState(surface: RouteSurface): boolean {
  return surface === "loom" || surface === "delivery" || surface === 'automations' || surface === 'workflows';
}

export const SURFACE_CHROME: Record<SurfaceSlug, SurfaceChrome> = {
  explorer: {
    slug: "explorer",
    channel: "Explorer",
    kicker: "",
    extra: {
      type: "query",
      query: "ingest http from git in rust",
      cancel: "Cancel",
      running: "RUNNING",
      pct: 74,
    },
    inspector: {
      title: "td::ingest::http::client",
      kind: "SOURCE IDENTITY / SNIPPET / CONTEXT / PROVENANCE",
      sections: [
        {
          k: "SOURCE IDENTITY",
          rows: [
            { label: "Name", value: "td::ingest::http::client" },
            { label: "Path", value: "crates/ingest/src/http/client.rs" },
            { label: "Kind", value: "source file" },
            { label: "Language", value: "Rust" },
            { label: "Project scope", value: "all" },
            { label: "Last seen", value: "2025-05-09 14:37:11 UTC" },
            { label: "Coverage", value: "98% (analyzed)" },
          ],
        },
        {
          k: "SNIPPET (exact match)",
          code: "pub async fn ingest_http(\n  client: &Client,\n  req: Request,\n) -> Result<Response, Error> {\n  let resp = client.execute(req).await?;\n  Ok(resp)\n}",
        },
        {
          k: "CONTEXT",
          rows: [
            { label: "Symbols", value: "12" },
            { label: "Calls", value: "24" },
            { label: "References", value: "8" },
          ],
        },
        {
          k: "PROVENANCE",
          rows: [
            { label: "session", value: "Ingest HTTP refactor" },
            { label: "id", value: "sess:7f3c9a21e8b4" },
            { label: "age", value: "2d ago" },
          ],
        },
      ],
    },
    status: [
      { icon: "link", lab: "DATA", val: "snapshot", tone: "quiet" },
      { icon: "pulse", lab: "QUERY", val: "running", sub: "74%", tone: "running", bar: 74 },
      { icon: "layers", lab: "SOURCES", val: "3 ready, 1 indexing", tone: "live" },
      { icon: "scope", lab: "SCOPE", val: "all", tone: "scope" },
    ],
    stamp: STAMP,
  },
  loom: {
    slug: "loom",
    channel: "Loom",
    kicker: "",
    extra: { type: "loom-follow" },
    inspector: null,
    status: [
      { icon: "link", lab: "DATA", val: "exported profile", tone: "quiet" },
      { icon: "wifi", lab: "FEED", val: "loaded snapshot", tone: "quiet" },
      { icon: "search", lab: "QUERY", val: "trace:loom", tone: "quiet" },
      { icon: "shield", lab: "AUTHORITY", val: "recorded sources", tone: "quiet" },
    ],
    stamp: STAMP,
  },
  sessions: {
    slug: "sessions",
    channel: "Sessions",
    kicker: "Complete message timeline and session index",
    inspector: null,
    status: [
      { icon: "link", lab: "DATA", val: "snapshot", tone: "quiet" },
      { icon: "wifi", lab: "FEED", val: "quiet", tone: "quiet" },
      { icon: "graph", lab: "LCM", val: "ready", tone: "ready" },
      { icon: "page", lab: "PAGE", val: "1 loaded", tone: "live" },
      { icon: "eye", lab: "INSPECTOR", val: "closed", tone: "quiet" },
      { icon: "target", lab: "SELECTION / QUERY", val: "none", tone: "quiet" },
    ],
    stamp: STAMP,
  },
  agents: {
    slug: "agents",
    channel: "Agents",
    kicker: "DELEGATION TOPOLOGY · PARENT_SESSION_ID",
    inspector: null,
    status: [
      { icon: "link", lab: "DATA", val: "snapshot", tone: "quiet" },
      { icon: "wifi", lab: "FEED", val: "live", tone: "live" },
      { icon: "graph", lab: "ANALYTICS", val: "ready", tone: "live" },
      { icon: "hex", lab: "DELEGATION", val: "partial", tone: "partial" },
      { icon: "target", lab: "SELECTED HANDOFF", val: "1", tone: "ready" },
    ],
    stamp: STAMP,
  },
  code: {
    slug: "code",
    channel: "Code",
    kicker: "CODE / CORTEX",
    inspector: null,
    status: [
      { icon: "link", lab: "DATA", val: "snapshot", tone: "quiet" },
      { icon: "wifi", lab: "FEED", val: "quiet", tone: "quiet" },
      { icon: "graph", lab: "GRAPH", val: "unsealed", tone: "partial" },
      { icon: "db", lab: "INDEX", val: "not sealed", tone: "partial" },
      { icon: "target", lab: "SELECTION", val: "none", tone: "quiet" },
    ],
    stamp: STAMP,
  },
  knowledge: {
    slug: "knowledge",
    channel: "Knowledge",
    kicker: "KNOWLEDGE / FACT PROVENANCE",
    inspector: null,
    status: [
      { icon: "link", lab: "DATA", val: "snapshot", tone: "quiet" },
      { icon: "layers", lab: "MEMORY", val: "absent", tone: "quiet" },
      { icon: "graph", lab: "GRAPH", val: "unavailable", tone: "quiet" },
      { icon: "camera", lab: "CAMERA", val: "Facts", tone: "live" },
    ],
    stamp: STAMP,
  },
  delivery: {
    slug: "delivery",
    channel: "Delivery",
    kicker: "DELIVERY / GLOBAL INBOX",
    inspector: {
      title: "V2 code-intelligence release",
      kind: "UMBRELLA INSPECTOR",
      id: "READ-ONLY PROVIDER",
      sections: [
        {
          k: "UMBRELLA OUTCOME",
          text: "Ship TraceDecay V2 code-intelligence platform with cross-repo ingestion, analysis, and delivery stability improvements.",
        },
        {
          k: "CORRELATED PRs",
          rows: [
            { label: "rspeck", value: "13 PRs" },
            { label: "rspress", value: "7 PRs" },
            { label: "rebuild / Rslib", value: "9 PRs" },
          ],
        },
        {
          k: "WORK TASK COVERAGE",
          rows: [
            { label: "tasks", value: "238 / 276" },
            { label: "coverage", value: "86%" },
          ],
        },
        {
          k: "PROVIDER",
          rows: [{ label: "mode", value: "read-only" }],
        },
      ],
    },
    status: [
      { icon: "link", lab: "DATA", val: "snapshot", tone: "quiet" },
      { icon: "inbox", lab: "PR INBOX", val: "partial", sub: "1,842 items", tone: "quiet" },
      { icon: "graph", lab: "REVIEW EVIDENCE", val: "mixed", sub: "87% average", tone: "live" },
      { icon: "target", lab: "SELECTION", val: "one umbrella", sub: "V2 code-intelligence release", tone: "live" },
      { icon: "lock", lab: "PROVIDER", val: "read-only", tone: "live" },
    ],
    stamp: STAMP,
  },
  automations: {
    slug: "automations",
    channel: "Automations",
    kicker: "SCHEDULER / RUN LEDGER",
    inspector: null,
    status: [
      { icon: "link", lab: "DATA", val: "authored example", tone: "quiet" },
      { icon: "wifi", lab: "SCHEDULER", val: "illustrated", tone: "quiet" },
      { icon: "db", lab: "LEDGER", val: "example runs", tone: "quiet" },
      { icon: "target", lab: "SELECTION", val: "example run", tone: "quiet" },
    ],
    stamp: "CONCEPT / SYNTHETIC DATA",
  },
  observatory: {
    slug: "observatory",
    channel: "Observatory",
    kicker: "OBSERVATORY / SYSTEM EVIDENCE",
    inspector: null,
    status: [
      { icon: "link", lab: "DATA", val: "snapshot", tone: "quiet" },
      { icon: "wifi", lab: "OBSERVATIONS", val: "partial", tone: "partial" },
      { icon: "graph", lab: "INDEX", val: "unsealed", tone: "partial" },
      { icon: "db", lab: "STORAGE", val: "snapshot", tone: "quiet" },
      { icon: "target", lab: "SELECTION", val: "observatory", tone: "scope" },
    ],
    stamp: STAMP,
  },
  costs: {
    slug: "costs",
    channel: "Costs",
    kicker: "COSTS / ACTUAL SPEND",
    inspector: null,
    status: [
      { icon: "link", lab: "DATA", val: "snapshot", tone: "quiet" },
      { icon: "cloud", lab: "PROVIDER SPEND", val: "unavailable", tone: "quiet" },
      { icon: "graph", lab: "USAGE", val: "spine-only", tone: "partial" },
      { icon: "db", lab: "PRICING COVERAGE", val: "0%", tone: "quiet" },
      { icon: "target", lab: "SELECTION", val: "all", tone: "scope" },
    ],
    stamp: STAMP,
  },
  settings: {
    slug: "settings",
    channel: "Settings",
    kicker: "SETTINGS / EFFECTIVE CONFIGURATION",
    inspector: null,
    status: [
      { icon: "link", lab: "DATA", val: "snapshot", tone: "quiet" },
      { icon: "sliders", lab: "CONFIG", val: "snapshot", tone: "partial" },
      { icon: "lock", lab: "SCOPE", val: "read-only", tone: "live" },
      { icon: "search", lab: "APPLY", val: "unavailable", tone: "quiet" },
      { icon: "target", lab: "SELECTION", val: "settings", tone: "scope" },
    ],
    stamp: STAMP,
  },
  work: {
    slug: "work",
    channel: "Work",
    kicker: "WORK / BOARD",
    inspector: null,
    status: [
      { icon: "link", lab: "DATA", val: "snapshot", tone: "quiet" },
      { icon: "graph", lab: "WORK GRAPH", val: "absent", tone: "quiet" },
      { icon: "lock", lab: "VERSION", val: "n/a", tone: "quiet" },
      { icon: "target", lab: "SELECTION", val: "none", tone: "quiet" },
    ],
    stamp: STAMP,
  },
  workflows: {
    slug: "workflows",
    channel: "Workflows",
    kicker: "WORKFLOWS / DEFINITION LEDGER",
    inspector: null,
    status: [
      { icon: "link", lab: "DATA", val: "authored example", tone: "quiet" },
      { icon: "wifi", lab: "FEED", val: "read-only", tone: "quiet" },
      { icon: "graph", lab: "REGISTRY", val: "example definitions", tone: "quiet" },
      { icon: "scope", lab: "SCOPE", val: "all", tone: "scope" },
      { icon: "search", lab: "SELECTION", val: "example definition", tone: "quiet" },
    ],
    stamp: "CONCEPT / SYNTHETIC DATA",
  },
};

export function chromeFor(surface: SurfaceSlug, source?: string | null, sourceContext = false, mode: DemoMode = 'snapshot'): SurfaceChrome {
  const chrome = SURFACE_CHROME[surface];
  if (sourceContext) return { ...chrome, kicker: `${surface.toUpperCase()} / LOOM SOURCE`, extra: undefined, inspector: null,
    stamp: source === 'design' ? 'CONCEPT / SYNTHETIC DATA' : STAMP,
    status: [
      { icon: 'link', lab: 'SOURCE', val: source === 'design' ? 'authored example' : source === 'ubuntu' ? 'Ubuntu snapshot' : !source || source === 'mac' ? 'Mac snapshot' : 'unavailable', tone: 'quiet' },
      { icon: 'lock', lab: 'MODE', val: 'read-only source context', tone: 'quiet' },
    ] };
  if (mode === 'fixture' && surface === 'code') return { ...chrome, stamp:'LABELED LOCAL SOURCES / NOT LIVE', status:[
    {icon:'graph',lab:'CORTEX / TRACE / CORE',val:'authored example',tone:'quiet'},
    {icon:'page',lab:'EXACT FILES',val:'measured Git snapshot',tone:'quiet'},
    {icon:'wifi',lab:'FEED',val:'not live',tone:'quiet'},
  ] };
  if (mode === 'fixture' && !['explorer', 'sessions'].includes(surface)) return { ...chrome, stamp:'DESIGN FIXTURES / LOCAL ONLY', status:[
    {icon:'link',lab:'DATA',val:'authored example',tone:'quiet'},
    {icon:'wifi',lab:'FEED',val:'not live',tone:'quiet'},
    {icon:'lock',lab:'ACTIONS',val:'local demonstration',tone:'quiet'},
    {icon:'camera',lab:'CAMERA',val:surface==='knowledge'?'Facts':surface,tone:'scope'},
  ] };
  const cell = (lab:string,val:string,tone:StatusTone='quiet'): StatusCell => ({icon:'graph',lab,val,tone});
  const status: Partial<Record<SurfaceSlug,StatusCell[]>> = {
    explorer:[cell('QUERY','local filter'),cell('SOURCES','Git paths + sessions'),cell('FACTS / SEMANTIC','unavailable')],
    sessions:[cell('SESSIONS',`${PACK.totals.sessions} captured`),cell('EVENTS','spine only'),cell('TOKEN BODIES','unavailable')],
    agents:[cell('RELATIONS','recorded parentage'),cell('COVERAGE','partial','partial'),cell('HANDOFF','no handoff receipt')],
    code:[cell('GIT',atlasData.revision.slice(0,8)),cell('STRUCTURE','containment + manifests'),cell('SYMBOL GRAPH','not included')],
    knowledge:[cell('FACTS','unavailable'),cell('SUBJECTS','independent Git structure'),cell('CAMERA','Facts','scope')],
    delivery:workload.trackingCoverage.state === 'unavailable'
      ? [cell('TRACKING','unavailable','partial'),cell('INDEXED WORK','not served'),cell('COUNTS','unknown')]
      : [cell('INDEXED BRANCHES',`${workload.trackedBranchCount ?? 'unknown'}`),cell('LINKED PRs',`${workload.prContextCount ?? 'unknown'}`),cell('SCOPE','captured sample','partial'),cell('CI','not captured')],
    automations:[cell('SCHEDULER','unavailable'),cell('RUNS','not exported'),cell('ACTIONS','unavailable')],
    workflows:[cell('DEFINITIONS','unavailable'),cell('RUNS','not exported'),cell('ACTIONS','unavailable')],
    work:[cell('WORK GRAPH','unavailable'),cell('DELEGATION','separate evidence'),cell('ADMISSION','unavailable')],
    costs:[cell('USAGE','spine only'),cell('PRICING','unserved'),cell('ATTRIBUTION','unavailable')],
    settings:[cell('CONFIG','snapshot'),cell('WRITE AUTHORITY','unserved'),cell('APPLY','unavailable')],
  };
  return { ...chrome, stamp:'RECORDED SNAPSHOTS / READ ONLY', status:[
    {icon:'link',lab:'DATA',val:surface==='delivery'?'TraceDecay tracked scope':'recorded snapshot',tone:'quiet'},
    ...(status[surface] ?? chrome.status.filter(item=>item.lab!=='DATA').map(item=>({...item,tone:item.tone==='live'?'quiet' as const:item.tone}))),
  ] };
}
