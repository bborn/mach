import { useEffect, useState } from "react";
import { Kbd } from "@/components/ui/kbd";
import { cn } from "@/lib/utils";
import { COMPOSER_KEYS } from "@/lib/compose";
import { MAX_KEYED_STANCES, SUGGESTION_KEYS, type Stance } from "@/lib/suggestions";

/**
 * The stance row: two or three buttons, each with a whole reply behind it,
 * and after them the three plain acts the reply strip has always offered.
 *
 * It stands in for the reply/reply-all/forward strip when a conversation has
 * suggestions, in the same place and at the same size — one subtle line at the
 * bottom of the reading pane. Not a panel, not a card, not a thing that expands.
 * A row that announced itself would be the notification this feature does not
 * send. And one row rather than two: stacking a strip under a strip is a fault
 * this file has already been through.
 *
 * # Everything here is already local
 *
 * The bodies arrived with the labels. Pressing one is a state change and a
 * `prepareDraft` — the same local call `r` makes — so there is nothing to wait
 * for and nothing to spin. `pressed` exists for the one frame between the
 * keystroke and the composer mounting: he must never act and see nothing happen,
 * and on a busy store the writer lock can make even a local prepare take a beat.
 *
 * # Why the plain acts are still drawn
 *
 * `r`, `a` and `f` keep working whatever is on this row, and an earlier version
 * of it stopped there — the suggestions took the strip's place, and reply-all
 * and forward kept only their bindings. That was a real loss: the strip exists
 * because those three acts were asked for by name, and a thread with
 * suggestions is not a thread you never want to forward. So they follow the
 * stances, in the strip's own faint register, separated from them because they
 * are a different kind of thing: the model's opinions, and then the acts that
 * owe nothing to it.
 *
 * "Write it myself" is `reply` under another name. It is worded that way
 * because a row that offered only the model's three opinions would read as
 * though those were the options.
 */
export function StanceRow({
  stances,
  onPick,
  onWriteMyself,
  onReplyAll,
  onForward,
}: {
  stances: Stance[];
  onPick: (index: number, stance: Stance) => void;
  onWriteMyself: () => void;
  onReplyAll: () => void;
  onForward: () => void;
}) {
  /**
   * Which button was just pressed, for the frame before the composer arrives.
   *
   * Keyed by index and cleared on a change of stances, so a press does not
   * survive into the next conversation as a button that looks half-open.
   */
  const [pressed, setPressed] = useState<number | null>(null);
  useEffect(() => setPressed(null), [stances]);

  const keyed = stances.slice(0, MAX_KEYED_STANCES);

  return (
    <div className="shrink-0 border-t border-border">
      {/*
        One row, always. The stances shrink and truncate before the row wraps —
        a second line would make this a panel, and the whole claim of the design
        is that it is the same one line the reply strip already occupied. A
        truncated label still has its whole reply behind it, and the `title`
        carries the text for a pointer that lingers.
      */}
      <div className="mx-auto flex max-w-[72ch] items-center gap-2 px-5 py-2">
        {keyed.map((stance, index) => (
          <button
            key={`${index}-${stance.label}`}
            type="button"
            data-stance={index}
            title={stance.body}
            onClick={() => {
              setPressed(index);
              onPick(index, stance);
            }}
            className={cn(
              "inline-flex min-w-0 items-center gap-1.5 rounded-[var(--radius)]",
              "border border-border px-2 py-0.5 text-micro",
              pressed === index
                ? "bg-hover text-foreground"
                : "text-muted-foreground hover:bg-hover hover:text-foreground",
            )}
          >
            <Kbd
              keys={SUGGESTION_KEYS.stance(index)}
              className="shrink-0 border-none bg-transparent px-0"
            />
            <span className="min-w-0 truncate">{stance.label}</span>
          </button>
        ))}
        {/*
          The stances end here. What follows is the reply strip, unchanged in
          register and in what it does — the hairline is the whole distinction
          between the two halves, and it is enough of one.
        */}
        <span aria-hidden className="h-3.5 w-px shrink-0 bg-border" />
        <button
          type="button"
          data-stance="mine"
          onClick={() => {
            setPressed(-1);
            onWriteMyself();
          }}
          className={cn(
            "inline-flex shrink-0 items-center gap-1.5 text-micro",
            pressed === -1 ? "text-foreground" : "text-faint-foreground hover:text-foreground",
          )}
        >
          <Kbd keys={SUGGESTION_KEYS.mine} className="border-none bg-transparent px-0" />
          <span>write it myself</span>
        </button>
        <button
          type="button"
          data-stance="all"
          onClick={onReplyAll}
          className="inline-flex shrink-0 items-center gap-1.5 text-micro text-faint-foreground hover:text-foreground"
        >
          <Kbd keys={COMPOSER_KEYS.replyAll} className="border-none bg-transparent px-0" />
          <span>reply all</span>
        </button>
        <button
          type="button"
          data-stance="forward"
          onClick={onForward}
          className="inline-flex shrink-0 items-center gap-1.5 text-micro text-faint-foreground hover:text-foreground"
        >
          <Kbd keys={COMPOSER_KEYS.forward} className="border-none bg-transparent px-0" />
          <span>forward</span>
        </button>
      </div>
    </div>
  );
}
