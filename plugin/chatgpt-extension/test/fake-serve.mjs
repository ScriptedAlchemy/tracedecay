#!/usr/bin/env node
// Stands in for `tracedecay serve` where a test needs exact daemon tool results.
import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";

const server = new McpServer({ name: "fake-tracedecay-serve", version: "0.0.0" });

server.registerTool("chatty", {}, async () => {
  await new Promise((resolve) => process.stderr.write("daemon log line\n".repeat(64 * 1024), resolve));
  return { content: [{ type: "text", text: JSON.stringify({ ok: true }) }] };
});

server.registerTool("refuses_with_problem", {}, async () => ({
  isError: true,
  content: [{ type: "text", text: "project proj_0000 is not registered" }],
  structuredContent: {
    problem: { kind: "not_found_or_not_authorized", code: "project_not_registered", message: "project proj_0000 is not registered" },
  },
}));

server.registerTool("node_not_found", {}, async () => ({
  isError: true,
  content: [
    {
      type: "text",
      text: JSON.stringify({ status: "not_found", reason_code: "node_not_found", node_id: "n1", message: "Node not found: n1" }),
    },
  ],
}));

await server.connect(new StdioServerTransport());
