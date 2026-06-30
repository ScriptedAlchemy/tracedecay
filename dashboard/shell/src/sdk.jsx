/**
 * Standalone plugin SDK — a Hermes-compatible `window.__HERMES_PLUGIN_SDK__`
 * built on the shell's own React bundle.
 *
 * The ported plugin bundles (holographic, hermes-lcm) were written against the
 * Hermes dashboard plugin SDK (see hermes-agent web/src/plugins/registry.ts).
 * This module re-creates the parts of that surface the bundles actually use,
 * so the same bundles run unmodified in both hosts:
 *
 *   - React + hooks (the bundles externalize React onto the SDK)
 *   - fetchJSON (plain same-origin fetch; the standalone server has no auth)
 *   - components: Card/CardHeader/CardTitle/CardContent/Badge/Button/Input/...
 *   - utils: cn / timeAgo / isoTimeAgo / makeSequence
 *   - useI18n (identity translations)
 */

import React, {
  useState,
  useEffect,
  useCallback,
  useMemo,
  useRef,
  useContext,
  createContext,
} from "react";
import { makeSequence } from "../../lib/sequence";
import { cn as cnImpl } from "../../lib/cn";

export { makeSequence };
export const cn = cnImpl;

const projectListeners = new Set();
let selectedProjectId = "";

function notifyProjectListeners() {
  for (const fn of projectListeners) {
    try {
      fn(selectedProjectId);
    } catch {
      /* listener errors should not break plugin fetches */
    }
  }
}

function isScopedApiUrl(url, prefix) {
  return url === prefix || url.startsWith(`${prefix}/`) || url.startsWith(`${prefix}?`);
}

export function getSelectedProjectId() {
  return selectedProjectId;
}

export function setShellSelectedProjectId(projectId) {
  const next = String(projectId || "");
  if (next === selectedProjectId) return;
  selectedProjectId = next;
  notifyProjectListeners();
}

export function subscribeSelectedProject(fn) {
  projectListeners.add(fn);
  return () => projectListeners.delete(fn);
}

function requestMethod(init) {
  return String(init?.method || "GET").toUpperCase();
}

export function projectScopedUrl(url, init) {
  if (!selectedProjectId || typeof url !== "string" || !url.startsWith("/")) return url;
  if (!["GET", "HEAD"].includes(requestMethod(init))) return url;
  if (
    !isScopedApiUrl(url, "/api/plugins") &&
    !isScopedApiUrl(url, "/api/automation") &&
    !isScopedApiUrl(url, "/api/capabilities")
  ) {
    return url;
  }
  return `/api/projects/${encodeURIComponent(selectedProjectId)}${url.slice("/api".length)}`;
}

export function authedFetch(url, init) {
  return fetch(projectScopedUrl(url, init), init);
}

export async function fetchJSON(url, init) {
  const res = await authedFetch(url, init);
  if (!res.ok) {
    let detail = `${res.status} ${res.statusText}`;
    let body;
    try {
      body = await res.json();
      if (body && body.detail) detail = String(body.detail);
    } catch {
      /* non-JSON error body */
    }
    const error = new Error(detail);
    if (body !== undefined) error.body = body;
    throw error;
  }
  return res.json();
}

function relativeTime(deltaSeconds) {
  if (Number.isNaN(deltaSeconds)) return "unknown";
  if (deltaSeconds < 60) return "just now";
  if (deltaSeconds < 3600) return `${Math.floor(deltaSeconds / 60)}m ago`;
  if (deltaSeconds < 86400) return `${Math.floor(deltaSeconds / 3600)}h ago`;
  if (deltaSeconds < 172800) return "yesterday";
  return `${Math.floor(deltaSeconds / 86400)}d ago`;
}

/** Relative time from a unix-seconds timestamp. */
export function timeAgo(ts) {
  return relativeTime(Date.now() / 1000 - Number(ts));
}

/** Relative time from an ISO string; future timestamps read "unknown". */
export function isoTimeAgo(iso) {
  const delta = (Date.now() - new Date(iso).getTime()) / 1000;
  return delta < 0 ? "unknown" : relativeTime(delta);
}

/* Minimal stand-ins for the Hermes design-system components the plugin
 * bundles render. Styling lives in shell/src/styles.css (.ts-* classes). */

function block(tag, base) {
  return function Component({ className, children, ...rest }) {
    return React.createElement(
      tag,
      { className: cn(base, className), ...rest },
      children,
    );
  };
}

export const Card = block("div", "ts-card");
export const CardHeader = block("div", "ts-card-header");
export const CardTitle = block("h3", "ts-card-title");
export const CardContent = block("div", "ts-card-content");
export const Badge = block("span", "ts-badge");
export const Label = block("label", "ts-label");
export const Separator = block("hr", "ts-separator");

export function Button({
  className,
  variant,
  size,
  ghost,
  outlined,
  secondary,
  destructive,
  children,
  ...rest
}) {
  const resolvedVariant =
    variant ||
    (destructive
      ? "destructive"
      : ghost
        ? "ghost"
        : outlined
          ? "outline"
          : secondary
            ? "secondary"
            : "");
  return React.createElement(
    "button",
    {
      className: cn(
        "ts-button",
        resolvedVariant ? `ts-button-${resolvedVariant}` : "",
        size ? `ts-button-${size}` : "",
        className,
      ),
      ...rest,
    },
    children,
  );
}

export function Input({ className, ...rest }) {
  return React.createElement("input", { className: cn("ts-input", className), ...rest });
}

export function Checkbox({ className, ...rest }) {
  return React.createElement("input", {
    type: "checkbox",
    className: cn("ts-checkbox", className),
    ...rest,
  });
}

export function Select({ className, children, ...rest }) {
  return React.createElement("select", { className: cn("ts-input", className), ...rest }, children);
}

export function SelectOption({ children, ...rest }) {
  return React.createElement("option", rest, children);
}

export function Tabs({ className, children, ...rest }) {
  return React.createElement("div", { className: cn("ts-tabs", className), ...rest }, children);
}
export const TabsList = block("div", "ts-tabs-list");
export const TabsTrigger = block("button", "ts-tabs-trigger");

export function PluginSlot() {
  return null;
}

export function useI18n() {
  return {
    t: (_key, fallback) => (fallback !== undefined ? fallback : _key),
    lang: "en",
  };
}

/** Assemble the SDK object exposed on window for plugin bundles. */
export function buildSDK() {
  return {
    sdkVersion: "1.2.0",
    host: "tracedecay-standalone",
    React,
    hooks: { useState, useEffect, useCallback, useMemo, useRef, useContext, createContext },
    api: {},
    fetchJSON,
    authedFetch,
    buildWsUrl: (p) => p,
    buildWsAuthParam: () => ["", ""],
    /**
     * Populated by the shell after a successful GET /api/capabilities response.
     * Plugin tabs may read this to feature-gate behavior:
     *   const caps = window.__HERMES_PLUGIN_SDK__.capabilities;
     *   if (caps?.curation) { ... }
     * Null until the first successful fetch.
     */
    capabilities: null,
    projects: {
      getSelectedProjectId,
      subscribe: subscribeSelectedProject,
    },
    components: {
      Card,
      CardHeader,
      CardTitle,
      CardContent,
      Badge,
      Button,
      Checkbox,
      Input,
      Label,
      Select,
      SelectOption,
      Separator,
      Tabs,
      TabsList,
      TabsTrigger,
      PluginSlot,
    },
    utils: { cn, timeAgo, isoTimeAgo, makeSequence },
    useI18n,
  };
}
