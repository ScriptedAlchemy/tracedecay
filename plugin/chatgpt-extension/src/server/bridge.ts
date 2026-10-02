import {
  createClient,
  TraceDecayAuthenticationError,
  TraceDecayDisconnectedError,
  TraceDecayProblemError,
  TraceDecayProtocolError,
  TraceDecayTransportError,
  type TraceDecayClient,
} from "@tracedecay/sdk";
import type {
  OperationApplicationCodeCalleesResult,
  OperationApplicationCodeCallersResult,
  OperationApplicationCodeSymbolSearchResult,
  OperationApplicationImpactResult,
  OperationApplicationNodeResult,
  OperationApplicationSearchResult,
  OperationApplicationStatusResult,
  PublicCodeProject,
} from "@tracedecay/sdk";
import type { DaemonState, Failure, ProjectRef } from "../shared/view.js";
import { httpBaseUrl, probeDaemon, readDaemonAuthority, type AuthorityLookup, type DaemonAuthorityRecord } from "./authority.js";
import { classifyCode, DaemonFailure, ServeSession, toFailure } from "./serve-client.js";

export type BridgeOptions = {
  readonly binary: string;
  readonly profileRoot: string;
  readonly env: NodeJS.ProcessEnv;
  readonly cwd: string;
};

type ProjectHandle = {
  readonly project: ProjectRef;
  readonly session: ServeSession;
  readonly client: TraceDecayClient;
  /** Daemon process run the HTTP token belongs to; a restart invalidates it. */
  readonly authority: DaemonAuthorityRecord;
};

/**
 * The only path from the extension to TraceDecay data. Registry reads go
 * through a project-less `tracedecay serve`; project reads go through a
 * `tracedecay serve --path <root>` session whose root comes from the daemon's
 * own project registry, so an unregistered project id can never reach a
 * project session. HTTP reads carry the daemon authority token, which stays
 * in this process.
 */
export class DaemonBridge {
  readonly #options: BridgeOptions;
  #registry: ServeSession | null = null;
  readonly #projects = new Map<string, ProjectHandle>();
  readonly #pending = new Map<string, Promise<ProjectHandle>>();
  #closing = false;

  constructor(options: BridgeOptions) {
    this.#options = options;
  }

  async authority(): Promise<AuthorityLookup> {
    return readDaemonAuthority(this.#options.profileRoot);
  }

  async daemonState(): Promise<DaemonState> {
    const lookup = await this.authority();
    if (lookup.kind !== "available") return { state: "disconnected", failure: lookup.failure };
    const unreachable = await probeDaemon(lookup.record);
    if (unreachable !== null) return { state: "disconnected", failure: unreachable };
    return {
      state: "connected",
      profile_root: lookup.record.profile_root,
      version: lookup.record.version,
      pid: lookup.record.pid,
    };
  }

  async #requireAuthority(): Promise<DaemonAuthorityRecord> {
    const lookup = await this.authority();
    if (lookup.kind !== "available") throw new DaemonFailure(lookup.failure);
    return lookup.record;
  }

  #registrySession(): ServeSession {
    if (this.#registry === null || this.#registry.closed) {
      this.#registry = new ServeSession({
        binary: this.#options.binary,
        projectRoot: null,
        env: this.#options.env,
        cwd: this.#options.cwd,
      });
    }
    return this.#registry;
  }

  #registryClient(): TraceDecayClient {
    return createClient({
      baseUrl: "http://127.0.0.1",
      projectId: "registry",
      token: "unused-registry-reads-use-mcp",
      mcp: {
        callTool: (toolName, request, options) =>
          this.#registrySession().callJson(toolName, asRecord(request), options.signal),
      },
    });
  }

  async listProjects(signal?: AbortSignal): Promise<readonly ProjectRef[]> {
    await this.#requireAuthority();
    const client = this.#registryClient();
    const listed = await runSdk(() => client.operations.project_list({ limit: 10 }, optional(signal)));
    return listed.projects.map(projectRef);
  }

  async resolveProject(projectId: string, signal?: AbortSignal): Promise<ProjectRef> {
    await this.#requireAuthority();
    const client = this.#registryClient();
    const context = await runSdk(() => client.operations.project_context({ project_selector: { project_id: projectId } }, optional(signal)));
    if (context.status === "not_found") {
      const cached = this.#projects.get(projectId);
      this.#projects.delete(projectId);
      await cached?.session.close();
      throw new DaemonFailure({
        kind: "denied",
        code: "project_not_registered",
        message: `Project ${projectId} is not registered in this TraceDecay profile; only registered projects can be explored.`,
      });
    }
    return projectRef(context.project);
  }

  async #handle(projectId: string, signal?: AbortSignal): Promise<ProjectHandle> {
    if (this.#closing) throw bridgeClosed();
    const project = await this.resolveProject(projectId, signal);
    let pending = this.#pending.get(projectId);
    if (pending === undefined) {
      // One acquisition at a time per project: two concurrent callers must not
      // each spawn a serve child and orphan the loser's process. The shared
      // work takes no caller's signal — one cancelled request must not abort
      // an acquisition its other waiters still need.
      const acquire = this.#acquire(project).finally(() => {
        if (this.#pending.get(projectId) === acquire) this.#pending.delete(projectId);
      });
      // Every waiter races its own signal, so the stored promise can settle
      // with no one left awaiting it.
      void acquire.catch(() => undefined);
      this.#pending.set(projectId, acquire);
      pending = acquire;
    }
    return await withSignal(pending, signal);
  }

  async #acquire(project: ProjectRef): Promise<ProjectHandle> {
    const projectId = project.project_id;
    const record = await this.#requireAuthority();
    const cached = this.#projects.get(projectId);
    if (cached !== undefined) {
      if (!cached.session.closed && cached.authority.process_run_id === record.process_run_id && cached.project.project_root === project.project_root) return cached;
      this.#projects.delete(projectId);
      await cached.session.close();
    }
    const baseUrl = httpBaseUrl(record);
    if (baseUrl === null) {
      throw new DaemonFailure({
        kind: "unavailable",
        code: "http_application_endpoint_absent",
        message: "The daemon has not published its HTTP application endpoint yet.",
      });
    }
    const session = new ServeSession({
      binary: this.#options.binary,
      projectRoot: project.project_root,
      env: this.#options.env,
      cwd: project.project_root,
    });
    const client = createClient({
      baseUrl,
      projectId: project.project_id,
      token: record.auth_token,
      mcp: {
        callTool: (toolName, request, options) => session.callJson(toolName, asRecord(request), options.signal),
      },
    });
    const handle: ProjectHandle = { project, session, client, authority: record };
    if (this.#closing) {
      // Shutdown raced this acquisition: never cache the child or let it
      // outlive close().
      await session.close();
      throw bridgeClosed();
    }
    this.#projects.set(projectId, handle);
    return handle;
  }

  async status(projectId: string, signal?: AbortSignal): Promise<{ status: OperationApplicationStatusResult; authority: DaemonAuthorityRecord }> {
    const { client, authority } = await this.#handle(projectId, signal);
    return { status: await runSdk(() => client.operations.status({}, optional(signal))), authority };
  }

  async search(projectId: string, query: string, limit: number, signal?: AbortSignal): Promise<OperationApplicationSearchResult> {
    const { client } = await this.#handle(projectId, signal);
    return runSdk(() => client.operations.search({ query, limit, prefer_symbol: true }, optional(signal)));
  }

  async node(projectId: string, nodeId: string, signal?: AbortSignal): Promise<OperationApplicationNodeResult> {
    const { client } = await this.#handle(projectId, signal);
    return runSdk(() => client.operations.node({ node_id: nodeId }, optional(signal)));
  }

  async impact(projectId: string, nodeId: string, maxDepth: number, signal?: AbortSignal): Promise<OperationApplicationImpactResult> {
    const { client } = await this.#handle(projectId, signal);
    return runSdk(() => client.operations.impact({ node_id: nodeId, max_depth: maxDepth }, optional(signal)));
  }

  async callers(projectId: string, nodeId: string, depth: number, signal?: AbortSignal): Promise<OperationApplicationCodeCallersResult> {
    const { client } = await this.#handle(projectId, signal);
    return runSdk(() =>
      client.operations.code_callers(
        { node_id: nodeId, maximum_depth: depth, meta: { order: "stable_identity", projection: "summary" } },
        optional(signal),
      ),
    ).then(payloadOf);
  }

  async callees(
    projectId: string,
    nodeId: string,
    generation: string,
    depth: number,
    signal?: AbortSignal,
  ): Promise<OperationApplicationCodeCalleesResult> {
    const { client } = await this.#handle(projectId, signal);
    return runSdk(() =>
      client.operations.code_callees(
        {
          node_id: nodeId,
          maximum_depth: depth,
          scope: { generation },
          meta: { order: "stable_identity", projection: "summary" },
        },
        optional(signal),
      ),
    ).then(payloadOf);
  }

  async symbolSearch(projectId: string, query: string, signal?: AbortSignal): Promise<OperationApplicationCodeSymbolSearchResult> {
    const { client } = await this.#handle(projectId, signal);
    return runSdk(() =>
      client.operations.code_symbol_search(
        {
          query,
          lazy_index_ignored_dependencies: false,
          scope: {},
          meta: { order: "relevance", projection: "summary" },
        },
        optional(signal),
      ),
    ).then(payloadOf);
  }

  async close(): Promise<void> {
    this.#closing = true;
    // In-flight acquisitions close their own children once they see the flag;
    // wait for them so a session created mid-race cannot escape shutdown.
    await Promise.allSettled([...this.#pending.values()]);
    const sessions = [...this.#projects.values()].map((handle) => handle.session);
    if (this.#registry !== null) sessions.push(this.#registry);
    this.#projects.clear();
    this.#registry = null;
    await Promise.all(sessions.map((session) => session.close()));
  }
}

function optional(signal: AbortSignal | undefined): { signal?: AbortSignal } {
  return signal === undefined ? {} : { signal };
}

function bridgeClosed(): DaemonFailure {
  return new DaemonFailure({
    kind: "unavailable",
    code: "bridge_closed",
    message: "the extension bridge is shutting down",
  });
}

// Shared work outlives any single request: a caller that cancels gets its own
// abort while the promise it shared keeps running for the remaining waiters.
async function withSignal<T>(promise: Promise<T>, signal?: AbortSignal): Promise<T> {
  if (signal === undefined) return await promise;
  const abortError = () =>
    signal.reason instanceof Error ? signal.reason : new DOMException("This operation was aborted", "AbortError");
  if (signal.aborted) throw abortError();
  return await new Promise<T>((resolve, reject) => {
    const onAbort = () => reject(abortError());
    signal.addEventListener("abort", onAbort, { once: true });
    promise.then(resolve, reject).finally(() => signal.removeEventListener("abort", onAbort));
  });
}

function asRecord(request: unknown): Record<string, unknown> {
  if (typeof request === "object" && request !== null && !Array.isArray(request)) {
    return request as Record<string, unknown>;
  }
  throw new DaemonFailure({ kind: "invalid_request", code: "non_object_request", message: "MCP tool requests must be objects" });
}

type HttpSuccess<T> = { outcome: { outcome: string; value: { payload: T | null } } };

function payloadOf<T>(envelope: HttpSuccess<T>): T {
  const payload = envelope.outcome.value.payload;
  if (payload === null) {
    throw new DaemonFailure({
      kind: "unavailable",
      code: "empty_payload",
      message: `the daemon returned a ${envelope.outcome.outcome} outcome without a payload`,
    });
  }
  return payload;
}

async function runSdk<T>(operation: () => Promise<T>): Promise<T> {
  try {
    return await operation();
  } catch (error) {
    throw new DaemonFailure(failureFromSdk(error), { cause: error });
  }
}

export function failureFromSdk(error: unknown): Failure {
  if (error instanceof DaemonFailure) return error.failure;
  if (error instanceof TraceDecayProblemError) {
    const kind = error.problem.kind;
    const code = `${kind}/${error.problem.code}`;
    return { kind: problemKind(kind, error.status), code, message: error.problem.message };
  }
  if (error instanceof TraceDecayAuthenticationError) {
    return { kind: "protocol", code: "daemon_authentication", message: error.message };
  }
  if (error instanceof TraceDecayDisconnectedError) {
    return { kind: "disconnected", code: "daemon_disconnected", message: error.message };
  }
  if (error instanceof TraceDecayTransportError) {
    if (error.cause !== undefined) return toFailure(error.cause);
    return { kind: "disconnected", code: "transport", message: error.message };
  }
  if (error instanceof TraceDecayProtocolError) {
    return { kind: "protocol", code: error.name, message: error.message };
  }
  return toFailure(error);
}

function problemKind(kind: string, status: number): Failure["kind"] {
  if (kind === "not_found_or_not_authorized" || status === 403) return "denied";
  if (status === 404) return "not_found";
  return classifyCode(kind);
}

export function projectRef(project: PublicCodeProject): ProjectRef {
  return {
    project_id: project.project_id,
    label: project.label,
    project_root: project.project_root,
    head_branch: project.head_branch ?? null,
    default_branch: project.default_branch ?? null,
  };
}
