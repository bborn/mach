import { useEffect, useState } from "react";

/**
 * Whether the dark palette is live.
 *
 * The calendar's eight hues are generated rather than declared as tokens (there
 * is no eight-hue ramp in `globals.css` to borrow), so the fill colours are
 * built in JS and have to know which theme they are being built for. The theme
 * itself is owned elsewhere: `useMach` toggles `.dark` on the root element, and
 * this just watches that class. One observer, no polling.
 */
export function useIsDark(): boolean {
  const [dark, setDark] = useState(
    () => typeof document !== "undefined" && document.documentElement.classList.contains("dark"),
  );

  useEffect(() => {
    const root = document.documentElement;
    const read = () => setDark(root.classList.contains("dark"));
    read();
    const observer = new MutationObserver(read);
    observer.observe(root, { attributes: true, attributeFilter: ["class"] });
    return () => observer.disconnect();
  }, []);

  return dark;
}
