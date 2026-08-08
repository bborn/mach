import { CalendarDays, ChevronLeft, ChevronRight, Clock } from "lucide-react";
import {
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
  type ReactNode,
} from "react";
import {
  addDaysToValue,
  addMonthsToValue,
  dateFromValue,
  dateValue,
  formatDateField,
  formatTimeField,
  parseDateField,
  parseTimeField,
  timeChoices,
} from "@/lib/datetime-field";
import { daysOfMonthGrid, isSameDay, isToday, monthYear, startOfMonth } from "@/lib/time";
import { cn } from "@/lib/utils";
import { Popover, PopoverContent, PopoverTrigger } from "./popover";

/**
 * The date and time fields.
 *
 * # Why they are text boxes
 *
 * `<input type="date">` and `<input type="time">` are the two controls that
 * made the event modal look like a form on a website: WebKit draws them as
 * fixed-metric spin-boxes with a stepper the app cannot style, cannot resize
 * and cannot make dense. But a bad custom date picker is worse than a native
 * one, so the rule here is that **typing is the fast path and it has to be
 * more forgiving than the thing it replaced**:
 *
 *     8/12 · 12 · aug 12 · tomorrow · next friday · 2026-08-12
 *     9 · 930 · 9:30 · 9:30pm · 21:30 · noon
 *
 * The grammar lives in `lib/datetime-field.ts` and is tested there. The field
 * shows the canonical spelling ("Wed, Aug 12, 2026") whenever it is not being
 * typed in, so what you read back is unambiguous, and re-parsing what it shows
 * always gives the same value.
 *
 * # The keyboard
 *
 *   type            anything above; committed on Enter or on leaving the field
 *   ↑ / ↓           step — a day for a date, fifteen minutes for a time
 *   ⌥↓              open the picker
 *   Esc             (in the picker) close it, leaving the field alone
 *
 * Stepping with the arrows is deliberate: it is the one thing the native
 * stepper was good for, and losing it would have been a real regression rather
 * than a cosmetic one. The picker's own button is `tabIndex={-1}` so four date
 * fields do not cost four extra stops on the way to Save — the keyboard route
 * to it is ⌥↓, and the keyboard never needs it anyway.
 */

interface FieldShellProps {
  id?: string;
  label: string;
  display: string;
  invalid: boolean;
  disabled?: boolean;
  className?: string;
  inputRef: React.RefObject<HTMLInputElement | null>;
  onTextChange: (text: string) => void;
  onCommit: () => void;
  onFocus: () => void;
  onKeyDown: (event: ReactKeyboardEvent<HTMLInputElement>) => void;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  icon: ReactNode;
  picker: ReactNode;
  pickerWidth: string;
}

function FieldShell({
  id,
  label,
  display,
  invalid,
  disabled,
  className,
  inputRef,
  onTextChange,
  onCommit,
  onFocus,
  onKeyDown,
  open,
  onOpenChange,
  icon,
  picker,
  pickerWidth,
}: FieldShellProps) {
  // The popup hangs off the whole field, not off the little icon that opens it
  // — a grid that starts two thirds of the way across its own field reads as
  // belonging to whatever is next to it.
  const box = useRef<HTMLDivElement | null>(null);

  return (
    <Popover open={open} onOpenChange={onOpenChange}>
      <div
        ref={box}
        className={cn(
          "flex h-7 items-center rounded-[var(--radius)] border bg-background transition-colors",
          invalid ? "border-danger" : "border-border hover:border-border-strong",
          "focus-within:border-accent",
          disabled && "pointer-events-none opacity-40",
          className,
        )}
      >
        <input
          id={id}
          ref={inputRef}
          type="text"
          role="combobox"
          aria-expanded={open}
          aria-haspopup="dialog"
          aria-label={label}
          aria-invalid={invalid || undefined}
          autoComplete="off"
          autoCorrect="off"
          spellCheck={false}
          disabled={disabled}
          value={display}
          onFocus={onFocus}
          onChange={(event) => onTextChange(event.target.value)}
          onBlur={onCommit}
          onKeyDown={onKeyDown}
          className="min-w-0 flex-1 bg-transparent px-2 text-body text-foreground placeholder:text-faint-foreground focus:outline-none"
        />
        <PopoverTrigger
          // Not in the tab order on purpose — see the note at the top of the
          // file. ⌥↓ from the field does the same thing.
          tabIndex={-1}
          aria-label={`Pick ${label.toLowerCase()}`}
          className="flex h-full items-center px-1.5 text-faint-foreground transition-colors hover:text-muted-foreground"
        >
          {icon}
        </PopoverTrigger>
      </div>
      <PopoverContent
        align="start"
        anchor={box}
        className={cn("p-1.5", pickerWidth)}
        finalFocus={inputRef}
        aria-label={label}
      >
        {picker}
      </PopoverContent>
    </Popover>
  );
}

/** Shared editing machinery: display text, commit-on-blur, invalid state. */
function useTypedField(
  value: string,
  format: (value: string) => string,
  parse: (text: string) => string | null,
  onChange: (next: string) => void,
) {
  const [text, setText] = useState<string | null>(null);
  const [invalid, setInvalid] = useState(false);
  const inputRef = useRef<HTMLInputElement | null>(null);

  // A value changed from outside — a drag on the grid, the all-day toggle
  // rewriting the end date — wins over half-typed text.
  useEffect(() => {
    setText(null);
    setInvalid(false);
  }, [value]);

  const commit = () => {
    if (text === null) return;
    if (text.trim() === "") {
      setText(null);
      setInvalid(false);
      return;
    }
    const parsed = parse(text);
    if (parsed === null) {
      setInvalid(true);
      return;
    }
    setText(null);
    setInvalid(false);
    if (parsed !== value) onChange(parsed);
  };

  const set = (next: string) => {
    setText(null);
    setInvalid(false);
    onChange(next);
  };

  return {
    inputRef,
    invalid,
    display: text ?? format(value),
    onTextChange: (next: string) => {
      setText(next);
      if (invalid) setInvalid(false);
    },
    commit,
    set,
    focus: () => {
      setText(format(value));
      window.requestAnimationFrame(() => inputRef.current?.select());
    },
  };
}

export interface DateFieldProps {
  id?: string;
  value: string;
  onChange: (value: string) => void;
  label: string;
  disabled?: boolean;
  className?: string;
}

export function DateField({ id, value, onChange, label, disabled, className }: DateFieldProps) {
  const [open, setOpen] = useState(false);
  const field = useTypedField(
    value,
    formatDateField,
    (text) => parseDateField(text, { current: value }),
    onChange,
  );

  return (
    <FieldShell
      id={id}
      label={label}
      display={field.display}
      invalid={field.invalid}
      disabled={disabled}
      className={className}
      inputRef={field.inputRef}
      onTextChange={field.onTextChange}
      onCommit={field.commit}
      onFocus={field.focus}
      open={open}
      onOpenChange={setOpen}
      icon={<CalendarDays size={12} strokeWidth={1.75} />}
      pickerWidth="w-[15rem]"
      picker={
        <MonthGrid
          value={value}
          onPick={(next) => {
            field.set(next);
            setOpen(false);
          }}
        />
      }
      onKeyDown={(event) => {
        if (event.key === "Enter") {
          event.preventDefault();
          field.commit();
          return;
        }
        if (event.altKey && event.key === "ArrowDown") {
          event.preventDefault();
          setOpen(true);
          return;
        }
        if (event.altKey || event.metaKey || event.ctrlKey) return;
        if (event.key === "ArrowUp" || event.key === "ArrowDown") {
          event.preventDefault();
          const base = field.invalid ? value : (parseDateField(field.display, { current: value }) ?? value);
          field.set(addDaysToValue(base, event.key === "ArrowUp" ? 1 : -1));
        }
      }}
    />
  );
}

export interface TimeFieldProps extends DateFieldProps {
  /** Minutes an arrow key moves. Fifteen is the grid's own resolution. */
  step?: number;
}

export function TimeField({
  id,
  value,
  onChange,
  label,
  disabled,
  className,
  step = 15,
}: TimeFieldProps) {
  const [open, setOpen] = useState(false);
  const field = useTypedField(value, formatTimeField, parseTimeField, onChange);

  return (
    <FieldShell
      id={id}
      label={label}
      display={field.display}
      invalid={field.invalid}
      disabled={disabled}
      className={className}
      inputRef={field.inputRef}
      onTextChange={field.onTextChange}
      onCommit={field.commit}
      onFocus={field.focus}
      open={open}
      onOpenChange={setOpen}
      icon={<Clock size={12} strokeWidth={1.75} />}
      pickerWidth="w-[8.5rem]"
      picker={
        <TimeList
          value={value}
          onPick={(next) => {
            field.set(next);
            setOpen(false);
          }}
        />
      }
      onKeyDown={(event) => {
        if (event.key === "Enter") {
          event.preventDefault();
          field.commit();
          return;
        }
        if (event.altKey && event.key === "ArrowDown") {
          event.preventDefault();
          setOpen(true);
          return;
        }
        if (event.altKey || event.metaKey || event.ctrlKey) return;
        if (event.key === "ArrowUp" || event.key === "ArrowDown") {
          event.preventDefault();
          const base = field.invalid ? value : (parseTimeField(field.display) ?? value);
          field.set(shiftTime(base, event.key === "ArrowUp" ? step : -step));
        }
      }}
    />
  );
}

/** `HH:mm` moved by `minutes`, wrapping at midnight rather than overflowing. */
function shiftTime(value: string, minutes: number): string {
  const match = value.match(/^(\d{1,2}):(\d{2})$/);
  const total = match ? Number(match[1]) * 60 + Number(match[2]) : 0;
  const next = ((total + minutes) % 1440 + 1440) % 1440;
  return `${String(Math.floor(next / 60)).padStart(2, "0")}:${String(next % 60).padStart(2, "0")}`;
}

/* -------------------------------------------------------------------------- */
/* The pickers                                                                 */
/* -------------------------------------------------------------------------- */

const WEEKDAY_INITIALS = ["M", "T", "W", "T", "F", "S", "S"];

/**
 * Six weeks of days, arrow-navigable.
 *
 * A roving `tabIndex` rather than 42 tab stops: the grid is one control, and
 * Tab out of it should reach the next field, not the ninth of August.
 */
function MonthGrid({ value, onPick }: { value: string; onPick: (value: string) => void }) {
  const [cursor, setCursor] = useState(value);
  const cursorRef = useRef<HTMLButtonElement | null>(null);
  const moved = useRef(false);

  useEffect(() => setCursor(value), [value]);

  // Focus follows the cursor, but only once the user has actually moved it —
  // stealing focus on mount would fight Base UI's own initial focus.
  useLayoutEffect(() => {
    if (moved.current) cursorRef.current?.focus();
  }, [cursor]);

  const anchor = dateFromValue(cursor) ?? new Date();
  const selected = dateFromValue(value);
  const month = startOfMonth(anchor);
  const days = daysOfMonthGrid(anchor);

  const move = (deltaDays: number) => {
    moved.current = true;
    setCursor(addDaysToValue(cursor, deltaDays));
  };

  return (
    <div
      className="flex flex-col gap-1"
      onKeyDown={(event) => {
        const by: Record<string, number> = {
          ArrowLeft: -1,
          ArrowRight: 1,
          ArrowUp: -7,
          ArrowDown: 7,
        };
        if (event.key in by) {
          event.preventDefault();
          move(by[event.key]);
          return;
        }
        if (event.key === "PageUp" || event.key === "PageDown") {
          event.preventDefault();
          moved.current = true;
          setCursor(addMonthsToValue(cursor, event.key === "PageUp" ? -1 : 1));
        }
      }}
    >
      <div className="flex items-center gap-1 px-0.5">
        <MonthStep label="Previous month" onClick={() => setCursor(addMonthsToValue(cursor, -1))}>
          <ChevronLeft size={13} strokeWidth={1.75} />
        </MonthStep>
        <span className="flex-1 text-center text-micro font-medium text-muted-foreground">
          {monthYear(month)}
        </span>
        <MonthStep label="Next month" onClick={() => setCursor(addMonthsToValue(cursor, 1))}>
          <ChevronRight size={13} strokeWidth={1.75} />
        </MonthStep>
      </div>

      <div className="grid grid-cols-7">
        {WEEKDAY_INITIALS.map((initial, i) => (
          <span key={i} className="py-0.5 text-center text-micro text-faint-foreground">
            {initial}
          </span>
        ))}
        {days.map((day) => {
          const dayValue = dateValue(day);
          const isSelected = selected !== null && isSameDay(day, selected);
          const outside = day.getMonth() !== month.getMonth();
          return (
            <button
              key={dayValue}
              ref={dayValue === cursor ? cursorRef : undefined}
              type="button"
              tabIndex={dayValue === cursor ? 0 : -1}
              aria-current={isToday(day) ? "date" : undefined}
              aria-pressed={isSelected}
              onClick={() => onPick(dayValue)}
              className={cn(
                "flex h-6 items-center justify-center rounded-[3px] text-micro tabular-nums transition-colors",
                outside ? "text-faint-foreground/60" : "text-foreground",
                isToday(day) && !isSelected && "text-accent",
                isSelected
                  ? "bg-accent text-accent-foreground"
                  : "hover:bg-row-hover focus-visible:bg-row-hover",
              )}
            >
              {day.getDate()}
            </button>
          );
        })}
      </div>
    </div>
  );
}

function MonthStep({
  label,
  onClick,
  children,
}: {
  label: string;
  onClick: () => void;
  children: ReactNode;
}) {
  return (
    <button
      type="button"
      tabIndex={-1}
      aria-label={label}
      onClick={onClick}
      className="flex size-5 items-center justify-center rounded-[3px] text-faint-foreground transition-colors hover:bg-row-hover hover:text-foreground"
    >
      {children}
    </button>
  );
}

const CHOICES = timeChoices(30);

/** Half hours, scrolled to the one in the field. */
function TimeList({ value, onPick }: { value: string; onPick: (value: string) => void }) {
  const listRef = useRef<HTMLDivElement | null>(null);
  const nearest =
    CHOICES.find((choice) => choice >= value) ?? CHOICES[CHOICES.length - 1];

  useLayoutEffect(() => {
    const list = listRef.current;
    const target = list?.querySelector<HTMLElement>(`[data-time="${nearest}"]`);
    if (list && target) list.scrollTop = target.offsetTop - list.clientHeight / 2 + 12;
  }, [nearest]);

  return (
    <div ref={listRef} className="max-h-56 overflow-y-auto" role="listbox" aria-label="Time">
      {CHOICES.map((choice) => {
        const isSelected = choice === value;
        return (
          <button
            key={choice}
            type="button"
            role="option"
            aria-selected={isSelected}
            data-time={choice}
            tabIndex={choice === nearest ? 0 : -1}
            onClick={() => onPick(choice)}
            className={cn(
              "flex h-6 w-full items-center rounded-[3px] px-2 text-micro tabular-nums transition-colors",
              isSelected
                ? "bg-accent text-accent-foreground"
                : "text-foreground hover:bg-row-hover focus-visible:bg-row-hover",
            )}
          >
            {formatTimeField(choice)}
          </button>
        );
      })}
    </div>
  );
}
