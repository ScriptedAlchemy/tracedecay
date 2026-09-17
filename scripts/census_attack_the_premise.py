#!/usr/bin/env python3
"""Census which actors hold a role that repeated fixes left in place.

Two premises failed the same gate more than once on PR #707. This script
counts actors (presence), not magnitudes. Rerun from the repo root:

    python3 scripts/census_attack_the_premise.py
    python3 scripts/census_attack_the_premise.py --check
    python3 scripts/census_attack_the_premise.py --self-test

Premise A — nested chunk admission.
    "A parent that already holds a background-CPU unit must return that unit
    to the global FIFO around every nested chunk join."
Failed the wedge/merge gate as call-site yield wraps (dropped in integration),
restored wraps, then a welded yield inside chunks::fan_out. The return path
left the parent holding the role on every large batch.

Premise B — terminal publication admission.
    "The worker task that observed publication corruption holds terminal
    suppress, so a local bool is the admission authority."
Review passes on the same tip both rejected that bool beside the typed park.
A restarted worker on the same mount does not see the bool. The park slot is
the actor every reader already shares.
"""

from __future__ import annotations

import argparse
import sys
from dataclasses import dataclass
from pathlib import Path


POOL_TOKENS = ("par_iter", "par_chunks", "par_bridge")
YIELD_LEAF = "with_yielded_background_cpu_permits"
YIELD_BOUNDARY = "with_yielded_permits"
PERMIT_LEAF = "with_background_cpu_permit"
TERMINAL_BOOL = "publication_authority_terminal"


@dataclass(frozen=True)
class Actor:
    kind: str
    path: str
    line: int
    detail: str

    def render(self) -> str:
        return f"{self.kind}\t{self.path}:{self.line}\t{self.detail}"


def _blank_span(span: str) -> str:
    return "".join("\n" if char == "\n" else " " for char in span)


def strip_comments_and_strings(source: str) -> str:
    """Drop comments and string literals while keeping newlines.

    Line numbers stay aligned with the original file. String literals are
    blanked so a test that mentions a token inside `contains("...")` is not
    counted as an actor that calls it.
    """
    out: list[str] = []
    index = 0
    length = len(source)
    while index < length:
        if source.startswith("//", index):
            end = source.find("\n", index)
            if end < 0:
                out.append(" " * (length - index))
                break
            out.append(" " * (end - index))
            index = end
            continue
        if source.startswith("/*", index):
            end = source.find("*/", index + 2)
            if end < 0:
                out.append(" " * (length - index))
                break
            out.append(_blank_span(source[index : end + 2]))
            index = end + 2
            continue
        if source.startswith('r#"', index) or source.startswith('r"', index):
            if source.startswith('r#"', index):
                end = source.find('"#', index + 3)
                end = length if end < 0 else end + 2
            else:
                end = source.find('"', index + 2)
                end = length if end < 0 else end + 1
            out.append(_blank_span(source[index:end]))
            index = end
            continue
        if source[index] == '"':
            end = index + 1
            while end < length:
                if source[end] == "\\":
                    end += 2
                    continue
                if source[end] == '"':
                    end += 1
                    break
                end += 1
            out.append(_blank_span(source[index:end]))
            index = end
            continue
        out.append(source[index])
        index += 1
    return "".join(out)


def line_of(source: str, offset: int) -> int:
    return source.count("\n", 0, offset) + 1


def enclosing_fn(source: str, offset: int) -> str:
    window = source[:offset]
    marker = window.rfind("fn ")
    while marker >= 0:
        before = source[marker - 1] if marker else " "
        if before.isalnum() or before == "_":
            marker = window.rfind("fn ", 0, marker)
            continue
        name_start = marker + 3
        name_end = name_start
        while name_end < len(source) and (source[name_end].isalnum() or source[name_end] == "_"):
            name_end += 1
        if name_end > name_start and name_end < len(source) and source[name_end] in "(<":
            return source[name_start:name_end]
        marker = window.rfind("fn ", 0, marker)
    return "<unknown>"


def matching_brace(source: str, open_at: int) -> int | None:
    depth = 0
    for index in range(open_at, len(source)):
        char = source[index]
        if char == "{":
            depth += 1
        elif char == "}":
            depth -= 1
            if depth == 0:
                return index
    return None


def closure_body(source: str, call_at: int) -> tuple[int, int] | None:
    paren = source.find("(", call_at)
    if paren < 0:
        return None
    brace = source.find("{", paren)
    if brace < 0:
        return None
    end = matching_brace(source, brace)
    if end is None:
        return None
    return brace + 1, end


def in_cfg_test(source: str, offset: int) -> bool:
    marker = "#[cfg(test)]"
    start = 0
    while True:
        found = source.find(marker, start)
        if found < 0 or found > offset:
            return False
        brace = source.find("{", found)
        if brace < 0 or brace > offset:
            return False
        end = matching_brace(source, brace)
        if end is None:
            return False
        if brace < offset < end:
            return True
        start = end + 1


def scan_source(path: str, source: str) -> list[Actor]:
    code = strip_comments_and_strings(source)
    actors: list[Actor] = []
    for kind, token in (
        ("nested-yield", YIELD_LEAF),
        ("pool-boundary-yield", YIELD_BOUNDARY),
    ):
        start = 0
        while True:
            found = code.find(token, start)
            if found < 0:
                break
            start = found + len(token)
            if code.startswith("(", found + len(token)) is False and not code[
                found + len(token) :
            ].lstrip().startswith("("):
                # definition, not a call
                if "fn " in code[max(0, found - 24) : found]:
                    continue
            if "fn " in code[max(0, found - 8) : found]:
                continue
            function = enclosing_fn(code, found)
            site_kind = f"test:{kind}" if in_cfg_test(source, found) else kind
            if site_kind == "pool-boundary-yield" and function == "install":
                actors.append(
                    Actor(kind, path, line_of(source, found), "install boundary")
                )
                continue
            if function == "with_yielded_background_cpu_permits":
                actors.append(
                    Actor(
                        "nested-yield",
                        path,
                        line_of(source, found),
                        "compensation wrapper",
                    )
                )
                continue
            actors.append(Actor(site_kind, path, line_of(source, found), function))

    start = 0
    while True:
        found = code.find(PERMIT_LEAF, start)
        if found < 0:
            break
        start = found + len(PERMIT_LEAF)
        # with_background_cpu_permits is the weighted sibling, same role.
        body = closure_body(code, found)
        if body is None:
            continue
        snippet = code[body[0] : body[1]]
        if any(token in snippet for token in POOL_TOKENS):
            kind = "nested-permit-join"
            if in_cfg_test(source, found):
                kind = f"test:{kind}"
            actors.append(
                Actor(kind, path, line_of(source, found), enclosing_fn(code, found))
            )

    start = 0
    while True:
        found = code.find(TERMINAL_BOOL, start)
        if found < 0:
            break
        start = found + len(TERMINAL_BOOL)
        kind = "worker-local-terminal"
        if in_cfg_test(source, found):
            kind = f"test:{kind}"
        actors.append(Actor(kind, path, line_of(source, found), enclosing_fn(code, found)))
    return actors


def crate_sources(root: Path) -> list[tuple[str, str]]:
    files: list[tuple[str, str]] = []
    crates = root / "crates"
    if not crates.is_dir():
        return files
    for path in sorted(crates.rglob("*.rs")):
        relative = path.relative_to(root).as_posix()
        if "/target/" in f"/{relative}/":
            continue
        files.append((relative, path.read_text(encoding="utf-8")))
    return files


def census(root: Path) -> list[Actor]:
    actors: list[Actor] = []
    for path, source in crate_sources(root):
        actors.extend(scan_source(path, source))
    return actors


def violations(actors: list[Actor]) -> list[Actor]:
    """Production actors that still hold a role the failed fixes assumed.

    The install-boundary yield is listed in the census and is not a violation:
    it is one actor, and the failed gate was nested leaf joins plus the
    worker-local terminal flag. Test-module mentions are evidence, not a
    production assignment.
    """
    return [
        actor
        for actor in actors
        if not actor.kind.startswith("test:")
        and actor.kind in {"nested-yield", "nested-permit-join", "worker-local-terminal"}
    ]


def render(actors: list[Actor]) -> str:
    lines = [
        "actor\tlocation\tdetail",
        "----",
    ]
    if not actors:
        lines.append("(no actors hold the repeated role)")
    else:
        lines.extend(actor.render() for actor in actors)
    kinds: dict[str, int] = {}
    for actor in actors:
        kinds[actor.kind] = kinds.get(actor.kind, 0) + 1
    lines.append("----")
    lines.append(
        "counts: "
        + ", ".join(f"{kind}={count}" for kind, count in sorted(kinds.items()) or ["none"])
    )
    return "\n".join(lines)


def self_test() -> int:
    nested = """
fn map_yielding() {
    with_yielded_background_cpu_permits(|| {
        items.into_par_iter().map(map).collect()
    });
}
"""
    leaf = """
fn collect() {
    items.par_iter().map(|item| {
        with_background_cpu_permit(|| operation(item))
    }).collect();
}
"""
    inside = """
fn bad() {
    with_background_cpu_permit(|| {
        items.par_iter().for_each(|_| {});
    });
}
"""
    boundary = """
fn install() {
    self.background_cpu.with_yielded_permits(|| self.pool.install(operation))
}
"""
    terminal = """
fn worker() {
    let mut publication_authority_terminal = false;
    if publication_authority_terminal {
        publication_authority_terminal = true;
    }
}
"""
    # The detector must not count its own documentation of the tokens.
    comment = """
fn note() {
    // with_yielded_background_cpu_permits is the failed return path
    // publication_authority_terminal was the task-local role
}
"""
    mentioned = """
fn pin() {
    assert!(source.contains("with_yielded_background_cpu_permits("));
    assert!(!source.contains("publication_authority_terminal"));
}
"""
    cases = {
        "nested yield": (nested, {"nested-yield"}),
        "leaf permit outside join": (leaf, set()),
        "permit closure joins the pool": (inside, {"nested-permit-join"}),
        "install boundary": (boundary, {"pool-boundary-yield"}),
        "worker-local terminal": (terminal, {"worker-local-terminal"}),
        "commented tokens": (comment, set()),
        "string mention": (mentioned, set()),
    }
    failed = False
    for name, (source, expected) in cases.items():
        found = {actor.kind for actor in scan_source("fixture.rs", source)}
        if found != expected:
            print(f"self-test failed: {name}: got {sorted(found)} expected {sorted(expected)}")
            failed = True
    if failed:
        return 1
    print("census_attack_the_premise self-test: ok")
    return 0


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--check",
        action="store_true",
        help="exit non-zero when a repeated role is still assigned",
    )
    parser.add_argument("--self-test", action="store_true", help="run classifier fixtures")
    parser.add_argument(
        "--root",
        type=Path,
        default=Path(__file__).resolve().parents[1],
        help="repository root (default: parent of scripts/)",
    )
    args = parser.parse_args(argv)
    if args.self_test:
        return self_test()
    actors = census(args.root)
    print(render(actors))
    failed = violations(actors)
    if args.check and failed:
        print(f"check failed: {len(failed)} actor(s) still hold a repeated role", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
