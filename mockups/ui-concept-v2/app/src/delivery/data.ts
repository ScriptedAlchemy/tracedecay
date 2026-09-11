import type { EvidenceGrade } from "../data/fixtures";

export const DELIVERY_STATE_IDS = [
  "01",
  "02",
  "03",
  "04",
  "05",
  "06",
  "07",
  "08",
  "09",
  "10",
  "11",
  "12",
] as const;

export type DeliveryStateId = (typeof DELIVERY_STATE_IDS)[number];

export type HonestInbox =
  | "unauthorized"
  | "not_published"
  | "rate-limited"
  | "denied"
  | "stale"
  | "unavailable"
  | "served-empty";

export type AttentionSource =
  | "unresolved"
  | "test_risk"
  | "unsafe_patterns"
  | "weak_evidence"
  | "contradictions"
  | "unreviewed";

export type StatusTone = "live" | "quiet" | "ready" | "scope" | "danger" | "violet" | "amber";

export type DeliveryMeta = {
  id: DeliveryStateId;
  slug: string;
  kicker: string;
  scope: string;
  scopeNote: string;
  provider: string;
  status: { lab: string; val: string; tone: StatusTone }[];
};

export const ATTENTION: { id: AttentionSource; label: string; count: number; tone: StatusTone }[] = [
  { id: "unresolved", label: "unresolved", count: 176, tone: "danger" },
  { id: "test_risk", label: "test_risk", count: 48, tone: "amber" },
  { id: "unsafe_patterns", label: "unsafe_patterns", count: 22, tone: "violet" },
  { id: "weak_evidence", label: "weak evidence", count: 96, tone: "amber" },
  { id: "contradictions", label: "contradictions", count: 11, tone: "danger" },
  { id: "unreviewed", label: "unreviewed", count: 312, tone: "quiet" },
];

export const HONEST_INBOX: { id: HonestInbox; label: string; note: string }[] = [
  { id: "unauthorized", label: "unauthorized", note: "provider credential refused" },
  { id: "not_published", label: "not_published", note: "github_read_authority absent" },
  { id: "rate-limited", label: "rate-limited", note: "provider window exhausted" },
  { id: "denied", label: "denied", note: "org policy blocked the read" },
  { id: "stale", label: "stale", note: "freshness window elapsed" },
  { id: "unavailable", label: "unavailable", note: "authority or transport missing" },
  { id: "served-empty", label: "served-empty", note: "complete zero — not a blank panel" },
];

export const DELIVERY_META: Record<DeliveryStateId, DeliveryMeta> = {
  "01": {
    id: "01",
    slug: "global-pr-inbox",
    kicker: "DELIVERY / GLOBAL INBOX",
    scope: "all",
    scopeNote: "unscoped · all enrolled projects",
    provider: "read-only",
    status: [
      { lab: "DATA", val: "snapshot", tone: "quiet" },
      { lab: "PR INBOX", val: "partial · 1,842 items", tone: "amber" },
      { lab: "PROVIDER", val: "read-only", tone: "violet" },
      { lab: "SELECTION", val: "umbrella · V2 release", tone: "scope" },
    ],
  },
  "02": {
    id: "02",
    slug: "project-scoped-inbox",
    kicker: "DELIVERY / PROJECT INBOX",
    scope: "tracedecay",
    scopeNote: "scoped · proj_ae394425f7837d4f",
    provider: "read-only",
    status: [
      { lab: "DATA", val: "snapshot", tone: "quiet" },
      { lab: "PR INBOX", val: "tracedecay · 612 items", tone: "live" },
      { lab: "PROVIDER", val: "read-only", tone: "violet" },
      { lab: "SELECTION", val: "#707 · remote retry backoff", tone: "scope" },
    ],
  },
  "03": {
    id: "03",
    slug: "registered-pr-evidence-graph",
    kicker: "DELIVERY / REGISTERED PR EVIDENCE",
    scope: "all",
    scopeNote: "registered repositories · tracked indexed heads",
    provider: "read-only",
    status: [
      { lab: "DATA", val: "authored example", tone: "quiet" },
      { lab: "PR INBOX", val: "3 admitted · 1 not joined", tone: "amber" },
      { lab: "PROVIDER", val: "fixture only", tone: "violet" },
      { lab: "SELECTION", val: "click a PR or signal", tone: "quiet" },
    ],
  },
  "04": {
    id: "04",
    slug: "pr-journey-overview",
    kicker: "DELIVERY / PR JOURNEY OVERVIEW",
    scope: "tracedecay",
    scopeNote: "repository · tracedecay",
    provider: "read-only",
    status: [
      { lab: "DATA", val: "snapshot", tone: "quiet" },
      { lab: "JOURNEY", val: "23 PRs · 7 agents", tone: "live" },
      { lab: "EVENTS", val: "1,842 persisted", tone: "ready" },
      { lab: "SCOPE", val: "this repo · full history", tone: "scope" },
    ],
  },
  "05": {
    id: "05",
    slug: "temporal-replay",
    kicker: "DELIVERY / TEMPORAL REPLAY",
    scope: "all",
    scopeNote: "PR #18337 · recorded / loaded evidence",
    provider: "read-only",
    status: [
      { lab: "DATA", val: "snapshot", tone: "quiet" },
      { lab: "REPLAY", val: "paused · 63%", tone: "amber" },
      { lab: "SOURCE", val: "recorded complete", tone: "ready" },
      { lab: "POSITION", val: "12:47 / 20:11", tone: "scope" },
    ],
  },
  "06": {
    id: "06",
    slug: "expanded-agent-branches",
    kicker: "DELIVERY / PR #707 CAUSAL JOURNEY",
    scope: "all",
    scopeNote: "expanded agent / subagent branches",
    provider: "read-only",
    status: [
      { lab: "DATA", val: "snapshot", tone: "quiet" },
      { lab: "PR #707", val: "selected · 10h 42m", tone: "live" },
      { lab: "AGENTS", val: "7 involved · 5 unique", tone: "ready" },
      { lab: "PROVIDER", val: "read-only", tone: "violet" },
    ],
  },
  "07": {
    id: "07",
    slug: "honest-partial-unknown",
    kicker: "DELIVERY / PR #709 HONEST PARTIAL JOURNEY",
    scope: "tracedecay",
    scopeNote: "partial · 53% covered · gaps retained",
    provider: "read-only",
    status: [
      { lab: "JOURNEY", val: "partial · 53% observed", tone: "amber" },
      { lab: "ATTRIBUTION", val: "ambiguous · 2 intervals", tone: "violet" },
      { lab: "CI SOURCE", val: "denied · 1 interval", tone: "danger" },
      { lab: "PROVIDER REVIEW", val: "stale · 2 intervals", tone: "amber" },
    ],
  },
  "08": {
    id: "08",
    slug: "review-coverage-diff-checks",
    kicker: "DELIVERY / PR REVIEW",
    scope: "tracedecay",
    scopeNote: "PR #707 · exact diff / threads / checks",
    provider: "read-only",
    status: [
      { lab: "CHANGES", val: "12 / 48 files · 7 commits", tone: "ready" },
      { lab: "REVIEWS", val: "5 current · 2 outdated", tone: "live" },
      { lab: "CI", val: "6/8 checks · 1 rate-limited", tone: "amber" },
      { lab: "RELEASE", val: "not_published", tone: "violet" },
    ],
  },
  "09": {
    id: "09",
    slug: "follow-the-story-review-workspace",
    kicker: "DELIVERY / FOLLOW THE STORY",
    scope: "tracedecay",
    scopeNote: "PR #8127 · synthetic reconstruction",
    provider: "read-only",
    status: [
      { lab: "DATA", val: "snapshot", tone: "quiet" },
      { lab: "COVERAGE", val: "4 of 7 episodes", tone: "amber" },
      { lab: "FEEDBACK", val: "local · provider write unavailable", tone: "violet" },
      { lab: "SCOPE", val: "this repo · full history", tone: "scope" },
    ],
  },
  "10": {
    id: "10",
    slug: "decision-to-code-pr743",
    kicker: "FOLLOW THE PRODUCER STORY · EXACT YAML REVIEW",
    scope: "tracedecay",
    scopeNote: "PR #743 · MERGED · Decision to Code",
    provider: "read-only",
    status: [
      { lab: "DATA", val: "snapshot", tone: "quiet" },
      { lab: "CHANGE", val: "2 YAML files · +24 / −17", tone: "ready" },
      { lab: "FINDINGS", val: "3 unresolved", tone: "danger" },
      { lab: "REASONING", val: "private · UNAVAILABLE", tone: "violet" },
    ],
  },
  "11": {
    id: "11",
    slug: "local-first-provider-not-configured",
    kicker: "DELIVERY · LOCAL-FIRST · PROVIDER NOT CONFIGURED",
    scope: "all",
    scopeNote: "local git available · github_read_authority absent",
    provider: "not configured",
    status: [
      { lab: "DATA", val: "snapshot", tone: "quiet" },
      { lab: "LOCAL GIT", val: "available", tone: "ready" },
      { lab: "PROVIDER", val: "not configured", tone: "violet" },
      { lab: "INBOX", val: "not_published", tone: "violet" },
    ],
  },
  "12": {
    id: "12",
    slug: "dense-128-agent-delivery",
    kicker: "DELIVERY · DENSE FAN-OUT · 128 AGENTS",
    scope: "all",
    scopeNote: "concept population · not a product ceiling",
    provider: "read-only",
    status: [
      { lab: "DATA", val: "snapshot", tone: "quiet" },
      { lab: "AGENTS", val: "128 total · 6 workstreams", tone: "live" },
      { lab: "EPISODES", val: "449 · 100% persisted", tone: "ready" },
      { lab: "PROVIDER", val: "read-only", tone: "violet" },
    ],
  },
};

export function parseDeliveryState(raw: string | null): DeliveryStateId {
  if (raw && (DELIVERY_STATE_IDS as readonly string[]).includes(raw)) return raw as DeliveryStateId;
  return "01";
}

export function parseSurface(): "brain" | "delivery" {
  return new URLSearchParams(window.location.search).get("surface") === "delivery" ? "delivery" : "brain";
}

export type PrRow = {
  id: string;
  repo: string;
  title: string;
  author: string;
  chg: string;
  cov: string;
  evid: string;
  ci: "passed" | "partial" | "failed" | "rate-limited" | "denied" | "unavailable";
  attention: AttentionSource[];
  fresh: string;
  freshness: "fresh" | "stale" | "unavailable";
  provider: HonestInbox | "ok";
};

export type DeliveryFixturePr = {
  id: string;
  repository: string;
  trackedHead: string | null;
  /** Recorded fixture activity time in UTC hours; drives shared graph axis placement. */
  lastActivity: number;
  changedFiles: number | null;
  title: string;
  agent: string;
  code: string | null;
  ci: string | null;
  review: string | null;
  nextAction: string;
  attention: ("ci-failed" | "diagnostics" | "review" | "stale" | "conflicting-edits")[];
  admission: "joined" | "not-joined";
  gap?: string;
  edges?: { to: string; kind: "shared commit / PR reference" | "CI job link" | "review reference" }[];
};

/** Authored delivery fixture: only `joined` records have a tracked indexed head. */
export const DELIVERY_FIXTURE_PRS: DeliveryFixturePr[] = [
  {
    id: "#12977",
    repository: "web-infra-dev/rspack",
    trackedHead: "ecd6feb8cec8",
    lastActivity: 10.4,
    changedFiles: 57,
    title: "feat(mf): add layer-aware shared module core",
    agent: "agent session / explicit",
    code: "57 changed files / indexed head",
    ci: "CI failed · integration tests",
    review: "review requested",
    nextAction: "Inspect failing integration check",
    attention: ["ci-failed", "review"],
    admission: "joined",
    edges: [{ to: "#707", kind: "shared commit / PR reference" }],
  },
  {
    id: "#707",
    repository: "ScriptedAlchemy/tracedecay",
    trackedHead: "d4e56a7f2c91",
    lastActivity: 14.2,
    changedFiles: 12,
    title: "feat: add ingest retry backoff",
    agent: "agent session / exact",
    code: "12 changed files / indexed head",
    ci: "CI passed",
    review: "changes requested",
    nextAction: "Resolve requested retry review",
    attention: ["review", "diagnostics"],
    admission: "joined",
  },
  {
    id: "#2314",
    repository: "module-federation/core",
    trackedHead: "a6c18d2f7510",
    lastActivity: 18.1,
    changedFiles: 187,
    title: "feat: remote retry policy",
    agent: "session unavailable",
    code: "187 changed files / indexed head",
    ci: "CI stale · observed 35m ago",
    review: null,
    nextAction: "Refresh CI evidence before review",
    attention: ["stale"],
    admission: "joined",
  },
  {
    id: "#8187",
    repository: "rslib/rslib",
    trackedHead: null,
    lastActivity: 19.3,
    changedFiles: null,
    title: "fix: tree-shake side effects",
    agent: "not joined",
    code: null,
    ci: "provider rate-limited",
    review: null,
    nextAction: "Index the tracked head to admit this PR",
    attention: ["conflicting-edits"],
    admission: "not-joined",
    gap: "not joined to indexed head",
  },
];

export const STATUS_FILTERS = [
  { label: "Unresolved", count: "1,842", tone: "danger" as StatusTone, pct: 100 },
  { label: "High risk", count: "326", tone: "amber" as StatusTone, pct: 18 },
  { label: "Weak evidence", count: "612", tone: "amber" as StatusTone, pct: 33 },
  { label: "Unreviewed", count: "871", tone: "violet" as StatusTone, pct: 47 },
  { label: "Stale", count: "284", tone: "quiet" as StatusTone, pct: 15 },
];

export const AGENT_FILTERS = [
  { label: "claude-code", count: "1,102" },
  { label: "gemini-cli", count: "892" },
  { label: "codegpt", count: "534" },
  { label: "human", count: "612" },
  { label: "other", count: "143" },
];

export const PROVIDER_OUTCOMES = [
  { label: "Passed", count: "2,107", tone: "ready" as StatusTone },
  { label: "Partial", count: "412", tone: "amber" as StatusTone },
  { label: "Failed", count: "196", tone: "danger" as StatusTone },
  { label: "Unknown", count: "318", tone: "quiet" as StatusTone },
];

export const RESPONSIBLE_AGENTS = [
  { label: "claude-code (primary)", prs: "28 PRs", pct: "61%" },
  { label: "gemini-cli (secondary)", prs: "14 PRs", pct: "29%" },
  { label: "codegpt", prs: "4 PRs", pct: "8%" },
  { label: "human", prs: "2 PRs", pct: "8%" },
];

export const REVIEW_FINDINGS = [
  { label: "High risk", count: "176", tone: "danger" as StatusTone },
  { label: "Medium risk", count: "312", tone: "amber" as StatusTone },
  { label: "Low risk", count: "684", tone: "live" as StatusTone },
  { label: "Informational", count: "—", tone: "quiet" as StatusTone },
];

export const CI_COVERAGE = [
  { label: "passed", count: "2,187", pct: 85, tone: "live" as StatusTone },
  { label: "partial", count: "412", pct: 13, tone: "amber" as StatusTone },
  { label: "failed", count: "196", pct: 2, tone: "danger" as StatusTone },
];

export const GLOBAL_PRS: PrRow[] = [
  { id: "#18337", repo: "rspack / rspack", title: "feat: persistent caching v2", author: "claude-code", chg: "512", cov: "78%", evid: "62%", ci: "passed", attention: ["unreviewed"], fresh: "4m", freshness: "fresh", provider: "ok" },
  { id: "#18315", repo: "rslib / rslib", title: "fix: tree-shake side effects", author: "gemini-cli", chg: "213", cov: "64%", evid: "48%", ci: "passed", attention: ["weak_evidence"], fresh: "12m", freshness: "fresh", provider: "ok" },
  { id: "#707", repo: "trace-decay / tracedecay", title: "feat: add ingest retry backoff", author: "claude-code", chg: "142", cov: "86%", evid: "71%", ci: "passed", attention: ["unsafe_patterns", "unreviewed"], fresh: "12h", freshness: "fresh", provider: "ok" },
  { id: "#5628", repo: "rspress / rspress", title: "docs: improve build guide", author: "codegpt", chg: "98", cov: "41%", evid: "33%", ci: "partial", attention: [], fresh: "18m", freshness: "fresh", provider: "ok" },
  { id: "#3241", repo: "lynx / lynx", title: "perf: reduce bundle size", author: "claude-code", chg: "324", cov: "73%", evid: "54%", ci: "passed", attention: ["unreviewed"], fresh: "22m", freshness: "fresh", provider: "ok" },
  { id: "#2314", repo: "module-federation / core", title: "feat: remote retry policy", author: "gemini-cli", chg: "187", cov: "60%", evid: "46%", ci: "partial", attention: ["weak_evidence"], fresh: "35m", freshness: "stale", provider: "stale" },
  { id: "#6421", repo: "rsbuild / rsbuild", title: "refactor: plugin container", author: "claude-code", chg: "271", cov: "68%", evid: "49%", ci: "passed", attention: [], fresh: "41m", freshness: "fresh", provider: "ok" },
  { id: "#183", repo: "trace-decay / scheduler", title: "fix: jitter on retries", author: "human", chg: "76", cov: "92%", evid: "80%", ci: "passed", attention: [], fresh: "55m", freshness: "fresh", provider: "ok" },
  { id: "#18291", repo: "rspack / rspack", title: "fix: chunk graph leak", author: "gemini-cli", chg: "143", cov: "51%", evid: "38%", ci: "failed", attention: ["test_risk", "unresolved"], fresh: "1h", freshness: "fresh", provider: "ok" },
  { id: "#8162", repo: "rslib / rslib", title: "feat: dts emit cache", author: "codegpt", chg: "201", cov: "63%", evid: "46%", ci: "passed", attention: ["unreviewed"], fresh: "1h", freshness: "fresh", provider: "ok" },
  { id: "#8187", repo: "rslib / rslib", title: "fix: tree-shake side effects", author: "human", chg: "76", cov: "—", evid: "—", ci: "rate-limited", attention: ["unresolved"], fresh: "—", freshness: "unavailable", provider: "rate-limited" },
  { id: "#681", repo: "trace-decay / tracedecay", title: "refactor: emit cache", author: "codegpt", chg: "201", cov: "44%", evid: "31%", ci: "denied", attention: ["contradictions"], fresh: "3d", freshness: "stale", provider: "denied" },
];

export type ClusterNode = {
  id: string;
  label: string;
  sub: string;
  color: string;
  x: number;
  y: number;
  r: number;
  prs: { id: string; x: number; y: number }[];
};

export const UMBRELLA = {
  id: "v2-release",
  title: "V2 code-intelligence release",
  kind: "UMBRELLA OUTCOME",
  projects: 12,
  prs: 48,
  agents: 6,
  files: "78,341",
  loc: "1.84M",
  ci: "94% CI coverage",
  review: "unreviewed 312 · unresolved 176",
  updated: "Updated 6h ago",
  objective:
    "Ship TraceDecay V2 code-intelligence platform with cross-repo ingestion, analysis, and delivery stability improvements.",
};

export const CLUSTERS: ClusterNode[] = [
  {
    id: "rspack",
    label: "Rspack",
    sub: "13 PRs",
    color: "#5ee7ff",
    x: 18,
    y: 38,
    r: 46,
    prs: [
      { id: "#18337", x: -18, y: -12 },
      { id: "#18315", x: 16, y: -8 },
      { id: "#18291", x: 2, y: 16 },
    ],
  },
  {
    id: "rspress",
    label: "Rspress",
    sub: "7 PRs",
    color: "#c084fc",
    x: 28,
    y: 62,
    r: 38,
    prs: [
      { id: "#5628", x: -12, y: 8 },
      { id: "#5612", x: 14, y: -6 },
    ],
  },
  {
    id: "lynx",
    label: "Lynx",
    sub: "6 PRs",
    color: "#67e8f9",
    x: 42,
    y: 46,
    r: 36,
    prs: [
      { id: "#3241", x: -10, y: -10 },
      { id: "#3228", x: 12, y: 10 },
    ],
  },
  {
    id: "rslib",
    label: "Rsbuild / Rslib",
    sub: "9 PRs",
    color: "#f0b429",
    x: 78,
    y: 32,
    r: 40,
    prs: [
      { id: "#8187", x: -14, y: -8 },
      { id: "#8162", x: 12, y: 4 },
      { id: "#8139", x: 4, y: 16 },
    ],
  },
  {
    id: "mf",
    label: "Module Federation",
    sub: "5 PRs",
    color: "#9be15d",
    x: 84,
    y: 58,
    r: 36,
    prs: [
      { id: "#2314", x: -10, y: -8 },
      { id: "#2299", x: 12, y: 8 },
    ],
  },
  {
    id: "td",
    label: "TraceDecay",
    sub: "8 PRs",
    color: "#5ee7ff",
    x: 58,
    y: 70,
    r: 42,
    prs: [
      { id: "#707", x: -16, y: 6 },
      { id: "#694", x: 4, y: -12 },
      { id: "#681", x: 14, y: 10 },
    ],
  },
  {
    id: "tooling",
    label: "Tooling & Infra",
    sub: "6 PRs",
    color: "#7dd3fc",
    x: 16,
    y: 84,
    r: 32,
    prs: [{ id: "+6", x: 0, y: 0 }],
  },
  {
    id: "unrelated",
    label: "Unrelated Activity",
    sub: "23 PRs",
    color: "#6b7784",
    x: 86,
    y: 84,
    r: 34,
    prs: [{ id: "+23", x: 0, y: 0 }],
  },
];

export type LaneEvent = {
  t: number;
  label: string;
  kind: "task" | "spawn" | "handoff" | "commit" | "test" | "review" | "ci" | "gap" | "fail" | "ghost";
  grade: EvidenceGrade;
  /** Wall-clock caption shown under the label (plate style). */
  time?: string;
  /** Gap/segment end position for interval events. */
  end?: number;
  /** Emphasized node (journey terminus). */
  big?: boolean;
  /** Vertical row offset in lane heights (ghost fan rows). */
  row?: number;
};

export type JourneyLane = {
  id: string;
  label: string;
  role: string;
  color: string;
  events: LaneEvent[];
  collapsed?: boolean;
  extra?: string;
  /** Density of unlabeled activity bursts drawn along the lane. */
  burst?: number;
  /** Suppress the unlabeled activity texture (honest-partial lanes). */
  quiet?: boolean;
};

export const JOURNEY_PHASES = [
  { t: 0.04, label: "Human Objective", time: "08:31" },
  { t: 0.14, label: "Planning", time: "09:21" },
  { t: 0.24, label: "Investigation", time: "10:36" },
  { t: 0.36, label: "Decisions", time: "11:52" },
  { t: 0.5, label: "Implementation", time: "13:15" },
  { t: 0.64, label: "Verification", time: "15:02" },
  { t: 0.76, label: "Review", time: "16:48" },
  { t: 0.88, label: "CI", time: "18:36" },
  { t: 0.97, label: "Release", time: "20:11" },
];

export const JOURNEY_LANES: JourneyLane[] = [
  {
    id: "human",
    label: "Human",
    role: "author",
    color: "#5ee7ff",
    burst: 40,
    events: [
      { t: 0.04, label: "objective", kind: "task", grade: "EXPLICIT" },
      { t: 0.36, label: "decision", kind: "task", grade: "EXPLICIT" },
      { t: 0.76, label: "review", kind: "review", grade: "EXACT" },
      { t: 0.97, label: "release", kind: "commit", grade: "EXACT" },
    ],
  },
  {
    id: "lynx",
    label: "Lynx",
    role: "planner",
    color: "#67e8f9",
    burst: 54,
    events: [
      { t: 0.14, label: "plan", kind: "task", grade: "EXPLICIT" },
      { t: 0.22, label: "spec", kind: "task", grade: "EXPLICIT" },
      { t: 0.3, label: "assign", kind: "handoff", grade: "EXACT" },
    ],
  },
  {
    id: "codegpt",
    label: "CodeGPT",
    role: "architect",
    color: "#9be15d",
    burst: 66,
    events: [
      { t: 0.24, label: "design", kind: "task", grade: "EXPLICIT" },
      { t: 0.4, label: "policy", kind: "commit", grade: "EXACT" },
      { t: 0.52, label: "handoff", kind: "handoff", grade: "EXACT" },
    ],
  },
  {
    id: "rslib",
    label: "rsbuild",
    role: "tools",
    color: "#c084fc",
    burst: 60,
    events: [
      { t: 0.28, label: "codegen", kind: "task", grade: "EXACT" },
      { t: 0.48, label: "build", kind: "commit", grade: "EXACT" },
      { t: 0.6, label: "test", kind: "test", grade: "EXACT" },
    ],
  },
  {
    id: "td",
    label: "TraceDecay",
    role: "analysis",
    color: "#5ee7ff",
    burst: 42,
    events: [
      { t: 0.42, label: "analyze", kind: "task", grade: "INFERRED" },
      { t: 0.58, label: "report", kind: "task", grade: "EXPLICIT" },
    ],
  },
  {
    id: "mf",
    label: "Module Fed.",
    role: "runtime",
    color: "#9be15d",
    burst: 38,
    events: [
      { t: 0.5, label: "validate", kind: "test", grade: "EXACT" },
      { t: 0.62, label: "simulate", kind: "test", grade: "EXACT" },
    ],
  },
  {
    id: "gha",
    label: "github-actions",
    role: "ci",
    color: "#f0b429",
    burst: 48,
    events: [
      { t: 0.7, label: "ci: start", kind: "ci", grade: "EXACT" },
      { t: 0.78, label: "ci: run", kind: "ci", grade: "EXACT" },
      { t: 0.84, label: "ci: test fail", kind: "fail", grade: "EXACT" },
      { t: 0.9, label: "retest", kind: "test", grade: "EXACT" },
      { t: 0.97, label: "merge", kind: "commit", grade: "EXACT" },
    ],
  },
];

export const CROSS_REPO_RAILS: JourneyLane[] = [
  {
    id: "rspack-rail",
    label: "Rspack",
    role: "#18337",
    color: "#38cfe8",
    burst: 48,
    events: [
      { t: 0.2, label: "open", kind: "commit", grade: "EXACT" },
      { t: 0.55, label: "ci", kind: "ci", grade: "EXACT" },
      { t: 0.88, label: "review", kind: "review", grade: "EXACT" },
    ],
  },
  {
    id: "rslib-rail",
    label: "Rsbuild / Rslib",
    role: "#8187",
    color: "#f0b429",
    burst: 40,
    events: [
      { t: 0.32, label: "open", kind: "commit", grade: "EXACT" },
      { t: 0.7, label: "rate-limited", kind: "gap", grade: "UNAVAILABLE", end: 0.82 },
    ],
  },
  {
    id: "rspress-rail",
    label: "Rspress",
    role: "#412",
    color: "#c084fc",
    burst: 38,
    events: [
      { t: 0.44, label: "open", kind: "commit", grade: "EXACT" },
      { t: 0.8, label: "stale", kind: "gap", grade: "STALE", end: 0.9 },
    ],
  },
];

/** Replay lanes (state 05) — labeled episodes with wall-clock captions, PR #18337. */
export const REPLAY_LANES: JourneyLane[] = [
  {
    id: "r-human",
    label: "Human",
    role: "author",
    color: "#5ee7ff",
    quiet: true,
    events: [
      { t: 0.1, label: "spawn", time: "09:14", kind: "spawn", grade: "EXACT" },
      { t: 0.17, label: "plan", time: "09:16", kind: "task", grade: "EXPLICIT" },
      { t: 0.44, label: "review", time: "11:02", kind: "review", grade: "EXACT" },
    ],
  },
  {
    id: "r-lynx",
    label: "Lynx",
    role: "planner",
    color: "#60a5fa",
    quiet: true,
    events: [
      { t: 0.16, label: "plan", time: "09:16", kind: "task", grade: "EXPLICIT" },
      { t: 0.24, label: "spec", time: "09:27", kind: "task", grade: "EXPLICIT" },
      { t: 0.32, label: "assign", time: "09:28", kind: "handoff", grade: "EXACT" },
    ],
  },
  {
    id: "r-rsbuild",
    label: "rsbuild",
    role: "tools",
    color: "#c084fc",
    quiet: true,
    events: [
      { t: 0.19, label: "codegen", time: "09:29", kind: "task", grade: "EXACT" },
      { t: 0.3, label: "build", time: "09:45", kind: "commit", grade: "EXACT" },
      { t: 0.47, label: "test", time: "11:48", kind: "test", grade: "EXACT" },
    ],
  },
  {
    id: "r-mf",
    label: "Module Fed.",
    role: "runtime",
    color: "#9be15d",
    quiet: true,
    events: [
      { t: 0.5, label: "validate", time: "11:50", kind: "test", grade: "EXACT" },
      { t: 0.56, label: "simulate", time: "12:03", kind: "test", grade: "EXACT" },
    ],
  },
  {
    id: "r-td",
    label: "TraceDecay",
    role: "analysis",
    color: "#5ee7ff",
    quiet: true,
    events: [
      { t: 0.53, label: "analyze", time: "12:05", kind: "task", grade: "INFERRED" },
      { t: 0.58, label: "report", time: "12:19", kind: "task", grade: "EXPLICIT" },
    ],
  },
  {
    id: "r-gha",
    label: "github-actions",
    role: "ci",
    color: "#f0b429",
    quiet: true,
    events: [
      { t: 0.5, label: "ci: start", time: "12:22", kind: "ci", grade: "EXACT" },
      { t: 0.57, label: "ci: run", time: "12:24", kind: "ci", grade: "EXACT" },
      { t: 0.63, label: "ci: test fail", time: "12:47", kind: "fail", grade: "EXACT" },
      { t: 0.71, label: "fix", kind: "ghost", grade: "AMBIGUOUS", row: -1 },
      { t: 0.79, label: "retest", kind: "ghost", grade: "AMBIGUOUS", row: -1 },
      { t: 0.87, label: "review", kind: "ghost", grade: "AMBIGUOUS", row: -1 },
      { t: 0.73, label: "revise", kind: "ghost", grade: "AMBIGUOUS" },
      { t: 0.8, label: "push", kind: "ghost", grade: "AMBIGUOUS" },
      { t: 0.88, label: "retest again", kind: "ghost", grade: "AMBIGUOUS" },
      { t: 0.95, label: "merge", kind: "ghost", grade: "AMBIGUOUS" },
    ],
  },
];

export const PARTIAL_LANES: JourneyLane[] = [
  {
    id: "human-p",
    label: "Human",
    role: "author",
    color: "#5ee7ff",
    quiet: true,
    events: [
      { t: 0.03, label: "task", time: "11:18", kind: "task", grade: "EXACT" },
      { t: 0.08, label: "session", time: "11:28", kind: "task", grade: "EXACT" },
      { t: 0.12, label: "private reasoning unavailable", time: "11:28 – 12:02", kind: "gap", grade: "UNAVAILABLE", end: 0.24 },
      { t: 0.28, label: "worktree", time: "14:03", kind: "commit", grade: "EXACT" },
      { t: 0.36, label: "commit", time: "15:12", kind: "commit", grade: "EXACT" },
      { t: 0.43, label: "code · 32 files", time: "16:02", kind: "commit", grade: "EXACT" },
      { t: 0.48, label: "no matching episode", time: "16:02 – 18:24", kind: "gap", grade: "AMBIGUOUS", end: 0.64 },
      { t: 0.82, label: "review", time: "20:03", kind: "review", grade: "EXACT" },
      { t: 0.88, label: "revision", time: "20:12", kind: "review", grade: "EXACT" },
      { t: 0.975, label: "delivery", time: "21:59", kind: "commit", grade: "EXACT", big: true },
    ],
  },
  {
    id: "gemini-p",
    label: "Gemini-CLI",
    role: "@investigator",
    color: "#f0b429",
    quiet: true,
    events: [
      { t: 0.06, label: "spawn", time: "12:15", kind: "spawn", grade: "EXACT" },
      { t: 0.13, label: "search codebase", time: "12:34", kind: "task", grade: "EXACT" },
      { t: 0.2, label: "agent attribution ambiguous", time: "12:34 – 14:02", kind: "gap", grade: "AMBIGUOUS", end: 0.36 },
      { t: 0.46, label: "provider review stale", time: "16:02 – 20:03", kind: "gap", grade: "STALE", end: 0.8 },
    ],
  },
  {
    id: "codegpt-p",
    label: "CodeGPT",
    role: "@architect",
    color: "#9be15d",
    quiet: true,
    events: [
      { t: 0.09, label: "spawn", time: "13:22", kind: "spawn", grade: "EXACT" },
      { t: 0.16, label: "retry policy", time: "13:22", kind: "task", grade: "EXPLICIT" },
      { t: 0.22, label: "spec", time: "13:47", kind: "task", grade: "EXPLICIT" },
      { t: 0.27, label: "handoff inferred (low confidence)", time: "13:47 – 15:12", kind: "gap", grade: "INFERRED", end: 0.4 },
      { t: 0.56, label: "test plan", time: "17:05", kind: "test", grade: "EXACT" },
      { t: 0.63, label: "tests · 31 passed", time: "17:42", kind: "test", grade: "EXACT" },
      { t: 0.7, label: "integration", time: "18:10", kind: "test", grade: "EXACT" },
    ],
  },
  {
    id: "reviewer-p",
    label: "Human",
    role: "@reviewer",
    color: "#c084fc",
    quiet: true,
    events: [
      { t: 0.03, label: "transcript unavailable", time: "11:18 – 20:03", kind: "gap", grade: "UNAVAILABLE", end: 0.78 },
      { t: 0.82, label: "review", time: "20:03", kind: "review", grade: "EXACT" },
      { t: 0.87, label: "comment", time: "20:18", kind: "review", grade: "EXACT" },
      { t: 0.93, label: "approval", time: "20:52", kind: "review", grade: "EXACT" },
    ],
  },
  {
    id: "ci-p",
    label: "CI",
    role: "denied",
    color: "#f07178",
    quiet: true,
    events: [
      { t: 0.18, label: "ci source denied", time: "13:11 – 15:42", kind: "gap", grade: "UNAVAILABLE", end: 0.42 },
      { t: 0.44, label: "ci", time: "15:42", kind: "ci", grade: "EXACT" },
      { t: 0.5, label: "build", time: "16:05", kind: "ci", grade: "EXACT" },
      { t: 0.56, label: "tests", time: "16:48", kind: "test", grade: "EXACT" },
      { t: 0.62, label: "merged", time: "17:21", kind: "commit", grade: "EXACT" },
      { t: 0.68, label: "provider stale", time: "17:21 – 21:59", kind: "gap", grade: "STALE", end: 0.97 },
    ],
  },
];

export const GAP_TYPES = [
  { label: "Transcript unavailable", count: 2, grade: "UNAVAILABLE" as EvidenceGrade },
  { label: "Private reasoning unavailable", count: 1, grade: "UNAVAILABLE" as EvidenceGrade },
  { label: "Agent attribution ambiguous", count: 1, grade: "AMBIGUOUS" as EvidenceGrade },
  { label: "Provider review stale", count: 2, grade: "STALE" as EvidenceGrade },
  { label: "Code: no matching episode", count: 1, grade: "AMBIGUOUS" as EvidenceGrade },
  { label: "Handoff inferred (named basis: shared worktree)", count: 1, grade: "INFERRED" as EvidenceGrade },
  { label: "CI source denied", count: 1, grade: "UNAVAILABLE" as EvidenceGrade },
];

export const LOCAL_REPOS = [
  { id: "infra-map", branch: "main", ahead: "+3 / -7", dirty: 7, commit: "2d ago  patel · infra-map sync", fresh: "2m ago", state: "FRESH" as const },
  { id: "neuronet", branch: "main", ahead: "+12 / -1", dirty: 12, commit: "6h ago  nguyen · refine intake", fresh: "3m ago", state: "FRESH" as const },
  { id: "probe-lattice", branch: "main", ahead: "+8 / -3", dirty: 4, commit: "1d ago  ortega · adjust parser", fresh: "4m ago", state: "FRESH" as const },
  { id: "signal-forge", branch: "main", ahead: "+1 / -8", dirty: 3, commit: "9h ago  lee · feature flags", fresh: "5m ago", state: "FRESH" as const },
  { id: "behavioral-graph", branch: "main", ahead: "+2 / -4", dirty: 5, commit: "1d ago  kim · graph update", fresh: "3m ago", state: "FRESH" as const },
  { id: "causality-lab", branch: "main", ahead: "+5 / -2", dirty: 6, commit: "1d ago  rossi · causality fix", fresh: "6m ago", state: "FRESH" as const },
  { id: "echo-archive", branch: "main", ahead: "+1 / -11", dirty: 8, commit: "6d ago  sung · archive sweep", fresh: "1h ago", state: "STALE" as const },
  { id: "vector-ops", branch: "main", ahead: "+5 / -0", dirty: 2, commit: "9d ago  wu · ops cleanup", fresh: "3m ago", state: "FRESH" as const },
  { id: "trace-core", branch: "main", ahead: "+1 / -2", dirty: 1, commit: "12d ago  diaz · refactor core", fresh: "12m ago", state: "FRESH" as const },
];

export const UNKNOWN_DIRS = [
  { id: "data-vault", path: "~/workspace/data-vault" },
  { id: "nl-models", path: "~/workspace/nl-models" },
  { id: "notebooks", path: "~/workspace/notebooks" },
  { id: "datasets", path: "~/workspace/datasets" },
  { id: "reports", path: "~/workspace/reports" },
];

export const PIPELINE = [
  { n: "1", name: "CHANGES", source: "Source: Local Git", state: "READY" as const },
  { n: "2", name: "COMMITS", source: "Source: Local Git", state: "READY" as const },
  { n: "3", name: "PULL REQUESTS", source: "Requires: github_read_authority", state: "NOT_PUBLISHED" as const },
  { n: "4", name: "REVIEWS", source: "Requires: github_read_authority", state: "NOT_PUBLISHED" as const },
  { n: "5", name: "CI CHECKS", source: "Provider identity absent", state: "UNAVAILABLE" as const },
  { n: "6", name: "FAILURE LOCALIZATION", source: "Requires CI and mapping", state: "NOT_CONFIGURED" as const },
  { n: "7", name: "RELEASES", source: "Requires: github_read_authority", state: "NOT_PUBLISHED" as const },
  { n: "8", name: "INDEX FRESHNESS", source: "8 fresh · 1 stale · 0 unavailable", state: "MIXED" as const },
];

export const WORKSTREAMS = [
  { id: "benchmark", label: "benchmark corpus", agents: 38, episodes: 142, color: "#5ee7ff", unresolved: 2, test_risk: 4 },
  { id: "guards", label: "workflow guards", agents: 27, episodes: 91, color: "#f0b429", unresolved: 4, test_risk: 2 },
  { id: "provider", label: "provider evidence", agents: 18, episodes: 64, color: "#c084fc", unresolved: 1, test_risk: 1 },
  { id: "review", label: "review fixes", agents: 16, episodes: 48, color: "#67e8f9", unresolved: 0, test_risk: 0 },
  { id: "docs", label: "docs + release", agents: 12, episodes: 36, color: "#9be15d", unresolved: 1, test_risk: 0 },
  { id: "integration", label: "integration", agents: 17, episodes: 68, color: "#7dd3fc", unresolved: 3, test_risk: 6 },
];

export const CHECK_MATRIX = [
  { wf: "ci.yml", job: "build", check: "Build (TypeScript)", status: "Success", observed: "12h ago", provider: "COMPLETE" as const },
  { wf: "ci.yml", job: "test", check: "Unit Tests", status: "Success", observed: "12h ago", provider: "COMPLETE" as const },
  { wf: "ci.yml", job: "test", check: "Integration Tests", status: "Failure", observed: "12h ago", provider: "COMPLETE" as const },
  { wf: "ci.yml", job: "lint", check: "Lint", status: "Success", observed: "12h ago", provider: "COMPLETE" as const },
  { wf: "ci.yml", job: "security", check: "Snyk Scan", status: "Skipped", observed: "12h ago", provider: "UNAVAILABLE" as const },
  { wf: "ci.yml", job: "coverage", check: "Code Coverage", status: "—", observed: "—", provider: "RATE-LIMITED" as const },
  { wf: "ci.yml", job: "release", check: "Publish Release", status: "—", observed: "—", provider: "DENIED" as const },
  { wf: "release.yml", job: "notes", check: "Release Notes", status: "—", observed: "—", provider: "NOT_PUBLISHED" as const },
];

export const REVIEW_THREADS = [
  {
    id: "R1234567890",
    state: "CURRENT",
    file: "src/ingest/retry.ts",
    line: 142,
    body: "Should shouldRetry accept attempt param? Passing attempt allows limiting retries without capturing outer scope.",
    age: "12h ago",
  },
  {
    id: "R1234567901",
    state: "CURRENT",
    file: "src/ingest/retry.ts",
    line: 158,
    body: "Prefer const for cfg after the first assignment.",
    age: "11h ago",
  },
  {
    id: "R1234567912",
    state: "OUTDATED",
    file: "src/ingest/retry.ts",
    line: 95,
    body: "Prefer const for cfg.",
    age: "1d ago",
  },
];

export const STORY_STEPS = [
  { n: "01", label: "OBJECTIVE", time: "08:31", state: "done" as const },
  { n: "02", label: "OBSERVATION", time: "09:21", state: "done" as const },
  { n: "03", label: "DECISION", time: "10:36", state: "done" as const },
  { n: "04", label: "IMPLEMENTATION", time: "13:15", state: "active" as const },
  { n: "05", label: "VERIFICATION", time: "15:02", state: "todo" as const },
  { n: "06", label: "FEEDBACK", time: "16:48", state: "todo" as const },
  { n: "07", label: "OUTCOME", time: "20:11", state: "todo" as const },
];

export const PR743_STEPS = [
  { n: "01", label: "USER REQUEST", time: "21:08:17", state: "done" as const },
  { n: "02", label: "DIRECTIVE", time: "21:09:02", state: "done" as const },
  { n: "03", label: "DELEGATION", time: "21:09:40", state: "done" as const },
  { n: "04", label: "PREREQUISITE", time: "21:12:18", state: "done" as const },
  { n: "05", label: "DECISION → CODE", time: "21:14:02", state: "active" as const },
  { n: "06", label: "RUN EVIDENCE", time: "21:16:40", state: "todo" as const },
  { n: "07", label: "REVIEW FINDINGS", time: "21:18:11", state: "todo" as const },
  { n: "08", label: "MERGE + MIRROR", time: "21:40:02", state: "todo" as const },
];

export const PR743_TRANSCRIPT = [
  { t: "21:08:17", who: "USER TRANSCRIPT", text: "Is there an experimental Hotpath Github Action?", grade: "EXACT" as EvidenceGrade },
  { t: "21:09:02", who: "DIRECTIVE", text: "Integrate full CI into #707 or a standing mergeable PR.", grade: "EXPLICIT" as EvidenceGrade },
  { t: "21:09:40", who: "DELEGATION", text: "Spawn daemon-free indexing benchmark agent.", grade: "EXACT" as EvidenceGrade },
  { t: "21:12:18", who: "OBSERVED ONLY", text: "Wasted API researcher spawned; evidence: no #743 hunks.", grade: "EXACT" as EvidenceGrade },
  { t: "21:14:08", who: "UNAVAILABLE", text: "Private chain-of-thought remains unavailable.", grade: "UNAVAILABLE" as EvidenceGrade },
  { t: "21:41:18", who: "ASSISTANT SUMMARY", text: "Blackbox recorded: master lacks benchmark binary; native PR trigger would break older Lines.", grade: "EXPLICIT" as EvidenceGrade },
  { t: "21:42:04", who: "EXACT PRODUCER", text: "Commit created; push 21:14:34; PR created.", grade: "EXACT" as EvidenceGrade },
  { t: "21:42:18", who: "CHECK RESULT", text: "PR #743 merges; same workflows already mirrored to #707 as 49fcb0e6.", grade: "EXACT" as EvidenceGrade },
];

export const PR743_FINDINGS = [
  {
    id: "F1",
    tag: "TRUST BOUNDARY",
    title: "Comment workflow uses an attacker-controlled artifact.",
    body: "PR comment can become a trust attacker-controlled artifact.",
  },
  {
    id: "F2",
    tag: "ENABLED COMPARISON",
    title: "Bounded sealed-date digests warn but still persist comparison.",
    body: "Comparison remains enabled after the warning path.",
  },
  {
    id: "F3",
    tag: "PIPE FAILURE",
    title: "Missing pipeline can hide timing failure through tee.",
    body: "A broken pipe can hide timing failure through tee.",
  },
];

/** Represented umbrella sample: identities already drawn in the concept, not provider records. */
export const UMBRELLA_SAMPLE = CLUSTERS.filter((cluster) => !["tooling", "unrelated"].includes(cluster.id));
export const UMBRELLA_SAMPLE_COUNT = UMBRELLA_SAMPLE.reduce((count, cluster) => count + cluster.prs.length, 0);
