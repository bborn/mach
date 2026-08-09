import { usePendingSequence } from "@/hooks/useKeymap";
import { useMach } from "@/hooks/useMach";
import { formatBinding } from "@/lib/keymap";
import { Hint } from "@/components/ui/kbd";
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
 * The bottom rail carries what is *true*, not what just happened: how much is
 * in front of you, where a half-typed sequence stands, and the bindings for the
 * current mode.
 *
 * It used to carry the transient status message as well, and that was the whole
 * problem — an acknowledgement of something that just moved rows off screen,
 * set in 11px on a 24px rail beside the sync spinner. `chrome/Toast.tsx` shows
 * it now, loudly and with the button attached.
 *
 * # Nothing here is a copy of a message
 *
 * The rail kept one piece of that for a while: a permanent "⌘Z Undo archived 1
 * conversation", on the argument that a key nobody can name is a key nobody
 * presses. The toast already names it, at the moment it means something, with
 * the button attached — so the line was a second copy of a transient message,
 * sitting in the corner of the window long after the archive it described. ⌘Z
 * works exactly as it did; the stack still never expires. It simply is not
 * announced by furniture any more.
 *
 * What is left passes the same test: every item is a fact about the current
 * view that nothing else on screen states. The count of conversations, the
 * count selected, the pending `g …`, the fixture warning, and the sync
 * indicator — which renders nothing at all unless a sync is running or has
 * failed. The hints are a reference card rather than news.
 */
export function StatusBar() {
  const { ui, visibleThreads, hasMore, live } = useMach();
  const pending = usePendingSequence();
  const hints = ui.mode === "mail" ? MAIL_HINTS : CALENDAR_HINTS;
  const selected = ui.mode === "mail" ? ui.selection.ids.length : 0;

  return (
    // No border of its own: the container in `App.tsx` draws the one edge the
    // bottom of the window needs, whichever of these strips is topmost.
    <footer className="flex h-6 shrink-0 items-center gap-3 overflow-hidden bg-surface px-3">
      {/* How many rows the next keystroke will act on. It sits *beside* the
          status message rather than inside it: the message is transient, and
          the count has to survive it or the number the user is acting on
          disappears for six seconds after every ⌘A. */}
      {selected > 0 && (
        <span className="shrink-0 whitespace-nowrap font-mono text-micro tabular-nums text-accent">
          {selected} selected
        </span>
      )}

      <span className="shrink-0 whitespace-nowrap font-mono text-micro tabular-nums text-faint-foreground">
        {ui.mode === "mail"
          ? `${visibleThreads.length}${hasMore ? "+" : ""} conversations`
          : `${ui.calendarView} view`}
      </span>

      {pending && (
        <span className="shrink-0 whitespace-nowrap font-mono text-micro text-accent">
          {formatBinding(pending)} …
        </span>
      )}

      {/* Fixture data looks exactly like real mail, so say when it isn't. */}
      {!live && (
        <span
          title="Fixture data"
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
