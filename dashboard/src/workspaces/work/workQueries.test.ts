/**
 * Which URL a Work call is sent to, and which scopes accept a command.
 *
 * This is the one place the Work surface can go wrong quietly. Sending every
 * scope unprefixed would show the active project's tasks under a different
 * project's name; sending a command to a project the gateway serves read-only
 * would report a routing refusal as though the command itself had been
 * rejected.
 */
import { describe, expect, it } from "vitest";
import {
  type DashboardScope,
  scopedUrl,
  scopeWritable,
} from "../../data/scope/store.ts";

import { resumeCursor } from "./workQueries.ts";
import {
  WORK_ACCEPT_TASK_ROUTE,
  WORK_SNAPSHOT_ROUTE,
  WORK_TOPOLOGY_ROUTE,
} from "./workRoutes.ts";

function project(
  activation: "active" | "selected" | "unresolved",
): DashboardScope {
  return {
    kind: "project",
    projectId: "project.beta",
    label: "Beta",
    activation,
  };
}

describe("where a Work read is sent", () => {
  it("leaves the aggregate view and the active project unprefixed", () => {
    expect(scopedUrl({ kind: "all" }, WORK_SNAPSHOT_ROUTE.path)).toBe(
      "/api/work/snapshot",
    );
  });

  /** The defect this guards: a selected project must be asked about by name, or
   * the board would answer with the active project's tasks. */
  it("names the project a selected scope is asking about", () => {
    expect(scopedUrl(project("selected"), WORK_SNAPSHOT_ROUTE.path)).toBe(
      "/api/projects/project.beta/work/snapshot",
    );
    expect(scopedUrl(project("unresolved"), WORK_SNAPSHOT_ROUTE.path)).toBe(
      "/api/projects/project.beta/work/snapshot",
    );
  });

  it("routes the canonical execution-topology projection through the selected project", () => {
    expect(WORK_TOPOLOGY_ROUTE.operation).toBe("operation.work.topology");
    expect(scopedUrl(project("selected"), WORK_TOPOLOGY_ROUTE.path)).toBe(
      "/api/projects/project.beta/work/topology",
    );
  });
});

describe("which scopes accept a Work command", () => {
  it("accepts a command for the active project and the aggregate view", () => {
    expect(scopeWritable({ kind: "all" }).state).toBe("writable");
    expect(scopeWritable(project("active")).state).toBe("writable");
  });

  /** A selected project is read-only at the gateway, so the command is refused
   * here with the gateway's own reason rather than sent and 405'd. */
  it("refuses a command for a selected project, and names it", () => {
    const writability = scopeWritable(project("selected"));
    expect(writability.state).toBe("read_only");
    expect(writability.state === "read_only" && writability.reason).toContain(
      "Beta",
    );
  });

  /** An unresolved activation is not a licence to guess. */
  it("does not treat an unresolved activation as writable", () => {
    expect(scopeWritable(project("unresolved")).state).toBe("unknown");
  });

  it("sends an accepted command to the scope it was written for", () => {
    expect(scopedUrl({ kind: "all" }, WORK_ACCEPT_TASK_ROUTE.path)).toBe(
      "/api/work/accept-task",
    );
  });
});

describe("continuing a snapshot", () => {
  it("offers a cursor only where the daemon gave one", () => {
    expect(
      resumeCursor({ state: "complete", returned: 2, total: 2 }),
    ).toBeUndefined();
    expect(
      resumeCursor({
        state: "capped",
        cap: 1,
        returned: 1,
        total: 9,
        cursor: { generation_id: "g", token: "resume" },
        range: { start_exclusive: 0, end_inclusive: 1 },
      }),
    ).toEqual({ generation_id: "g", token: "resume" });
    expect(
      resumeCursor({
        state: "partial",
        returned: 1,
        total: 9,
        cursor: { generation_id: "g", token: "resume-2" },
        range: { start_exclusive: 0, end_inclusive: 1 },
      }),
    ).toEqual({ generation_id: "g", token: "resume-2" });
  });
});
