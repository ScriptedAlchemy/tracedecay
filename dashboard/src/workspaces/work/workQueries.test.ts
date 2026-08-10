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

import { workGraphReadRequest } from "./workViewsQueries.ts";
import {
  WORK_MUTATE_GRAPH_ROUTE,
  WORK_TOPOLOGY_ROUTE,
  WORK_VIEWS_ROUTE,
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
    expect(scopedUrl({ kind: "all" }, WORK_VIEWS_ROUTE.path)).toBe(
      "/api/work/views",
    );
  });

  /** The defect this guards: a selected project must be asked about by name, or
   * the board would answer with the active project's tasks. */
  it("names the project a selected scope is asking about", () => {
    expect(scopedUrl(project("selected"), WORK_VIEWS_ROUTE.path)).toBe(
      "/api/projects/project.beta/work/views",
    );
    expect(scopedUrl(project("unresolved"), WORK_VIEWS_ROUTE.path)).toBe(
      "/api/projects/project.beta/work/views",
    );
  });

  it("routes the canonical execution-topology projection through the selected project", () => {
    expect(WORK_TOPOLOGY_ROUTE.operation).toBe("operation.work.topology");
    expect(scopedUrl(project("selected"), WORK_TOPOLOGY_ROUTE.path)).toBe(
      "/api/projects/project.beta/work/topology",
    );
  });
});

describe("which Work product authority is read", () => {
  it("uses the exact daemon-resolved repository after bootstrap", () => {
    expect(
      workGraphReadRequest(123, {
        project_id: "project.beta",
        repository_id: "repository.beta",
        worktree_id: "worktree.beta",
        reference: null,
        scope_digest: "sha256:scope",
      }),
    ).toMatchObject({
      selection: {
        selection: "relations",
        relation_scopes: [
          {
            kind: "repository",
            project_id: "project.beta",
            repository_id: "repository.beta",
          },
        ],
      },
    });
  });

  it("keeps the profile-owned bootstrap when no resolved scope exists", () => {
    expect(workGraphReadRequest(123).selection).toEqual({
      selection: "profile_owned_no_git",
    });
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
    expect(scopedUrl({ kind: "all" }, WORK_MUTATE_GRAPH_ROUTE.path)).toBe(
      "/api/work/mutate-graph",
    );
  });
});
