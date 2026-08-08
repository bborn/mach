import { useEffect, useMemo, useRef, useState } from "react";
import { Overlay } from "@/components/ui/dialog";
import { BareInput } from "@/components/ui/input";
import { Kbd } from "@/components/ui/kbd";
import { describeParsed, parseEventText, type CalendarChoice, type ParsedEvent } from "@/lib/calendar-nlp";

interface QuickCreateProps {
  open: boolean;
  /** Seeded when the inline editor hands a half-written event over on Tab. */
  seed?: string;
  calendars: readonly CalendarChoice[];
  onClose: () => void;
  onCreate: (parsed: ParsedEvent, text: string) => void;
}

/**
 * Natural-language event entry (§3, path B).
 *
 * One field, and a line underneath that resolves *as you type*. That line is
 * the whole design: Enter has to be a confirmation of something already on
 * screen, not a leap of faith. Natural-language input that people abandon is
 * the kind that silently schedules something in 2027 and only tells you later.
 */
export function QuickCreate({ open, seed, calendars, onClose, onCreate }: QuickCreateProps) {
  const [text, setText] = useState("");
  const field = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (open) {
      setText(seed ?? "");
      // The Overlay focuses the first field; this puts the caret at the end.
      requestAnimationFrame(() => field.current?.setSelectionRange(999, 999));
    }
  }, [open, seed]);

  const parsed = useMemo(
    () => (text.trim() ? parseEventText(text, { calendars }) : null),
    [text, calendars],
  );

  // `self-start` so the panel hugs its content: the dialog frame stretches to
  // the overlay's height otherwise, and a one-field composer filling two thirds
  // of the window reads as a modal — the thing §3 is avoiding.
  return (
    <Overlay open={open} onClose={onClose} align="top" className="max-w-[34rem] self-start">
      <div className="flex flex-col gap-2 p-3">
        <BareInput
          ref={field}
          value={text}
          placeholder="Standup tomorrow 2pm /w"
          onChange={(event) => setText(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === "Enter" && parsed) {
              event.preventDefault();
              onCreate(parsed, text);
            }
          }}
        />

        <div className="flex items-baseline gap-2 border-t border-border pt-2 text-micro">
          <span className="min-w-0 flex-1 truncate text-muted-foreground">
            {parsed ? describeParsed(parsed) : "date, time, /calendar, with…, at…, alert…"}
          </span>
          <Kbd keys="enter" />
        </div>

        {parsed?.title && (
          <div className="truncate text-list text-foreground">{parsed.title}</div>
        )}
      </div>
    </Overlay>
  );
}
