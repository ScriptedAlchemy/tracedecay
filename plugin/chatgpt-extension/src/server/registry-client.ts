import {
  OPERATIONS,
  TraceDecayAbortError,
  TraceDecayMalformedResponseError,
  TraceDecayTransportError,
  type OperationApplicationProjectContextRequest,
  type OperationApplicationProjectContextResult,
  type OperationApplicationProjectListRequest,
  type OperationApplicationProjectListResult,
} from "@tracedecay/sdk";

/** Executes a generated MCP tool binding against a projectless daemon session. */
export type RegistryToolCaller = (toolName: string, request: Record<string, unknown>, signal?: AbortSignal) => Promise<unknown>;

type RegistryDescriptor<Req, Res> = {
  readonly operation: string;
  readonly transport: { readonly kind: string; readonly toolName?: string };
  readonly decodeRequest: (value: unknown) => Req;
  readonly decodeResult: (value: unknown) => Res;
};

function registryDescriptor<Req, Res>(operation: string): RegistryDescriptor<Req, Res> {
  const descriptor = OPERATIONS.find((candidate) => candidate.operation === operation);
  if (descriptor === undefined || descriptor.transport.kind !== "mcp_tool") {
    throw new TypeError(`${operation} is not an MCP-backed operation in the canonical catalog`);
  }
  return descriptor as unknown as RegistryDescriptor<Req, Res>;
}

const PROJECT_LIST = registryDescriptor<OperationApplicationProjectListRequest, OperationApplicationProjectListResult>("project_list");
const PROJECT_CONTEXT = registryDescriptor<OperationApplicationProjectContextRequest, OperationApplicationProjectContextResult>("project_context");

/**
 * Projectless registry capability. It has no base URL, project id, or token:
 * registry reads are admitted by the profile-scoped serve session alone, with
 * operation identity and request/result contracts taken from the generated
 * catalog.
 */
export class RegistryClient {
  readonly #call: RegistryToolCaller;

  constructor(call: RegistryToolCaller) {
    this.#call = call;
  }

  projectList(request: OperationApplicationProjectListRequest, signal?: AbortSignal): Promise<OperationApplicationProjectListResult> {
    return this.#invoke(PROJECT_LIST, request, signal);
  }

  projectContext(request: OperationApplicationProjectContextRequest, signal?: AbortSignal): Promise<OperationApplicationProjectContextResult> {
    return this.#invoke(PROJECT_CONTEXT, request, signal);
  }

  async #invoke<Req, Res>(descriptor: RegistryDescriptor<Req, Res>, request: Req, signal?: AbortSignal): Promise<Res> {
    const toolName = descriptor.transport.toolName as string;
    const decoded = descriptor.decodeRequest(request) as Record<string, unknown>;
    let response: unknown;
    try {
      response = await this.#call(toolName, decoded, signal);
    } catch (cause) {
      if (signal?.aborted) throw new TraceDecayAbortError(signal.reason ?? cause);
      throw new TraceDecayTransportError(`MCP tool ${toolName} failed`, cause);
    }
    try {
      return descriptor.decodeResult(response);
    } catch (cause) {
      throw new TraceDecayMalformedResponseError(`MCP tool ${toolName} returned an invalid ${descriptor.operation} result`, {
        payload: { response, cause },
      });
    }
  }
}
