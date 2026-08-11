import { useEffect, useLayoutEffect, useState } from "react";

/**
 * How tall the window is, as state, so a resize re-clamps whatever was dragged.
 *
 * Without it a drawer dragged tall on an external display would still be tall
 * on the laptop the window was moved to, with the mail list behind it reduced
 * to a strip — and the same is true of a composer dragged to half the reading
 * pane. Both read this; neither owns it.
 */
export function useViewportHeight(): number {
  const [height, setHeight] = useState(() =>
    typeof window === "undefined" ? 0 : window.innerHeight,
  );
  useEffect(() => {
    const measure = () => setHeight(window.innerHeight);
    measure();
    window.addEventListener("resize", measure);
    return () => window.removeEventListener("resize", measure);
  }, []);
  return height;
}

/**
 * How tall one box on screen is, as state.
 *
 * The window is the wrong question for anything sharing a column with something
 * else that can grow: `window.innerHeight` does not go down when a drawer opens
 * at the bottom of it, so a box that sizes itself from the window overflows the
 * column it is in by however much the drawer took. That is the defect
 * {@link clampComposerHeight} documents.
 *
 * A `ResizeObserver` rather than a resize listener, because the column changes
 * height for reasons the window knows nothing about — the agent drawer opening,
 * being dragged, closing again. Zero only while there is nothing carrying the
 * attribute, which is what a consumer has to have an answer for; see
 * {@link readingColumnHeight} for the one here.
 */
export function useElementHeight(attribute: string): number {
  const [height, setHeight] = useState(0);
  // Layout, not effect: the first measurement has to be taken before the
  // browser paints, or the box reading it renders once at "not measured yet"
  // and the caller has to have an answer for a number it will never see again.
  useLayoutEffect(() => {
    const node = document.querySelector(`[${attribute}]`);
    if (!node) return;
    setHeight(Math.round(node.getBoundingClientRect().height));
    const observer = new ResizeObserver(([entry]) => {
      setHeight(Math.round(entry.contentRect.height));
    });
    observer.observe(node);
    return () => observer.disconnect();
  }, [attribute]);
  return height;
}
