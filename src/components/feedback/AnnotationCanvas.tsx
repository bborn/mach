import { useEffect, useState, type PointerEvent, type RefObject } from "react";
import {
  strokeWidthFor,
  visibleShapes,
  type AnnotationAction,
  type AnnotationState,
  type AnnotationTool,
  type Point,
  type Shape,
} from "@/lib/feedback";

interface AnnotationCanvasProps {
  /** The capture, as a `data:image/png;base64,…` URL. */
  src: string;
  tool: AnnotationTool;
  state: AnnotationState;
  dispatch: (action: AnnotationAction) => void;
  /** The parent exports the flattened PNG straight off this canvas. */
  canvasRef: RefObject<HTMLCanvasElement | null>;
}

/**
 * One canvas, drawn at the capture's native resolution and scaled down by CSS.
 *
 * Compositing the image and the ink into a single canvas rather than layering a
 * transparent canvas over an `<img>` is what makes the export a one-liner:
 * `canvas.toDataURL()` already *is* the flattened result, at full resolution,
 * with no second draw pass that could disagree with what was on screen.
 */
export function AnnotationCanvas({ src, tool, state, dispatch, canvasRef }: AnnotationCanvasProps) {
  const [image, setImage] = useState<HTMLImageElement | null>(null);

  useEffect(() => {
    let cancelled = false;
    const img = new Image();
    img.onload = () => {
      if (!cancelled) setImage(img);
    };
    img.src = src;
    return () => {
      cancelled = true;
    };
  }, [src]);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas || !image) return;
    if (canvas.width !== image.naturalWidth) canvas.width = image.naturalWidth;
    if (canvas.height !== image.naturalHeight) canvas.height = image.naturalHeight;

    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    ctx.clearRect(0, 0, canvas.width, canvas.height);
    ctx.drawImage(image, 0, 0);

    const width = strokeWidthFor(canvas.width);
    for (const shape of visibleShapes(state)) draw(ctx, shape, width);
  }, [image, state, canvasRef]);

  const point = (event: PointerEvent<HTMLCanvasElement>): Point => {
    const canvas = event.currentTarget;
    const box = canvas.getBoundingClientRect();
    // The canvas is displayed smaller than it is; everything is stored in
    // image pixels so the export needs no second mapping.
    return {
      x: ((event.clientX - box.left) / box.width) * canvas.width,
      y: ((event.clientY - box.top) / box.height) * canvas.height,
    };
  };

  return (
    <canvas
      ref={canvasRef}
      aria-label="Annotate the screenshot"
      className="block w-full cursor-crosshair touch-none select-none rounded-[var(--radius)] border border-border"
      onPointerDown={(event) => {
        event.currentTarget.setPointerCapture(event.pointerId);
        dispatch({ type: "start", tool, point: point(event) });
      }}
      onPointerMove={(event) => {
        if (!state.draft) return;
        dispatch({ type: "move", point: point(event) });
      }}
      onPointerUp={(event) => {
        event.currentTarget.releasePointerCapture(event.pointerId);
        dispatch({ type: "commit" });
      }}
      onPointerCancel={() => dispatch({ type: "commit" })}
    />
  );
}

/* -------------------------------------------------------------------------- */
/* Drawing                                                                     */
/* -------------------------------------------------------------------------- */

const INK = "#ff3b30";
const HALO = "rgba(255,255,255,0.7)";

/**
 * Every mark is drawn twice: a white halo, then the red on top. A screenshot of
 * this app can be nearly black or nearly white, and a single-colour annotation
 * disappears into one of them.
 */
function draw(ctx: CanvasRenderingContext2D, shape: Shape, width: number) {
  ctx.lineCap = "round";
  ctx.lineJoin = "round";
  for (const pass of [0, 1]) {
    ctx.strokeStyle = pass === 0 ? HALO : INK;
    ctx.lineWidth = pass === 0 ? width + Math.max(2, width * 0.8) : width;
    path(ctx, shape, width);
  }
}

function path(ctx: CanvasRenderingContext2D, shape: Shape, width: number) {
  const first = shape.points[0];
  const last = shape.points[shape.points.length - 1];
  if (!first || !last) return;

  if (shape.tool === "pen") {
    ctx.beginPath();
    ctx.moveTo(first.x, first.y);
    for (const p of shape.points.slice(1)) ctx.lineTo(p.x, p.y);
    ctx.stroke();
    return;
  }

  if (shape.tool === "rect") {
    ctx.beginPath();
    ctx.rect(
      Math.min(first.x, last.x),
      Math.min(first.y, last.y),
      Math.abs(last.x - first.x),
      Math.abs(last.y - first.y),
    );
    ctx.stroke();
    return;
  }

  // Arrow: the shaft, then two barbs at a fixed angle off it.
  const angle = Math.atan2(last.y - first.y, last.x - first.x);
  const head = Math.max(width * 4, 14);
  const spread = Math.PI / 7;
  ctx.beginPath();
  ctx.moveTo(first.x, first.y);
  ctx.lineTo(last.x, last.y);
  ctx.moveTo(last.x, last.y);
  ctx.lineTo(last.x - head * Math.cos(angle - spread), last.y - head * Math.sin(angle - spread));
  ctx.moveTo(last.x, last.y);
  ctx.lineTo(last.x - head * Math.cos(angle + spread), last.y - head * Math.sin(angle + spread));
  ctx.stroke();
}
