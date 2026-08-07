import { once } from "node:events";
import {
  createServer,
  type IncomingMessage,
  type Server,
  type ServerResponse,
} from "node:http";

import { describe, expect, expectTypeOf, it, vi } from "vitest";

import { OPERATIONS, UNAVAILABLE_OPERATIONS } from "../src/operations";
import {
  decodeCanonicalSchema,
  decodeHttpSuccessEnvelope,
  type HttpSuccessEnvelope,
} from "../src/types";

import {
  TraceDecayAbortError,
  TraceDecayDisconnectedError,
  TraceDecayMalformedResponseError,
  TraceDecayProtocolError,
  createClient,
  type OperationRequestOptions,
} from "../src/client";

type RequestHandler = (
  request: IncomingMessage,
  response: ServerResponse,
  body: string,
) => void | Promise<void>;

const RECEIPT = {
  started_at: 10,
  ended_at: 20,
  effective_deadline: { expires_at: 30 },
  cancellation: null,
  budget: {
    units_consumed: 1,
    bytes_consumed: 2,
    elapsed_micros: 3,
  },
  termination: "completed",
  future_receipt_field: "preserved",
};

function successEnvelope(payload: unknown) {
  return {
    kind: "success",
    value: {
      binding_id: "binding.http.workflow.list_definitions",
      contract: {
        schema_id: "schema.workflow.list_definitions.result",
        schema_revision: 1,
      },
      request_id: "request.http.1",
      scope: {
        project_id: "project.sdk",
        future_scope_field: true,
      },
      outcome: {
        outcome: "evidence",
        value: {
          temporal: {},
          authority: {},
          evidence_authorities: [],
          coverage: {},
          omissions: [],
          scores: [],
          contributions: [],
          page: {
            sort_contract_id: "sort.sdk-test",
            sort_revision: 1,
            total: 1,
            returned: 1,
            cursor: null,
            expires_at: null,
          },
          payload,
          execution: structuredClone(RECEIPT),
          future_outcome_field: "preserved",
        },
      },
      future_envelope_field: "preserved",
    },
  };
}

function problemEnvelope(
  kind: string,
  code: string,
  options: {
    bindingId?: string;
    retry?: string;
    retryable?: boolean;
    legalActions?: string[];
    retryAfterMillis?: number | null;
  } = {},
) {
  const value: Record<string, unknown> = {
    contract: {
      schema_id: "schema.application.problem",
      schema_revision: 1,
    },
    request_id: `request.${code}`,
    problem: {
      revision: 1,
      kind,
      code,
      message: code,
      diagnostic: { code, message: code },
      owning_layer: "application",
      terminality: "terminal",
      retryable: options.retryable ?? false,
      retry: options.retry ?? "never",
      retry_scope: null,
      retry_after_millis: options.retryAfterMillis ?? null,
      cancellation_stage: null,
      request_id: `request.${code}`,
      trace_id: `trace.${code}`,
      details: [],
      legal_actions: options.legalActions ?? [],
      coverage: null,
      future_problem_field: "preserved",
    },
  };
  if (options.bindingId !== undefined) {
    value.binding_id = options.bindingId;
  }
  return { kind: "problem", value };
}

function requestThroughTransport(
  client: ReturnType<typeof createClient>,
  options: OperationRequestOptions = {},
): Promise<HttpSuccessEnvelope<unknown>> {
  return client.operations.workflow_list_definitions({}, options);
}

async function readBody(request: IncomingMessage): Promise<string> {
  const chunks: Buffer[] = [];
  for await (const chunk of request) {
    chunks.push(Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk));
  }
  return Buffer.concat(chunks).toString("utf8");
}

async function listen(handlers: RequestHandler[]): Promise<{
  server: Server;
  baseUrl: string;
  requests: IncomingMessage[];
}> {
  const requests: IncomingMessage[] = [];
  let index = 0;
  const server = createServer(async (request, response) => {
    requests.push(request);
    const handler = handlers[index];
    index += 1;
    if (handler === undefined) {
      response.writeHead(500, { "content-type": "text/plain" });
      response.end(`unexpected request ${index}: ${request.url}`);
      return;
    }
    try {
      await handler(request, response, await readBody(request));
    } catch (error) {
      response.destroy(error instanceof Error ? error : new Error(String(error)));
    }
  });
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  const address = server.address();
  if (address === null || typeof address === "string") {
    throw new Error("test server did not bind a TCP address");
  }
  return {
    server,
    baseUrl: `http://127.0.0.1:${address.port}`,
    requests,
  };
}

async function withServer(
  handlers: RequestHandler[],
  run: (baseUrl: string, requests: IncomingMessage[]) => Promise<void>,
): Promise<void> {
  const fixture = await listen(handlers);
  try {
    await run(fixture.baseUrl, fixture.requests);
  } finally {
    fixture.server.closeAllConnections();
    fixture.server.close();
    await once(fixture.server, "close");
  }
}

function json(
  response: ServerResponse,
  status: number,
  value: unknown,
): void {
  response.writeHead(status, { "content-type": "application/json" });
  response.end(JSON.stringify(value));
}

describe("canonical JSON Schema decoding", () => {
  it("enforces integer formats and rejects unsafe numbers", () => {
    const uint32 = { type: "integer", format: "uint32" } as const;
    const uint64 = { type: "integer", format: "uint64" } as const;
    const int64 = { type: "integer", format: "int64" } as const;

    expect(decodeCanonicalSchema(4_294_967_295, uint32)).toBe(4_294_967_295);
    expect(decodeCanonicalSchema(Number.MAX_SAFE_INTEGER, uint64)).toBe(
      Number.MAX_SAFE_INTEGER,
    );
    expect(decodeCanonicalSchema(Number.MIN_SAFE_INTEGER, int64)).toBe(
      Number.MIN_SAFE_INTEGER,
    );
    expect(() => decodeCanonicalSchema(4_294_967_296, uint32)).toThrow(TypeError);
    expect(() =>
      decodeCanonicalSchema(Number.MAX_SAFE_INTEGER + 1, uint64),
    ).toThrow(TypeError);
    expect(() => decodeCanonicalSchema(true, uint64)).toThrow(TypeError);
  });

  it("canonicalizes unique object keys in one serialization per item", () => {
    const schema = {
      type: "array",
      uniqueItems: true,
      items: { type: "object" },
    } as const;
    expect(() =>
      decodeCanonicalSchema(
        [
          { alpha: 1, beta: 2 },
          { beta: 2, alpha: 1 },
        ],
        schema,
      ),
    ).toThrow(TypeError);

    let serializations = 0;
    const originalStringify = JSON.stringify.bind(JSON);
    const stringify = vi.spyOn(JSON, "stringify").mockImplementation((value: unknown) => {
      serializations += 1;
      return originalStringify(value);
    });
    try {
      decodeCanonicalSchema(
        Array.from({ length: 100 }, (_, index) => ({ index })),
        schema,
      );
    } finally {
      stringify.mockRestore();
    }
    expect(serializations).toBe(100);
  });
});

describe("TraceDecayClient generated operation bindings", () => {
  it("publishes typed Workflow methods from canonical bindings", () => {
    expectTypeOf<
      Parameters<
        ReturnType<typeof createClient>["operations"]["workflow_get_definition"]
      >[0]
    >().toEqualTypeOf<{
      readonly definition_id: string;
      readonly definition_version: number;
    }>();

    const client = createClient({
      baseUrl: "http://127.0.0.1:43123",
      projectId: "project.sdk",
      token: "sdk-secret",
    });

    expect(new Set(OPERATIONS.map((operation) => operation.operation)).size).toBe(
      OPERATIONS.length,
    );
    // Operations without a canonical SDK binding stay absent. Asserting the
    // whole deferred set — rather than one named operation — keeps this
    // invariant true as families gain schemas and leave the deferred list.
    const bound = new Set(Object.keys(client.operations));
    for (const unavailable of UNAVAILABLE_OPERATIONS) {
      expect(bound.has(unavailable.operation)).toBe(false);
      expect(
        bound.has(unavailable.operation.replace(/^application_/, "")),
      ).toBe(false);
    }
  });

  it("preserves remote base paths and origin policy", async () => {
    let requestedUrl = "";
    let requestedOrigin = "";
    let requestedDeadline = "";
    const client = createClient({
      baseUrl: "https://remote.example/api/v1/",
      projectId: "project.sdk",
      token: "sdk-secret",
      origin: "https://consumer.example",
      fetch: async (input, init) => {
        requestedUrl = String(input);
        const headers = new Headers(init?.headers);
        requestedOrigin = headers.get("origin") ?? "";
        requestedDeadline =
          headers.get("x-tracedecay-deadline-micros") ?? "";
        return new Response(JSON.stringify(successEnvelope({ status: "ok" })), {
          status: 200,
          headers: { "content-type": "application/json" },
        });
      },
    });

    await expect(
      client.operations.workflow_list_definitions(
        {},
        { deadlineMicros: 1_800_000_000_000_003 },
      ),
    ).rejects.toBeInstanceOf(TraceDecayMalformedResponseError);

    expect(requestedUrl).toBe(
      "https://remote.example/api/v1/projects/project.sdk/application/workflow/list-definitions",
    );
    expect(requestedOrigin).toBe("https://consumer.example");
    expect(requestedDeadline).toBe("1800000000000003");
  });

  it("fails closed on malformed typed Workflow requests before transport", async () => {
    let fetchCalls = 0;
    const client = createClient({
      baseUrl: "http://127.0.0.1:43123",
      projectId: "project.sdk",
      token: "sdk-secret",
      fetch: async () => {
        fetchCalls += 1;
        throw new Error("transport must not be reached");
      },
    });

    await expect(
      client.operations.workflow_get_definition(
        // @ts-expect-error Deliberately malformed at the package boundary.
        { definition_id: "workflow.sdk", definition_version: "1" },
      ),
    ).rejects.toBeInstanceOf(TypeError);
    expect(fetchCalls).toBe(0);
    expect("invoke" in client).toBe(false);
    expect("requestOperation" in client).toBe(false);
    expect(Reflect.get(client, "requestOperation")).toBeUndefined();
  });

  it("publishes all mounted Workflow routes as executable operations", () => {
    const available: string[] = OPERATIONS.map((operation) => operation.operation);
    const unavailable = (
      UNAVAILABLE_OPERATIONS as readonly { readonly operation: string }[]
    ).map((operation) => operation.operation);
    expect(available.length).toBeGreaterThan(0);
    expect(new Set(available).size).toBe(available.length);
    expect(new Set(OPERATIONS.map((operation) => operation.operationId)).size).toBe(
      available.length,
    );
    expect(new Set(OPERATIONS.map((operation) => operation.bindingId)).size).toBe(
      available.length,
    );
    expect(unavailable.some((operation) => available.includes(operation))).toBe(false);
    expect(available).toEqual(
      expect.arrayContaining([
        "workflow_definition_history",
        "workflow_diff_definition",
        "workflow_get_definition",
        "workflow_handoff_issue",
        "workflow_handoff_redeem",
        "workflow_list_definitions",
        "workflow_register_definition",
        "workflow_validate_definition",
      ]),
    );
    expect(
      "workflow_list_definitions" in createClient({
        baseUrl: "http://127.0.0.1:43123",
        projectId: "project.sdk",
        token: "sdk-secret",
      }).operations,
    ).toBe(true);
    // Handoff and multi-root are mounted HTTP families, so the generator must
    // publish them like Work and Workflow. Multi-root was quarantined here
    // while its public surface was torn out; the multi-root HTTP owner landed
    // on 2026-08-07 and its production router test answers on all three
    // routes, so the quarantine no longer describes the daemon.
    const mountedFamilies = [
      "handoff_open_investigation_handoff",
      "handoff_open_task_handoff",
      "multi_root_scope_set_read",
      "multi_root_scope_set_compare_and_swap",
      "multi_root_execute",
    ];
    const clientOperations = createClient({
      baseUrl: "http://127.0.0.1:43123",
      projectId: "project.sdk",
      token: "sdk-secret",
    }).operations;
    for (const operation of mountedFamilies) {
      expect(available).toContain(operation);
      expect(unavailable).not.toContain(operation);
      expect(operation in clientOperations).toBe(true);
    }
  });

  it("publishes workflow_register_definition with the canonical descriptor identity", () => {
    const client = createClient({
      baseUrl: "http://127.0.0.1:43123",
      projectId: "project.sdk",
      token: "sdk-secret",
    });
    expect("workflow_register_definition" in client.operations).toBe(true);

    const descriptor = OPERATIONS.find(
      (operation) => operation.operation === "workflow_register_definition",
    );
    expect(descriptor).toBeDefined();
    expect(descriptor?.operationId).toBe("operation.workflow.register_definition");
    expect(descriptor?.transport).toEqual({
      kind: "http",
      route: "/application/workflow/register-definition",
      method: "POST",
    });
    expect(descriptor?.effect).toBe("administrative");
    expect(descriptor?.idempotency).toBe("required");
    expect(descriptor?.bindingId).toBe("binding.http.workflow.register_definition");
    expect(descriptor?.requestSchema).toEqual({
      schemaId: "schema.workflow.register_definition.request",
      revision: 1,
    });
    expect(descriptor?.resultSchema).toEqual({
      schemaId: "schema.workflow.register_definition.result",
      revision: 1,
    });
    expect(descriptor?.deadline).toEqual({
      maximum_millis: 30_000,
      behavior: "return_effect_receipt",
    });
  });

  it("publishes every configuration operation with its effect lifecycle", () => {
    const expected = [
      "configuration_list",
      "configuration_explain",
      "configuration_get",
      "configuration_set",
      "configuration_unset",
      "configuration_batch",
      "configuration_write_credential",
      "configuration_observed_state",
      "configuration_protected_preview",
      "configuration_protected_apply",
      "configuration_rollback_preview",
      "configuration_rollback_apply",
      "configuration_audit",
    ]
      .map((operation) => `application_${operation}`)
      .sort();
    const configuration = OPERATIONS.filter((operation) =>
      operation.operation.startsWith("application_configuration_"),
    );

    expect(configuration.map((operation) => operation.operation)).toEqual(expected);
    expect(
      UNAVAILABLE_OPERATIONS.some((operation) =>
        operation.operation.startsWith("application_configuration_"),
      ),
    ).toBe(false);
    for (const operation of configuration) {
      expect(operation.transport).toEqual({
        kind: "http",
        route: `/application/configuration/${operation.operation.replace(
          /^application_/,
          "",
        )}`,
        method: "POST",
      });
      expect(operation.bindingId).toBe(
        `binding.http.${operation.operation.replace(/^application_/, "")}.v1`,
      );
      expect(operation.deadline.maximum_millis).toBe(15_000);
      if (operation.effect === "configuration_write") {
        expect(operation.idempotency).toBe("required");
        expect(operation.cancellation).toEqual({ mode: "not_cancellable" });
        expect(operation.deadline.behavior).toBe("return_effect_receipt");
        expect(operation.reconciliation).toBe("required");
        expect(operation.receipt).toBe("durable_effect");
        expect(operation.terminalStates).toEqual([
          "completed",
          "timed_out",
          "failed",
          "effect_unknown",
          "partial",
        ]);
      } else {
        expect(operation.idempotency).toBe("not_required");
        expect(operation.cancellation).toEqual({
          mode: "cooperative",
          points: ["before_admission", "before_read", "during_read"],
        });
        expect(operation.deadline.behavior).toBe("return_operation_receipt");
        expect(operation.reconciliation).toBe("not_required");
        expect(operation.receipt).toBe("operation");
        expect(operation.terminalStates).toEqual([
          "completed",
          "cancelled",
          "timed_out",
          "failed",
          "partial",
        ]);
      }
    }
  });

  it("rejects an operation-illegal terminal before decoding its payload", () => {
    const descriptor = OPERATIONS.find(
      (operation) => operation.operation === "application_configuration_set",
    );
    expect(descriptor).toBeDefined();
    const response = successEnvelope({});
    response.value.binding_id = "binding.http.configuration_set.v1";
    response.value.contract = {
      schema_id: "schema.application.configuration.configuration_set.result",
      schema_revision: 1,
    };
    // An effect outcome deliberately replaces the helper's evidence shape;
    // decodeSuccess treats the envelope as unknown wire input.
    response.value.outcome = {
      outcome: "effect",
      value: {
        effect_id: "effect.configuration.sdk",
        effect_class: "configuration_write",
        idempotency_key: "configuration.idempotency.sdk",
        authority: {},
        expected_state: "configuration.revision.sdk",
        reconciliation: "required",
        receipt: {},
        payload: {},
        execution: {
          ...structuredClone(RECEIPT),
          termination: "cancelled",
        },
      },
    } as unknown as typeof response.value.outcome;

    expect(() => descriptor?.decodeSuccess(response.value)).toThrow(
      /termination cancelled is not legal for this operation/,
    );
  });

  it("publishes git_status on its MCP tool transport", () => {
    const descriptor = OPERATIONS.find(
      (operation) => operation.operation === "git_status",
    );
    expect(descriptor).toBeDefined();
    expect(descriptor?.operationId).toBe("operation.application.git_status");
    expect(descriptor?.transport).toEqual({
      kind: "mcp_tool",
      toolName: "tracedecay_git_status",
    });
    expect(descriptor?.bindingId).toBe("binding.mcp.git_status.v1");
    expect(
      UNAVAILABLE_OPERATIONS.some((operation) =>
        operation.operation.startsWith("application_git_"),
      ),
    ).toBe(false);
  });

  it("routes Git operations through the caller's MCP tool adapter", async () => {
    const calls: Array<{ toolName: string; request: unknown }> = [];
    const client = createClient({
      baseUrl: "http://127.0.0.1:43123",
      projectId: "project.sdk",
      token: "sdk-secret",
      mcp: {
        async callTool(toolName, request) {
          calls.push({ toolName, request });
          return {};
        },
      },
    });

    await expect(client.operations.git_status({})).rejects.toBeInstanceOf(
      TraceDecayMalformedResponseError,
    );
    expect(calls).toEqual([{ toolName: "tracedecay_git_status", request: {} }]);
  });

  it("refuses Git operations without an MCP tool adapter as a typed transport failure", async () => {
    const client = createClient({
      baseUrl: "http://127.0.0.1:43123",
      projectId: "project.sdk",
      token: "sdk-secret",
    });

    await expect(client.operations.git_status({})).rejects.toThrow(
      /git_status requires an MCP tool adapter/,
    );
  });

  it("propagates the caller's abort through the MCP adapter as an abort error", async () => {
    const controller = new AbortController();
    const client = createClient({
      baseUrl: "http://127.0.0.1:43123",
      projectId: "project.sdk",
      token: "sdk-secret",
      mcp: {
        async callTool(_toolName, _request, { signal }) {
          controller.abort(new Error("caller cancelled"));
          signal?.throwIfAborted();
          return {};
        },
      },
    });

    await expect(
      client.operations.git_status({}, { signal: controller.signal }),
    ).rejects.toBeInstanceOf(TraceDecayAbortError);
  });
});

describe("TraceDecayClient transport envelopes", () => {
  it("rejects invalid canonical page options before transport", async () => {
    let fetchCalls = 0;
    const client = createClient({
      baseUrl: "http://127.0.0.1:43123",
      projectId: "project.sdk",
      token: "sdk-secret",
      fetch: async () => {
        fetchCalls += 1;
        throw new Error("transport must not be reached");
      },
    });

    await expect(
      requestThroughTransport(client, { page: { size: 0 } }),
    ).rejects.toBeInstanceOf(TraceDecayProtocolError);
    await expect(
      requestThroughTransport(client, { page: { size: 1_001 } }),
    ).rejects.toBeInstanceOf(TraceDecayProtocolError);
    await expect(
      requestThroughTransport(client, { page: { cursor: " cursor " } }),
    ).rejects.toBeInstanceOf(TraceDecayProtocolError);
    await expect(
      requestThroughTransport(client, { deadlineMicros: 0 }),
    ).rejects.toBeInstanceOf(TraceDecayProtocolError);
    await expect(
      requestThroughTransport(client, { deadlineMicros: 1.5 }),
    ).rejects.toBeInstanceOf(TraceDecayProtocolError);
    expect(fetchCalls).toBe(0);
  });

  it("fails closed when a success envelope contains an invalid page", async () => {
    const envelope = successEnvelope({ files: [] });
    envelope.value.outcome.value.page.returned = 2;
    envelope.value.outcome.value.page.total = 1;

    await withServer(
      [
        (request, response) => {
          expect(request.method).toBe("POST");
          expect(request.url).toBe(
            "/projects/project.sdk/application/workflow/list-definitions",
          );
          json(response, 200, envelope);
        },
      ],
      async (baseUrl) => {
        const client = createClient({
          baseUrl,
          projectId: "project.sdk",
          token: "sdk-secret",
        });

        await expect(requestThroughTransport(client)).rejects.toBeInstanceOf(
          TraceDecayMalformedResponseError,
        );
      },
    );
  });

  it("fails closed when problem envelope identities disagree", async () => {
    const envelope = problemEnvelope("unavailable", "service_unavailable");
    (envelope.value.problem as Record<string, unknown>).request_id =
      "request.different";

    await withServer(
      [
        (_request, response) => {
          json(response, 503, envelope);
        },
      ],
      async (baseUrl) => {
        const client = createClient({
          baseUrl,
          projectId: "project.sdk",
          token: "sdk-secret",
        });

        await expect(
          client.cancelOperation("request.operation"),
        ).rejects.toBeInstanceOf(TraceDecayMalformedResponseError);
      },
    );
  });

  it("rejects a problem envelope from a different executable binding", async () => {
    const envelope = problemEnvelope("unavailable", "service_unavailable", {
      bindingId: "binding.http.different.v1",
    });

    await withServer(
      [
        (_request, response) => {
          json(response, 503, envelope);
        },
      ],
      async (baseUrl) => {
        const client = createClient({
          baseUrl,
          projectId: "project.sdk",
          token: "sdk-secret",
        });

        await expect(requestThroughTransport(client)).rejects.toBeInstanceOf(
          TraceDecayMalformedResponseError,
        );
      },
    );
  });
});

describe("TraceDecayClient operation lifecycle", () => {
  it("accepts the server media type, UTF-8 BOM, and inherited SSE event ID", async () => {
    await withServer(
      [
        (_request, response) => {
          response.writeHead(200, {
            "content-type": "text/event-stream; charset=utf-8",
          });
          response.end(
            [
              "\uFEFFevent: open",
              'data: {"event":"open","data":{"correlation_id":"request.operation","frontier":{"next_sequence":0,"retained_from_sequence":0,"resume_token":"resume"}}}',
              "",
              "id: 0",
              "",
              "event: item",
              'data: {"event":"item","data":{"sequence":0,"item":{"kind":"accepted"}}}',
              "",
              "event: completed",
              "id: 1",
              `data: ${JSON.stringify({
                event: "completed",
                data: {
                  sequence: 1,
                  terminal: { termination: "completed", receipt: RECEIPT },
                },
              })}`,
              "",
              "",
            ].join("\r\n"),
          );
        },
      ],
      async (baseUrl) => {
        const client = createClient({
          baseUrl,
          projectId: "project.sdk",
          token: "sdk-secret",
        });
        const events = [];
        for await (const event of client.streamOperation("request.operation")) {
          events.push(event);
        }

        expect(events.map((event) => [event.event, event.id])).toEqual([
          ["open", null],
          ["item", "0"],
          ["completed", "1"],
        ]);
      },
    );
  });

  it("reconnects after a transport interruption following open", async () => {
    await withServer(
      [
        async (_request, response) => {
          response.writeHead(200, {
            "content-type": "text/event-stream",
            connection: "close",
          });
          response.write(
            [
              "event: open",
              'data: {"event":"open","data":{"correlation_id":"request.operation","frontier":{"next_sequence":0,"retained_from_sequence":0,"resume_token":"resume.interrupted"}}}',
              "",
              "",
            ].join("\n"),
          );
          await new Promise<void>((resolve) => setImmediate(resolve));
          response.destroy(new Error("simulated body interruption"));
        },
        (request, response) => {
          expect(request.url).toBe(
            "/projects/project.sdk/application/operations/request.operation/events?next_sequence=0&resume_token=resume.interrupted",
          );
          response.writeHead(200, { "content-type": "text/event-stream" });
          response.end(
            [
              "event: open",
              'data: {"event":"open","data":{"correlation_id":"request.operation","frontier":{"next_sequence":0,"retained_from_sequence":0,"resume_token":"resume.interrupted"}}}',
              "",
              "event: completed",
              "id: 0",
              `data: ${JSON.stringify({
                event: "completed",
                data: {
                  sequence: 0,
                  terminal: { termination: "completed", receipt: RECEIPT },
                },
              })}`,
              "",
              "",
            ].join("\n"),
          );
        },
      ],
      async (baseUrl) => {
        const client = createClient({
          baseUrl,
          projectId: "project.sdk",
          token: "sdk-secret",
        });
        const events = [];
        for await (const event of client.streamOperation("request.operation", {
          maxReconnects: 1,
        })) {
          events.push(event.event);
        }
        expect(events).toEqual(["open", "open", "completed"]);
      },
    );
  });

  it("rejects an open event for a different operation", async () => {
    await withServer(
      [
        (_request, response) => {
          response.writeHead(200, { "content-type": "text/event-stream" });
          response.end(
            [
              "event: open",
              'data: {"event":"open","data":{"correlation_id":"request.other","frontier":{"next_sequence":0,"retained_from_sequence":0,"resume_token":"resume"}}}',
              "",
              "",
            ].join("\n"),
          );
        },
      ],
      async (baseUrl) => {
        const client = createClient({
          baseUrl,
          projectId: "project.sdk",
          token: "sdk-secret",
        });
        const consume = async () => {
          for await (const _event of client.streamOperation("request.operation")) {
            // Drain the stream.
          }
        };
        await expect(consume()).rejects.toBeInstanceOf(
          TraceDecayMalformedResponseError,
        );
      },
    );
  });

  it("rejects a successful stream response with a non-SSE media type", async () => {
    await withServer(
      [
        (_request, response) => {
          response.writeHead(200, { "content-type": "application/json" });
          response.end(
            [
              "event: completed",
              "id: 0",
              `data: ${JSON.stringify({
                event: "completed",
                data: {
                  sequence: 0,
                  terminal: { termination: "completed", receipt: RECEIPT },
                },
              })}`,
              "",
              "",
            ].join("\n"),
          );
        },
      ],
      async (baseUrl) => {
        const client = createClient({
          baseUrl,
          projectId: "project.sdk",
          token: "sdk-secret",
        });
        const consume = async () => {
          for await (const _event of client.streamOperation("request.operation")) {
            // Drain the stream.
          }
        };

        await expect(consume()).rejects.toBeInstanceOf(
          TraceDecayMalformedResponseError,
        );
      },
    );
  });

  it("preserves SSE order and resumes from the canonical frontier", async () => {
    await withServer(
      [
        (request, response) => {
          expect(request.method).toBe("GET");
          expect(request.url).toBe(
            "/projects/project.sdk/application/operations/request.operation/events",
          );
          response.writeHead(200, {
            "content-type": "text/event-stream",
            connection: "close",
          });
          response.end(
            [
              "event: open",
              'data: {"event":"open","data":{"correlation_id":"request.operation","frontier":{"next_sequence":1,"retained_from_sequence":0,"resume_token":"opaque+/token=="}}}',
              "",
              "event: item",
              "id: 0",
              'data: {"event":"item","data":{"sequence":0,"item":{"kind":"accepted","future_item_field":true}}}',
              "",
              "",
            ].join("\n"),
          );
        },
        (request, response) => {
          expect(request.method).toBe("GET");
          expect(request.url).toBe(
            "/projects/project.sdk/application/operations/request.operation/events?next_sequence=1&resume_token=opaque%2B%2Ftoken%3D%3D",
          );
          response.writeHead(200, {
            "content-type": "text/event-stream",
            connection: "close",
          });
          response.end(
            [
              "event: open",
              'data: {"event":"open","data":{"correlation_id":"request.operation","frontier":{"next_sequence":1,"retained_from_sequence":0,"resume_token":"opaque+/token=="}}}',
              "",
              "event: progress",
              "id: 1",
              'data: {"event":"progress","data":{"sequence":1,"completed":1,"total":2}}',
              "",
              "event: future_signal",
              "id: 2",
              'data: {"event":"future_signal","data":{"sequence":2,"future_field":{"kept":true}},"future_event_field":"preserved"}',
              "",
              "event: completed",
              "id: 3",
              `data: ${JSON.stringify({
                event: "completed",
                data: {
                  sequence: 3,
                  terminal: {
                    termination: "completed",
                    receipt: RECEIPT,
                  },
                },
              })}`,
              "",
              "",
            ].join("\n"),
          );
        },
      ],
      async (baseUrl, requests) => {
        const client = createClient({
          baseUrl,
          projectId: "project.sdk",
          token: "sdk-secret",
        });
        const events = [];
        for await (const event of client.streamOperation("request.operation", {
          maxReconnects: 1,
        })) {
          events.push(event);
        }

        expect(events.map((event) => event.event)).toEqual([
          "open",
          "item",
          "open",
          "progress",
          "future_signal",
          "completed",
        ]);
        expect(events.filter((event) => event.id === "0")).toHaveLength(1);
        expect(events[4]?.data).toMatchObject({
          event: "future_signal",
          data: { future_field: { kept: true } },
          future_event_field: "preserved",
        });
        expect(events[5]?.data).toMatchObject({
          data: {
            terminal: {
              receipt: {
                termination: "completed",
                future_receipt_field: "preserved",
              },
            },
          },
        });
        expect(requests).toHaveLength(2);
      },
    );
  });

  it("parses canonical event-stream framing and publishes resume gaps explicitly", async () => {
    await withServer(
      [
        (_request, response) => {
          response.writeHead(200, {
            "content-type": "text/event-stream",
          });
          response.end(
            [
              ": heartbeat",
              "event: open",
              'data: {"event":"open","data":{"correlation_id":"request.operation",',
              'data: "frontier":{"next_sequence":1,"retained_from_sequence":1,"resume_token":"resume"}}}',
              "",
              "event: resume_gap",
              "id: 1",
              'data: {"event":"resume_gap","data":{"sequence":1,"gap":{"first_missing_sequence":0,',
              'data: "last_missing_sequence":0,"frontier":{"next_sequence":2,"retained_from_sequence":1,"resume_token":"resume"}}}}',
              "",
              "event: partial",
              "id: 2",
              `data: ${JSON.stringify({
                event: "partial",
                data: {
                  sequence: 2,
                  terminal: {
                    termination: "partial",
                    receipt: { ...RECEIPT, termination: "partial" },
                  },
                },
              })}`,
              "",
              "",
            ].join("\r"),
          );
        },
      ],
      async (baseUrl) => {
        const client = createClient({
          baseUrl,
          projectId: "project.sdk",
          token: "sdk-secret",
        });
        const events = [];
        for await (const event of client.streamOperation("request.operation")) {
          events.push(event);
        }

        expect(events.map((event) => event.event)).toEqual([
          "open",
          "resume_gap",
          "partial",
        ]);
        expect(events[1]?.data).toMatchObject({
          data: {
            gap: {
              first_missing_sequence: 0,
              last_missing_sequence: 0,
            },
          },
        });
      },
    );
  });

  it("accepts valid retry fields, ignores invalid values, and fails closed on incomplete events", async () => {
    await withServer(
      [
        (_request, response) => {
          response.writeHead(200, { "content-type": "text/event-stream" });
          response.end(
            [
              "event: open",
              "retry: 1",
              "retry: invalid",
              'data: {"event":"open","data":{"correlation_id":"request.operation","frontier":{"next_sequence":1,"retained_from_sequence":0,"resume_token":"resume"}}}',
              "",
              "event: item",
              "id: 0",
              'data: {"event":"item","data":{"sequence":0,"item":{"kind":"accepted"}}}',
              "",
              "",
            ].join("\n"),
          );
        },
        (_request, response) => {
          response.writeHead(200, { "content-type": "text/event-stream" });
          response.end(
            [
              "event: open",
              'data: {"event":"open","data":{"correlation_id":"request.operation","frontier":{"next_sequence":1,"retained_from_sequence":0,"resume_token":"resume"}}}',
              "",
              "event: completed",
              "id: 1",
              `data: ${JSON.stringify({
                event: "completed",
                data: {
                  sequence: 1,
                  terminal: { termination: "completed", receipt: RECEIPT },
                },
              })}`,
              "",
              "",
            ].join("\n"),
          );
        },
        (_request, response) => {
          response.writeHead(200, { "content-type": "text/event-stream" });
          response.end(
            [
              "event: completed",
              "id: 0",
              `data: ${JSON.stringify({
                event: "completed",
                data: {
                  sequence: 0,
                  terminal: { termination: "completed", receipt: RECEIPT },
                },
              })}`,
            ].join("\n"),
          );
        },
      ],
      async (baseUrl) => {
        const client = createClient({
          baseUrl,
          projectId: "project.sdk",
          token: "sdk-secret",
        });

        const events = [];
        for await (const event of client.streamOperation("request.operation", {
          maxReconnects: 1,
        })) {
          events.push(event);
        }
        expect(events.map((event) => event.event)).toEqual([
          "open",
          "item",
          "open",
          "completed",
        ]);

        await expect(async () => {
          for await (const _event of client.streamOperation("request.operation")) {
            // Drain the stream.
          }
        }).rejects.toBeInstanceOf(TraceDecayDisconnectedError);
      },
    );
  });

  it("cancels through the canonical operation route without claiming rollback", async () => {
    await withServer(
      [
        (request, response, body) => {
          expect(request.method).toBe("POST");
          expect(request.url).toBe(
            "/projects/project.sdk/application/operations/request.operation/cancel",
          );
          expect(body).toBe("");
          json(response, 202, { status: "requested", future_field: true });
        },
      ],
      async (baseUrl) => {
        const client = createClient({
          baseUrl,
          projectId: "project.sdk",
          token: "sdk-secret",
        });
        await expect(
          client.cancelOperation("request.operation"),
        ).resolves.toMatchObject({
          status: "requested",
          future_field: true,
        });
      },
    );
  });

  it("fails closed on a non-canonical cancellation outcome", async () => {
    await withServer(
      [
        (_request, response) => {
          json(response, 202, { status: "accepted" });
        },
      ],
      async (baseUrl) => {
        const client = createClient({
          baseUrl,
          projectId: "project.sdk",
          token: "sdk-secret",
        });

        await expect(
          client.cancelOperation("request.operation"),
        ).rejects.toBeInstanceOf(TraceDecayMalformedResponseError);
      },
    );
  });

  it("preserves resume gaps and receipt-bearing partial termination", async () => {
    await withServer(
      [
        (_request, response) => {
          response.writeHead(200, {
            "content-type": "text/event-stream",
            connection: "close",
          });
          response.end(
            [
              "event: open",
              'data: {"event":"open","data":{"correlation_id":"request.operation","frontier":{"next_sequence":5,"retained_from_sequence":5,"resume_token":"resume.gap"}}}',
              "",
              "event: resume_gap",
              "id: 5",
              'data: {"event":"resume_gap","data":{"sequence":5,"gap":{"first_missing_sequence":2,"last_missing_sequence":4,"frontier":{"next_sequence":5,"retained_from_sequence":5,"resume_token":"resume.gap"}}}}',
              "",
              "event: partial",
              "id: 6",
              `data: ${JSON.stringify({
                event: "partial",
                data: {
                  sequence: 6,
                  terminal: {
                    termination: "partial",
                    receipt: { ...RECEIPT, termination: "partial" },
                  },
                },
              })}`,
              "",
              "",
            ].join("\n"),
          );
        },
      ],
      async (baseUrl) => {
        const client = createClient({
          baseUrl,
          projectId: "project.sdk",
          token: "sdk-secret",
        });
        const events = [];
        for await (const event of client.streamOperation("request.operation")) {
          events.push(event);
        }

        expect(events.map((event) => event.event)).toEqual([
          "open",
          "resume_gap",
          "partial",
        ]);
        expect(events[1]?.data).toMatchObject({
          event: "resume_gap",
          data: {
            gap: {
              first_missing_sequence: 2,
              last_missing_sequence: 4,
            },
          },
        });
        expect(events[2]?.data).toMatchObject({
          data: {
            terminal: {
              termination: "partial",
              receipt: { termination: "partial" },
            },
          },
        });
      },
    );
  });

  it("reports a non-resumable stream close as disconnected", async () => {
    await withServer(
      [
        (_request, response) => {
          response.writeHead(200, {
            "content-type": "text/event-stream",
            connection: "close",
          });
          response.end();
        },
      ],
      async (baseUrl) => {
        const client = createClient({
          baseUrl,
          projectId: "project.sdk",
          token: "sdk-secret",
        });
        const consume = async () => {
          for await (const _event of client.streamOperation(
            "request.operation",
            { maxReconnects: 0 },
          )) {
            // No canonical event is available before disconnect.
          }
        };

        await expect(consume()).rejects.toBeInstanceOf(
          TraceDecayDisconnectedError,
        );
      },
    );
  });

  it("propagates caller abort without a rollback claim", async () => {
    await withServer(
      [
        (request, response) => {
          request.once("aborted", () => response.destroy());
        },
      ],
      async (baseUrl) => {
        const client = createClient({
          baseUrl,
          projectId: "project.sdk",
          token: "sdk-secret",
        });
        const controller = new AbortController();
        const pending = (async () => {
          for await (const _event of client.streamOperation(
            "request.operation",
            { signal: controller.signal },
          )) {
            // Drain the stream.
          }
        })();
        controller.abort("caller stopped waiting");

        const aborted = await pending.catch((error: unknown) => error);
        expect(aborted).toBeInstanceOf(TraceDecayAbortError);
        expect((aborted as Error).message).not.toMatch(/rollback|rolled back/i);
      },
    );
  });
});
