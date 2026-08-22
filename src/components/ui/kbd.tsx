import { type ModKey, formatBinding } from "@/lib/keymap";
import { cn } from "@/lib/utils";

/**
 * A key hint. Not a chiclet with a 3D border — just the glyph, set in the
 * mono face on a surface step, so a row of them reads as a legend rather than
 * a keyboard.
 */
export function Kbd({
  keys,
  className,
  mod,
}: {
  keys: string;
  className?: string;
  /**
   * Which key `mod` stands for. Left out it is the platform's, which is the
   * right answer everywhere except a test: `detectModKey` reads `navigator`,
   * so an assertion about `⌘` passes on a Mac and fails on Linux, where CI
   * runs. Pinning it is the same escape hatch `connectMenu` takes for the
   * same reason — see the note in `menu.test.ts`.
   */
  mod?: ModKey;
}) {
  return (
    <kbd
      className={cn(
        "inline-flex h-[1.125rem] min-w-[1.125rem] items-center justify-center rounded-[3px]",
        "border border-border bg-surface-raised px-1",
        "font-mono text-micro leading-none text-muted-foreground",
        className,
      )}
    >
      {formatBinding(keys, mod)}
    </kbd>
  );
}

/** "j / k  move" — the status bar's unit of vocabulary. */
export function Hint({ keys, label }: { keys: string[]; label: string }) {
  return (
    <span className="inline-flex items-center gap-1 whitespace-nowrap">
      {keys.map((k) => (
        <Kbd key={k} keys={k} />
      ))}
      <span className="text-micro text-faint-foreground">{label}</span>
    </span>
  );
}
