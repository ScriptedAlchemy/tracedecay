export function formatHistoryTime(ts?: string | null): string {
  if (!ts) return "";
  const date = new Date(ts);
  if (Number.isNaN(date.getTime())) return ts;
  try {
    return date.toLocaleString([], {
      month: "short",
      day: "2-digit",
      hour: "2-digit",
      minute: "2-digit",
    });
  } catch {
    return ts;
  }
}

export function formatOplogTime(ts?: number | string | null): string {
  if (ts == null) return "--";
  const date = typeof ts === "number" ? new Date(ts * 1000) : new Date(ts);
  if (Number.isNaN(date.getTime())) return String(ts);
  try {
    return date.toLocaleTimeString([], {
      hour: "2-digit",
      minute: "2-digit",
      second: "2-digit",
    });
  } catch {
    return String(ts);
  }
}
