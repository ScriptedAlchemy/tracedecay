import { describe, expect, it } from "vitest";

import { OPERATIONS } from "../src/operations";

const digest = `sha256:${"a".repeat(64)}`;
const identity = {
  original_operation: "use-case.application.source-edit.str-replace",
  effect_id: "effect.source-edit.retained",
  idempotency_key: "source-edit.original",
  attempt_idempotency_key: "source-edit.reconciliation",
  input_digest: digest,
};

function decode(value: unknown): unknown {
  const operation = OPERATIONS.find(
    (entry) => entry.operation === "application_source_edit_reconcile",
  );
  if (!operation) throw new Error("source edit reconciliation operation missing");
  return operation.decodeRequest(value);
}

describe("source edit reconciliation wire requests", () => {
  it.each([
    { disposition: "inspect", confirm: false },
    { disposition: "confirm_unchanged", confirm: true },
    { disposition: "confirm_committed", committed_state: digest, confirm: true },
    { disposition: "acknowledge_partial", observed_state: digest, confirm: true },
  ])("accepts $disposition and rejects foreign fields", (fields) => {
    const request = { ...identity, ...fields };
    expect(decode(request)).toEqual(request);
    expect(() => decode({ ...request, unexpected: true })).toThrow();
    const wrongField = fields.disposition === "confirm_committed"
      ? "observed_state"
      : "committed_state";
    expect(() => decode({ ...request, [wrongField]: digest })).toThrow();
  });

  it.each(["confirm_committed", "acknowledge_partial", "confirm_rolled_back"])(
    "rejects incomplete or removed disposition %s",
    (disposition) => {
      expect(() => decode({ ...identity, disposition, confirm: true })).toThrow();
    },
  );
});
