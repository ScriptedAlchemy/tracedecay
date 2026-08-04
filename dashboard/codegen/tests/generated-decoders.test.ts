// Decoder tests for the generated public contract boundary, verifying it
// decodes the read_model.rs envelope shape and keeps closed unions honest.
import { describe, it, expect } from "vitest";
import { z } from "zod";
import {
  DashboardDomainStateV1Schema,
  DashboardEnvelopeV1Schema,
  StorageFindingsPayloadV1Schema,
  StorageTelemetryPayloadV1Schema,
  WIRE_SCHEMA_REVISION,
  assertNever,
  type DashboardDomainStateV1,
} from "../../src/contracts/index.ts";

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
    expect(DashboardDomainStateV1Schema.parse("ready")).toBe("ready");
  });

  it("keeps `unsupported` and `unsupported_schema` distinct (both server-canonical)", () => {
    expect(DashboardDomainStateV1Schema.parse("unsupported")).toBe("unsupported");
    expect(DashboardDomainStateV1Schema.parse("unsupported_schema")).toBe("unsupported_schema");
  });

  it("maps an UNKNOWN value to unsupported_schema instead of throwing", () => {
    expect(DashboardDomainStateV1Schema.parse("brand_new_state")).toBe("unsupported_schema");
    expect(DashboardDomainStateV1Schema.parse(42)).toBe("unsupported_schema");
  });

  it("is exhaustively switchable with assertNever (never-checked)", () => {
    function label(state: DashboardDomainStateV1): string {
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
  const Envelope = DashboardEnvelopeV1Schema(PayloadSchema);

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
  const tableGrowthCoverage = {
    completeness: "complete",
    eligible: 1,
    examined: 1,
    matched: null,
    excluded: null,
    omitted: 0,
    unknown: null,
    denominator: 1,
    unit: "store_table_growth_reads",
    omission_reasons: [],
  };
  const tableGrowthPayload = {
    table_growth_coverage: tableGrowthCoverage,
    table_growth_threshold: {
      absolute_bytes: 67_108_864,
      relative_floor_bytes: 1_048_576,
      relative_percent: 10,
    },
  };

  it("decodes a storage telemetry payload", () => {
    const parsed = StorageTelemetryPayloadV1Schema.parse({
      stores: [
        {
          store: "s",
          role: "graph",
          roles: ["graph", "memory"],
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
          budget: {
            state: "evaluated",
            evaluation: { state: "over_budget", observed: 100, soft_limit: 50, overage: 50 },
            setting_key: "sync.retention.v1 store_soft_budgets_bytes",
            reason: "evaluated against the owner-configured soft limit of 50 bytes",
          },
          growth: {
            state: "unknown",
            reason: "no execution-owned store-size watermark is available",
          },
          table_growth: {
            state: "observed",
            coverage: tableGrowthCoverage,
            significant_samples: [],
            omissions: [],
            omission_reasons: [],
          },
        },
      ],
      budget_note: "n",
      growth_note: "m",
      ...tableGrowthPayload,
    });
    expect(parsed.stores[0]!.read.kind).toBe("observed");
    expect(parsed.stores[0]!.roles).toEqual(["graph", "memory"]);
    // A dashboard read cannot establish its own growth baseline.
    const growth = parsed.stores[0]!.growth;
    expect(growth.state).toBe("unknown");
  });

  it("rejects the retired unsupported budget and absent growth variants", () => {
    const entry = {
      store: "s",
      role: "graph",
      roles: ["graph"],
      path: "/p",
      read: { kind: "unknown", store: "s" },
      total_bytes: null,
      free_bytes: null,
      free_page_ratio: null,
      budget: { state: "unknown", reason: "r" },
      growth: { state: "unknown", reason: "r" },
      table_growth: {
        state: "unknown",
        coverage: {
          ...tableGrowthCoverage,
          completeness: "partial",
          examined: 0,
          omitted: 1,
          omission_reasons: ["measurement unavailable"],
        },
        omission_reasons: ["measurement unavailable"],
      },
    };
    expect(
      StorageTelemetryPayloadV1Schema.safeParse({
        stores: [entry],
        budget_note: "n",
        growth_note: "m",
        ...tableGrowthPayload,
      }).success,
    ).toBe(true);
    // `unsupported` budgets and `absent` growth no longer exist on the wire.
    for (const drift of [
      { ...entry, budget: { state: "unsupported", reason: "not wired" } },
      { ...entry, growth: { state: "absent", reason: "no prior sample" } },
      // `roles` is required: a store must always name every role it serves.
      { ...entry, roles: undefined },
    ]) {
      expect(
        StorageTelemetryPayloadV1Schema.safeParse({
          stores: [drift],
          budget_note: "n",
          growth_note: "m",
          ...tableGrowthPayload,
        }).success,
      ).toBe(false);
    }
  });

  it("decodes a storage findings payload", () => {
    const parsed = StorageFindingsPayloadV1Schema.parse({
      family_filter: "storage",
      entries: [],
      report_coverage: null,
      remediations: [],
      known_families: ["storage"],
      kind_statuses: [
        {
          kind: "orphan_store",
          state: "unsupported",
          observed_entries: 0,
          reason: "r",
        },
      ],
      note: "n",
    });
    expect(parsed.kind_statuses?.[0]?.kind).toBe("orphan_store");
  });
});
