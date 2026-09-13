import type { NodeHoverDrawingFunction } from 'sigma/rendering';
import { rgb, rgba, type ThemeBox } from './palette.ts';

/**
 * The hover pass, drawn in the field's own palette.
 *
 * Sigma's default (`drawDiscNodeHover`) paints an opaque white shadowed disc
 * fused with a white label backdrop over the hovered body — on the dark
 * instrument field that read as the node "going white" and growing a blob,
 * and on the light field it erased the body against the paper. The reducers
 * already carry the hover response (the body recolours to the hot accent and
 * its neighbourhood isolates), so this 2d layer only adds a thin accent ring
 * and a substrate-backed label that stay in the theme.
 *
 * Reads the {@link ThemeBox} per draw rather than capturing its colors, so a
 * theme flip re-lights hovers without rebuilding the renderer — the same
 * contract every other drawing pass on this canvas holds.
 */
export function createNodeHoverDrawer(theme: ThemeBox): NodeHoverDrawingFunction {
  return (context, data, settings) => {
    const colors = theme.colors;
    context.beginPath();
    context.arc(data.x, data.y, data.size + 3, 0, Math.PI * 2);
    context.strokeStyle = rgba(colors.hot, 0.9);
    context.lineWidth = 1.5;
    context.stroke();
    if (!data.label) return;
    const size = settings.labelSize;
    context.font = `${settings.labelWeight} ${size}px ${settings.labelFont}`;
    const width = context.measureText(data.label).width;
    const x = data.x + data.size + 6;
    const y = data.y + size / 3;
    context.fillStyle = rgba(colors.substrate, 0.85);
    context.fillRect(x - 3, y - size, width + 6, size + 5);
    context.fillStyle = rgb(colors.label);
    context.fillText(data.label, x, y);
  };
}
