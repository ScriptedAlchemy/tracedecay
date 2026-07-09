export interface ProjectConfig {
  version: number;
  root_dir: string;
  include: string[];
  exclude: string[];
  max_file_size: number;
  extract_docstrings: boolean;
  track_call_sites: boolean;
  git_ignore: boolean;
  telemetry: TelemetryConfig;
  sync: SyncConfig;
}

export interface TelemetryConfig {
  timings: boolean;
}

// The server serializes the full `[sync]` table; the settings panel only edits
// the PR auto-tracking fields, so the rest is captured loosely.
export interface SyncConfig {
  auto_track_pr_branches: boolean;
  auto_track_pr_poll_secs: number;
  [key: string]: unknown;
}

export interface TrackedPrBranch {
  branch: string;
  pr: number;
  head_branch: string;
}

export interface PrAutotrackInfo {
  tracked: TrackedPrBranch[];
}

export interface ProjectSettings {
  config_path: string;
  config: ProjectConfig;
  tracedecay_dir_gitignored: boolean;
  pr_autotrack?: PrAutotrackInfo;
}

export interface UserSettings {
  config_path: string;
  upload_enabled: boolean;
  watcher_debounce: string;
  extraction_timeout_secs: number;
  installed_agents: string[];
}

export interface AutomationSummary {
  config_endpoint: string;
  enabled: boolean;
  backend: string;
  host_mode: string;
}

export interface EnvVariable {
  name: string;
  active: boolean;
  value: string | null;
  description: string;
}

export interface EnvironmentSettings {
  global_accounting_mode: string;
  global_accounting_enabled: boolean;
  pricing_offline: boolean;
  variables: EnvVariable[];
}

export interface StorageSettings {
  project_id: string | null;
  project_root: string;
  storage_mode: string;
  store_root: string;
  dashboard_root: string;
  graph_db: string;
  memory_db: string;
  lcm_db: string;
  lcm_scope: string;
  savings_db: string;
}

export interface VersionInfo {
  version: string;
  channel: string;
  cached_latest_version: string | null;
}

export interface SettingsPayload {
  project: ProjectSettings;
  user: UserSettings;
  automation: AutomationSummary;
  environment: EnvironmentSettings;
  storage: StorageSettings;
  version: VersionInfo;
  resync_recommended?: boolean;
  restart_recommended?: boolean;
}

export interface ProjectSettingsPatch {
  include?: string[];
  exclude?: string[];
  max_file_size?: number;
  extract_docstrings?: boolean;
  track_call_sites?: boolean;
  git_ignore?: boolean;
  telemetry?: Partial<TelemetryConfig>;
  sync?: SyncSettingsPatch;
}

export interface SyncSettingsPatch {
  auto_track_pr_branches?: boolean;
  auto_track_pr_poll_secs?: number;
}

export interface UserSettingsPatch {
  upload_enabled?: boolean;
  watcher_debounce?: string;
  extraction_timeout_secs?: number;
}

export type FieldErrors = Record<string, string>;
