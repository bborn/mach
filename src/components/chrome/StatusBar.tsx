import { usePendingSequence } from "@/hooks/useKeymap";
import { useMach } from "@/hooks/useMach";
import { formatBinding } from "@/lib/keymap";
import { describeUndo, peekUndo } from "@/lib/undo-stack";
import { cn } from "@/lib/utils";
import { Hint, Kbd } from "@/components/ui/kbd";
import { SyncIndicator } from "./SyncIndicator";

const MAIL_HINTS: [string[], string][] = [
  [["j", "k"], "move"],
  [["enter"], "open"],
  [["x"], "select"],
  [["e"], "archive"],
  [["b"], "snooze"],
  [["r"], "reply"],
  [["c"], "new"],
];

const CALENDAR_HINTS: [string[], string][] = [
  [["1", "2", "3"], "day / week / month"],
  [["j", "k"], "period"],
  [["t"], "today"],
];

/**
 * The bottom rail carries four things and nothing else: what just happened,
 * what ⌘Z would take back, where a half-typed sequence stands, and the bindings
 * for the current mode.
 */
export function StatusBar() {
  const { ui, visibleThreads, hasMore, live, actions, undoState } = useMach();
  const pending = usePendingSequence();
  const hints = ui.mode === "mail" ? MAIL_HINTS : CALENDAR_HINTS;
  const selected = ui.mode === "mail" ? ui.selection.ids.length : 0;
  /*
   * What ⌘Z would do, named — "Undo archived 3 conversations".
   *
   * An undo nobody can see is worse than none: press it, something changes
   * somewhere off screen, and the only way to find out what is to go looking.
   * So the rail says it, and it says it from the *stack* rather than from the
   * status message, which is what makes it survive the status message timing
   * out. Nothing to undo, nothing shown.
   */
  const undoLabel = describeUndo(peekUndo(undoState));

  return (
    <footer className="flex h-6 shrink-0 items-center gap-3 overflow-hidden border-t border-border bg-surface px-3">
      {/* How many rows the next keystroke will act on. It sits *beside* the
          status message rather than inside it: the message is transient, and
          the count has to survive it or the number the user is acting on
          disappears for six seconds after every ⌘A. */}
      {selected > 0 && (
        <span className="shrink-0 whitespace-nowrap font-mono text-micro tabular-nums text-accent">
          {selected} selected
        </span>
      )}

      {ui.status ? (
        <span className="flex min-w-0 items-center gap-2">
          <span
            className={cn(
              "truncate text-micro",
              ui.status.tone === "error" ? "text-danger" : "text-foreground",
            )}
          >
            {ui.status.message}
          </span>
          {/* The offer, while the offer stands. It is driven by the stack and
              not by `ui.status.undo`, so the button is never on screen for an
              undo that has already been spent. */}
          {undoLabel && (
            <button
              type="button"
              title={undoLabel}
              onClick={actions.undo}
              className="shrink-0 whitespace-nowrap text-micro text-accent hover:underline"
            >
              Undo
            </button>
          )}
        </span>
      ) : (
        <span className="flex min-w-0 items-center gap-2">
          <span className="shrink-0 whitespace-nowrap font-mono text-micro tabular-nums text-faint-foreground">
            {ui.mode === "mail"
              ? `${visibleThreads.length}${hasMore ? "+" : ""} conversations`
              : `${ui.calendarView} view`}
          </span>
          {/* And once the status message has timed out, the quiet version of
              the same sentence — because ⌘Z still works, and a key whose effect
              is unnamed is a key nobody presses. */}
          {undoLabel && (
            <button
              type="button"
              title={undoLabel}
              onClick={actions.undo}
              className="flex min-w-0 items-center gap-1 text-faint-foreground hover:text-foreground"
            >
              <Kbd keys="mod+z" />
              <span className="truncate text-micro">{undoLabel}</span>
            </button>
          )}
        </span>
      )}

      {pending && (
        <span className="shrink-0 whitespace-nowrap font-mono text-micro text-accent">
          {formatBinding(pending)} …
        </span>
      )}

      {/* Fixture data looks exactly like real mail, so say when it isn't. */}
      {!live && (
        <span
          title="No Tauri backend — rendering fixture data"
          className="shrink-0 whitespace-nowrap font-mono text-micro uppercase tracking-[0.06em] text-warning"
        >
          fixtures
        </span>
      )}

      <span className="ml-auto flex min-w-0 shrink items-center justify-end">
        <SyncIndicator />
      </span>

      <span className="flex shrink-0 items-center gap-3 overflow-hidden">
        {hints.map(([keys, label]) => (
          <Hint key={label} keys={keys} label={label} />
        ))}
      </span>
    </footer>
  );
}
