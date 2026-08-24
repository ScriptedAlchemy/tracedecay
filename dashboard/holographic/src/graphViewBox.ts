import type { PointerEvent, TouchEvent } from "react";
import type { RefObject } from "react";
import type { ViewBox } from "./associationGraphTypes";

export type ViewAnimRef = { current: number | null };

export function zoomViewBoxAtPoint(
  view: ViewBox,
  px: number,
  py: number,
  factor: number,
): ViewBox {
  const nw = view.w * factor;
  const nh = view.h * factor;
  return {
    x: view.x + (view.w - nw) * px,
    y: view.y + (view.h - nh) * py,
    w: nw,
    h: nh,
  };
}

export function panViewBox(view: ViewBox, dx: number, dy: number): ViewBox {
  return { ...view, x: view.x - dx, y: view.y - dy };
}

/** Whether a wheel event should zoom the graph rather than scroll the page. */
function wheelShouldZoom(event: WheelEvent, svg: SVGSVGElement): boolean {
  // Ctrl/Cmd (also set by trackpad pinch) or an already-focused graph. This
  // stops the graph from hijacking ordinary page scroll when the pointer just
  // passes over it.
  if (event.ctrlKey || event.metaKey) return true;
  const active = document.activeElement;
  return !!active && (active === svg || svg.contains(active));
}

export function handleWheelZoom(
  event: WheelEvent,
  svg: SVGSVGElement,
  setView: (updater: (v: ViewBox) => ViewBox) => void,
  onInteract?: () => void,
) {
  if (!wheelShouldZoom(event, svg)) return; // let the page scroll
  event.preventDefault();
  const rect = svg.getBoundingClientRect();
  if (rect.width === 0 || rect.height === 0) return;
  onInteract?.();
  const px = (event.clientX - rect.left) / rect.width;
  const py = (event.clientY - rect.top) / rect.height;
  const factor = event.deltaY > 0 ? 1.12 : 0.89;
  setView((v) => zoomViewBoxAtPoint(v, px, py, factor));
}

export function pointerPanDelta(
  clientX: number,
  clientY: number,
  prevX: number,
  prevY: number,
  viewW: number,
  viewH: number,
  rectWidth: number,
  rectHeight: number,
): { dx: number; dy: number; x: number; y: number } {
  const dx = ((clientX - prevX) * viewW) / rectWidth;
  const dy = ((clientY - prevY) * viewH) / rectHeight;
  return { dx, dy, x: clientX, y: clientY };
}

export function createPointerHandlers(
  dragRef: RefObject<{ x: number; y: number } | null>,
  view: ViewBox,
  setView: (updater: (v: ViewBox) => ViewBox) => void,
  onInteract?: () => void,
) {
  const onPointerDown = (event: PointerEvent<SVGSVGElement>) => {
    dragRef.current = { x: event.clientX, y: event.clientY };
    onInteract?.();
    event.currentTarget.setPointerCapture(event.pointerId);
  };

  const onPointerMove = (event: PointerEvent<SVGSVGElement>) => {
    const drag = dragRef.current;
    if (!drag) return;
    const rect = event.currentTarget.getBoundingClientRect();
    if (rect.width === 0 || rect.height === 0) return;
    const { dx, dy, x, y } = pointerPanDelta(
      event.clientX,
      event.clientY,
      drag.x,
      drag.y,
      view.w,
      view.h,
      rect.width,
      rect.height,
    );
    dragRef.current = { x, y };
    setView((v) => panViewBox(v, dx, dy));
  };

  const onPointerUp = (event: PointerEvent<SVGSVGElement>) => {
    dragRef.current = null;
    if (event.currentTarget.hasPointerCapture(event.pointerId)) {
      event.currentTarget.releasePointerCapture(event.pointerId);
    }
  };

  return { onPointerDown, onPointerMove, onPointerUp };
}

export function createTouchHandlers(
  svgRef: RefObject<SVGSVGElement | null>,
  dragRef: RefObject<{ x: number; y: number } | null>,
  pinchRef: RefObject<{
    id1: number;
    id2: number;
    x1: number;
    y1: number;
    x2: number;
    y2: number;
    view: ViewBox;
  } | null>,
  view: ViewBox,
  setView: (v: ViewBox) => void,
  onInteract?: () => void,
) {
  const onTouchStart = (event: TouchEvent<SVGSVGElement>) => {
    onInteract?.();
    if (event.touches.length === 2) {
      const t1 = event.touches[0];
      const t2 = event.touches[1];
      pinchRef.current = {
        id1: t1.identifier,
        id2: t2.identifier,
        x1: t1.clientX,
        y1: t1.clientY,
        x2: t2.clientX,
        y2: t2.clientY,
        view: { ...view },
      };
    } else if (event.touches.length === 1) {
      const t = event.touches[0];
      dragRef.current = { x: t.clientX, y: t.clientY };
    }
  };

  const onTouchMove = (event: TouchEvent<SVGSVGElement>) => {
    const svg = svgRef.current;
    if (!svg) return;
    const rect = svg.getBoundingClientRect();
    if (rect.width === 0 || rect.height === 0) return;

    if (event.touches.length === 2 && pinchRef.current) {
      event.preventDefault();
      const t1 = event.touches[0];
      const t2 = event.touches[1];
      const p = pinchRef.current;

      const prevDist = Math.hypot(p.x2 - p.x1, p.y2 - p.y1);
      const currDist = Math.hypot(t2.clientX - t1.clientX, t2.clientY - t1.clientY);
      if (prevDist < 1) return;

      const factor = currDist / prevDist;
      const midX = (t1.clientX + t2.clientX) / 2;
      const midY = (t1.clientY + t2.clientY) / 2;
      const px = (midX - rect.left) / rect.width;
      const py = (midY - rect.top) / rect.height;

      const nw = p.view.w / factor;
      const nh = p.view.h / factor;
      setView({
        x: p.view.x + (p.view.w - nw) * px,
        y: p.view.y + (p.view.h - nh) * py,
        w: nw,
        h: nh,
      });
    } else if (event.touches.length === 1 && dragRef.current) {
      event.preventDefault();
      const t = event.touches[0];
      const { dx, dy, x, y } = pointerPanDelta(
        t.clientX,
        t.clientY,
        dragRef.current.x,
        dragRef.current.y,
        view.w,
        view.h,
        rect.width,
        rect.height,
      );
      dragRef.current = { x, y };
      setView(panViewBox(view, dx, dy));
    }
  };

  const onTouchEnd = () => {
    pinchRef.current = null;
    dragRef.current = null;
  };

  return { onTouchStart, onTouchMove, onTouchEnd };
}

export function cancelViewAnimation(animRef: ViewAnimRef): void {
  if (animRef.current !== null) {
    cancelAnimationFrame(animRef.current);
    animRef.current = null;
  }
}

const easeOutCubic = (t: number): number => 1 - Math.pow(1 - t, 3);

/** Smoothly tween the view box (zoom-to-fit / zoom-to-node) over `durationMs`. */
export function animateViewBox(
  from: ViewBox,
  to: ViewBox,
  durationMs: number,
  setView: (v: ViewBox) => void,
  animRef: ViewAnimRef,
): void {
  cancelViewAnimation(animRef);
  const dist =
    Math.abs(from.x - to.x) +
    Math.abs(from.y - to.y) +
    Math.abs(from.w - to.w) +
    Math.abs(from.h - to.h);
  // Skip imperceptible moves so we don't fight the settle loop.
  if (dist < 0.5 || durationMs <= 0) {
    setView(to);
    return;
  }
  const start = performance.now();
  const step = (now: number) => {
    const t = Math.min(1, (now - start) / durationMs);
    const k = easeOutCubic(t);
    setView({
      x: from.x + (to.x - from.x) * k,
      y: from.y + (to.y - from.y) * k,
      w: from.w + (to.w - from.w) * k,
      h: from.h + (to.h - from.h) * k,
    });
    animRef.current = t < 1 ? requestAnimationFrame(step) : null;
  };
  animRef.current = requestAnimationFrame(step);
}
