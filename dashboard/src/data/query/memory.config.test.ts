import { describe, expect, it } from "vitest";

import {
  CurationConfigMutationSchema,
  CurationConfigPayloadSchema,
} from "./memory.ts";

describe("automatic curation configuration wire shapes", () => {
  it("requires revision CAS and idempotency alongside the enabled setting", () => {
    expect(
      CurationConfigMutationSchema.safeParse({ enabled: false }).success,
    ).toBe(false);
    expect(
      CurationConfigMutationSchema.safeParse({
        enabled: false,
        expected_revision_id: "configuration.revision.test",
        idempotency_key: "idempotency.dashboard-settings.test",
      }).success,
    ).toBe(true);
  });

  it("rejects retired browser-owned policy toggles", () => {
    expect(
      CurationConfigMutationSchema.safeParse({
        enabled: true,
        auto_apply_memory_ops: true,
        expected_revision_id: "configuration.revision.test",
        idempotency_key: "idempotency.dashboard-settings.test",
      }).success,
    ).toBe(false);
  });

  it("requires the daemon pinned source and revision on reads", () => {
    const result = CurationConfigPayloadSchema.safeParse({
      configuration_revision_id: "configuration.revision.test",
      source: "daemon_pinned_snapshot",
      effective: {
        schema_version: 1,
        enabled: true,
        backend: "disabled",
        host_mode: "standalone",
        model_id: null,
        timeout_secs: 60,
        scheduler_tick_secs: 60,
        combine_due_tasks: true,
        allow_job_commands: false,
        tasks: {
          memory_curator: {
            enabled: false,
            schedule: null,
            interval_secs: null,
            cooldown_secs: null,
            min_idle_secs: null,
            stale_lock_secs: null,
          },
          session_reflector: {
            enabled: false,
            schedule: null,
            interval_secs: null,
            cooldown_secs: null,
            min_idle_secs: null,
            stale_lock_secs: null,
          },
          skill_writer: {
            enabled: false,
            schedule: null,
            interval_secs: null,
            cooldown_secs: null,
            min_idle_secs: null,
            stale_lock_secs: null,
          },
        },
      },
      backend_availability: {
        backend: "disabled",
        available: false,
        executable: null,
        reason: "disabled",
      },
    });
    expect(result.success).toBe(true);
  });
});
