import { useCallback } from "react";
import { useMach } from "@/hooks/useMach";
import { useKeyBindings } from "@/hooks/useKeymap";
import { FlexPane, Pane, Resizer } from "@/components/ui/split";
import { AccountRail } from "./AccountRail";
import { ComposerDock } from "./ComposerDock";
import { ReadingPane } from "./ReadingPane";
import { ThreadList } from "./ThreadList";

export function MailMode() {
  const { ui, dispatch, actions, accounts } = useMach();
  // Two gates, not one. `mail` is "this mode is on screen" — enough for the
  // account switches, which mean the same thing wherever the cursor is. `list`
  // adds "and the keyboard is in the list", which is what keeps `j`/`k`/Enter
  // from being claimed by both the list and the rail at the same moment.
  const mail = ui.mode === "mail" && !ui.paletteOpen;
  const active = mail && ui.focus === "list";
  const selecting = ui.selection.ids.length > 0;

  const onResize = useCallback(
    (width: number) => dispatch({ type: "listWidth", width }),
    [dispatch],
  );

  useKeyBindings([
    {
      keys: "j",
      group: "Mail",
      description: "Next conversation",
      when: () => active,
      handler: () => actions.moveCursor(1),
    },
    {
      keys: "k",
      group: "Mail",
      description: "Previous conversation",
      when: () => active,
      handler: () => actions.moveCursor(-1),
    },
    {
      keys: "down",
      when: () => active,
      handler: () => actions.moveCursor(1),
    },
    {
      keys: "up",
      when: () => active,
      handler: () => actions.moveCursor(-1),
    },
    {
      keys: "enter",
      group: "Mail",
      description: "Open conversation",
      when: () => active,
      handler: () => actions.openSelected(),
    },
    /* Multi-select. `x` is the Gmail/Superhuman verb and the one his hands
       already know; ⇧J/⇧K are the same movement keys with the range dragged
       along behind them. */
    {
      keys: "x",
      group: "Mail",
      description: "Select conversation and move on",
      when: () => active,
      handler: () => actions.toggleAtCursor(),
    },
    {
      keys: "shift+j",
      group: "Mail",
      description: "Extend selection down",
      when: () => active,
      handler: () => actions.extendCursor(1),
    },
    {
      keys: "shift+k",
      group: "Mail",
      description: "Extend selection up",
      when: () => active,
      handler: () => actions.extendCursor(-1),
    },
    {
      // Safe to claim: `globals.css` puts `user-select: none` on the chrome, so
      // the WebView's own select-all has nothing to select here. It is still
      // preventDefault-ed (the registry does that by default) so the WebView
      // does not try anyway.
      keys: "mod+a",
      group: "Mail",
      description: "Select all loaded conversations",
      when: () => active,
      handler: () => actions.selectAllThreads(),
    },
    {
      keys: "e",
      group: "Mail",
      description: "Archive",
      when: () => active,
      handler: () => actions.archiveSelected(),
    },
    {
      keys: "h",
      group: "Mail",
      description: "Snooze",
      when: () => active,
      handler: () => actions.snoozeSelected(),
    },
    {
      keys: "s",
      group: "Mail",
      description: "Star",
      when: () => active,
      handler: () => actions.starSelected(),
    },
    {
      keys: "#",
      group: "Mail",
      description: "Trash",
      when: () => active,
      handler: () => actions.trashSelected(),
    },
    // `r`, `a` and `f` are registered by <ComposerDock/>, which owns the draft
    // they open. A binding here would have to reach into that state through the
    // shell to do anything useful.
    {
      // ⇧F, not F: `f` is forward, and Superhuman's muscle memory outranks a
      // new feature's first choice of key. The mnemonic survives the shift.
      keys: "shift+f",
      group: "Mail",
      description: "Favorite the conversation, or the mailbox",
      when: () => active,
      handler: () => actions.toggleFavoriteFocused(),
    },
    {
      keys: "z",
      group: "Mail",
      description: "Undo last action",
      when: () => mail,
      handler: () => actions.undo(),
    },
    {
      // The rail and the list are one loop, not two worlds. Tab is unclaimed in
      // mail mode and is the idiom for exactly this.
      keys: "tab",
      group: "Mail",
      description: "Move between the sidebar and the list",
      when: () => mail,
      handler: () => actions.toggleFocus(),
    },
    {
      keys: "shift+tab",
      when: () => mail,
      handler: () => actions.toggleFocus(),
    },
    /* Two Escapes, mutually exclusive so the registry sees no conflict: with a
       selection up it drops the selection, otherwise it closes the thread. */
    {
      keys: "escape",
      group: "Mail",
      description: "Clear the selection",
      priority: 10,
      when: () => active && selecting,
      handler: () => actions.clearSelection(),
    },
    {
      keys: "escape",
      group: "Mail",
      description: "Close conversation",
      priority: 10,
      when: () => active && !selecting && ui.threadId !== null,
      handler: () => actions.closeThread(),
    },
    /*
     * ⌃1–5 filters to an account, ⌃0 clears the filter.
     *
     * ⌘ picks the surface (⌘1 mail, ⌘2 calendar) and ⌃ picks the account —
     * one modifier per axis, which is the only version of this anyone can
     * remember. Not ⌥: Option is the text-entry modifier, and ⌥e in the
     * composer is the start of an accented character, not a shortcut. (It was
     * also silently broken until `tokenFromEvent` learned to read `event.code`,
     * because macOS turns ⌥1 into `¡` before the app ever sees it.)
     */
    ...accounts.map((account, index) => ({
      keys: `ctrl+${index + 1}`,
      group: "Mail",
      description: `Filter to ${account.name}`,
      when: () => mail,
      handler: () => dispatch({ type: "account", accountId: account.id }),
    })),
    {
      keys: "ctrl+0",
      group: "Mail",
      description: "All accounts",
      when: () => mail,
      handler: () => dispatch({ type: "account", accountId: null }),
    },
  ]);

  return (
    <div className="flex h-full min-h-0">
      <AccountRail />
      <Pane width={ui.listWidth}>
        <ThreadList />
      </Pane>
      <Resizer width={ui.listWidth} onResize={onResize} />
      <FlexPane>
        {/* The composer grows at the bottom of the conversation it answers, so
            the message being replied to never leaves the screen. */}
        <ReadingPane />
        <ComposerDock />
      </FlexPane>
    </div>
  );
}
