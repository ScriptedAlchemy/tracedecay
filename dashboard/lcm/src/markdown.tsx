/* eslint-disable @typescript-eslint/no-explicit-any */

/**
 * Minimal self-contained markdown -> React renderer for the hermes-lcm plugin.
 *
 * Ported 1:1 from the original IIFE. This module manipulates React element
 * trees programmatically (it appends to a previous <li>'s `.props.children`
 * when nesting sub-lists, and reads `.key`), so it intentionally keeps using
 * `React.createElement` directly — JSX has no idiomatic equivalent for the
 * "mutate the previously-pushed element" algorithm, and rewriting it would
 * change behavior. JSX is used only for the public `MarkdownText` wrapper.
 */

import React from "react";

const h = React.createElement;

function mdInlineNodes(text: string, kp: string): any[] {
  const nodes: any[] = [];
  const codeRe = /`([^`]+)`/g;
  let last = 0, m: RegExpExecArray | null, i = 0;
  while ((m = codeRe.exec(text)) !== null) {
    if (m.index > last) mdEmphasis(text.slice(last, m.index), nodes, kp + "t" + i);
    nodes.push(h("code", { key: kp + "c" + i, className: "hermes-lcm-md-code" }, m[1]));
    last = codeRe.lastIndex; i++;
  }
  if (last < text.length) mdEmphasis(text.slice(last), nodes, kp + "t" + i);
  return nodes;
}

// Underscores are left literal so snake_case and paths (kanban_block,
// auto_model_routing) are not mangled into emphasis.
function mdEmphasis(str: string, nodes: any[], kp: string): void {
  const re = /(\*\*)([\s\S]+?)\*\*|(\*)([^*\n]+?)\*|\[([^\]]+)\]\(([^)\s]+)\)/;
  let rest = str, i = 0, m: RegExpExecArray | null;
  while ((m = re.exec(rest)) !== null) {
    if (m.index > 0) nodes.push(rest.slice(0, m.index));
    if (m[1]) nodes.push(h("strong", { key: kp + "b" + i }, mdInlineNodes(m[2], kp + "b" + i + "-")));
    else if (m[3]) nodes.push(h("em", { key: kp + "e" + i }, mdInlineNodes(m[4], kp + "e" + i + "-")));
    else nodes.push(h("a", {
      key: kp + "a" + i, href: m[6], target: "_blank", rel: "noopener noreferrer",
      className: "hermes-lcm-md-link",
    }, m[5]));
    rest = rest.slice(m.index + m[0].length); i++;
  }
  if (rest) nodes.push(rest);
}

function mdBuildList(items: any[], kp: string): any {
  const base = items[0].indent;
  const ordered = items[0].ordered;
  const children: any[] = [];
  let i = 0, li = 0;
  while (i < items.length) {
    if (items[i].indent > base) {
      const start = i;
      while (i < items.length && items[i].indent > base) i++;
      const nested = mdBuildList(items.slice(start), kp + "n" + li);
      if (children.length) {
        const prev = children[children.length - 1];
        children[children.length - 1] = h("li", { key: prev.key },
          [].concat(prev.props.children, nested));
      } else {
        children.push(h("li", { key: kp + "li" + li++ }, nested));
      }
      continue;
    }
    children.push(h("li", { key: kp + "li" + li++ }, mdInlineNodes(items[i].text, kp + "x" + li)));
    i++;
  }
  return h(ordered ? "ol" : "ul", { key: kp, className: "hermes-lcm-md-list" }, children);
}

export function mdToReact(src: any): any[] {
  const lines = String(src == null ? "" : src).replace(/\r\n?/g, "\n").split("\n");
  const blocks: any[] = [];
  let i = 0, key = 0;
  while (i < lines.length) {
    const line = lines[i];
    if (/^\s*```/.test(line)) {
      const buf: string[] = [];
      i++;
      while (i < lines.length && !/^\s*```/.test(lines[i])) { buf.push(lines[i]); i++; }
      i++;
      blocks.push(h("pre", { key: "p" + key++, className: "hermes-lcm-md-pre" },
        h("code", null, buf.join("\n"))));
      continue;
    }
    if (/^\s*$/.test(line)) { i++; continue; }
    const hd = line.match(/^(#{1,6})\s+(.*)$/);
    if (hd) {
      blocks.push(h("div", {
        key: "p" + key++,
        className: "hermes-lcm-md-h hermes-lcm-md-h" + hd[1].length,
      }, mdInlineNodes(hd[2], "h" + key)));
      i++; continue;
    }
    if (/^\s*>\s?/.test(line)) {
      const buf: string[] = [];
      while (i < lines.length && /^\s*>\s?/.test(lines[i])) {
        buf.push(lines[i].replace(/^\s*>\s?/, "")); i++;
      }
      blocks.push(h("blockquote", { key: "p" + key++, className: "hermes-lcm-md-quote" },
        mdInlineNodes(buf.join(" "), "q" + key)));
      continue;
    }
    if (/^\s*([-*+]|\d+[.)])\s+/.test(line)) {
      const items: any[] = [];
      while (i < lines.length && /^\s*([-*+]|\d+[.)])\s+/.test(lines[i])) {
        const mm = lines[i].match(/^(\s*)([-*+]|\d+[.)])\s+(.*)$/);
        items.push({ indent: mm![1].length, ordered: /\d/.test(mm![2]), text: mm![3] });
        i++;
      }
      blocks.push(mdBuildList(items, "l" + key++));
      continue;
    }
    const buf: string[] = [];
    while (i < lines.length && !/^\s*$/.test(lines[i])
      && !/^\s*```/.test(lines[i])
      && !/^(#{1,6})\s+/.test(lines[i])
      && !/^\s*>\s?/.test(lines[i])
      && !/^\s*([-*+]|\d+[.)])\s+/.test(lines[i])) {
      buf.push(lines[i]); i++;
    }
    const kids: any[] = [];
    buf.forEach(function (ln, idx) {
      if (idx) kids.push(h("br", { key: "br" + idx }));
      const sub = mdInlineNodes(ln, "p" + key + "-" + idx);
      for (let s = 0; s < sub.length; s++) kids.push(sub[s]);
    });
    blocks.push(h("p", { key: "p" + key++, className: "hermes-lcm-md-p" }, kids));
  }
  return blocks;
}

export function MarkdownText(props: { text: any; className?: string }): React.ReactElement {
  const text = String(props.text == null ? "" : props.text);
  let nodes: any[];
  try { nodes = mdToReact(text); } catch (e) { nodes = [text]; }
  return (
    <div className={"hermes-lcm-md" + (props.className ? " " + props.className : "")}>
      {nodes}
    </div>
  );
}
