import { clsx, type ClassValue } from 'clsx';

/** Class combiner for the variant layer. Semantic-token classes only. */
export function cn(...inputs: ClassValue[]): string {
  return clsx(inputs);
}
