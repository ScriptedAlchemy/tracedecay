import { useEffect, useMemo, useRef, useState } from "react";
import { useWorkspaceState } from "../app/workspace";
import type { CodeLens, CoreFile, SemanticDataset, SemanticNode, SemanticRegion } from "./semanticData";

type Camera = { x: number; y: number; scale: number };
type Hit =
  | { id: string; shape: "circle"; x: number; y: number; radius: number }
  | { id: string; shape: "ellipse"; x: number; y: number; rx: number; ry: number }
  | { id: string; shape: "rect"; x: number; y: number; width: number; height: number };
const WORLD = { width: 1400, height: 900 };

function validCamera(value: Camera) {
  return Number.isFinite(value.x) && Number.isFinite(value.y) && Number.isFinite(value.scale) && value.scale >= .55 && value.scale <= 3.5;
}

function hash(value: string) {
  let out = 2166136261;
  for (const char of value) out = Math.imul(out ^ char.charCodeAt(0), 16777619);
  return out >>> 0;
}

function blob(ctx: CanvasRenderingContext2D, region: SemanticRegion, shrink = 0) {
  const seed = hash(region.id);
  const points = 64;
  ctx.beginPath();
  for (let index = 0; index <= points; index += 1) {
    const angle = (index / points) * Math.PI * 2;
    const wobble = 1 + Math.sin(angle * (3 + seed % 3) + seed * .001) * .07 + Math.cos(angle * 7 + seed * .003) * .035;
    const x = region.x + Math.cos(angle) * Math.max(4, region.rx - shrink) * wobble;
    const y = region.y + Math.sin(angle) * Math.max(3, region.ry - shrink * .6) * wobble;
    if (index === 0) ctx.moveTo(x, y); else ctx.lineTo(x, y);
  }
  ctx.closePath();
}

function curve(ctx: CanvasRenderingContext2D, ax: number, ay: number, bx: number, by: number) {
  const bend = Math.max(35, Math.abs(bx - ax) * .12);
  ctx.beginPath();
  ctx.moveTo(ax, ay);
  ctx.bezierCurveTo(ax, ay + (by > ay ? bend : -bend), bx, by - (by > ay ? bend : -bend), bx, by);
}

function lineColor(kind: string) {
  if (kind === "test") return "#c48dff";
  if (kind === "struct" || kind === "trait") return "#67d7ff";
  if (kind === "unresolved") return "#f0b46b";
  return "#74f5e3";
}

function wrapCoreName(name: string) {
  const words = name.split(/[_\s]+/).filter(Boolean);
  const lines: string[] = [];
  for (const word of words) {
    const prior = lines.at(-1);
    if (prior && `${prior}_${word}`.length <= 14) lines[lines.length - 1] = `${prior}_${word}`;
    else lines.push(word);
  }
  return lines.length ? lines : [name];
}

export function SemanticField({ dataset, lens, selectedId, coreView = "overview", onCoreView, onSelect, onPreview, onDrill, onBack }: {
  dataset: SemanticDataset;
  lens: CodeLens;
  selectedId: string;
  coreView?: "overview" | "range";
  onCoreView?: (view: "overview" | "range") => void;
  onSelect: (id: string) => void;
  onPreview: (id: string | null) => void;
  onDrill: () => void;
  onBack: () => void;
}) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const miniRef = useRef<HTMLCanvasElement>(null);
  const hostRef = useRef<HTMLDivElement>(null);
  const sizeRef = useRef({ width: 1000, height: 650 });
  const hits = useRef<Hit[]>([]);
  const drag = useRef<{ x: number; y: number; camera: Camera; moved: boolean } | null>(null);
  const [hovered, setHovered] = useState<string | null>(null);
  const fitCamera = useMemo<Camera>(() => ({ x: 0, y: lens === "core" ? -110 : 0, scale: 1 }), [lens]);
  const [storedCamera, setStoredCamera] = useWorkspaceState<Camera>(`code.semantic-camera:${lens}`, fitCamera);
  const camera = validCamera(storedCamera) ? storedCamera : fitCamera;
  function setCamera(action: Camera | ((prior: Camera) => Camera)) {
    setStoredCamera((prior) => {
      const safePrior = validCamera(prior) ? prior : fitCamera;
      const next = typeof action === "function" ? action(safePrior) : action;
      return validCamera(next) ? next : fitCamera;
    });
  }
  const reduced = useMemo(() => matchMedia("(prefers-reduced-motion: reduce)").matches, []);
  const nodeById = useMemo(() => new Map(dataset.nodes.map((node) => [node.id, node])), [dataset]);

  useEffect(() => { if (!validCamera(storedCamera)) setStoredCamera(fitCamera); }, [fitCamera, setStoredCamera, storedCamera]);

  function toWorld(clientX: number, clientY: number) {
    const box = canvasRef.current!.getBoundingClientRect();
    const { width, height } = sizeRef.current;
    const fit = Math.min(width / WORLD.width, height / WORLD.height) * .94;
    return {
      x: WORLD.width / 2 + (clientX - box.left - width / 2 - camera.x) / (fit * camera.scale),
      y: WORLD.height / 2 + (clientY - box.top - height / 2 - camera.y) / (fit * camera.scale),
    };
  }

  useEffect(() => {
    const canvas = canvasRef.current;
    const host = hostRef.current;
    if (!canvas || !host) return;
    const observer = new ResizeObserver(() => {
      sizeRef.current = { width: Math.max(320, host.clientWidth), height: Math.max(360, host.clientHeight) };
      draw(performance.now());
    });
    observer.observe(host);
    let frame = 0;
    const animate = (time: number) => {
      draw(time);
      if (hovered && !reduced) frame = requestAnimationFrame(animate);
    };
    draw(performance.now());
    if (hovered && !reduced) frame = requestAnimationFrame(animate);
    return () => { observer.disconnect(); cancelAnimationFrame(frame); };

    function draw(time: number) {
      const ctx = canvas!.getContext("2d");
      if (!ctx) return;
      const { width, height } = sizeRef.current;
      const dpr = Math.min(devicePixelRatio || 1, 2);
      canvas!.width = Math.round(width * dpr);
      canvas!.height = Math.round(height * dpr);
      canvas!.style.width = `${width}px`;
      canvas!.style.height = `${height}px`;
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
      const wash = ctx.createRadialGradient(width * .48, height * .44, 10, width * .48, height * .44, width * .7);
      wash.addColorStop(0, "#0b1c29"); wash.addColorStop(.58, "#06111c"); wash.addColorStop(1, "#030810");
      ctx.fillStyle = wash; ctx.fillRect(0, 0, width, height);
      const fit = Math.min(width / WORLD.width, height / WORLD.height) * .94;
      ctx.save();
      ctx.translate(width / 2 + camera.x, height / 2 + camera.y);
      ctx.scale(fit * camera.scale, fit * camera.scale);
      ctx.translate(-WORLD.width / 2, -WORLD.height / 2);
      hits.current = [];
      if (lens === "cortex" || !dataset.semanticAvailable) drawCortex(ctx, time, !dataset.semanticAvailable && lens !== "cortex");
      else if (lens === "trace") drawTrace(ctx, time);
      else drawCore(ctx, time);
      ctx.restore();
      drawMini();
    }

    function drawCortex(ctx: CanvasRenderingContext2D, time: number, dimmed: boolean) {
      const regions = new Map(dataset.regions.map((region) => [region.id, region]));
      const chosenModule = nodeById.get(selectedId)?.module ?? selectedId;
      ctx.globalAlpha = dimmed ? .22 : 1;
      for (const depth of [...new Set(dataset.regions.map((region) => region.depth))].sort((a, b) => b - a)) {
        const y = dataset.regions.find((region) => region.depth === depth)?.y; if (y === undefined) continue;
        ctx.strokeStyle = "rgba(78,135,151,.14)"; ctx.lineWidth = .8; ctx.setLineDash([3, 9]); ctx.beginPath(); ctx.moveTo(26, y); ctx.lineTo(WORLD.width - 26, y); ctx.stroke(); ctx.setLineDash([]);
        ctx.fillStyle = "#4f737e"; ctx.font = "11px ui-monospace, monospace"; ctx.textAlign = "left"; ctx.fillText(`STRATUM ${depth}`, 30, y - 9);
      }
      for (const edge of dataset.edges) {
        const aNode = nodeById.get(edge.source); const bNode = nodeById.get(edge.target);
        const a = regions.get(aNode?.module ?? edge.source); const b = regions.get(bNode?.module ?? edge.target);
        if (!a || !b || a === b) continue;
        const selectedManifest = a.id === chosenModule || b.id === chosenModule;
        if (dataset.source === "measured-snapshot" && !selectedManifest) continue;
        curve(ctx, a.x, a.y, b.x, b.y);
        ctx.strokeStyle = edge.kind === "manifest" ? "rgba(105,211,241,.58)" : "rgba(86,236,221,.34)";
        ctx.lineWidth = edge.kind === "manifest" ? 2.4 : Math.min(9, 1.5 + Math.sqrt(edge.weight));
        ctx.shadowColor = edge.kind === "manifest" ? "#48bddc" : "#45e4d5"; ctx.shadowBlur = 3; ctx.stroke(); ctx.shadowBlur = 0;
      }
      for (const region of dataset.regions) {
        const selected = region.id === chosenModule;
        blob(ctx, region);
        const fill = ctx.createRadialGradient(region.x - region.rx * .22, region.y - region.ry * .32, 2, region.x, region.y, region.rx);
        const warmth = region.warmth ?? 0;
        fill.addColorStop(0, `rgba(78,229,225,${.20 + warmth * .15})`);
        fill.addColorStop(.7, region.warmth === null ? "rgba(37,75,89,.23)" : `rgba(${35 + Math.round(warmth * 100)},80,103,.25)`);
        fill.addColorStop(1, "rgba(6,18,28,.74)");
        ctx.fillStyle = fill; ctx.fill();
        const contours = Math.min(6, Math.max(1, Math.round(region.density + 1)));
        for (let i = 0; i < contours; i += 1) {
          blob(ctx, region, 8 + i * 8);
          ctx.strokeStyle = selected && i === 0 ? "#c8ffff" : `rgba(89,210,218,${.18 + i * .035})`;
          ctx.lineWidth = selected && i === 0 ? 2.5 : .85; ctx.stroke();
        }
        if (selected) { blob(ctx, region, -5); ctx.strokeStyle = "rgba(136,252,255,.9)"; ctx.lineWidth = 2; ctx.shadowColor = "#5ef5ff"; ctx.shadowBlur = 18; ctx.stroke(); ctx.shadowBlur = 0; }
        const showLabel = dataset.source === "authored-fixture" || selected || region.rx >= 26;
        if (!showLabel) {
          hits.current.push({ id: region.nodeIds[0] ?? region.id, shape: "ellipse", x: region.x, y: region.y, rx: Math.max(14, region.rx), ry: Math.max(12, region.ry) });
          continue;
        }
        const labelParts = region.label.split(/\s*\/\s*|\s+\+\s+/);
        const compact = region.rx < 74;
        const labelX = compact ? region.x - region.rx - 12 : region.x;
        const labelY = compact ? region.y - 7 : region.y - (labelParts.length - 1) * 8;
        ctx.fillStyle = selected ? "#efffff" : "#b2d1d8"; ctx.font = `${selected ? 17 : 14}px ui-monospace, monospace`; ctx.textAlign = compact ? "right" : "center";
        labelParts.forEach((part, index) => ctx.fillText(part, labelX, labelY + index * 16));
        ctx.fillStyle = "#7899a3"; ctx.font = "11px ui-monospace, monospace";
        ctx.fillText(`${region.mass} ${dataset.source === "authored-fixture" ? "symbols" : "files"} · depth ${region.depth}`, compact ? labelX : region.x, compact ? region.y + 19 : region.y + region.ry + 15);
        ctx.textAlign = "center";
        hits.current.push({ id: region.nodeIds[0] ?? region.id, shape: "ellipse", x: region.x, y: region.y, rx: Math.max(14, region.rx), ry: Math.max(12, region.ry) });
      }
      ctx.globalAlpha = 1;
    }

    function drawTrace(ctx: CanvasRenderingContext2D, time: number) {
      ctx.globalAlpha = .15;
      for (const region of dataset.regions) { blob(ctx, { ...region, x: region.x, y: region.y * .92 + 26 }, 0); ctx.fillStyle = "#133341"; ctx.fill(); ctx.strokeStyle = "#2b6872"; ctx.lineWidth = 1; ctx.stroke(); }
      ctx.globalAlpha = 1;
      for (const membrane of dataset.membranes) {
        const members = membrane.nodeIds.map((id) => nodeById.get(id)).filter((node): node is SemanticNode => !!node);
        if (members.length < 2) continue;
        const left = Math.min(...members.map((node) => node.x)) - 52, right = Math.max(...members.map((node) => node.x)) + 52;
        const top = Math.min(...members.map((node) => node.y)) - 35, bottom = Math.max(...members.map((node) => node.y)) + 35;
        ctx.beginPath(); ctx.roundRect(left, top, right - left, bottom - top, 24);
        ctx.fillStyle = membrane.kind === "trait" ? "rgba(200,139,255,.055)" : "rgba(74,224,217,.055)"; ctx.fill();
        ctx.strokeStyle = membrane.kind === "trait" ? "rgba(198,139,255,.62)" : "rgba(91,224,219,.56)"; ctx.setLineDash([4, 6]); ctx.lineWidth = 1.25; ctx.stroke(); ctx.setLineDash([]);
        ctx.fillStyle = "#89a9b3"; ctx.font = "11px ui-monospace, monospace"; ctx.textAlign = "left"; ctx.fillText(membrane.label, left + 12, top - 8);
      }
      for (const edge of dataset.edges) {
        const a = nodeById.get(edge.source), b = nodeById.get(edge.target); if (!a || !b) continue;
        const active = selectedId === a.id || selectedId === b.id;
        curve(ctx, a.x, a.y, b.x, b.y);
        ctx.strokeStyle = edge.direction === "unknown" ? "rgba(242,171,95,.9)" : active ? "rgba(165,255,248,.98)" : edge.direction === "caller" ? "rgba(93,208,244,.66)" : "rgba(100,241,210,.61)";
        ctx.setLineDash(edge.direction === "unknown" ? [7, 8] : []);
        ctx.lineWidth = Math.min(10, 1.4 + Math.sqrt(edge.weight) * .75); ctx.shadowColor = active ? "#4cf4e8" : "transparent"; ctx.shadowBlur = active ? 5 : 0; ctx.stroke(); ctx.shadowBlur = 0;
        ctx.setLineDash([]);
      }
      for (const node of dataset.nodes) {
        const selected = node.id === selectedId, hover = node.id === hovered;
        const pulse = hover && !reduced ? 1 + Math.sin(time / 150) * .07 : 1;
        const radius = (8 + Math.sqrt(Math.max(1, node.degree)) * .8) * pulse;
        ctx.beginPath(); ctx.ellipse(node.x, node.y, radius * 1.35, radius * .55, 0, 0, Math.PI * 2);
        ctx.fillStyle = selected ? "#f1ffff" : lineColor(node.kind); ctx.globalAlpha = selected ? 1 : .92; ctx.fill(); ctx.globalAlpha = 1;
        if (selected || hover) { ctx.strokeStyle = selected ? "#fff" : "#8efff2"; ctx.lineWidth = 1.5; ctx.shadowColor = "#56f5e9"; ctx.shadowBlur = 14; ctx.stroke(); ctx.shadowBlur = 0; }
        const above = node.ring <= 0; ctx.fillStyle = selected ? "#f7ffff" : "#bad1d8"; ctx.font = `${selected ? 16 : 13}px ui-monospace, monospace`; ctx.textAlign = "center";
        ctx.fillText(node.name, node.x, node.y + (above ? -18 : 26), 150);
        if (selected) { ctx.fillStyle = "#77a4af"; ctx.font = "9px ui-monospace, monospace"; ctx.fillText(`${node.kind} · degree ${node.degree}`, node.x, node.y + (above ? -6 : 38)); }
        hits.current.push({ id: node.id, shape: "circle", x: node.x, y: node.y, radius: Math.max(18, radius * 1.6) });
      }
      for (const ring of [-3, -2, -1, 0, 1, 2, 3]) {
        const node = dataset.nodes.find((candidate) => candidate.ring === ring); if (!node) continue;
        ctx.fillStyle = "#45636d"; ctx.font = "10px ui-monospace, monospace"; ctx.textAlign = "left";
        ctx.fillText(ring === 0 ? "FOCUS BASIN" : `${Math.abs(ring)} HOP${Math.abs(ring) > 1 ? "S" : ""} ${ring < 0 ? "UPSTREAM" : "DOWNSTREAM"}`, 30, node.y + 4);
      }
    }

    function drawCore(ctx: CanvasRenderingContext2D, _time: number) {
      const files = dataset.files;
      const gap = 20, side = 250, left = side, width = (WORLD.width - side * 2 - gap * (files.length - 1)) / Math.max(1, files.length);
      const maxLines = Math.max(...files.map((file) => file.lines), 1);
      const plotTop = 140, plotHeight = 1100, plotBottom = plotTop + plotHeight;
      const fileByNode = new Map(files.flatMap((file, index) => file.symbols.map((symbol) => [symbol.id, { file, index, symbol }] as const)));
      const selectedSource = fileByNode.get(selectedId);
      const padding = selectedSource ? Math.max(34, Math.round((selectedSource.symbol.end - selectedSource.symbol.start) * .32)) : 40;
      const rangeStart = coreView === "range" && selectedSource ? Math.max(0, selectedSource.symbol.start - padding) : 0;
      const rangeEnd = coreView === "range" && selectedSource ? Math.min(maxLines, selectedSource.symbol.end + padding) : maxLines;
      const pixelsPerLine = plotHeight / Math.max(1, rangeEnd - rangeStart);
      const top = plotTop - rangeStart * pixelsPerLine;
      const incident = new Set(dataset.edges.flatMap((edge) => edge.source === selectedId ? [edge.target] : edge.target === selectedId ? [edge.source] : []));
      const callouts: Array<{ lines: string[]; x: number; y: number; anchorX: number; anchorY: number; align: CanvasTextAlign; selected: boolean }> = [];
      const ticks = coreView === "range" && selectedSource
        ? [...new Set([rangeStart, selectedSource.symbol.start, selectedSource.symbol.end, rangeEnd])]
        : [...new Set([0, 250, 500, 750, 1000, maxLines].filter((line) => line <= maxLines))];
      ctx.save();
      ctx.beginPath(); ctx.rect(0, plotTop, WORLD.width, plotHeight); ctx.clip();
      for (const line of ticks) {
        const y = top + line * pixelsPerLine;
        ctx.strokeStyle = line === selectedSource?.symbol.start || line === selectedSource?.symbol.end ? "rgba(115,236,237,.28)" : "rgba(75,124,139,.16)";
        ctx.lineWidth = 1; ctx.setLineDash([3, 8]); ctx.beginPath(); ctx.moveTo(74, y); ctx.lineTo(WORLD.width - 74, y); ctx.stroke(); ctx.setLineDash([]);
      }
      for (const edge of dataset.edges) {
        const a = fileByNode.get(edge.source), b = fileByNode.get(edge.target);
        if (!a || !b || a.file === b.file || (edge.source !== selectedId && edge.target !== selectedId)) continue;
        const ax = left + a.index * (width + gap) + width * .92, ay = top + a.symbol.start * pixelsPerLine;
        const bx = left + b.index * (width + gap) + width * .08, by = top + b.symbol.start * pixelsPerLine;
        ctx.beginPath(); ctx.moveTo(ax, ay); ctx.bezierCurveTo((ax + bx) / 2, ay, (ax + bx) / 2, by, bx, by);
        ctx.strokeStyle = edge.direction === "unknown" ? "rgba(240,174,102,.62)" : "rgba(81,220,218,.28)";
        ctx.setLineDash(edge.direction === "unknown" ? [6, 7] : []); ctx.lineWidth = Math.min(5, 1 + Math.sqrt(edge.weight) * .3); ctx.stroke(); ctx.setLineDash([]);
      }
      files.forEach((file, fileIndex) => {
        const x = left + fileIndex * (width + gap), height = file.lines * pixelsPerLine;
        ctx.fillStyle = "rgba(7,20,29,.94)"; ctx.fillRect(x, top, width, height);
        ctx.strokeStyle = file.symbols.some((symbol) => symbol.id === selectedId) ? "#76f5ef" : "#214657"; ctx.lineWidth = file.symbols.some((symbol) => symbol.id === selectedId) ? 2 : 1; ctx.strokeRect(x, top, width, height);
        ctx.save(); ctx.beginPath(); ctx.rect(x, top, width, height); ctx.clip();
        for (let line = 200; line < file.lines; line += 200) { const y = top + line * pixelsPerLine; ctx.strokeStyle = "rgba(79,121,137,.18)"; ctx.lineWidth = .6; ctx.beginPath(); ctx.moveTo(x, y); ctx.lineTo(x + width, y); ctx.stroke(); }
        for (const call of dataset.edges) {
          if (call.source !== selectedId && call.target !== selectedId) continue;
          const a = file.symbols.find((symbol) => symbol.id === call.source), b = file.symbols.find((symbol) => symbol.id === call.target); if (!a || !b) continue;
          const ay = top + a.start * pixelsPerLine, by = top + b.start * pixelsPerLine;
          ctx.beginPath(); ctx.moveTo(x + width * .22, ay); ctx.bezierCurveTo(x + 4, ay, x + 4, by, x + width * .22, by);
          ctx.strokeStyle = "rgba(81,235,219,.55)"; ctx.lineWidth = 1 + Math.sqrt(call.weight) * .35; ctx.stroke();
        }
        for (const symbol of file.symbols) {
          const y = top + symbol.start * pixelsPerLine, h = Math.max(1, (symbol.end - symbol.start) * pixelsPerLine);
          const selected = symbol.id === selectedId, hover = symbol.id === hovered;
          ctx.fillStyle = selected ? "#e6fffc" : lineColor(symbol.kind); ctx.globalAlpha = selected ? .96 : .72; ctx.fillRect(x + width * .25, y, width * .69, h); ctx.globalAlpha = 1;
          if (selected || hover || incident.has(symbol.id)) {
            const placeRight = dataset.edges.some((edge) => edge.source === selectedId && edge.target === symbol.id);
            const labelX = placeRight ? WORLD.width - side + 16 : side - 16;
            const labelY = Math.max(plotTop + 32, Math.min(plotBottom - 20, y + Math.max(15, Math.min(24, h / 2))));
            callouts.push({ lines: wrapCoreName(symbol.name), x: labelX, y: labelY, anchorX: placeRight ? x + width * .94 : x + width * .25, anchorY: y + h / 2, align: placeRight ? "left" : "right", selected });
          }
          hits.current.push({ id: symbol.id, shape: "rect", x: x + width * .25, y, width: width * .69, height: h });
        }
        if (file.indexedTo) { const y = top + file.indexedTo * pixelsPerLine; ctx.fillStyle = "rgba(227,168,91,.11)"; ctx.fillRect(x, y, width, top + height - y); ctx.strokeStyle = "#d8a15f"; ctx.setLineDash([4, 5]); ctx.beginPath(); ctx.moveTo(x, y); ctx.lineTo(x + width, y); ctx.stroke(); ctx.setLineDash([]); for (let hy = y + 7; hy < top + height; hy += 10) { ctx.strokeStyle = "rgba(216,161,95,.18)"; ctx.beginPath(); ctx.moveTo(x, hy); ctx.lineTo(x + width, hy - 15); ctx.stroke(); } }
        ctx.restore();
      });
      for (const align of ["left", "right"] as const) {
        const lane = callouts.filter((callout) => callout.align === align).sort((a, b) => a.y - b.y);
        lane.forEach((callout, index) => { if (index > 0) { const prior = lane[index - 1]; callout.y = Math.max(callout.y, prior.y + prior.lines.length * 29 + 16); } });
      }
      for (const callout of callouts) {
        const lineHeight = 29;
        const calloutTop = callout.y - 26;
        const calloutBottom = callout.y + (callout.lines.length - 1) * lineHeight + 5;
        if (calloutTop < plotTop || calloutBottom > plotBottom) continue;
        ctx.strokeStyle = callout.selected ? "rgba(212,255,255,.9)" : "rgba(90,211,216,.55)"; ctx.lineWidth = callout.selected ? 2 : 1;
        ctx.beginPath(); ctx.moveTo(callout.anchorX, callout.anchorY); ctx.lineTo(callout.x, callout.y - 6); ctx.stroke();
        ctx.fillStyle = callout.selected ? "#edffff" : "#a9cbd1"; ctx.font = "26px ui-monospace, monospace"; ctx.textAlign = callout.align;
        ctx.shadowColor = "#041019"; ctx.shadowBlur = 5;
        callout.lines.forEach((line, index) => ctx.fillText(line, callout.x, callout.y + index * lineHeight));
        ctx.shadowBlur = 0;
      }
      ctx.restore();
      ctx.fillStyle = "#7497a1"; ctx.font = "22px ui-monospace, monospace"; ctx.textAlign = "left"; ctx.fillText(coreView === "range" ? "SOURCE LINES" : "LINES", 32, plotTop - 20);
      for (const line of ticks) {
        const y = top + line * pixelsPerLine;
        ctx.fillStyle = line === selectedSource?.symbol.start || line === selectedSource?.symbol.end ? "#91ffff" : "#7696a0";
        ctx.font = "26px ui-monospace, monospace"; ctx.textAlign = "right"; ctx.fillText(String(line), 64, Math.max(plotTop + 22, Math.min(plotBottom - 4, y + 8)));
      }
      files.forEach((file, fileIndex) => {
        const x = left + fileIndex * (width + gap), basename = file.path.split("/").at(-1) ?? file.path;
        const label = basename === "vendor_bridge.rs" ? "vendor…rs" : basename;
        const headerY = fileIndex % 2 === 0 ? 34 : 76;
        ctx.fillStyle = "#d0e7eb"; ctx.font = "600 27px system-ui, sans-serif"; ctx.textAlign = "center";
        ctx.fillText(label, x + width / 2, headerY);
        ctx.fillStyle = "#7897a1"; ctx.font = "20px ui-monospace, monospace"; ctx.fillText(`${file.lines} lines`, x + width / 2, headerY + 27);
      });
    }

    function drawMini() {
      const mini = miniRef.current; const ctx = mini?.getContext("2d"); if (!mini || !ctx) return;
      mini.width = 170; mini.height = 108; ctx.fillStyle = "#04101a"; ctx.fillRect(0, 0, 170, 108);
      ctx.strokeStyle = "#2a6571"; ctx.strokeRect(1, 1, 168, 106);
      if (lens === "cortex" || !dataset.semanticAvailable) {
        for (const region of dataset.regions) { ctx.beginPath(); ctx.ellipse(region.x / WORLD.width * 170, region.y / WORLD.height * 108, Math.max(2, region.rx / WORLD.width * 170), Math.max(1, region.ry / WORLD.height * 108), 0, 0, Math.PI * 2); ctx.fillStyle = "#276674"; ctx.fill(); }
      } else if (lens === "trace") {
        for (const edge of dataset.edges) { const a = nodeById.get(edge.source), b = nodeById.get(edge.target); if (!a || !b) continue; ctx.beginPath(); ctx.moveTo(a.x / WORLD.width * 170, a.y / WORLD.height * 108); ctx.lineTo(b.x / WORLD.width * 170, b.y / WORLD.height * 108); ctx.strokeStyle = "rgba(89,220,215,.28)"; ctx.stroke(); }
        for (const node of dataset.nodes) { ctx.fillStyle = node.id === selectedId ? "#dfffff" : "#3a8992"; ctx.fillRect(node.x / WORLD.width * 170 - 1, node.y / WORLD.height * 108 - 1, 3, 3); }
      } else {
        const maxLines = Math.max(...dataset.files.map((file) => file.lines), 1), gap = 20, side = 250, left = side, width = (WORLD.width - side * 2 - gap * (dataset.files.length - 1)) / Math.max(1, dataset.files.length);
        const mapHeight = 1380, plotTop = 140, plotHeight = 1100;
        dataset.files.forEach((file, index) => {
          const x = (left + index * (width + gap)) / WORLD.width * 170, columnY = plotTop / mapHeight * 108, columnW = width / WORLD.width * 170;
          ctx.fillStyle = "#1e5261"; ctx.fillRect(x, columnY, columnW, (file.lines / maxLines * plotHeight) / mapHeight * 108);
          const selected = file.symbols.find((symbol) => symbol.id === selectedId);
          if (selected) { ctx.fillStyle = "#cfffff"; ctx.fillRect(x + 1, (plotTop + selected.start / maxLines * plotHeight) / mapHeight * 108, Math.max(2, columnW - 2), Math.max(2, (selected.end - selected.start) / maxLines * plotHeight / mapHeight * 108)); }
        });
      }
      const { width, height } = sizeRef.current; const fit = Math.min(width / WORLD.width, height / WORLD.height) * .94;
      const leftWorld = WORLD.width / 2 + (-width / 2 - camera.x) / (fit * camera.scale), topWorld = WORLD.height / 2 + (-height / 2 - camera.y) / (fit * camera.scale);
      const rightWorld = WORLD.width / 2 + (width / 2 - camera.x) / (fit * camera.scale), bottomWorld = WORLD.height / 2 + (height / 2 - camera.y) / (fit * camera.scale);
      ctx.strokeStyle = "#b8ffff"; ctx.lineWidth = 1;
      if (lens === "core") {
        const maxLines = Math.max(...dataset.files.map((file) => file.lines), 1), mapHeight = 1380, plotTop = 140, plotHeight = 1100;
        const source = dataset.files.flatMap((file) => file.symbols.map((symbol) => ({ file, symbol }))).find(({ symbol }) => symbol.id === selectedId);
        const padding = source ? Math.max(34, Math.round((source.symbol.end - source.symbol.start) * .32)) : 40;
        const rangeStart = coreView === "range" && source ? Math.max(0, source.symbol.start - padding) : 0;
        const rangeEnd = coreView === "range" && source ? Math.min(maxLines, source.symbol.end + padding) : maxLines;
        const toLine = (worldY: number) => rangeStart + (worldY - plotTop) / plotHeight * (rangeEnd - rangeStart);
        const topLine = Math.max(0, Math.min(maxLines, toLine(topWorld))), bottomLine = Math.max(0, Math.min(maxLines, toLine(bottomWorld)));
        const miniTop = (plotTop + topLine / maxLines * plotHeight) / mapHeight * 108, miniBottom = (plotTop + bottomLine / maxLines * plotHeight) / mapHeight * 108;
        ctx.strokeRect(leftWorld / WORLD.width * 170, miniTop, (rightWorld - leftWorld) / WORLD.width * 170, Math.max(2, miniBottom - miniTop));
      } else {
        ctx.strokeRect(leftWorld / WORLD.width * 170, topWorld / WORLD.height * 108, (rightWorld - leftWorld) / WORLD.width * 170, (bottomWorld - topWorld) / WORLD.height * 108);
      }
    }
  }, [camera, coreView, dataset, hovered, lens, nodeById, onBack, onDrill, onPreview, onSelect, reduced, selectedId]);

  function pick(event: { clientX: number; clientY: number }) {
    const point = toWorld(event.clientX, event.clientY);
    return [...hits.current].reverse().find((hit) => hit.shape === "circle" ? Math.hypot(hit.x - point.x, hit.y - point.y) <= hit.radius : hit.shape === "ellipse" ? ((point.x - hit.x) / hit.rx) ** 2 + ((point.y - hit.y) / hit.ry) ** 2 <= 1 : point.x >= hit.x && point.x <= hit.x + hit.width && point.y >= hit.y && point.y <= hit.y + hit.height);
  }
  function changeHover(id: string | null) { setHovered(id); onPreview(id); }
  function zoom(factor: number, client?: { x: number; y: number }) {
    setCamera((prior) => {
      const scale = Math.max(.55, Math.min(3.5, prior.scale * factor));
      if (!client || !canvasRef.current) return { ...prior, scale };
      const box = canvasRef.current.getBoundingClientRect(), { width, height } = sizeRef.current, fit = Math.min(width / WORLD.width, height / WORLD.height) * .94;
      const sx = client.x - box.left, sy = client.y - box.top;
      const wx = WORLD.width / 2 + (sx - width / 2 - prior.x) / (fit * prior.scale), wy = WORLD.height / 2 + (sy - height / 2 - prior.y) / (fit * prior.scale);
      return { scale, x: sx - width / 2 - (wx - WORLD.width / 2) * fit * scale, y: sy - height / 2 - (wy - WORLD.height / 2) * fit * scale };
    });
  }

  function recenterMini(event: { currentTarget: HTMLCanvasElement; clientX: number; clientY: number }) {
    const box = event.currentTarget.getBoundingClientRect();
    const wx = (event.clientX - box.left) / box.width * WORLD.width;
    let wy = (event.clientY - box.top) / box.height * WORLD.height;
    if (lens === "core") {
      const maxLines = Math.max(...dataset.files.map((file) => file.lines), 1), mapHeight = 1380, plotTop = 140, plotHeight = 1100;
      const source = dataset.files.flatMap((file) => file.symbols.map((symbol) => ({ file, symbol }))).find(({ symbol }) => symbol.id === selectedId);
      const padding = source ? Math.max(34, Math.round((source.symbol.end - source.symbol.start) * .32)) : 40;
      const rangeStart = coreView === "range" && source ? Math.max(0, source.symbol.start - padding) : 0;
      const rangeEnd = coreView === "range" && source ? Math.min(maxLines, source.symbol.end + padding) : maxLines;
      const clickedWorld = (event.clientY - box.top) / box.height * mapHeight;
      const line = Math.max(0, Math.min(maxLines, (clickedWorld - plotTop) / plotHeight * maxLines));
      wy = plotTop + (line - rangeStart) / Math.max(1, rangeEnd - rangeStart) * plotHeight;
    }
    const { width, height } = sizeRef.current, fit = Math.min(width / WORLD.width, height / WORLD.height) * .94;
    setCamera((prior) => ({ ...prior, x: -(wx - WORLD.width / 2) * fit * prior.scale, y: -(wy - WORLD.height / 2) * fit * prior.scale }));
  }

  return <div ref={hostRef} className="cd-semantic-field">
    <canvas ref={canvasRef} className="cd-semantic-canvas" tabIndex={0} title={lens === "core" ? dataset.files.map((file) => file.path).join("\n") : undefined}
      aria-label={`${lens.toUpperCase()} semantic field. Drag to pan; wheel or plus and minus zoom; Enter drills; Escape returns.`}
      onWheel={(event) => { event.preventDefault(); zoom(event.deltaY < 0 ? 1.13 : 1 / 1.13, { x: event.clientX, y: event.clientY }); }}
      onPointerDown={(event) => { if (event.button !== 0) return; event.currentTarget.setPointerCapture(event.pointerId); drag.current = { x: event.clientX, y: event.clientY, camera, moved: false }; }}
      onPointerMove={(event) => { const active = drag.current; if (active) { const dx = event.clientX - active.x, dy = event.clientY - active.y; active.moved ||= Math.hypot(dx, dy) > 3; if (active.moved) setCamera({ ...active.camera, x: active.camera.x + dx, y: active.camera.y + dy }); } else changeHover(pick(event)?.id ?? null); }}
      onPointerUp={(event) => { const active = drag.current; drag.current = null; if (active && !active.moved) { const hit = pick(event); if (hit) onSelect(hit.id); } }}
      onPointerLeave={() => { if (!drag.current) changeHover(null); }} onPointerCancel={() => { drag.current = null; }}
      onDoubleClick={(event) => { const hit = pick(event); if (hit) { onSelect(hit.id); if (lens === "core") onCoreView?.("range"); else onDrill(); } }}
      onKeyDown={(event) => {
        if (event.key === "Enter") { if (lens === "core") onCoreView?.("range"); else onDrill(); event.preventDefault(); }
        else if (event.key === "Escape" || event.key === "Backspace") { if (lens === "core" && coreView === "range") onCoreView?.("overview"); else onBack(); event.preventDefault(); }
        else if (event.key === "+" || event.key === "=") { zoom(1.2); event.preventDefault(); }
        else if (event.key === "-") { zoom(1 / 1.2); event.preventDefault(); }
        else if (event.key === "Home") { setCamera({ x: 0, y: lens === "core" ? -110 : 0, scale: 1 }); event.preventDefault(); }
        else if (["ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown"].includes(event.key)) {
          const ordered = lens === "core" ? dataset.files.flatMap((file: CoreFile) => file.symbols.map((symbol) => symbol.id)) : dataset.nodes.map((node) => node.id);
          const current = Math.max(0, ordered.indexOf(selectedId)); const delta = event.key === "ArrowLeft" || event.key === "ArrowUp" ? -1 : 1;
          onSelect(ordered[(current + delta + ordered.length) % ordered.length] ?? selectedId); event.preventDefault();
        }
      }} />
    <canvas ref={miniRef} className="cd-semantic-minimap" aria-label={`${lens.toUpperCase()} minimap`} onClick={recenterMini} />
    <div className="cd-field-controls" aria-label="Semantic camera controls">
      <button type="button" onClick={() => { if (lens === "core" && coreView === "range") onCoreView?.("overview"); else onBack(); }} disabled={lens === "cortex"}>BACK</button>
      {lens === "core" ? <><button type="button" className={coreView === "overview" ? "is-on" : ""} aria-pressed={coreView === "overview"} onClick={() => onCoreView?.("overview")}>OVERVIEW</button><button type="button" className={coreView === "range" ? "is-on" : ""} aria-pressed={coreView === "range"} onClick={() => onCoreView?.("range")}>SOURCE RANGE</button></> : null}
      <span>{Math.round(camera.scale * 100)}%</span>
      <button type="button" onClick={() => zoom(1 / 1.2)} aria-label="Zoom semantic field out">−</button>
      <button type="button" onClick={() => zoom(1.2)} aria-label="Zoom semantic field in">+</button>
      <button type="button" onClick={() => setCamera({ x: 0, y: lens === "core" ? -110 : 0, scale: 1 })}>FIT</button>
    </div>
  </div>;
}
