import { usePendingSequence } from "@/hooks/useKeymap";
import { useMach } from "@/hooks/useMach";
import { formatBinding } from "@/lib/keymap";
import { SyncIndicator } from "./SyncIndicator";

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
 * view that nothing else on screen states. The count selected, the pending
 * `g …`, the fixture warning, and the sync indicator — which renders nothing
 * at all unless a sync is running or has failed.
 *
 * # The keymap is not one of those facts
 *
 * The rail also carried a permanent legend of the current mode's bindings —
 * `j k move · ⏎ open · x select · e archive · b snooze · r reply · c new` in
 * mail, the three view keys in calendar. It failed the same test twice over.
 * In mail it sat directly under the composer strip, which offers `r` and `c`
 * again as buttons; in calendar every one of its three rows was a second copy
 * of a button in the calendar header. And a legend of the whole keymap is not
 * a fact about the current view at all — it says the same thing in an empty
 * inbox as in an open thread, which is what makes it furniture.
 *
 * `?` opens the reference card. That is where a legend belongs, and the sheet
 * is one key away rather than buried in settings. Bindings are untouched.
 *
 * # Neither was the count
 *
 * The rail also carried `${n} conversations` in mail mode and `${view} view`
 * in calendar mode. Both failed the same test: the mail list header already
 * shows `Inbox · All accounts · 48` above the column being counted, and the
 * calendar header already highlights the active view in its Day/Week/Month
 * control. Both were a second copy of a number already on screen.
 */
export function StatusBar() {
  const { ui, live, progress } = useMach();
  const pending = usePendingSequence();
  const selected = ui.mode === "mail" ? ui.selection.ids.length : 0;
  const syncing = progress.active || progress.errors.length > 0;
  // Empty chrome is also bland. A strip that says nothing — no selection, no
  // pending `g …`, live data, idle sync — costs 24px forever to be a second
  // copy of the window's bottom edge. Collapse it; the container in App.tsx
  // hides itself when every child is gone.
  if (selected === 0 && !pending && live && !syncing) return null;

  return (
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
    </footer>
  );
}
