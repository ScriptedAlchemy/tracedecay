import { describe, it, expect } from "vitest";
import {
  DashboardDomainStateSchema,
  DashboardEnvelopeSchema,
  FindingPayloadSchema,
  assertNever,
  SCHEMA_REVISION,
  type DashboardDomainState,
  type DashboardEnvelope,
  type FindingPayload,
} from "../../src/contracts/generated.ts";

function findingEnvelope(domain: unknown): unknown {
  return {
    schema_revision: SCHEMA_REVISION,
    scope: { key: "scope-1", revision: "r1" },
    versions: { entity: "e1", graph: "g1" },
    valid_time: "vt",
    observation_time: "ot",
    watermark: "wm",
    authorization: { outcome: "authorized" },
    coverage: {
      eligible: 10,
      examined: 10,
      matched: 3,
      excluded: 1,
      omitted: 0,
      unknown: 0,
      denominator: 10,
      unit: "rows",
    },
    freshness: { observed_at: "ot", watermark: "wm", stale: false },
    domain_state: domain,
    legal_actions: [{ action_id: "a1", kind: "acknowledge" }],
    payload: {
      finding_id: "f1",
      title: "t",
      summary: "s",
      severity: "high",
      evidence_quality: "corroborated",
    },
  };
}

describe("generated domain-state decoder", () => {
  it("decodes a valid known variant", () => {
    const parsed = DashboardDomainStateSchema.parse({ kind: "ready" });
    expect(parsed).toEqual({ kind: "ready" });
  });

  it("decodes a data-carrying known variant (redacted metadata allowlist)", () => {
    const input: DashboardDomainState = {
      kind: "redacted",
      source_kind: "session",
      source_id: "s1",
      source_revision: "r1",
      locator_class: "opaque",
      display_label: "Redacted item",
      reason_code: "policy",
    };
    expect(DashboardDomainStateSchema.parse(input)).toEqual(input);
  });

  it("maps an UNKNOWN variant to unsupported_schema instead of throwing", () => {
    const parsed = DashboardDomainStateSchema.parse({ kind: "brand_new_state", extra: 1 });
    expect(parsed).toEqual({
      kind: "unsupported_schema",
      raw: { kind: "brand_new_state", extra: 1 },
    });
  });

  it("maps a non-object / missing discriminant to unsupported_schema", () => {
    expect(DashboardDomainStateSchema.parse(42)).toEqual({ kind: "unsupported_schema", raw: 42 });
    expect(DashboardDomainStateSchema.parse({})).toEqual({ kind: "unsupported_schema", raw: {} });
  });

  it("produces a typed decode error for a MALFORMED known variant (not a silent downgrade)", () => {
    // error variant requires code + message; omit them.
    const res = DashboardDomainStateSchema.safeParse({ kind: "error" });
    expect(res.success).toBe(false);
    if (!res.success) {
      expect(res.error.issues.length).toBeGreaterThan(0);
    }
  });

  it("is exhaustively switchable with assertNever", () => {
    function describeState(state: DashboardDomainState): string {
      switch (state.kind) {
        case "loading":
        case "complete_zero_findings":
        case "ready":
        case "partial":
        case "stale":
        case "locked":
        case "denied":
        case "unauthorized":
        case "conflicting":
        case "offline":
        case "unknown":
        case "cancelled":
        case "timed_out":
          return state.kind;
        case "redacted":
          return state.display_label;
        case "error":
          return `${state.code}:${state.message}`;
        case "unsupported_schema":
          return "unsupported";
        default:
          return assertNever(state);
      }
    }
    expect(describeState({ kind: "ready" })).toBe("ready");
    expect(describeState({ kind: "unsupported_schema", raw: null })).toBe("unsupported");
  });
});

describe("generated finding payload decoder", () => {
  it("decodes a valid finding", () => {
    const finding: FindingPayload = {
      finding_id: "f1",
      title: "t",
      summary: "s",
      severity: "critical",
      evidence_quality: "verified",
    };
    expect(FindingPayloadSchema.parse(finding)).toEqual(finding);
  });

  it("rejects an out-of-range severity enum", () => {
    const res = FindingPayloadSchema.safeParse({
      finding_id: "f1",
      title: "t",
      summary: "s",
      severity: "apocalyptic",
      evidence_quality: "verified",
    });
    expect(res.success).toBe(false);
  });
});

describe("generated generic envelope decoder", () => {
  const EnvelopeSchema = DashboardEnvelopeSchema(FindingPayloadSchema);

  it("decodes a complete envelope carrying every normative field", () => {
    const parsed = EnvelopeSchema.parse(findingEnvelope({ kind: "ready" }));
    const envelope: DashboardEnvelope<FindingPayload> = parsed;
    expect(envelope.schema_revision).toBe(SCHEMA_REVISION);
    expect(envelope.domain_state).toEqual({ kind: "ready" });
    expect(envelope.payload.finding_id).toBe("f1");
    expect(envelope.legal_actions[0]!.action_id).toBe("a1");
    expect(envelope.coverage.denominator).toBe(10);
  });

  it("decodes an unknown domain state inside the envelope to unsupported_schema", () => {
    const parsed = EnvelopeSchema.parse(findingEnvelope({ kind: "future_state" }));
    expect(parsed.domain_state).toEqual({
      kind: "unsupported_schema",
      raw: { kind: "future_state" },
    });
  });

  it("fails when a required envelope field is missing (typed decode error)", () => {
    const bad = findingEnvelope({ kind: "ready" }) as Record<string, unknown>;
    delete bad.watermark;
    const res = EnvelopeSchema.safeParse(bad);
    expect(res.success).toBe(false);
  });

  it("allows a null coverage denominator (unknown denominator)", () => {
    const env = findingEnvelope({ kind: "partial" }) as Record<string, unknown>;
    (env.coverage as Record<string, unknown>).denominator = null;
    const res = EnvelopeSchema.safeParse(env);
    expect(res.success).toBe(true);
  });
});
