// Decoder tests for the canonical hand-maintained wire boundary
// (src/contracts/generated.ts), verifying it decodes the read_model.rs
// envelope shape and keeps the closed unions honest.
import { describe, it, expect } from "vitest";
import { z } from "zod";
import {
  DomainStateSchema,
  EnvelopeSchema,
  StorageFindingsPayloadSchema,
  StorageTelemetryPayloadSchema,
  WIRE_SCHEMA_REVISION,
  assertNever,
  type WireDomainState,
} from "../../src/contracts/generated.ts";

const PayloadSchema = z.object({ ok: z.boolean() });

function readyEnvelope(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    schema_revision: WIRE_SCHEMA_REVISION,
    scope: { project_id: "p", storage_mode: "profile_sharded", store_root: "/store" },
    version: { entity_version: null, graph_version: null },
    time: { valid_time_micros: null, observation_time_micros: 123 },
    source_watermark: null,
    authorization: { outcome: "authorized" },
    coverage: {
      completeness: "complete",
      eligible: 1,
      examined: 1,
      matched: 1,
      excluded: 0,
      omitted: 0,
      unknown: 0,
      denominator: 1,
      unit: "stores",
      omission_reasons: [],
    },
    freshness: { state: "fresh", observed_at_micros: 123, watermark: null },
    domain_state: "ready",
    legal_actions: [{ kind: "refresh", operation: "use-case.dashboard.refresh" }],
    payload: { ok: true },
    ...overrides,
  };
}

describe("wire domain-state decoder", () => {
  it("decodes a known variant", () => {
    expect(DomainStateSchema.parse("ready")).toBe("ready");
  });

  it("keeps `unsupported` and `unsupported_schema` distinct (both server-canonical)", () => {
    expect(DomainStateSchema.parse("unsupported")).toBe("unsupported");
    expect(DomainStateSchema.parse("unsupported_schema")).toBe("unsupported_schema");
  });

  it("maps an UNKNOWN value to unsupported_schema instead of throwing", () => {
    expect(DomainStateSchema.parse("brand_new_state")).toBe("unsupported_schema");
    expect(DomainStateSchema.parse(42)).toBe("unsupported_schema");
  });

  it("is exhaustively switchable with assertNever (never-checked)", () => {
    function label(state: WireDomainState): string {
      switch (state) {
        case "loading":
        case "complete_zero_findings":
        case "ready":
        case "partial":
        case "stale":
        case "locked":
        case "denied":
        case "unauthorized":
        case "redacted":
        case "conflicting":
        case "offline":
        case "unknown":
        case "cancelled":
        case "timed_out":
        case "error":
        case "unsupported_schema":
        case "unsupported":
          return state;
        default:
          return assertNever(state);
      }
    }
    expect(label("ready")).toBe("ready");
    expect(label("unsupported")).toBe("unsupported");
  });
});

describe("wire envelope decoder", () => {
  const Envelope = EnvelopeSchema(PayloadSchema);

  it("decodes a complete envelope carrying every normative field", () => {
    const parsed = Envelope.parse(readyEnvelope());
    expect(parsed.schema_revision).toBe(WIRE_SCHEMA_REVISION);
    expect(parsed.scope.store_root).toBe("/store");
    expect(parsed.authorization.outcome).toBe("authorized");
    expect(parsed.domain_state).toBe("ready");
    expect(parsed.coverage.denominator).toBe(1);
    expect(parsed.legal_actions[0]!.operation).toBe("use-case.dashboard.refresh");
    expect(parsed.payload.ok).toBe(true);
  });

  it("downgrades an unknown domain state inside the envelope to unsupported_schema", () => {
    const parsed = Envelope.parse(readyEnvelope({ domain_state: "future_state" }));
    expect(parsed.domain_state).toBe("unsupported_schema");
  });

  it("rejects a wrong schema_revision (hard-fail on drift)", () => {
    const res = Envelope.safeParse(readyEnvelope({ schema_revision: 2 }));
    expect(res.success).toBe(false);
  });

  it("allows a null coverage denominator (unknown denominator)", () => {
    const env = readyEnvelope();
    (env.coverage as Record<string, unknown>).completeness = "unknown";
    (env.coverage as Record<string, unknown>).denominator = null;
    expect(Envelope.safeParse(env).success).toBe(true);
  });

  it("fails when a required envelope field is missing (typed decode error)", () => {
    const bad = readyEnvelope();
    delete bad.authorization;
    expect(Envelope.safeParse(bad).success).toBe(false);
  });
});

describe("wire storage payload decoders", () => {
  it("decodes a storage telemetry payload", () => {
    const parsed = StorageTelemetryPayloadSchema.parse({
      stores: [
        {
          store: "s",
          role: "graph",
          path: "/p",
          read: {
            kind: "observed",
            sample: {
              store: "s",
              page_size_bytes: 4096,
              page_count: 10,
              freelist_pages: 0,
              observed_at: 1,
            },
          },
          total_bytes: 100,
          free_bytes: 0,
          free_page_ratio: 0,
          budget: { state: "unsupported", reason: "not wired" },
          growth: { state: "absent", reason: "no prior sample" },
        },
      ],
      budget_note: "n",
      growth_note: "m",
    });
    expect(parsed.stores[0]!.read.kind).toBe("observed");
  });

  it("decodes a storage findings payload", () => {
    const parsed = StorageFindingsPayloadSchema.parse({
      kinds: [
        {
          kind: "orphan_store",
          state: "absent",
          required_source: "orphan_store_census",
          reason: "r",
        },
      ],
      note: "n",
    });
    expect(parsed.kinds[0]!.kind).toBe("orphan_store");
  });
});
