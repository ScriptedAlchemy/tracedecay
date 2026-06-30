export type IdleBackfillMode = "off" | "idle";
export type EngineState =
  | "unavailable"
  | "disabled"
  | "inactive"
  | "starting"
  | "indexing"
  | "ready"
  | "refreshing"
  | "crashed";
export type DiagnosticSeverity = "error" | "warning" | "information" | "hint";

export interface LanguageSettings {
  enabled: boolean;
  command_override?: string | null;
}

export interface CodeDiagnosticsSettings {
  idle_backfill: IdleBackfillMode;
  languages: Record<string, LanguageSettings>;
}

export interface LspInstallOption {
  label: string;
  command: string;
  notes: string | null;
}

export interface DiagnosticsSummary {
  total_errors: number;
  total_warnings: number;
  pending_refreshes: number;
  last_refresh_age_seconds: number | null;
}

export interface EngineStatus {
  language: string;
  language_id: string;
  command: string;
  default_command: string;
  args: string[];
  enabled: boolean;
  state: EngineState;
  install_options: LspInstallOption[];
  last_error: string | null;
  last_diagnostic_update: number | null;
}

export interface CodeDiagnostic {
  language: string;
  source: string;
  file: string;
  line_start: number;
  line_end: number;
  character_start: number | null;
  character_end: number | null;
  severity: DiagnosticSeverity;
  code: string | null;
  message: string;
  enclosing_node: string | null;
  updated_at: number;
}

export interface BackfillProgress {
  queued_files: number;
  opened_files: number;
  files_with_diagnostics: number;
  last_completed_sweep: number | null;
}

export interface DiagnosticsSnapshot {
  summary: DiagnosticsSummary;
  engines: EngineStatus[];
  diagnostics: CodeDiagnostic[];
  backfill: Record<string, BackfillProgress>;
  settings: CodeDiagnosticsSettings;
}
