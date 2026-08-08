import { Kbd } from "./kbd";
import { cn } from "@/lib/utils";

/**
 * The grey continuation, drawn over the field.
 *
 * There is no way to put text *inside* a `<textarea>` that the textarea does
 * not own, so this is the mirror trick: a div holding the same characters in
 * the same typography, laid over the field, with the part you actually typed
 * rendered transparent and the suggestion after it in a fainter colour. Over
 * rather than under, deliberately — every field in `components/ui` has an
 * opaque background and a border, and a mirror underneath one would be
 * invisible. On top, the field keeps its own chrome and this only ever adds
 * grey text.
 *
 * The alignment comes from the caller passing the same classes to both: the
 * typography of the field, and `frame` for whatever padding and border sit
 * between the field's edge and its text. One string each, shared, so they
 * cannot drift into a one-pixel offset nobody can find.
 *
 * Two things keep it honest:
 *
 *  * **It only ever renders when the caret is at the end.** `useGhostText`
 *    enforces that, and it is what lets the mirror be a plain concatenation
 *    instead of a caret-aware layout.
 *  * **It is `aria-hidden` and untouchable.** A screen reader reading the field
 *    must not hear a sentence nobody wrote; the announcement belongs to the
 *    hint beside the field, not to the text.
 */
export function GhostText({
  value,
  suggestion,
  typography,
  frame,
  multiline = true,
  scrollTop = 0,
}: {
  value: string;
  suggestion: string;
  /** The exact type classes the real field uses. Both must agree, or it drifts. */
  typography: string;
  /** Padding and border classes the field's text sits inside, if any. */
  frame?: string;
  multiline?: boolean;
  /** The field's scroll offset, so a long message keeps the suggestion in place. */
  scrollTop?: number;
}) {
  if (!suggestion) return null;

  return (
    <div
      aria-hidden
      className={cn(
        "pointer-events-none absolute inset-0 z-10 select-none overflow-hidden",
        multiline ? "whitespace-pre-wrap break-words" : "whitespace-pre",
        typography,
        frame,
      )}
    >
      <div style={scrollTop ? { transform: `translateY(${-scrollTop}px)` } : undefined}>
        {/* Transparent, not omitted: the suggestion has to start exactly where
            the typed text stops, and only the text itself can measure that. */}
        <span className="text-transparent">{value}</span>
        <span className="text-faint-foreground">{suggestion}</span>
      </div>
    </div>
  );
}

/** The one-line legend that says a suggestion is on screen and how to take it. */
export function GhostHint({ shown }: { shown: boolean }) {
  if (!shown) return null;
  return (
    <span className="inline-flex items-center gap-1 text-micro text-faint-foreground">
      <Kbd keys="tab" /> accept
    </span>
  );
}
