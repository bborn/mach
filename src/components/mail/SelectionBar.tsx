import { useMemo } from "react";
import { useKeyBindings } from "@/hooks/useKeymap";
import { overlayOwnsKeyboard, useMach } from "@/hooks/useMach";
import { cn } from "@/lib/utils";
import { Kbd } from "@/components/ui/kbd";
import { mailActionHandlers } from "./mail-bindings";
import { selectionActions, type SelectionAction } from "./selection-actions";

/**
 * What can be done with what is ticked, drawn under the list header.
 *
 * # Why here and not in the status bar
 *
 * Both surfaces counted the selection, and the count is where the verbs belong,
 * so only one of them could keep it. This one, for three reasons: the rows it
 * describes are directly underneath it, so the eye does not have to cross the
 * window to find out what a tick did; it is scoped to the thread list, and the
 * status bar is app-wide chrome that also serves a calendar where a mail
 * selection means nothing; and a 24px rail in the far corner has room for a
 * number and no room for five verbs. `StatusBar` and the list header have both
 * given the count up — see the comments there.
 *
 * It appears only with a selection. `commandTargets` also resolves to the
 * cursor row when nothing is ticked, and a bar that was always up would be a
 * permanent toolbar for a keyboard app: the point is to answer "what now?" at
 * the moment the question is asked.
 *
 * # It is a legend, not a toolbar
 *
 * Every button draws its key beside its label, because the key is the thing
 * worth learning and the button is the thing that teaches it. Nothing here is
 * mouse-only: an action exists in this bar only if it exists in
 * `mail-bindings.ts`, and both go through the same entry in
 * `MailActionHandlers` — see `selection-actions.ts` for the table both read.
 */
export function SelectionBar() {
  const { ui, dispatch, visibleThreads, commandTargets, isUnread, actions } = useMach();
  const on = useMemo(() => mailActionHandlers(actions), [actions]);
  const count = ui.selection.ids.length;

  /*
   * What the labels have to know about the selection: whether "Star" or
   * "Unstar" is the honest word, and whether "Mark read" is.
   *
   * Read off `visibleThreads`, which has the optimistic projection already
   * applied, so a star pressed a moment ago has already flipped the label
   * rather than waiting out a round trip to say so.
   */
  const marks = useMemo(() => {
    const chosen = new Set(ui.selection.ids);
    const rows = visibleThreads.filter((thread) => chosen.has(thread.id));
    return {
      allStarred: rows.length > 0 && rows.every((thread) => thread.starred),
      anyUnread: rows.some((thread) => isUnread(thread)),
    };
  }, [visibleThreads, ui.selection.ids, isUnread]);

  const items = useMemo(
    () => selectionActions(ui.labelId, marks),
    [ui.labelId, marks],
  );

  const confirming = ui.confirmDiscard && items.some((item) => item.confirm);

  /*
   * Escape while the question is up takes back the question, not the selection.
   *
   * Above the priority of the Escape in `MailMode` that clears the selection,
   * because they are asked in that order: the shallowest thing on screen is the
   * question, so it is the first thing Escape should undo. A second Escape then
   * reaches the selection exactly as it always did.
   */
  useKeyBindings([
    {
      keys: "escape",
      priority: 11,
      when: () => confirming && !overlayOwnsKeyboard(ui),
      handler: () => dispatch({ type: "confirmDiscard", armed: false }),
    },
  ]);

  /*
   * A question that has been asked is always on screen.
   *
   * The count is what usually puts the bar up, and a selection is the usual way
   * to arm the discard — but not the only one: with nothing ticked,
   * `commandTargets` resolves to the open conversation, so `#` on a single
   * draft arms it too. If the bar drew only on `count > 0` that press would arm
   * silently and the next `#` would destroy the draft with nothing having been
   * asked, which is the one thing this action exists to prevent.
   */
  if (count === 0 && !confirming) return null;

  const targets = commandTargets.length;

  return (
    <div
      role="toolbar"
      aria-label={confirming ? "Discard drafts" : `${count} selected`}
      className={cn(
        "flex h-8 shrink-0 items-center gap-3 overflow-x-auto border-b px-3",
        confirming ? "border-danger/40 bg-danger/5" : "border-border bg-surface",
      )}
    >
      {/* The count steps aside for the question, which states it anyway. In a
          strip this narrow — the list pane can be dragged to a third of the
          window — the number twice costs the sentence its second half. */}
      {count > 0 && !confirming && (
        <span className="shrink-0 whitespace-nowrap font-mono text-micro tabular-nums text-accent">
          {count} selected
        </span>
      )}

      {confirming ? (
        /*
         * The same question the composer asks for one draft, asked about
         * several. Its wording is the reason it is asked at all: a draft is the
         * only thing in this app with no copy anywhere else once it is gone,
         * and unlike an archive there is no inverse for ⌘Z to run.
         *
         * "It is not kept anywhere else" loses its subject here — the composer
         * has a full-width strip to itself and this shares one with two
         * buttons, and at the widths the list actually gets, the longer
         * sentence truncated away the half that gives the reason.
         */
        <>
          <span className="min-w-0 flex-1 truncate text-micro text-muted-foreground">
            Throw {targets === 1 ? "this draft" : `these ${targets} drafts`} away? Not
            kept anywhere else.
          </span>
          <button
            type="button"
            onClick={() => on.discard()}
            className="inline-flex shrink-0 items-center gap-1.5 whitespace-nowrap text-micro text-danger hover:brightness-110"
          >
            <Kbd keys="#" /> Discard
          </button>
          <button
            type="button"
            onClick={() => dispatch({ type: "confirmDiscard", armed: false })}
            className="inline-flex shrink-0 items-center gap-1.5 whitespace-nowrap text-micro text-faint-foreground hover:text-foreground"
          >
            <Kbd keys="escape" /> Keep
          </button>
        </>
      ) : (
        items.map((item) => (
          <Action key={item.id} action={item} run={() => on[item.handler]()} />
        ))
      )}
    </div>
  );
}

function Action({ action, run }: { action: SelectionAction; run: () => void }) {
  return (
    <button
      type="button"
      onClick={run}
      className={cn(
        "inline-flex shrink-0 items-center gap-1.5 whitespace-nowrap text-micro",
        action.tone === "danger"
          ? "text-faint-foreground hover:text-danger"
          : "text-faint-foreground hover:text-foreground",
      )}
    >
      <Kbd keys={action.keys} /> {action.label}
    </button>
  );
}
