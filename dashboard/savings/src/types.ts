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

export interface PricingResponse {
  source: string;
  fetched_at: number | null;
  ttl_secs: number;
  offline: boolean;
  cache_path: string | null;
  model_count: number;
  models: Record<string, ModelPrice>;
}
