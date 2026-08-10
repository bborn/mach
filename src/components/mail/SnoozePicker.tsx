import { Clock, CornerDownLeft } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { useKeyBindings } from "@/hooks/useKeymap";
import { useMach } from "@/hooks/useMach";
import {
  moveSnoozeCursor,
  parseSnoozeAt,
  snoozeOptions,
  snoozeWhen,
  type SnoozeOption,
} from "@/lib/snooze";
import { cn } from "@/lib/utils";
import { Overlay } from "@/components/ui/dialog";
import { BareInput } from "@/components/ui/input";
import { Kbd } from "@/components/ui/kbd";
import { snoozePickerBindings } from "./mail-bindings";

/**
 * The picker `b` opens.
 *
 * Built on the same three parts as the command palette, because it is the same
 * interaction and the user should not have to notice they are in a different
 * surface: an `Overlay` that claims the keyboard, a list with a cursor, and a
 * footer that names the keys. The palette is the reference.
 *
 * Two things it does that the palette does not:
 *
 *  * **Every row shows the instant it resolves to.** "Next week" is not an
 *    answer to "when does this come back"; "Mon, 8:00 AM" is. Without it the
 *    user is doing date arithmetic in their head against a list of adverbs,
 *    which is how you snooze something into next month by accident.
 *  * **The last row opens a text field** rather than running a command. It
 *    parses with `chrono-node` — the same parser, and the same conventions, as
 *    natural-language event entry — and shows its answer under the field as you
 *    type, so Enter confirms something already on screen.
 *
 * `now` is captured once, when the picker opens, and every instant on screen is
 * computed from it. Reading the clock per render would let "Later today" drift
 * an hour under a picker somebody left open, and then commit the drifted value.
 */
export function SnoozePicker() {
  const { ui, actions } = useMach();
  const open = ui.snoozeOpen;

  const [now, setNow] = useState(() => Date.now());
  const [cursor, setCursor] = useState(0);
  const [custom, setCustom] = useState(false);
  const [text, setText] = useState("");
  const field = useRef<HTMLInputElement>(null);

  const options = useMemo(() => snoozeOptions(now), [now]);
  const typed = useMemo(() => (custom ? parseSnoozeAt(text, now) : null), [custom, text, now]);

  /* A fresh open is a fresh clock and a fresh list. */
  useEffect(() => {
    if (!open) return;
    setNow(Date.now());
    setCursor(0);
    setCustom(false);
    setText("");
  }, [open]);

  useEffect(() => {
    if (custom) field.current?.focus();
  }, [custom]);

  const commitAt = (at: number) => {
    actions.snoozeSelected(at);
    actions.setSnooze(false);
  };

  /** Take an option: either it resolves, or it is the one you have to type. */
  const take = (option: SnoozeOption | undefined) => {
    if (!option) return;
    if (option.at === null) setCustom(true);
    else commitAt(option.at);
  };

  const stage = () => (!open ? "closed" : custom ? "custom" : "list");

  useKeyBindings(
    snoozePickerBindings(stage, () => options.length, {
      move: (delta) => setCursor((c) => moveSnoozeCursor(c, delta, options.length)),
      pick: (index) => {
        setCursor(index);
        take(options[index]);
      },
      commit: () => {
        if (!custom) return take(options[cursor]);
        // A field that has not resolved yet must not commit anything. The line
        // under it already says why, so this stays silent rather than adding a
        // second complaint about the same character.
        if (typed !== null) commitAt(typed);
      },
      // Escape unwinds one step at a time: out of the typed field back to the
      // list it was reached from, and only then out of the picker.
      close: () => (custom ? setCustom(false) : actions.setSnooze(false)),
    }),
  );

  return (
    <Overlay
      open={open}
      onClose={() => actions.setSnooze(false)}
      labelledBy="snooze-title"
      /*
       * Hug the content. `Overlay` centres its panel in a row-direction flex
       * container, so the panel inherits `align-items: stretch` and grows to
       * the full height under the top padding — which is what the palette
       * wants, because the palette is a viewport onto a list that can be any
       * length. This one has at most five rows and a footer, and stretching
       * left two thirds of the panel as empty white.
       */
      className="self-start"
    >
      <div className="flex h-10 shrink-0 items-center gap-2 border-b border-border px-3">
        <Clock size={14} strokeWidth={1.75} className="shrink-0 text-faint-foreground" />
        <span id="snooze-title" className="text-list text-foreground">
          Snooze until
        </span>
        <span className="ml-auto text-micro text-faint-foreground">
          {ui.selection.ids.length > 1 ? `${ui.selection.ids.length} conversations` : null}
        </span>
      </div>

      {custom ? (
        <div className="px-3 py-3">
          <BareInput
            ref={field}
            id="snooze-custom"
            value={text}
            placeholder="next tuesday 9am"
            onChange={(event) => setText(event.target.value)}
          />
          <div className="pt-1.5 text-micro text-faint-foreground">
            {text.trim() === ""
              ? "Type a date"
              : typed === null
                ? "No date yet"
                : snoozeWhen(typed, now)}
          </div>
        </div>
      ) : (
        <div className="min-h-0 overflow-y-auto py-1">
          {/*
            Content-sized, not `flex-1`. The palette stretches its list because
            it is showing a slice of an unbounded set and the panel is the
            viewport onto it; this list is never longer than five rows, and
            borrowing that layout left a tall empty box under two options.
          */}
          {options.map((option, index) => {
            const active = index === cursor;
            return (
              <div
                key={option.id}
                data-snooze-option={option.id}
                data-active={active}
                onMouseMove={() => setCursor(index)}
                onClick={() => take(option)}
                className={cn(
                  "flex h-7 cursor-default items-center gap-2 px-3 text-list",
                  active ? "bg-row-selected" : "",
                )}
              >
                <span className="w-3 shrink-0 font-mono text-micro text-faint-foreground">
                  {index + 1}
                </span>
                <span className="shrink-0 text-foreground">{option.label}</span>
                <span className="ml-auto shrink-0 pl-2 font-mono text-micro text-faint-foreground">
                  {option.at === null ? "" : snoozeWhen(option.at, now)}
                </span>
              </div>
            );
          })}
        </div>
      )}

      <footer className="flex h-7 shrink-0 items-center gap-3 border-t border-border px-3">
        {custom ? (
          <span className="flex items-center gap-1">
            <Kbd keys="escape" />
            <span className="text-micro text-faint-foreground">back</span>
          </span>
        ) : (
          <span className="flex items-center gap-1">
            <Kbd keys="up" />
            <Kbd keys="down" />
            <span className="text-micro text-faint-foreground">navigate</span>
          </span>
        )}
        <span className="flex items-center gap-1">
          <CornerDownLeft size={11} strokeWidth={1.75} className="text-faint-foreground" />
          <span className="text-micro text-faint-foreground">snooze</span>
        </span>
        {!custom && (
          <span className="ml-auto text-micro text-faint-foreground">
            or press a number
          </span>
        )}
      </footer>
    </Overlay>
  );
}
