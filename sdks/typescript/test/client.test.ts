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
      binding_id: "binding.http.work.snapshot",
      contract: {
        schema_id: "schema.work.snapshot.result",
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
  return client.operations.work_snapshot({ page_size: 1 }, options);
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
  it("publishes typed Work and Workflow methods from canonical bindings", () => {
    expectTypeOf<
      Parameters<ReturnType<typeof createClient>["operations"]["work_snapshot"]>[0]
    >().toEqualTypeOf<{ readonly page_size: number }>();

    const client = createClient({
      baseUrl: "http://127.0.0.1:43123",
      projectId: "project.sdk",
      token: "sdk-secret",
    });

    expect(new Set(OPERATIONS.map((operation) => operation.operation)).size).toBe(
      OPERATIONS.length,
    );
    expect("health_read" in client.operations).toBe(false);
    // @ts-expect-error Operations without a canonical SDK binding stay absent.
    void client.operations.health_read;
  });

  it("preserves remote base paths and origin policy", async () => {
    let requestedUrl = "";
    let requestedOrigin = "";
    const client = createClient({
      baseUrl: "https://remote.example/api/v1/",
      projectId: "project.sdk",
      token: "sdk-secret",
      origin: "https://consumer.example",
      fetch: async (input, init) => {
        requestedUrl = String(input);
        requestedOrigin = new Headers(init?.headers).get("origin") ?? "";
        return new Response(JSON.stringify(successEnvelope({ status: "ok" })), {
          status: 200,
          headers: { "content-type": "application/json" },
        });
      },
    });

    await expect(
      client.operations.work_snapshot({ page_size: 25 }),
    ).rejects.toBeInstanceOf(TraceDecayMalformedResponseError);

    expect(requestedUrl).toBe(
      "https://remote.example/api/v1/projects/project.sdk/application/work/snapshot",
    );
    expect(requestedOrigin).toBe("https://consumer.example");
  });

  it("fails closed on malformed typed Work requests before transport", async () => {
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
      // @ts-expect-error Deliberately malformed at the package boundary.
      client.operations.work_snapshot({ page_size: "25" }),
    ).rejects.toBeInstanceOf(TypeError);
    expect(fetchCalls).toBe(0);
    expect("invoke" in client).toBe(false);
    expect("requestOperation" in client).toBe(false);
    expect(Reflect.get(client, "requestOperation")).toBeUndefined();
  });

  it("publishes all mounted Work and Workflow routes as executable operations", () => {
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
        "work_snapshot",
        "work_delta",
        "work_create",
        "work_replan_dependencies",
        "work_review_proposal",
        "work_accept_proposal",
        "work_admit_execution",
        "work_attach_runtime_evidence",
        "work_accept_task",
        "workflow_register_definition",
        "workflow_activate_definition",
        "workflow_execute_fan_out",
        "workflow_handoff_issue",
        "workflow_handoff_redeem",
      ]),
    );
    expect(
      "work_snapshot" in createClient({
        baseUrl: "http://127.0.0.1:43123",
        projectId: "project.sdk",
        token: "sdk-secret",
      }).operations,
    ).toBe(true);
    const quarantined = [
      "multi_root_scope_set_read",
      "multi_root_scope_set_compare_and_swap",
      "multi_root_execute",
    ];
    const clientOperations = createClient({
      baseUrl: "http://127.0.0.1:43123",
      projectId: "project.sdk",
      token: "sdk-secret",
    }).operations;
    for (const operation of quarantined) {
      expect(available).not.toContain(operation);
      expect(unavailable).not.toContain(operation);
      expect(operation in clientOperations).toBe(false);
    }
  });

  it("publishes work_attempt_finish with the canonical descriptor identity", () => {
    const client = createClient({
      baseUrl: "http://127.0.0.1:43123",
      projectId: "project.sdk",
      token: "sdk-secret",
    });
    expect("work_attempt_finish" in client.operations).toBe(true);

    const descriptor = OPERATIONS.find(
      (operation) => operation.operation === "work_attempt_finish",
    );
    expect(descriptor).toBeDefined();
    expect(descriptor?.operationId).toBe("operation.work.attempt_finish");
    expect(descriptor?.route).toBe("/application/work/attempt/finish");
    expect(descriptor?.method).toBe("POST");
    expect(descriptor?.effect).toBe("administrative");
    expect(descriptor?.idempotency).toBe("required");
    expect(descriptor?.bindingId).toBe("binding.http.work.attempt_finish");
    expect(descriptor?.requestSchema).toEqual({
      schemaId: "schema.work.attempt_finish.request",
      revision: 1,
    });
    expect(descriptor?.resultSchema).toEqual({
      schemaId: "schema.work.attempt_finish.result",
      revision: 1,
    });
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
            "/projects/project.sdk/application/work/snapshot",
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
