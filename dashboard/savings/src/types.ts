import type { ApiTokenRow, ModelPrice } from "./pricing";
import type { CostBasis } from "./logic";

export interface SavingsTotalJson {
  saved_tokens: number;
  calls: number;
}

/** Ledger-recording gate state as evaluated by the dashboard process. */
export interface LedgerRecording {
  enabled: boolean;
  mode: "default" | "enabled_by_env" | "disabled_by_env";
}

export interface SavingsOverview {
  savings: {
    available: boolean;
    db?: string;
    recording?: LedgerRecording;
    ledger?: {
      today: SavingsTotalJson;
      last_7d: SavingsTotalJson;
      last_30d: SavingsTotalJson;
      all_time: SavingsTotalJson;
    };
    lifetime_counters?: {
      total_tokens_saved: number;
      projects: Array<{ path: string; tokens_saved: number }>;
    };
  };
  sessions: {
    available: boolean;
    db?: string;
    scope?: string;
    session_count?: number;
    messages?: number;
    usage_messages?: number;
    tokenized_messages?: number;
    estimated_messages?: number;
    cost_basis?: CostBasis;
    model_count?: number;
    unknown_model_messages?: number;
    /** True when the server was built with the `token-counting` feature. */
    token_counting?: boolean;
    actual?: {
      input_tokens: number;
      output_tokens: number;
      cache_read_tokens: number;
      cache_write_tokens: number;
    };
    tokenized?: { input_tokens: number; output_tokens: number };
    estimated?: { input_tokens: number; output_tokens: number };
  };
  turns: {
    available: boolean;
    turn_count?: number;
    total_cost_usd?: number;
    total_tokens?: number;
  };
  pricing: {
    source: string;
    fetched_at: number | null;
    offline: boolean;
    model_count: number;
  };
}

export interface LedgerResponse {
  available: boolean;
  range: string;
  since?: number;
  db?: string;
  total?: SavingsTotalJson;
  by_day?: Array<{ day: number; saved_tokens: number; calls: number }>;
  by_tool?: Array<{ tool: string; saved_tokens: number; calls: number }>;
  by_project?: Array<{ project: string; saved_tokens: number; calls: number }>;
}

/** Which BPE the server counted a model row with (null = feature off). */
export interface TokenizerInfo {
  encoder: string;
  exact: boolean;
}

export interface SessionModelRow extends ApiTokenRow {
  messages: number;
  usage_messages: number;
  tokenized_messages: number;
  estimated_messages: number;
  tokenizer?: TokenizerInfo | null;
}

export interface SessionRow {
  provider: string;
  session_id: string;
  title: string | null;
  started_at: number | null;
  last_message_at: number | null;
  is_subagent: boolean;
  messages: number;
  usage_messages: number;
  tokenized_messages: number;
  estimated_messages: number;
  cost_basis: CostBasis;
  models: SessionModelRow[];
}

export interface SessionsResponse {
  available: boolean;
  range: string;
  total: number;
  scope?: string;
  db?: string;
  sessions: SessionRow[];
}

export interface ModelAggRow extends ApiTokenRow {
  sessions: number;
  messages: number;
  usage_messages: number;
  tokenized_messages: number;
  estimated_messages: number;
  tokenizer?: TokenizerInfo | null;
}

export interface ModelsResponse {
  available: boolean;
  range: string;
  models: ModelAggRow[];
  daily: Array<
    ApiTokenRow & { day: number; messages: number; usage_messages: number }
  >;
  turns: {
    available: boolean;
    by_model: Array<{
      model: string;
      cost_usd: number;
      total_tokens: number;
      cost_basis: "actual";
    }>;
    by_day: Array<{ day: number; cost_usd: number; total_tokens: number }>;
  };
}

export interface DiagnosticsCountRow {
  count: number;
  [key: string]: string | number;
}

export interface DiagnosticsRecentEvent {
  timestamp?: number | null;
  event_kind?: string;
  hook_name?: string;
  tool_name?: string;
  outcome?: string;
}

export interface DiagnosticsRecentHook {
  ts_unix_ms?: number | null;
  agent?: string;
  hook_name?: string;
  session_id?: string;
  tool_name?: string;
  prompt_category?: string;
}

export interface DiagnosticsResponse {
  available: boolean;
  source: string;
  message_count: number;
  event_count: number;
  tool_call_count: number;
  mcp_tool_call_count: number;
  tracedecay_call_count: number;
  hook_call_count: number;
  events_per_hour?: number;
  ratios: {
    events_per_message: number;
    tool_calls_per_message: number;
    mcp_tool_calls_per_message: number;
    hook_calls_per_message: number;
  };
  by_event_kind: DiagnosticsCountRow[];
  by_tool: DiagnosticsCountRow[];
  by_mcp_tool: DiagnosticsCountRow[];
  by_tool_category: DiagnosticsCountRow[];
  by_outcome: DiagnosticsCountRow[];
  by_hook: DiagnosticsCountRow[];
  by_prompt_category: DiagnosticsCountRow[];
  recent_events: DiagnosticsRecentEvent[];
  recent_hooks: DiagnosticsRecentHook[];
}

export interface PricingResponse {
  source: string;
  fetched_at: number | null;
  ttl_secs: number;
  offline: boolean;
  cache_path: string | null;
  model_count: number;
  models: Record<string, ModelPrice>;
}
