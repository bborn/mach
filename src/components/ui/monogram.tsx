import { initialsOf, monogramColor } from "@/lib/monogram";
import { cn } from "@/lib/utils";

/**
 * The sender, as a coloured tile with their initials in it.
 *
 * # Tinted, not filled
 *
 * The obvious drawing is what every webmail does: a saturated circle with white
 * letters. Forty of those down a list is a bag of sweets, and this app is flat,
 * dense and spends colour on one thing at a time. So the tile is the hue mixed
 * down into the page — the same move `tintedSurface` makes for a calendar block
 * — with the hue itself carrying the letters. It reads as one system at a
 * glance and still tells two senders apart.
 *
 * # Why `color-mix` rather than a colour computed in JS
 *
 * Both ends of the mix are theme tokens, so the same two declarations are
 * legible in light and in dark with nothing to keep in sync. `calendar-palette`
 * does this arithmetic in TypeScript because a calendar block has to hit a
 * contrast ratio against a *solid* fill and needs to know which theme it is in;
 * a tint of the page against ink drawn from the page cannot go wrong that way.
 *
 * # A square
 *
 * Rounded, not round. A circle is the avatar of a social product; the rest of
 * this interface is rectangles at a 6px radius, and a row of circles in it
 * reads as something that arrived from somewhere else.
 */
export function Monogram({
  name,
  email,
  size = 26,
  className,
}: {
  name?: string;
  email?: string;
  /** Edge length in px. 26 in the thread list, 20 beside a message header. */
  size?: number;
  className?: string;
}) {
  const hue = monogramColor(email);
  const initials = initialsOf(name, email);

  return (
    <span
      aria-hidden
      className={cn(
        "flex shrink-0 select-none items-center justify-center font-medium leading-none",
        className,
      )}
      style={{
        width: size,
        height: size,
        borderRadius: Math.round(size * 0.3),
        // Two letters have to fit across a tile that is mostly padding, so the
        // type is sized off the tile rather than off the type scale.
        fontSize: Math.round(size * (initials.length > 1 ? 0.37 : 0.44)),
        letterSpacing: "0.01em",
        background: `color-mix(in oklab, ${hue} var(--monogram-tint), var(--background))`,
        color: `color-mix(in oklab, ${hue} var(--monogram-ink), var(--foreground))`,
      }}
    >
      {initials}
    </span>
  );
}
