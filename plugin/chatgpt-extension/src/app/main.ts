import { App, type McpUiHostContext } from "@modelcontextprotocol/ext-apps";
import type { CallToolResult } from "@modelcontextprotocol/sdk/types.js";
import {
  deepLinkPath,
  isViewState,
  parseDeepLink,
  type BoundedGraph,
  type DeepLinkTarget,
  type Failure,
  type FreshnessState,
  type ProjectRef,
  type Provenance,
  type Section,
  type SymbolSummary,
  type ViewState,
} from "../shared/view.js";
import "./styles.css";

type Screen =
  | { readonly phase: "connecting" }
  | { readonly phase: "loading"; readonly label: string; readonly previous: ViewState | null }
  | { readonly phase: "view"; readonly view: ViewState }
  | { readonly phase: "host_error"; readonly message: string };

type ToolName = "tracedecay_workspace" | "tracedecay_list_projects" | "tracedecay_search_code" | "tracedecay_inspect_symbol";

const root = document.getElementById("root");
if (root === null) throw new Error("app root element missing");
const mount: HTMLElement = root;

const app = new App(
  { name: "tracedecay-code-explorer", version: "0.0.0" },
  { availableDisplayModes: ["inline", "fullscreen"] },
);

let screen: Screen = { phase: "connecting" };
let knownProjects: readonly ProjectRef[] = [];
let lastDeepLink: string | null = null;

function setScreen(next: Screen): void {
  screen = next;
  render();
}

function currentView(): ViewState | null {
  if (screen.phase === "view") return screen.view;
  if (screen.phase === "loading") return screen.previous;
  return null;
}

function rememberProjects(view: ViewState): void {
  if (view.page === "projects" && view.projects.state === "ready") knownProjects = view.projects.data;
}

function adoptView(view: ViewState): void {
  rememberProjects(view);
  setScreen({ phase: "view", view });
  void publishModelContext(view);
}

function viewFromResult(result: CallToolResult): ViewState | null {
  return isViewState(result.structuredContent) ? result.structuredContent : null;
}

async function callTool(name: ToolName, args: Record<string, unknown>, label: string): Promise<void> {
  setScreen({ phase: "loading", label, previous: currentView() });
  try {
    const result = await app.callServerTool({ name, arguments: args });
    const view = viewFromResult(result);
    if (view === null) {
      setScreen({ phase: "host_error", message: `${name} returned no view payload` });
      return;
    }
    adoptView(view);
  } catch (error) {
    setScreen({ phase: "host_error", message: error instanceof Error ? error.message : String(error) });
  }
}

async function publishModelContext(view: ViewState): Promise<void> {
  const summary = modelContextFor(view);
  if (summary === null) return;
  try {
    await app.updateModelContext(summary);
  } catch {
    // Hosts without model-context support (or a closed bridge) reject the request; the view stays usable.
  }
}

function modelContextFor(view: ViewState): { content: { type: "text"; text: string }[]; structuredContent: Record<string, unknown> } | null {
  switch (view.page) {
    case "projects":
      return null;
    case "search":
      return {
        content: [{ type: "text", text: `TraceDecay search in ${view.project.label} for "${view.query}": ${describeSection(view.results, (results) => `${results.hits.length} hit(s), recall ${results.recall}`)}${provenanceLine(view.provenance)}` }],
        structuredContent: { page: "search", project_id: view.project.project_id, query: view.query, deep_link: deepLinkPath({ kind: "search", project_id: view.project.project_id, query: view.query }), provenance: view.provenance },
      };
    case "symbol":
      if (view.evidence === null) return null;
      return {
        content: [{ type: "text", text: view.evidence.markdown }],
        structuredContent: view.evidence.structured,
      };
    case "failure":
      return {
        content: [{ type: "text", text: `TraceDecay could not answer (${view.failure.kind}/${view.failure.code}): ${view.failure.message}` }],
        structuredContent: { page: "failure", failure: view.failure },
      };
  }
}

function provenanceLine(provenance: Provenance | null): string {
  if (provenance === null) return "";
  return ` [branch ${provenance.branch ?? "?"} commit ${provenance.commit ?? "?"} generation ${provenance.generation ?? "?"} freshness ${provenance.freshness.state}]`;
}

function describeSection<T>(section: Section<T>, ready: (data: T) => string): string {
  switch (section.state) {
    case "ready":
      return ready(section.data);
    case "empty":
      return section.message;
    case "failed":
      return `${section.failure.kind}: ${section.failure.message}`;
  }
}

// Navigation -----------------------------------------------------------------

function openWorkspace(): void {
  void callTool("tracedecay_workspace", {}, "Loading registered projects…");
}

function openProject(project: ProjectRef): void {
  adoptView({ page: "search", project, query: "", results: { state: "empty", message: "Type a symbol name or phrase to search this project." }, provenance: null });
}

function runSearch(project_id: string, query: string): void {
  void callTool("tracedecay_search_code", { project_id, query }, `Searching for "${query}"…`);
}

function openSymbol(project_id: string, node_id: string): void {
  void callTool("tracedecay_inspect_symbol", { project_id, node_id }, "Loading symbol…");
}

function followDeepLink(target: DeepLinkTarget): void {
  switch (target.kind) {
    case "projects":
      openWorkspace();
      return;
    case "project": {
      const known = knownProjects.find((project) => project.project_id === target.project_id);
      if (known !== undefined) openProject(known);
      else runSearch(target.project_id, "");
      return;
    }
    case "search":
      runSearch(target.project_id, target.query);
      return;
    case "symbol":
      openSymbol(target.project_id, target.node_id);
      return;
  }
}

function deepLinkFromHost(context: Partial<McpUiHostContext>): string | null {
  const raw: unknown = Reflect.get(context, "openai/deepLink");
  if (typeof raw === "string") return raw;
  if (typeof raw === "object" && raw !== null && "url" in raw && typeof raw.url === "string") return raw.url;
  return null;
}

function applyHostContext(context: Partial<McpUiHostContext>): void {
  const link = deepLinkFromHost(context);
  if (link === null || link === lastDeepLink) return;
  lastDeepLink = link;
  const target = parseDeepLink(link);
  if (target !== null) followDeepLink(target);
}

// Rendering ------------------------------------------------------------------

function el<K extends keyof HTMLElementTagNameMap>(tag: K, attrs: Record<string, string> = {}, children: readonly (Node | string)[] = []): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  for (const [key, value] of Object.entries(attrs)) node.setAttribute(key, value);
  node.append(...children);
  return node;
}

function button(label: string, onClick: () => void, attrs: Record<string, string> = {}): HTMLButtonElement {
  const node = el("button", { type: "button", ...attrs }, [label]);
  node.addEventListener("click", onClick);
  return node;
}

function badge(text: string, tone: "ok" | "warn" | "bad" | "neutral" = "neutral", testid?: string): HTMLSpanElement {
  const attrs: Record<string, string> = { class: tone === "neutral" ? "badge" : `badge ${tone}` };
  if (testid !== undefined) attrs["data-testid"] = testid;
  return el("span", attrs, [text]);
}

function failureTone(failure: Failure): "warn" | "bad" {
  return failure.kind === "stale" || failure.kind === "unavailable" ? "warn" : "bad";
}

function freshnessTone(state: FreshnessState): "ok" | "warn" | "bad" {
  switch (state) {
    case "current":
    case "fresh":
      return "ok";
    case "stale":
    case "possibly_stale":
    case "warming":
    case "restoring":
      return "warn";
    case "parked":
    case "unavailable":
      return "bad";
  }
}

function notice(text: string, tone: "neutral" | "warn" | "bad", testid: string): HTMLElement {
  return el("p", { class: tone === "neutral" ? "notice" : `notice ${tone}`, "data-testid": testid }, [text]);
}

function failureNotice(failure: Failure, testid: string): HTMLElement {
  const node = notice(`${failure.message} (${failure.code})`, failureTone(failure), testid);
  node.prepend(badge(failure.kind, failureTone(failure), `${testid}-kind`), " ");
  return node;
}

function sectionBody<T>(section: Section<T>, testid: string, ready: (data: T) => Node): Node {
  switch (section.state) {
    case "ready":
      return ready(section.data);
    case "empty":
      return notice(section.message, "neutral", `${testid}-empty`);
    case "failed":
      return failureNotice(section.failure, `${testid}-failed`);
  }
}

function panel(title: string, testid: string, children: readonly Node[], extras: readonly Node[] = []): HTMLElement {
  return el("section", { class: "panel", "data-testid": testid }, [el("h2", {}, [title, ...extras]), ...children]);
}

function symbolLink(project_id: string, symbol: SymbolSummary): HTMLButtonElement {
  return button(symbol.qualified_name || symbol.name, () => openSymbol(project_id, symbol.node_id), { class: "mono", "data-testid": "symbol-link", "data-node-id": symbol.node_id });
}

function location(symbol: SymbolSummary): HTMLSpanElement {
  const line = symbol.start_line === null ? "" : `:${symbol.start_line}`;
  return el("span", { class: "loc" }, [`${symbol.file}${line}`]);
}

function renderHeader(view: ViewState | null): HTMLElement {
  const crumbs = el("nav", { class: "crumbs", "aria-label": "Breadcrumb" });
  crumbs.append(button("Projects", openWorkspace, { "data-testid": "crumb-projects" }));
  const project = view !== null && view.page !== "projects" ? view.project : null;
  if (project !== null) {
    crumbs.append("/", button(project.label, () => openProject(project), { "data-testid": "crumb-project" }));
  }
  if (view?.page === "search" && view.query !== "") crumbs.append("/", el("span", {}, [`"${view.query}"`]));
  if (view?.page === "symbol" && view.symbol.state === "ready") crumbs.append("/", el("span", { class: "mono" }, [view.symbol.data.name]));

  const bar = el("header", { class: "bar" }, [el("h1", {}, ["TraceDecay"]), crumbs, el("span", { class: "spacer" })]);
  const displayMode = app.getHostContext()?.displayMode;
  if (displayMode !== undefined) {
    const next = displayMode === "fullscreen" ? "inline" : "fullscreen";
    bar.append(button(next === "fullscreen" ? "Expand" : "Collapse", () => void app.requestDisplayMode({ mode: next }), { "data-testid": "toggle-display-mode" }));
  }
  return bar;
}

function renderProjects(view: Extract<ViewState, { page: "projects" }>): Node[] {
  const daemon =
    view.daemon.state === "connected"
      ? el("p", { class: "notice", "data-testid": "daemon-state" }, [badge("connected", "ok", "daemon-badge"), ` tracedecay ${view.daemon.version} (pid ${view.daemon.pid}) · profile `, el("code", {}, [view.daemon.profile_root])])
      : failureNotice(view.daemon.failure, "daemon-state");
  const list = sectionBody(view.projects, "projects", (projects) =>
    el(
      "ul",
      { class: "list" },
      projects.map((project) =>
        el("li", { "data-testid": "project-row" }, [
          button(project.label, () => openProject(project), { class: "primary", "data-testid": "project-open", "data-project-id": project.project_id }),
          el("span", { class: "loc" }, [project.project_root]),
          project.head_branch === null ? "" : badge(project.head_branch),
        ]),
      ),
    ),
  );
  return [daemon, panel("Authorized projects", "projects-panel", [list, el("p", { class: "notice" }, ["Only projects registered in this TraceDecay profile (via `tracedecay init`) can be explored. Nothing here writes to your code."])])];
}

function renderSearchForm(project: ProjectRef, query: string): HTMLFormElement {
  const input = el("input", { type: "search", name: "q", placeholder: `Search ${project.label}…`, value: query, "data-testid": "search-input", "aria-label": "Search query" });
  const submit = (): void => {
    const value = input.value.trim();
    if (value.length > 0) runSearch(project.project_id, value);
  };
  // Sandboxed app frames may lack `allow-forms`, which suppresses the submit
  // event entirely, so the search runs from the button and Enter key directly.
  const form = el("form", { class: "search", "data-testid": "search-form" }, [input, button("Search", submit, { class: "primary", "data-testid": "search-submit" })]);
  input.addEventListener("keydown", (event) => {
    if (event.key === "Enter") {
      event.preventDefault();
      submit();
    }
  });
  form.addEventListener("submit", (event) => event.preventDefault());
  return form;
}

function renderProvenance(provenance: Provenance | null, link: DeepLinkTarget): HTMLElement {
  if (provenance === null) {
    return panel("Provenance", "provenance", [notice("Provenance is reported once the project answers a request.", "neutral", "provenance-pending")]);
  }
  const rows: [string, Node | string][] = [
    ["project", el("code", {}, [provenance.project_id])],
    ["worktree", el("code", {}, [provenance.worktree_id ?? provenance.project_root])],
    ["repository", el("code", {}, [provenance.repository_id ?? "—"])],
    ["ref", provenance.reference ?? provenance.branch ?? "—"],
    ["commit", el("code", { "data-testid": "provenance-commit" }, [provenance.commit ?? "—"])],
    ["generation", el("code", {}, [provenance.generation ?? "—"])],
    ["coverage", el("span", {}, [badge(provenance.coverage.recall, provenance.coverage.recall === "full" ? "ok" : "warn", "coverage-badge"), provenance.coverage.detail === null ? "" : ` ${provenance.coverage.detail}`])],
    ["daemon", `${provenance.authority.daemon_version} · pid ${provenance.authority.daemon_pid}`],
    ["deep link", el("code", { "data-testid": "deep-link" }, [deepLinkPath(link)])],
  ];
  const dl = el("dl", { class: "prov" }, rows.flatMap(([k, v]) => [el("dt", {}, [k]), el("dd", {}, [v])]));
  const freshness = badge(provenance.freshness.state, freshnessTone(provenance.freshness.state), "freshness-badge");
  const children: Node[] = [dl];
  if (provenance.freshness.detail !== null) children.unshift(notice(provenance.freshness.detail, freshnessTone(provenance.freshness.state) === "ok" ? "neutral" : "warn", "freshness-detail"));
  return panel("Provenance", "provenance", children, [freshness]);
}

function renderSearch(view: Extract<ViewState, { page: "search" }>): Node[] {
  const results = sectionBody(view.results, "results", (data) => {
    const list = el(
      "ul",
      { class: "list" },
      data.hits.map((hit) => el("li", { "data-testid": "search-hit" }, [badge(hit.kind), symbolLink(view.project.project_id, hit), location(hit)])),
    );
    const extras: Node[] = [list];
    if (data.recall === "partial") extras.push(notice("Partial recall: the index is still converging, so some matches may be missing.", "warn", "results-partial"));
    if (data.truncated) extras.push(notice("Results were truncated to the first page.", "neutral", "results-truncated"));
    return el("div", {}, extras);
  });
  return [
    renderSearchForm(view.project, view.query),
    panel(view.query === "" ? "Search" : `Results for "${view.query}"`, "results-panel", [results]),
    renderProvenance(view.provenance, { kind: "search", project_id: view.project.project_id, query: view.query }),
  ];
}

function renderGraph(project_id: string, graph: BoundedGraph): Node {
  const callers = graph.nodes.filter((node) => node.role === "caller");
  const callees = graph.nodes.filter((node) => node.role === "callee");
  const focus = graph.nodes.find((node) => node.role === "focus");
  const rows = Math.max(callers.length, callees.length, 1);
  const width = 640;
  const rowHeight = 28;
  const height = rows * rowHeight + 20;
  const svgNs = "http://www.w3.org/2000/svg";
  const svg = document.createElementNS(svgNs, "svg");
  svg.setAttribute("class", "graph");
  svg.setAttribute("viewBox", `0 0 ${width} ${height}`);
  svg.setAttribute("role", "img");
  svg.setAttribute("aria-label", "Bounded caller/callee graph");
  svg.dataset["testid"] = "graph";
  const columns = { caller: 10, focus: width / 2 - 100, callee: width - 210 } as const;
  const positions = new Map<string, { x: number; y: number }>();
  const place = (node: BoundedGraph["nodes"][number], index: number, column: keyof typeof columns): void => {
    const x = columns[column];
    const y = column === "focus" ? height / 2 - 10 : 10 + index * rowHeight;
    positions.set(node.node_id, { x, y });
    const g = document.createElementNS(svgNs, "g");
    g.setAttribute("class", `node ${node.role}`);
    g.dataset["testid"] = "graph-node";
    g.dataset["nodeId"] = node.node_id;
    const rect = document.createElementNS(svgNs, "rect");
    rect.setAttribute("x", String(x));
    rect.setAttribute("y", String(y));
    rect.setAttribute("width", "200");
    rect.setAttribute("height", "20");
    rect.setAttribute("rx", "4");
    const text = document.createElementNS(svgNs, "text");
    text.setAttribute("x", String(x + 6));
    text.setAttribute("y", String(y + 14));
    text.textContent = node.name.length > 28 ? `${node.name.slice(0, 27)}…` : node.name;
    const title = document.createElementNS(svgNs, "title");
    title.textContent = `${node.qualified_name} (${node.file})`;
    g.append(title, rect, text);
    if (node.role !== "focus") g.addEventListener("click", () => openSymbol(project_id, node.node_id));
    svg.append(g);
  };
  callers.forEach((node, index) => place(node, index, "caller"));
  callees.forEach((node, index) => place(node, index, "callee"));
  if (focus !== undefined) place(focus, 0, "focus");
  for (const edge of graph.edges) {
    const from = positions.get(edge.from);
    const to = positions.get(edge.to);
    if (from === undefined || to === undefined) continue;
    const line = document.createElementNS(svgNs, "path");
    line.setAttribute("class", "edge");
    line.setAttribute("d", `M ${from.x + 200} ${from.y + 10} C ${from.x + 240} ${from.y + 10}, ${to.x - 40} ${to.y + 10}, ${to.x} ${to.y + 10}`);
    svg.prepend(line);
  }
  const wrapper = el("div", {}, [svg]);
  if (graph.truncated) wrapper.append(notice(`Graph bounded to depth ${graph.max_depth}; more relations exist than shown.`, "neutral", "graph-truncated"));
  return wrapper;
}

function renderSymbol(view: Extract<ViewState, { page: "symbol" }>): Node[] {
  const project_id = view.project.project_id;
  const detail = sectionBody(view.symbol, "symbol", (symbol) => {
    const children: Node[] = [
      el("p", {}, [badge(symbol.kind), " ", el("code", { "data-testid": "symbol-qualified-name" }, [symbol.qualified_name]), " ", location(symbol)]),
    ];
    if (symbol.signature !== null) children.push(el("pre", { class: "mono" }, [symbol.signature]));
    if (symbol.complexity !== null) children.push(el("p", { class: "notice" }, [`Cyclomatic complexity ${symbol.complexity}`]));
    if (symbol.unavailable_fields.length > 0) children.push(notice(`Not available for this symbol: ${symbol.unavailable_fields.join(", ")}`, "neutral", "symbol-unavailable-fields"));
    return el("div", {}, children);
  });
  const relations = (section: Extract<ViewState, { page: "symbol" }>["callers"], testid: string): Node =>
    sectionBody(section, testid, (items) =>
      el(
        "ul",
        { class: "list" },
        items.map((relation) => el("li", { "data-testid": `${testid}-row` }, [badge(relation.edge_kind), symbolLink(project_id, relation.symbol), location(relation.symbol), relation.dispatch_via_trait ? badge("via trait") : ""])),
      ),
    );
  const impact = sectionBody(view.impact, "impact", (report) => {
    const list = el(
      "ul",
      { class: "list" },
      report.nodes.map((node) => el("li", { "data-testid": "impact-row" }, [badge(`depth ${node.depth}`), button(node.name, () => openSymbol(project_id, node.node_id), { class: "mono" }), el("span", { class: "loc" }, [`${node.file}:${node.line}`])])),
    );
    const extras: Node[] = [list];
    extras.push(report.complete ? notice(`Complete within depth ${report.max_depth}.`, "neutral", "impact-complete") : notice(`Partial: traversal stopped at depth ${report.max_depth}.`, "warn", "impact-partial"));
    return el("div", {}, extras);
  });
  const graph = sectionBody(view.graph, "graph", (data) => renderGraph(project_id, data));
  const actions = el("div", { class: "actions" });
  const evidence = view.evidence;
  if (evidence !== null) {
    actions.append(
      button("Send evidence to chat", () => void app.sendMessage({ role: "user", content: [{ type: "text", text: evidence.markdown }] }), { class: "primary", "data-testid": "send-evidence" }),
      button("Re-share with model", () => void publishModelContext(view), { "data-testid": "share-context" }),
    );
  }
  return [
    renderSearchForm(view.project, ""),
    panel("Symbol", "symbol-panel", [detail, actions]),
    panel("Callers", "callers-panel", [relations(view.callers, "callers")]),
    panel("Callees", "callees-panel", [relations(view.callees, "callees")]),
    panel("Impact", "impact-panel", [impact]),
    panel("Graph", "graph-panel", [graph]),
    renderProvenance(view.provenance, view.symbol.state === "ready" ? { kind: "symbol", project_id, node_id: view.symbol.data.node_id } : { kind: "project", project_id }),
  ];
}

function renderFailure(view: Extract<ViewState, { page: "failure" }>): Node[] {
  const children: Node[] = [failureNotice(view.failure, "page-failure")];
  if (view.failure.kind === "denied") children.push(notice("Only projects registered in this TraceDecay profile can be explored. Run `tracedecay init` in the repository and reopen the workspace.", "neutral", "denied-help"));
  if (view.failure.kind === "disconnected") children.push(notice("Start the daemon with `tracedecay daemon run` (or any tracedecay command) and retry.", "neutral", "disconnected-help"));
  children.push(el("div", { class: "actions" }, [button("Back to projects", openWorkspace, { class: "primary", "data-testid": "back-to-projects" })]));
  if (view.project !== null) children.unshift(renderSearchForm(view.project, ""));
  return [panel("Request failed", "failure-panel", children)];
}

function renderBody(): Node[] {
  switch (screen.phase) {
    case "connecting":
      return [el("p", { class: "loading", "data-testid": "connecting" }, ["Connecting to ChatGPT…"])];
    case "loading":
      return [el("p", { class: "loading", "data-testid": "loading" }, [screen.label])];
    case "host_error":
      return [failureNotice({ kind: "protocol", code: "host_call_failed", message: screen.message }, "host-error"), el("div", { class: "actions" }, [button("Back to projects", openWorkspace, { class: "primary", "data-testid": "back-to-projects" })])];
    case "view":
      switch (screen.view.page) {
        case "projects":
          return renderProjects(screen.view);
        case "search":
          return renderSearch(screen.view);
        case "symbol":
          return renderSymbol(screen.view);
        case "failure":
          return renderFailure(screen.view);
      }
  }
}

function render(): void {
  mount.replaceChildren(renderHeader(currentView()), ...renderBody());
  mount.dataset["phase"] = screen.phase;
  mount.dataset["page"] = currentView()?.page ?? "";
}

// Host wiring ----------------------------------------------------------------

app.ontoolinput = (params) => {
  setScreen({ phase: "loading", label: `Loading ${params.arguments?.["query"] !== undefined ? `search for "${String(params.arguments["query"])}"` : "from TraceDecay"}…`, previous: currentView() });
};
app.ontoolresult = (result) => {
  const view = viewFromResult(result);
  if (view === null) setScreen({ phase: "host_error", message: "The tool result carried no TraceDecay view payload." });
  else adoptView(view);
};
app.onhostcontextchanged = (context) => {
  applyHostContext(context);
  render();
};

render();
app
  .connect()
  .then(() => {
    const context = app.getHostContext();
    if (context !== undefined) applyHostContext(context);
    if (screen.phase === "connecting") setScreen({ phase: "loading", label: "Waiting for TraceDecay…", previous: null });
  })
  .catch((error: unknown) => setScreen({ phase: "host_error", message: error instanceof Error ? error.message : String(error) }));
