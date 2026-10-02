import type { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { ResourceTemplate } from "@modelcontextprotocol/sdk/server/mcp.js";
import type { CallToolResult } from "@modelcontextprotocol/sdk/types.js";
import { OpenAIExtensions, type OpenAIMentionItem } from "@openai/mcp-extensions/server";
import { z } from "zod";
import { deepLinkPath, type ViewState } from "../shared/view.js";
import type { DaemonBridge } from "./bridge.js";
import { failureView, projectsView, searchView, symbolView, viewText } from "./view-model.js";

export const UI_RESOURCE_URI = "ui://tracedecay/code-explorer";
export const SYMBOL_RESOURCE_TEMPLATE = "tracedecay://projects/{project_id}/symbols/{node_id}";

const MENTION_PROJECT_LIMIT = 4;
const MENTION_ITEM_LIMIT = 8;

const readOnly = { readOnlyHint: true, destructiveHint: false, openWorldHint: false, idempotentHint: true };

export type RegisterOptions = {
  readonly server: McpServer;
  readonly bridge: DaemonBridge;
  readonly html: string;
  readonly iconSvg: string;
};

function toolResult(view: ViewState): CallToolResult {
  return { content: [{ type: "text", text: viewText(view) }], structuredContent: view };
}

export function registerTraceDecayExtension({ server, bridge, html, iconSvg }: RegisterOptions): void {
  const icon = { src: `data:image/svg+xml,${encodeURIComponent(iconSvg)}`, mimeType: "image/svg+xml", sizes: ["any"] };
  const ui = (entrypoints: readonly unknown[] = []) => ({
    ui: { resourceUri: UI_RESOURCE_URI },
    "openai/ui": { entrypoints },
    "openai/iconStyle": "monochrome",
  });
  const appOnly = { ui: { visibility: ["app"] } };

  const projectId = z.string().min(1).describe("Registered TraceDecay project id, as returned by tracedecay_list_projects");

  server.registerTool(
    "tracedecay_workspace",
    {
      title: "TraceDecay",
      description: "Open the TraceDecay code explorer: pick an authorized project, search code, inspect symbols and their graph.",
      inputSchema: z.object({}),
      annotations: readOnly,
      _meta: ui([{ type: "global" }]),
    },
    async (_args, extra) => toolResult(await projectsView(bridge, extra.signal)),
  );

  server.registerTool(
    "tracedecay_thread_panel",
    {
      title: "TraceDecay",
      description: "Open the TraceDecay code explorer beside this conversation.",
      inputSchema: z.object({}),
      annotations: readOnly,
      _meta: ui([{ type: "thread" }]),
    },
    async (_args, extra) => toolResult(await projectsView(bridge, extra.signal)),
  );

  server.registerTool(
    "tracedecay_list_projects",
    {
      title: "List TraceDecay projects",
      description: "List the projects registered in the local TraceDecay profile. Only these projects can be explored.",
      inputSchema: z.object({}),
      annotations: readOnly,
      _meta: appOnly,
    },
    async (_args, extra) => toolResult(await projectsView(bridge, extra.signal)),
  );

  server.registerTool(
    "tracedecay_search_code",
    {
      title: "Search TraceDecay code",
      description:
        "Search symbols in a registered TraceDecay project. Returns node ids usable with tracedecay_inspect_symbol plus the served code generation and freshness.",
      inputSchema: z.object({ project_id: projectId, query: z.string().describe("Symbol or free-text query") }),
      annotations: readOnly,
      _meta: ui(),
    },
    async ({ project_id, query }, extra) => {
      try {
        return toolResult(await searchView(bridge, project_id, query, extra.signal));
      } catch (error) {
        return toolResult(await failureView(bridge, project_id, error));
      }
    },
  );

  server.registerTool(
    "tracedecay_inspect_symbol",
    {
      title: "Inspect TraceDecay symbol",
      description:
        "Inspect one symbol by node id: definition, direct callers and callees, dependent impact, and separately reported project status. Relations carry their served generation; symbol and impact reads do not report snapshot identity.",
      inputSchema: z.object({ project_id: projectId, node_id: z.string().min(1).describe("Symbol node id from a search result") }),
      annotations: readOnly,
      _meta: ui(),
    },
    async ({ project_id, node_id }, extra) => {
      try {
        return toolResult(await symbolView(bridge, project_id, node_id, extra.signal));
      } catch (error) {
        return toolResult(await failureView(bridge, project_id, error));
      }
    },
  );

  const extensions = new OpenAIExtensions(server);
  extensions.mentions.setHandler(async ({ query }, extra) => {
    if (query.trim().length === 0) return { items: [] };
    const projects = (await bridge.listProjects(extra.signal)).slice(0, MENTION_PROJECT_LIMIT);
    const perProject = await Promise.allSettled(
      projects.map(async (project) => {
        const result = await bridge.symbolSearch(project.project_id, query, extra.signal);
        return result.items.slice(0, MENTION_ITEM_LIMIT).map(
          (item): OpenAIMentionItem => ({
            type: "resource_link",
            uri: symbolResourceUri(project.project_id, item.node_id),
            name: item.qualified_name,
            title: `${item.kind} ${item.name}`,
            description: `${project.label} · ${item.file}:${item.line}`,
            mimeType: "text/markdown",
          }),
        );
      }),
    );
    const items: OpenAIMentionItem[] = [];
    for (const outcome of perProject) if (outcome.status === "fulfilled") items.push(...outcome.value);
    return { items };
  });

  server.registerResource(
    "tracedecay-symbol",
    new ResourceTemplate(SYMBOL_RESOURCE_TEMPLATE, { list: undefined }),
    { title: "TraceDecay symbol evidence", mimeType: "text/markdown" },
    async (uri, variables, extra) => {
      const project_id = single(variables.project_id);
      const node_id = single(variables.node_id);
      const view = await symbolView(bridge, project_id, node_id, extra.signal);
      return {
        contents: [
          {
            uri: uri.href,
            mimeType: "text/markdown",
            text: viewText(view),
            _meta: { "openai/deepLink": deepLinkPath({ kind: "symbol", project_id, node_id }) },
          },
        ],
      };
    },
  );

  server.registerResource(
    "tracedecay-code-explorer",
    UI_RESOURCE_URI,
    { title: "TraceDecay code explorer", mimeType: "text/html;profile=mcp-app" },
    async () => ({
      contents: [
        {
          uri: UI_RESOURCE_URI,
          mimeType: "text/html;profile=mcp-app",
          text: html,
          _meta: {
            "openai/ui": { preferredDisplayMode: "inline", availableDisplayModes: ["inline", "fullscreen"] },
            ui: { prefersBorder: true, csp: { connectDomains: [], resourceDomains: [] } },
          },
        },
      ],
    }),
  );
}

export function symbolResourceUri(projectId: string, nodeId: string): string {
  return `tracedecay://projects/${encodeURIComponent(projectId)}/symbols/${encodeURIComponent(nodeId)}`;
}

function single(value: string | string[] | undefined): string {
  const first = Array.isArray(value) ? value[0] : value;
  if (first === undefined || first.length === 0) throw new Error("resource URI is missing a template variable");
  return decodeURIComponent(first);
}
