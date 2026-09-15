import { useRef } from 'react';
import { ActivationField } from './activation.ts';

/**
 * One activation field per component instance, constructed exactly once.
 *
 * `useRef(new ActivationField(...))` evaluates its argument on EVERY render
 * and keeps only the first result — on surfaces that re-render per SSE beat
 * that is a discarded allocation (heat map, listener set and all) per tick.
 * Lazy initialisation builds the field on the first render and returns the
 * same instance thereafter, which is also what keeps its subscribers valid.
 */
export function useActivationField(halfLifeMs: number): ActivationField {
  const field = useRef<ActivationField | null>(null);
  field.current ??= new ActivationField({ halfLifeMs });
  return field.current;
}
