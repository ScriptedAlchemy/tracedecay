import { useMutation, useQueryClient } from "@tanstack/react-query";
import {
  scopeKey,
  scopedUrl,
  scopeWritable,
  useScope,
} from "../../data/scope/store.ts";
import { callWork, type WorkResult, type WorkRoute } from "./workApi.ts";

/**
 * Work reads and commands as queries.
 *
 * Reads go through `scopedUrl`, the same rewrite every other scoped surface
 * uses: unprefixed for the active project and the aggregate view, and
 * `/api/projects/{id}/work/...` for a selected one. The project gateway serves
 * a selected project's Work reads from that project's own graph, so what comes
 * back belongs to the project the scope bar names.
 *
 * Commands do not follow. The gateway serves every non-active project
 * read-only, so a command outside the active project would be refused there.
 * `scopeWritable` is the one place that rule is stated, and Work reads it
 * rather than keeping a second account of the same thing.
 */

/** The refusal a command outside the writable scope reports, without issuing a
 * request.
 *
 * `locked` rather than `denied`: nothing was refused by an authority, the
 * gateway simply will not accept a write for this scope, and the remedy is to
 * change scope rather than to gain permission. */
function notWritable<T>(reason: string): WorkResult<T> {
  return { outcome: "refused", state: "locked", detail: reason };
}

/**
 * One Work command.
 *
 * A command that lands invalidates every Work read rather than splicing its
 * returned receipt into the cached graph. A product graph is a coherent version
 * with one verified runtime projection; splicing a newer event into it would
 * assemble a head the daemon never produced. Refetching costs a round trip and
 * keeps the page showing a state that actually existed.
 */
export function useWorkCommand<Request, Response>(
  route: WorkRoute<Request, Response>,
) {
  const scope = useScope((state) => state.scope);
  const client = useQueryClient();
  const writability = scopeWritable(scope);
  return useMutation<WorkResult<Response>, never, Request>({
    mutationKey: ["work", "command", route.operation, scopeKey(scope)],
    mutationFn: (request: Request) =>
      writability.state === "writable"
        ? callWork(route, request, scopedUrl(scope, route.path))
        : Promise.resolve(notWritable<Response>(writability.reason)),
    onSuccess: (result) => {
      // Only a committed command changes what a read would return. Invalidating
      // on a refusal would refetch on every rejected keystroke and, worse, make
      // a denied command look like it had moved something.
      if (result.outcome === "value") {
        void client.invalidateQueries({ queryKey: ["work"] });
      }
    },
  });
}

/**
 * A Work action that observes or prepares authority without changing the graph.
 * Unlike commands it remains available through the selected-project read
 * gateway, and it deliberately does not invalidate graph reads.
 */
export function useWorkReadAction<Request, Response>(
  route: WorkRoute<Request, Response>,
) {
  const scope = useScope((state) => state.scope);
  return useMutation<WorkResult<Response>, never, Request>({
    mutationKey: ["work", "read-action", route.operation, scopeKey(scope)],
    mutationFn: (request: Request) =>
      callWork(route, request, scopedUrl(scope, route.path)),
  });
}
