import { useCallback, useRef, useState } from "react";
import type { KeyboardEvent } from "react";
import { makeSequence } from "../../lib/sequence";
import type { GraphNode } from "./types";

export function useGraphSearch({
  search,
  focusSymbol,
  onError,
  debounceMs = 180,
}: {
  search: (params: { q?: string; limit?: number; offset?: number }) => Promise<{ results: GraphNode[] }>;
  focusSymbol: (node: Pick<GraphNode, "id" | "name">) => void;
  onError: (message: string) => void;
  debounceMs?: number;
}) {
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<GraphNode[]>([]);
  const [searchOpen, setSearchOpen] = useState(false);
  const searchTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const searchSeq = useRef(makeSequence()).current;
  const searchBoxRef = useRef<HTMLDivElement | null>(null);

  const onQueryChange = useCallback((value: string) => {
    setQuery(value);
    setSearchOpen(true);
    if (searchTimer.current) clearTimeout(searchTimer.current);
    searchTimer.current = setTimeout(() => {
      const ticket = searchSeq.next();
      search({ q: value, limit: 20 })
        .then((payload) => {
          if (searchSeq.isCurrent(ticket)) setResults(payload.results);
        })
        .catch((err) => {
          if (searchSeq.isCurrent(ticket)) {
            onError(err instanceof Error ? err.message : String(err));
          }
        });
    }, debounceMs);
  }, [debounceMs, onError, search, searchSeq]);

  const onSearchKeyDown = useCallback(
    (event: KeyboardEvent<HTMLElement>) => {
      const box = searchBoxRef.current;
      if (!box) return;
      if (event.key === "Escape") {
        event.preventDefault();
        setSearchOpen(false);
        box.querySelector<HTMLInputElement>("input")?.focus();
        return;
      }
      if (!searchOpen || results.length === 0) return;
      const buttons: HTMLButtonElement[] = Array.from(
        box.querySelectorAll<HTMLButtonElement>(".tsg-search-pop button"),
      );
      const active = document.activeElement as HTMLElement | null;
      const index = buttons.indexOf(active as HTMLButtonElement);
      if (event.key === "Enter") {
        event.preventDefault();
        if (index >= 0 && index < results.length) focusSymbol(results[index]);
        else focusSymbol(results[0]);
      } else if (event.key === "ArrowDown") {
        event.preventDefault();
        const next = index < 0 ? buttons[0] : buttons[Math.min(index + 1, buttons.length - 1)];
        next?.focus();
      } else if (event.key === "ArrowUp") {
        event.preventDefault();
        if (index <= 0) box.querySelector<HTMLInputElement>("input")?.focus();
        else buttons[index - 1]?.focus();
      }
    },
    [focusSymbol, results, searchOpen],
  );

  return {
    query,
    results,
    searchOpen,
    searchBoxRef,
    setSearchOpen,
    onQueryChange,
    onSearchKeyDown,
  };
}
