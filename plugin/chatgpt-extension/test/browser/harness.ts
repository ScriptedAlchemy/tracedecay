/**
 * Minimal MCP Apps host built on the maintained `AppBridge`. It stands in for
 * ChatGPT so the real app bundle can be driven in a browser: tool calls are
 * forwarded to a Node-side MCP client (exposed by Playwright) that talks to
 * the packaged extension server and a live TraceDecay daemon.
 */
import { AppBridge, PostMessageTransport } from "@modelcontextprotocol/ext-apps/app-bridge";
import type { CallToolResult } from "@modelcontextprotocol/sdk/types.js";

type HostRecord = {
  readonly modelContext: unknown[];
  readonly messages: unknown[];
  readonly displayModes: string[];
  readonly links: string[];
};

declare global {
  interface Window {
    __mcpCall: (name: string, args: Record<string, unknown>) => Promise<CallToolResult>;
    __host: {
      record: HostRecord;
      sendToolResult: (result: CallToolResult) => Promise<void>;
      sendDeepLink: (url: string) => Promise<void> | void;
      ready: Promise<void>;
    };
  }
}

const iframe = document.getElementById("app");
const log = document.getElementById("log");
if (!(iframe instanceof HTMLIFrameElement) || log === null) throw new Error("harness markup missing");

const record: HostRecord = { modelContext: [], messages: [], displayModes: [], links: [] };
const note = (line: string): void => {
  log.textContent += `${line}\n`;
};

const hostContext: Record<string, unknown> = {
  displayMode: "inline",
  availableDisplayModes: ["inline", "fullscreen"],
  theme: "light",
  locale: "en-US",
  platform: "web",
};
const appHtml = new URLSearchParams(window.location.search).get("app") ?? "/app.html";

const bridge = new AppBridge(
  null,
  { name: "tracedecay-test-host", version: "0.0.0" },
  { serverTools: {}, updateModelContext: { text: {} }, message: { text: {} }, openLinks: {} },
  { hostContext },
);

bridge.oncalltool = async (params) => {
  note(`tools/call ${params.name} ${JSON.stringify(params.arguments ?? {})}`);
  return window.__mcpCall(params.name, params.arguments ?? {});
};
bridge.onupdatemodelcontext = async (params) => {
  record.modelContext.push(params);
  note(`ui/update-model-context ${(params.content ?? []).length} block(s)`);
  return {};
};
bridge.onmessage = async (params) => {
  record.messages.push(params);
  note(`ui/message role=${params.role}`);
  return {};
};
bridge.onopenlink = async (params) => {
  record.links.push(params.url);
  return {};
};
bridge.onrequestdisplaymode = async (params) => {
  record.displayModes.push(params.mode);
  hostContext["displayMode"] = params.mode;
  await bridge.sendHostContextChange({ displayMode: params.mode });
  return { mode: params.mode };
};

let resolveReady: () => void = () => undefined;
const ready = new Promise<void>((resolve) => {
  resolveReady = resolve;
});
bridge.oninitialized = () => {
  note("ui/notifications/initialized");
  resolveReady();
};

window.__host = {
  record,
  sendToolResult: (result) => bridge.sendToolResult(result),
  sendDeepLink: (url) => bridge.sendHostContextChange({ "openai/deepLink": url } as never),
  ready,
};

iframe.src = appHtml;
if (iframe.contentWindow === null) throw new Error("iframe has no window");
void bridge.connect(new PostMessageTransport(iframe.contentWindow, iframe.contentWindow));
