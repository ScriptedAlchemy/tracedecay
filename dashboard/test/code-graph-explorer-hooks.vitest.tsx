import React from "react";
import { act, fireEvent, render, renderHook, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useGraphInspection } from "../graph/src/useGraphInspection";
import { useGraphSearch } from "../graph/src/useGraphSearch";

function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

function SearchHarness({ search, focusSymbol, onError = vi.fn() }) {
  const searchState = useGraphSearch({ search, focusSymbol, onError, debounceMs: 180 });
  return (
    <div ref={searchState.searchBoxRef} onKeyDown={searchState.onSearchKeyDown}>
      <input
        aria-label="Search"
        value={searchState.query}
        onChange={(event) => searchState.onQueryChange(event.target.value)}
      />
      {searchState.searchOpen && searchState.results.length > 0 ? (
        <div className="tsg-search-pop">
          {searchState.results.map((row) => (
            <button key={row.id} type="button" onClick={() => focusSymbol(row)}>
              {row.name}
            </button>
          ))}
        </div>
      ) : null}
    </div>
  );
}

describe("code graph explorer hooks", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("useGraphSearch drops stale async responses and keeps only the latest debounced query", async () => {
    const alpha = deferred();
    const beta = deferred();
    const search = vi.fn(({ q }) => (q === "alpha" ? alpha.promise : beta.promise));
    const focusSymbol = vi.fn();

    render(<SearchHarness search={search} focusSymbol={focusSymbol} />);
    const input = screen.getByLabelText("Search");

    fireEvent.change(input, { target: { value: "alpha" } });
    await act(async () => {
      vi.advanceTimersByTime(180);
    });
    fireEvent.change(input, { target: { value: "beta" } });
    await act(async () => {
      vi.advanceTimersByTime(180);
    });

    expect(search).toHaveBeenCalledTimes(2);
    expect(search.mock.calls[0][0]).toMatchObject({ q: "alpha", limit: 20 });
    expect(search.mock.calls[1][0]).toMatchObject({ q: "beta", limit: 20 });

    await act(async () => {
      beta.resolve({ results: [{ id: "b", name: "beta" }] });
      await beta.promise;
    });
    expect(screen.getByRole("button", { name: "beta" })).toBeTruthy();

    await act(async () => {
      alpha.resolve({ results: [{ id: "a", name: "alpha" }] });
      await alpha.promise;
    });
    expect(screen.queryByRole("button", { name: "alpha" })).toBeNull();
    expect(screen.getByRole("button", { name: "beta" })).toBeTruthy();
  });

  it("useGraphSearch supports Enter and arrow-key navigation through the result popover", async () => {
    const search = vi.fn().mockResolvedValue({
      results: [
        { id: "a", name: "Alpha" },
        { id: "b", name: "Beta" },
      ],
    });
    const focusSymbol = vi.fn();

    render(<SearchHarness search={search} focusSymbol={focusSymbol} />);
    const input = screen.getByLabelText("Search");

    fireEvent.change(input, { target: { value: "alp" } });
    await act(async () => {
      vi.advanceTimersByTime(180);
      await Promise.resolve();
    });

    const wrapper = input.parentElement;
    const buttons = screen.getAllByRole("button");
    expect(buttons).toHaveLength(2);

    input.focus();
    fireEvent.keyDown(wrapper, { key: "ArrowDown" });
    expect(document.activeElement).toBe(buttons[0]);

    fireEvent.keyDown(wrapper, { key: "ArrowDown" });
    expect(document.activeElement).toBe(buttons[1]);

    fireEvent.keyDown(wrapper, { key: "ArrowUp" });
    expect(document.activeElement).toBe(buttons[0]);

    input.focus();
    fireEvent.keyDown(wrapper, { key: "Enter" });
    expect(focusSymbol).toHaveBeenCalledWith({ id: "a", name: "Alpha" });
  });

  it("useGraphInspection keeps the latest inspected node when earlier requests resolve late", async () => {
    const firstNode = deferred();
    const secondNode = deferred();
    const firstNeighbors = deferred();
    const secondNeighbors = deferred();

    const loadNode = vi.fn((id) => (id === "first" ? firstNode.promise : secondNode.promise));
    const loadNeighbors = vi.fn((id) =>
      id === "first" ? firstNeighbors.promise : secondNeighbors.promise,
    );

    const { result } = renderHook(() =>
      useGraphInspection({ loadNode, loadNeighbors, onError: vi.fn() }),
    );

    await act(async () => {
      void result.current.inspect("first");
      void result.current.inspect("second");
    });

    await act(async () => {
      secondNode.resolve({ node: { id: "second", name: "Second" } });
      secondNeighbors.resolve({ callers: [{ id: "caller-2" }], callees: [] });
      await Promise.all([secondNode.promise, secondNeighbors.promise]);
    });

    expect(result.current.selected).toMatchObject({ id: "second", name: "Second" });
    expect(result.current.neighbors).toMatchObject({ callers: [{ id: "caller-2" }], callees: [] });

    await act(async () => {
      firstNode.resolve({ node: { id: "first", name: "First" } });
      firstNeighbors.resolve({ callers: [{ id: "caller-1" }], callees: [] });
      await Promise.all([firstNode.promise, firstNeighbors.promise]);
    });

    expect(result.current.selected).toMatchObject({ id: "second", name: "Second" });
    expect(result.current.neighbors).toMatchObject({ callers: [{ id: "caller-2" }], callees: [] });
  });
});
