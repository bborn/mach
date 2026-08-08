import { clsx, type ClassValue } from "clsx";
import { extendTailwindMerge } from "tailwind-merge";

/**
 * `cn`, taught about this app's type scale.
 *
 * `tailwind-merge` decides which of two conflicting utilities wins by putting
 * every class in a group, and it only knows the groups Tailwind ships with. The
 * scale in `globals.css` is ours — `text-micro`, `text-list`, `text-body`,
 * `text-reading` — so out of the box the merger filed them under *text colour*,
 * the only group whose prefix matches. That made `cn("text-micro
 * text-faint-foreground")` silently drop the size and render an 11px label at
 * 14px: no error, no warning, just a form whose labels are the same weight as
 * its values.
 *
 * Declaring the scale as a font-size group is the whole fix. Anything written
 * as a literal `className` was never affected, which is why the bug hid for so
 * long — it only bit the components that compose their classes.
 */
const twMerge = extendTailwindMerge({
  extend: {
    classGroups: {
      "font-size": [{ text: ["micro", "list", "body", "reading"] }],
    },
  },
});

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}
