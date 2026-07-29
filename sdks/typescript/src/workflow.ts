// Workflow definition builder and run lifecycle façade (PR17/PR18).
//
// Design boundary (Plan 32): workflow definitions are product DATA, composed
// here and validated/executed only by the daemon. This module performs no
// readiness, scheduling, provider selection, retry, or acceptance decisions.
// Structural integrity checks (required fields, edge targets, positive
// bounds) are authoring aids; the daemon's validate() remains the authority.
//
// The definition wire shape follows `pr17-residual-workflow.md` and Plan 32's
// typed-definition section (owner/scope, schemas, typed steps referencing
// cataloged operations, predecessor edges, bounded fan-out groups,
// concurrency/failure policy, route requirements, budgets, result conditions,
// retention). The server-side contract is still being built (PR14/PR17); the
// schema_version discriminator and additive pass-through keep this
// forward-compatible, and run routes are marked provisional until PR17
// freezes its public spellings.

import type { OperationReceipt, SafeDiagnostic } from "./types";
import type { InvokeOptions, InvokeResult, StreamItem, TraceDecayClient } from "./client";

export const WORKFLOW_DEFINITION_SCHEMA = "tracedecay.workflow-definition.v1";

// ---------------------------------------------------------------------------
// Definition data (additive — unknown future fields pass through)
// ---------------------------------------------------------------------------

export type JsonSchema = Record<string, unknown>;

export type FailurePolicy =
  | { kind: "fail_fast" }
  | { kind: "collect" }
  | { kind: "at_least"; successes: number };

export interface WorkflowBudget {
  maxUnits?: number;
  maxBytes?: number;
  maxWorkUnits?: number;
  deadlineMicros?: number;
  [key: string]: unknown;
}

export interface WorkflowStepDef {
  id: string;
  /** Cataloged operation name (from the generated OPERATIONS authority). */
  operation: string;
  params?: unknown;
  after?: string[];
  /** Validated references to prior step outputs, e.g. { "field": "stepId.output" }. */
  inputRefs?: Record<string, string>;
  routeRequirement?: string;
  budget?: WorkflowBudget;
  [key: string]: unknown;
}

export interface FanOutGroupDef {
  id: string;
  after: string[];
  /** Items are definition data; the daemon bounds concurrency server-side. */
  items: unknown[];
  maxConcurrency: number;
  failure: FailurePolicy;
  operation: string;
  paramsTemplate?: unknown;
  [key: string]: unknown;
}

export interface SynthesisStepDef {
  id: string;
  after: string[];
  /** Minority evidence is preserved and every source is cited (Plan 32). */
  policy: "preserve_minority";
  budget?: WorkflowBudget;
  [key: string]: unknown;
}

export interface WorkflowDefinitionData {
  schema_version: string;
  name: string;
  scope: { project: string; repository: string; worktree: string; reference?: string };
  input_schema?: JsonSchema;
  output_schema?: JsonSchema;
  steps: WorkflowStepDef[];
  fan_out: FanOutGroupDef[];
  synthesis: SynthesisStepDef[];
  budget?: WorkflowBudget;
  result_conditions?: Record<string, unknown>;
  retention?: Record<string, unknown>;
  [key: string]: unknown;
}

// ---------------------------------------------------------------------------
// Builder
// ---------------------------------------------------------------------------

export class WorkflowDefinitionBuilder {
  private readonly data: WorkflowDefinitionData;
  private readonly stepIds = new Set<string>();

  constructor(name: string, scope: WorkflowDefinitionData["scope"]) {
    this.data = {
      schema_version: WORKFLOW_DEFINITION_SCHEMA,
      name,
      scope,
      steps: [],
      fan_out: [],
      synthesis: [],
    };
  }

  private claimId(kind: string, id: string): void {
    if (!/^[a-z][a-z0-9_-]*$/.test(id)) {
      throw new Error(`${kind} id must be lowercase slug: ${id}`);
    }
    if (this.stepIds.has(id)) {
      throw new Error(`duplicate ${kind} id: ${id}`);
    }
    this.stepIds.add(id);
  }

  private requireAfter(after: string[] | undefined, owner: string): string[] {
    for (const dep of after ?? []) {
      if (!this.stepIds.has(dep)) {
        throw new Error(`${owner} depends on unknown step: ${dep}`);
      }
    }
    return after ?? [];
  }

  inputSchema(schema: JsonSchema): this {
    this.data.input_schema = schema;
    return this;
  }

  outputSchema(schema: JsonSchema): this {
    this.data.output_schema = schema;
    return this;
  }

  budget(budget: WorkflowBudget): this {
    this.data.budget = budget;
    return this;
  }

  /** Append a typed step referencing a cataloged operation. */
  step(
    id: string,
    operation: string,
    params?: unknown,
    options: { after?: string[]; inputRefs?: Record<string, string>; routeRequirement?: string; budget?: WorkflowBudget } = {},
  ): this {
    this.claimId("step", id);
    this.data.steps.push({
      id,
      operation,
      params,
      after: this.requireAfter(options.after, id),
      inputRefs: options.inputRefs,
      routeRequirement: options.routeRequirement,
      budget: options.budget,
    });
    return this;
  }

  /** Bounded fan-out group; items are data, the daemon owns release. */
  fanOut(
    id: string,
    after: string[],
    group: { items: unknown[]; maxConcurrency: number; failure: FailurePolicy; operation: string; paramsTemplate?: unknown },
  ): this {
    this.claimId("fan-out group", id);
    if (!Number.isInteger(group.maxConcurrency) || group.maxConcurrency < 1) {
      throw new Error(`fan-out ${id}: maxConcurrency must be a positive integer`);
    }
    if (group.items.length === 0) {
      throw new Error(`fan-out ${id}: items must not be empty`);
    }
    this.data.fan_out.push({
      id,
      after: this.requireAfter(after, id),
      items: group.items,
      maxConcurrency: group.maxConcurrency,
      failure: group.failure,
      operation: group.operation,
      paramsTemplate: group.paramsTemplate,
    });
    return this;
  }

  /** Optional minority-preserving synthesis step. */
  synthesis(id: string, options: { after: string[]; budget?: WorkflowBudget }): this {
    this.claimId("synthesis", id);
    this.data.synthesis.push({
      id,
      after: this.requireAfter(options.after, id),
      policy: "preserve_minority",
      budget: options.budget,
    });
    return this;
  }

  resultConditions(conditions: Record<string, unknown>): this {
    this.data.result_conditions = conditions;
    return this;
  }

  /**
   * Authoring aid only: render the topological order implied by edges. The
   * daemon's validation (SCC/cycle rejection, schema compat, capability)
   * is authoritative; this throws only on structurally missing deps, which
   * the builder already prevents.
   */
  previewOrder(): string[] {
    const order: string[] = [];
    const visited = new Set<string>();
    const visit = (id: string, after: string[]) => {
      if (visited.has(id)) return;
      for (const dep of after) {
        const depStep = this.data.steps.find((step) => step.id === dep);
        if (depStep) visit(depStep.id, depStep.after ?? []);
      }
      visited.add(id);
      order.push(id);
    };
    for (const step of this.data.steps) visit(step.id, step.after ?? []);
    for (const group of this.data.fan_out) visit(group.id, group.after);
    for (const synth of this.data.synthesis) visit(synth.id, synth.after);
    return order;
  }

  /** The immutable definition document to submit for validation/activation. */
  definition(): WorkflowDefinitionData {
    return structuredClone(this.data);
  }
}

// ---------------------------------------------------------------------------
// Run lifecycle (provisional routes until PR17 freezes public spellings)
// ---------------------------------------------------------------------------

/** PROVISIONAL: PR17 has not frozen its public operation spellings yet. */
export const WORKFLOW_ROUTES = {
  validate: "/v1/workflows/definitions/validate",
  activate: "/v1/workflows/definitions/activate",
  run: "/v1/workflows/runs",
  control: (runId: string, action: string) => `/v1/workflows/runs/${encodeURIComponent(runId)}/${action}`,
  events: (runId: string) => `/v1/workflows/runs/${encodeURIComponent(runId)}/events`,
  receipt: (runId: string) => `/v1/workflows/runs/${encodeURIComponent(runId)}/receipt`,
} as const;

export interface WorkflowValidation {
  valid: boolean;
  problems: SafeDiagnostic[];
  definitionVersion?: string;
}

export interface WorkflowRunHandle {
  runId: string;
  events(options?: InvokeOptions & { resumeFrom?: string }): AsyncGenerator<StreamItem>;
  pause(options?: InvokeOptions): Promise<InvokeResult<unknown>>;
  resume(options?: InvokeOptions): Promise<InvokeResult<unknown>>;
  cancel(options?: InvokeOptions): Promise<InvokeResult<unknown>>;
  retry(options?: InvokeOptions): Promise<InvokeResult<unknown>>;
  receipt(options?: InvokeOptions): Promise<InvokeResult<OperationReceipt>>;
}

export class WorkflowNamespace {
  private readonly client: TraceDecayClient;

  constructor(client: TraceDecayClient) {
    this.client = client;
  }

  /** Start a definition builder scoped to an exact project identity. */
  define(name: string, scope: WorkflowDefinitionData["scope"]): WorkflowDefinitionBuilder {
    return new WorkflowDefinitionBuilder(name, scope);
  }

  /** Daemon-authoritative validation (cycles, schemas, capability, bounds). */
  async validate(definition: WorkflowDefinitionData, options?: InvokeOptions): Promise<InvokeResult<WorkflowValidation>> {
    return this.client.invoke<WorkflowValidation>(
      { operation: "workflow_definition_validate", route: WORKFLOW_ROUTES.validate },
      definition,
      options,
    );
  }

  async activate(definition: WorkflowDefinitionData, options?: InvokeOptions): Promise<InvokeResult<{ definitionVersion: string }>> {
    return this.client.invoke<{ definitionVersion: string }>(
      { operation: "workflow_definition_activate", route: WORKFLOW_ROUTES.activate },
      definition,
      options,
    );
  }

  /** Admit a run of an activated definition version. */
  async run(definitionVersion: string, inputs?: unknown, options?: InvokeOptions): Promise<WorkflowRunHandle> {
    const admitted = await this.client.invoke<{ run_id: string }>(
      { operation: "workflow_run_admit", route: WORKFLOW_ROUTES.run },
      { definition_version: definitionVersion, inputs },
      options,
    );
    const runId = admitted.value.run_id;
    const client = this.client;
    return {
      runId,
      events: (eventOptions) =>
        client.stream(
          { operation: "workflow_run_events", route: WORKFLOW_ROUTES.events(runId) },
          {},
          eventOptions,
        ),
      pause: (controlOptions) =>
        client.invoke({ operation: "workflow_run_pause", route: WORKFLOW_ROUTES.control(runId, "pause") }, {}, controlOptions),
      resume: (controlOptions) =>
        client.invoke({ operation: "workflow_run_resume", route: WORKFLOW_ROUTES.control(runId, "resume") }, {}, controlOptions),
      cancel: (controlOptions) =>
        client.invoke({ operation: "workflow_run_cancel", route: WORKFLOW_ROUTES.control(runId, "cancel") }, {}, controlOptions),
      retry: (controlOptions) =>
        client.invoke({ operation: "workflow_run_retry", route: WORKFLOW_ROUTES.control(runId, "retry") }, {}, controlOptions),
      receipt: (receiptOptions) =>
        client.invoke<OperationReceipt>({ operation: "workflow_run_receipt", route: WORKFLOW_ROUTES.receipt(runId) }, {}, receiptOptions),
    };
  }
}
