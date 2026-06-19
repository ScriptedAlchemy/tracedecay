/**
 * React shim — maps bare `react` imports onto the host dashboard's React.
 *
 * The dashboard bundler aliases `react` to this module so the plugin bundle
 * shares the host dashboard's single React instance (exposed via
 * `window.__HERMES_PLUGIN_SDK__.React`) instead of bundling a second copy.
 * Bundled third-party deps (lucide-react, etc.) that `import { forwardRef,
 * createElement, useState, useEffect } from "react"` resolve through here too,
 * so every name those libraries need is re-exported below.
 */

interface HermesPluginSDK {
  React?: Record<string, unknown>;
}

const sdk: HermesPluginSDK =
  (typeof window !== "undefined" &&
    (window as unknown as { __HERMES_PLUGIN_SDK__?: HermesPluginSDK })
      .__HERMES_PLUGIN_SDK__) ||
  {};

// eslint-disable-next-line @typescript-eslint/no-explicit-any
const React: any = sdk.React || {};

export default React;

export const {
  createElement,
  cloneElement,
  createContext,
  createRef,
  forwardRef,
  memo,
  lazy,
  isValidElement,
  Children,
  Fragment,
  StrictMode,
  Suspense,
  startTransition,
  useState,
  useEffect,
  useLayoutEffect,
  useInsertionEffect,
  useCallback,
  useMemo,
  useRef,
  useContext,
  useReducer,
  useImperativeHandle,
  useId,
  useDebugValue,
  useDeferredValue,
  useTransition,
  useSyncExternalStore,
} = React;
