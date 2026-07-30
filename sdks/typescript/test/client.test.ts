import { once } from "node:events";
import {
  createServer,
  type IncomingMessage,
  type Server,
  type ServerResponse,
} from "node:http";

import { describe, expect, it } from "vitest";

import type { OperationDescriptor } from "../src/operations";
import { OPERATIONS, UNAVAILABLE_OPERATIONS } from "../src/operations";
import {
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
import { SERVER_OPERATIONS } from "../src/server-operations";

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

const TEST_OPERATION: OperationDescriptor<
  "work_snapshot",
  Record<string, unknown>,
  unknown
> = {
  operation: "work_snapshot",
  operationId: "operation.work.snapshot",
  route: "/application/work/snapshot",
  method: "POST",
  bindingId: "binding.http.work.snapshot",
  requestSchema: {
    schemaId: "schema.work.snapshot.request",
    revision: 1,
  },
  resultSchema: {
    schemaId: "schema.work.snapshot.result",
    revision: 1,
  },
  cancellation: {
    mode: "cooperative",
    points: ["before_admission", "before_read", "during_read"],
  },
  decodeRequest(value) {
    if (typeof value !== "object" || value === null || Array.isArray(value)) {
      throw new TypeError("test request must be an object");
    }
    return value as Record<string, unknown>;
  },
  decodeResult(value) {
    return value;
  },
  decodeSuccess(value) {
    return decodeHttpSuccessEnvelope(
      value,
      "binding.http.work.snapshot",
      "schema.work.snapshot.result",
      1,
      (payload) => payload,
    );
  },
};

function requestThroughTransport(
  client: ReturnType<typeof createClient>,
  options: OperationRequestOptions = {},
): Promise<HttpSuccessEnvelope<unknown>> {
  const transport = client as unknown as {
    requestOperation<Request, Result>(
      descriptor: OperationDescriptor<string, Request, Result>,
      request: unknown,
      requestOptions?: OperationRequestOptions,
    ): Promise<HttpSuccessEnvelope<Result>>;
  };
  return transport.requestOperation(TEST_OPERATION, {}, options);
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

describe("TraceDecayClient generated operation bindings", () => {
  it("publishes typed Work methods and fail-closed base discovery", () => {
    type Equal<Left, Right> =
      (<Value>() => Value extends Left ? 1 : 2) extends
      (<Value>() => Value extends Right ? 1 : 2) ? true : false;
    const requestMatches: Equal<
      Parameters<ReturnType<typeof createClient>["operations"]["work_snapshot"]>[0],
      { readonly page_size: number }
    > = true;
    const client = createClient({
      baseUrl: "http://127.0.0.1:43123",
      projectId: "project.sdk",
      token: "sdk-secret",
    });

    expect(requestMatches).toBe(true);
    expect(SERVER_OPERATIONS).toHaveLength(64);
    expect(
      SERVER_OPERATIONS.every(
        (operation) =>
          operation.sdkAvailability === "unavailable" &&
          operation.disposition === "schema_unavailable",
      ),
    ).toBe(true);
    expect(SERVER_OPERATIONS.map((operation) => operation.operation)).toContain(
      "git_status",
    );
    expect("health_read" in client.operations).toBe(false);
    // @ts-expect-error Base routes have no canonical schema bodies yet.
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
  });

  it("publishes all mounted Work routes as executable operations", () => {
    const available = OPERATIONS.map((operation) => operation.operation);
    expect(OPERATIONS).toHaveLength(17);
    expect(UNAVAILABLE_OPERATIONS).toHaveLength(0);
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
      ]),
    );
    expect(
      "work_snapshot" in createClient({
        baseUrl: "http://127.0.0.1:43123",
        projectId: "project.sdk",
        token: "sdk-secret",
      }).operations,
    ).toBe(true);
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
