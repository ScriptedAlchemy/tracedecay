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
