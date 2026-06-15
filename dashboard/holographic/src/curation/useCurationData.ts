import { useCallback, useEffect, useRef, useState } from "react";
import { api as defaultApi } from "../api";
import type {
  MemoryCurateResponse,
  MemoryCuratorActivityEvent,
  MemoryCuratorStatusResponse,
  MemoryOplogEvent,
} from "../types";

export type CurationTab = "plan" | "history" | "activity";

export function useCurationData({
  api = defaultApi,
  onApplied,
  now = () => new Date().toISOString(),
  pollFastMs = 900,
  pollIdleMs = 2500,
}: {
  api?: typeof defaultApi;
  onApplied?: () => void;
  now?: () => string;
  pollFastMs?: number;
  pollIdleMs?: number;
}) {
  const [report, setReport] = useState<MemoryCurateResponse | null>(null);
  const [loading, setLoading] = useState(false);
  const [applying, setApplying] = useState(false);
  const [previewSavedAt, setPreviewSavedAt] = useState<string | null>(null);
  const [previewStale, setPreviewStale] = useState(false);
  const [previewStaleReason, setPreviewStaleReason] = useState("");
  const [confirmOpen, setConfirmOpen] = useState(false);
  const [error, setError] = useState("");
  const [activeTab, setActiveTab] = useState<CurationTab>("plan");
  const [status, setStatus] = useState<MemoryCuratorStatusResponse | null>(null);
  const [statusLoading, setStatusLoading] = useState(false);
  const [statusError, setStatusError] = useState("");
  const [oplog, setOplog] = useState<MemoryOplogEvent[]>([]);
  const [oplogError, setOplogError] = useState("");
  const [activity, setActivity] = useState<MemoryCuratorActivityEvent[]>([]);
  const [activityLoading, setActivityLoading] = useState(false);
  const [activityError, setActivityError] = useState("");
  const activityRef = useRef<HTMLDivElement>(null);
  const previewSavedAtRef = useRef<string | null>(null);
  const previewLoadSeq = useRef(0);
  const panelRef = useRef<HTMLDivElement>(null);

  const applySavedPreview = useCallback((
    savedReport: MemoryCurateResponse,
    savedAt?: string | null,
    stale = false,
    staleReason = "",
  ) => {
    previewSavedAtRef.current = savedAt ?? null;
    setReport(savedReport);
    setPreviewSavedAt(savedAt ?? null);
    setPreviewStale(stale);
    setPreviewStaleReason(staleReason);
  }, []);

  const loadSavedPreview = useCallback((force = false) => {
    const ticket = ++previewLoadSeq.current;
    return api
      .getMemoryCuratorPreview()
      .then((response) => {
        if (ticket !== previewLoadSeq.current) return response;
        if (response.report && (force || response.saved_at !== previewSavedAtRef.current)) {
          applySavedPreview(
            response.report,
            response.saved_at ?? null,
            Boolean(response.stale),
            response.stale_reason || "",
          );
        } else if (!response.report && !loading && !applying) {
          previewSavedAtRef.current = null;
          setReport(null);
          setPreviewSavedAt(null);
          setPreviewStale(false);
          setPreviewStaleReason("");
        }
        return response;
      })
      .catch(() => {});
  }, [api, applySavedPreview, applying, loading]);

  const loadActivity = useCallback((showSpinner = false) => {
    if (showSpinner) setActivityLoading(true);
    setActivityError("");
    api
      .getMemoryCuratorActivity({ limit: 120 })
      .then((response) => {
        const events = response.events || [];
        setActivity(events);
        const latestFinish = [...events]
          .reverse()
          .find((event) => event.phase === "finish" && !event.synthetic);
        if (latestFinish?.dry_run) {
          loadSavedPreview(false);
        }
      })
      .catch((err) => setActivityError(err instanceof Error ? err.message : String(err)))
      .finally(() => {
        if (showSpinner) setActivityLoading(false);
      });
  }, [api, loadSavedPreview]);

  const loadStatus = useCallback(() => {
    setStatusLoading(true);
    setStatusError("");
    api
      .getMemoryCuratorStatus()
      .then((response) => setStatus(response))
      .catch((err) => setStatusError(err instanceof Error ? err.message : String(err)))
      .finally(() => setStatusLoading(false));
  }, [api]);

  const loadOplog = useCallback(() => {
    setOplogError("");
    api
      .getMemoryOplog({ limit: 30 })
      .then((response) => {
        setOplog(response.events || []);
        if (response.error) setOplogError(response.error);
      })
      .catch((err) => setOplogError(err instanceof Error ? err.message : String(err)));
  }, [api]);

  useEffect(() => {
    loadSavedPreview(true);
  }, [loadSavedPreview]);

  const preview = useCallback(async () => {
    setLoading(true);
    setError("");
    setActiveTab("activity");
    loadActivity(true);
    try {
      const response = await api.postMemoryCurate({ dry_run: true });
      setReport(response);
      const savedAt = now();
      previewSavedAtRef.current = savedAt;
      setPreviewSavedAt(savedAt);
      setPreviewStale(false);
      setPreviewStaleReason("");
      await loadSavedPreview(true);
      loadActivity();
      loadStatus();
      setActiveTab("plan");
      return response;
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
      throw err;
    } finally {
      setLoading(false);
    }
  }, [api, loadActivity, loadSavedPreview, loadStatus, now]);

  const apply = useCallback(async () => {
    previewLoadSeq.current += 1;
    setApplying(true);
    setError("");
    setActiveTab("activity");
    loadActivity(true);
    try {
      const response = await api.postMemoryCurate({ dry_run: false });
      setReport(response);
      previewSavedAtRef.current = null;
      setPreviewSavedAt(null);
      setPreviewStale(false);
      setPreviewStaleReason("");
      setConfirmOpen(false);
      loadActivity();
      loadStatus();
      onApplied?.();
      return response;
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
      throw err;
    } finally {
      setApplying(false);
    }
  }, [api, loadActivity, loadStatus, onApplied]);

  useEffect(() => {
    if (activeTab === "plan" && !loading && !applying) {
      loadSavedPreview(false);
    }
  }, [activeTab, applying, loadSavedPreview, loading]);

  useEffect(() => {
    if (activeTab === "history" && !status && !statusLoading) {
      loadStatus();
    }
  }, [activeTab, loadStatus, status, statusLoading]);

  useEffect(() => {
    if (activeTab === "history") {
      loadOplog();
    }
  }, [activeTab, loadOplog]);

  useEffect(() => {
    if (activeTab === "activity" && activity.length === 0) {
      loadActivity(true);
    }
  }, [activeTab, activity.length, loadActivity]);

  useEffect(() => {
    if (activeTab !== "activity" && !loading && !applying) return undefined;
    const interval = window.setInterval(() => {
      if (panelRef.current?.offsetParent === null) return;
      loadActivity(false);
    }, loading || applying ? pollFastMs : pollIdleMs);
    return () => window.clearInterval(interval);
  }, [activeTab, applying, loadActivity, loading, pollFastMs, pollIdleMs]);

  useEffect(() => {
    const element = activityRef.current;
    if (!element) return;
    element.scrollTop = element.scrollHeight;
  }, [activity]);

  return {
    report,
    loading,
    applying,
    previewSavedAt,
    previewStale,
    previewStaleReason,
    confirmOpen,
    error,
    activeTab,
    status,
    statusLoading,
    statusError,
    oplog,
    oplogError,
    activity,
    activityLoading,
    activityError,
    activityRef,
    panelRef,
    setConfirmOpen,
    setActiveTab,
    preview,
    apply,
    loadActivity,
    loadStatus,
    loadOplog,
  };
}
