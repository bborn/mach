import { useEffect, useState } from "react";

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
