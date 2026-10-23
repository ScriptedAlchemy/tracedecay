#!/usr/bin/env node
// Stands in for `tracedecay serve` where a test needs exact daemon tool results.
import { McpServer } from "@modelcontextprotocol/server";
import { StdioServerTransport } from "@modelcontextprotocol/server/stdio";
import { z } from "zod";

const server = new McpServer({ name: "fake-tracedecay-serve", version: "0.0.0" });
const responseBodies = new Map([
  ["valid", JSON.stringify({ items: ["界🧪", "ok"] })],
  ["near_limit_unicode", JSON.stringify({ padding: "🧪".repeat(250_000) })],
  ["oversized_bytes", JSON.stringify({ padding: "🧪".repeat(270_000) })],
  ["decreasing", JSON.stringify({ value: "x" })],
  ["terminal_continuation", "{}"],
  ["incomplete_terminal", "{}"],
  ["length_mismatch", "{}"],
  ["cancelled", JSON.stringify({ value: "x" })],
  ["page_limit", JSON.stringify({ value: "x".repeat(1_100) })],
]);
const retrieveRequests = [];

function scalarCount(text) {
  let count = 0;
  for (const _character of text) count += 1;
  return count;
}

function scalarSlice(text, offset, size) {
  let scalarOffset = 0;
  let start = offset === 0 ? 0 : -1;
  let end = text.length;
  for (let index = 0; index < text.length;) {
    const codePoint = text.codePointAt(index);
    index += codePoint !== undefined && codePoint > 0xffff ? 2 : 1;
    scalarOffset += 1;
    if (scalarOffset === offset) start = index;
    if (scalarOffset === offset + size) {
      end = index;
      break;
    }
  }
  return text.slice(start < 0 ? text.length : start, end);
}

function page(handle, offset, content, hasMore, nextOffset, overrides = {}) {
  const originalChars = scalarCount(responseBodies.get(handle) ?? "{}");
  return {
    content,
    handle,
    expired: false,
    original_chars: originalChars,
    total_chars: originalChars,
    offset,
    has_more: hasMore,
    ...(nextOffset === undefined ? {} : { next_offset: nextOffset }),
    ...overrides,
  };
}

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

server.registerTool("response_handle", { inputSchema: { handle: z.string() } }, async ({ handle }) => {
  const body = responseBodies.get(handle) ?? "{}";
  const originalChars = scalarCount(body);
  const payload = handle === "oversized_length"
    ? { handle, original_chars: 1_048_577, preview: "{}", preview_chars: 2, truncated: true }
    : handle === "unsafe_length"
      ? { handle, original_chars: Number.MAX_SAFE_INTEGER + 1, preview: "{}", preview_chars: 2, truncated: true }
    : { handle, original_chars: originalChars, preview: body.slice(0, 2), preview_chars: scalarCount(body.slice(0, 2)), truncated: true };
  return { content: [{ type: "text", text: JSON.stringify(payload) }] };
});

server.registerTool("tracedecay_retrieve", { inputSchema: { handle: z.string(), offset: z.number().int().nonnegative() } }, async ({ handle, offset }) => {
  retrieveRequests.push({ handle, offset });
  const body = responseBodies.get(handle) ?? "{}";
  const total = scalarCount(body);
  if (handle === "expired" || handle === "missing") {
    return {
      content: [{ type: "text", text: JSON.stringify({
        handle,
        expired: handle === "expired",
        content: null,
        reason_code: handle === "expired" ? "handle_expired" : "handle_not_found",
        message: `response handle ${handle} is unavailable`,
      }) }],
    };
  }
  const handleRequests = retrieveRequests.filter((request) => request.handle === handle).length;
  if ((handle === "repeated" || handle === "decreasing") && handleRequests > 3) {
    return { content: [{ type: "text", text: JSON.stringify({ handle, reason_code: "fixture_stop" }) }] };
  }
  if (handle === "offset_mismatch") return { content: [{ type: "text", text: JSON.stringify(page(handle, 1, "{}", false, undefined)) }] };
  if (handle === "repeated") return { content: [{ type: "text", text: JSON.stringify(page(handle, 0, "{}", true, 0)) }] };
  if (handle === "missing_continuation") return { content: [{ type: "text", text: JSON.stringify(page(handle, 0, "{}", true, null)) }] };
  if (handle === "unsafe_offset") return { content: [{ type: "text", text: JSON.stringify(page(handle, 0, "{", true, Number.MAX_SAFE_INTEGER + 1)) }] };
  if (handle === "terminal_continuation") return { content: [{ type: "text", text: JSON.stringify(page(handle, 0, body, false, total)) }] };
  if (handle === "continuation_at_end") return { content: [{ type: "text", text: JSON.stringify(page(handle, 0, body, true, total)) }] };
  if (handle === "incomplete_terminal") return { content: [{ type: "text", text: JSON.stringify(page(handle, 0, "{", false, undefined)) }] };
  if (handle === "length_mismatch") return { content: [{ type: "text", text: JSON.stringify(page(handle, 0, body, false, undefined, { original_chars: total + 1 })) }] };
  if (handle === "decreasing") {
    const content = offset === 0 ? "{" : "x";
    const nextOffset = offset === 0 ? 1 : 0;
    return { content: [{ type: "text", text: JSON.stringify(page(handle, offset, content, true, nextOffset)) }] };
  }
  if (handle === "cancelled" && offset > 0) await new Promise((resolve) => setTimeout(resolve, 250));

  const pageSize = handle === "near_limit_unicode" || handle === "oversized_bytes" ? 2_000 : handle === "page_limit" ? 1 : 7;
  const content = scalarSlice(body, offset, pageSize);
  const nextOffset = offset + scalarCount(content);
  return { content: [{ type: "text", text: JSON.stringify(page(handle, offset, content, nextOffset < total, nextOffset < total ? nextOffset : undefined)) }] };
});

server.registerTool("retrieve_calls", { inputSchema: { handle: z.string() } }, async ({ handle }) => ({
  content: [{ type: "text", text: JSON.stringify({ offsets: retrieveRequests.filter((request) => request.handle === handle).map((request) => request.offset) }) }],
}));

server.registerTool("hinted_read", {}, async () => ({
  content: [
    { type: "text", text: JSON.stringify({ ok: true }) },
    { type: "text", text: "\ntracedecay_metrics: response_tokens=4" },
    {
      type: "text",
      text: "tracedecay_repeated_query: this tracedecay_search call shares 100% of its query words with the tracedecay_search call 2 calls earlier in this session",
    },
  ],
}));

server.registerTool("plain_read", {}, async () => ({ content: [{ type: "text", text: JSON.stringify({ ok: true }) }] }));

await server.connect(new StdioServerTransport());
