export type SecondsField =
  | "interval_secs"
  | "cooldown_secs"
  | "min_idle_secs"
  | "stale_lock_secs";
export type TaskField = "schedule" | SecondsField;
export type ConfigFieldErrors = Partial<Record<string, string>>;
