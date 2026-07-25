/**
 * Reduced-motion preference for the felt surfaces.
 *
 * The sensory contract makes reduced motion a first-class rendering mode, not
 * a degradation, so it needs a first-class control: the OS preference is the
 * default, and a reader can pin it either way from the surface itself. The
 * choice persists, because being made to re-assert it on every visit is the
 * accessibility failure this is meant to answer.
 */
import { useEffect, useState } from 'react';

export type MotionPreference = 'system' | 'reduced' | 'full';

const STORAGE_KEY = 'td.motion-preference';
const QUERY = '(prefers-reduced-motion: reduce)';

function readStored(): MotionPreference {
  try {
    const raw = globalThis.localStorage?.getItem(STORAGE_KEY);
    return raw === 'reduced' || raw === 'full' ? raw : 'system';
  } catch {
    // A blocked storage partition is not a reason to lose the surface.
    return 'system';
  }
}

function systemPrefersReduced(): boolean {
  return typeof window !== 'undefined' && typeof window.matchMedia === 'function'
    ? window.matchMedia(QUERY).matches
    : false;
}

/** Listeners for cross-component sync inside one document. */
const listeners = new Set<() => void>();

export function setMotionPreference(next: MotionPreference): void {
  try {
    if (next === 'system') globalThis.localStorage?.removeItem(STORAGE_KEY);
    else globalThis.localStorage?.setItem(STORAGE_KEY, next);
  } catch {
    // Preference is still applied for this session even if it cannot persist.
  }
  for (const listener of listeners) listener();
}

export function getMotionPreference(): MotionPreference {
  return readStored();
}

/** Resolve a preference against the OS setting. Exported for tests. */
export function resolveReducedMotion(
  preference: MotionPreference,
  systemReduced: boolean,
): boolean {
  if (preference === 'reduced') return true;
  if (preference === 'full') return false;
  return systemReduced;
}

/**
 * @returns the effective reduced-motion flag and the stored preference that
 * produced it, so a control can show which of the three states is in force.
 */
export function useReducedMotion(): {
  reduced: boolean;
  preference: MotionPreference;
  setPreference: (next: MotionPreference) => void;
} {
  const [preference, setPreferenceState] = useState<MotionPreference>(readStored);
  const [systemReduced, setSystemReduced] = useState<boolean>(systemPrefersReduced);

  useEffect(() => {
    const sync = () => setPreferenceState(readStored());
    listeners.add(sync);
    // Another tab changing the preference is the same event as this one doing it.
    window.addEventListener('storage', sync);
    return () => {
      listeners.delete(sync);
      window.removeEventListener('storage', sync);
    };
  }, []);

  useEffect(() => {
    if (typeof window.matchMedia !== 'function') return;
    const media = window.matchMedia(QUERY);
    const onChange = () => setSystemReduced(media.matches);
    // Safari < 14 only has the deprecated form; both are guarded so a missing
    // one never throws during render of an otherwise working surface.
    media.addEventListener?.('change', onChange);
    return () => media.removeEventListener?.('change', onChange);
  }, []);

  return {
    reduced: resolveReducedMotion(preference, systemReduced),
    preference,
    setPreference: setMotionPreference,
  };
}
