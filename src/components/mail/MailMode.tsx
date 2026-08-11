import { useCallback } from "react";
import { overlayOwnsKeyboard, useMach } from "@/hooks/useMach";
import { useKeyBindings } from "@/hooks/useKeymap";
import { keyboardInComposer } from "@/lib/compose";
import { LIST_WIDTH_BOUNDS } from "@/lib/prefs";
import { FlexPane, Pane, Resizer } from "@/components/ui/split";
import { AccountRail } from "./AccountRail";
import { READING_COLUMN } from "./composer-layout";
import { ComposerDock } from "./ComposerDock";
import { mailActionBindings } from "./mail-bindings";
import { ReadingPane } from "./ReadingPane";
import { SearchView } from "./SearchView";
import { SnoozePicker } from "./SnoozePicker";
import { ThreadList } from "./ThreadList";

export function MailMode() {
  const { ui, dispatch, actions, accounts } = useMach();
  /*
   * Two gates, not one. `mail` is "this mode is on screen" — enough for the
   * account switches, which mean the same thing wherever the cursor is. `list`
   * adds "and the keyboard is in the list", which is what keeps `j`/`k`/Enter
   * from being claimed by both the list and the rail at the same moment.
   *
   * `overlayOwnsKeyboard` covers every dialog, the palette included, and
   * replaces the `!ui.paletteOpen` this used to read: the palette was never
   * special, it was only the one overlay this line had heard of. With
   * preferences open, `e` archived a conversation the user could not see.
   */
  const mail = ui.mode === "mail" && !overlayOwnsKeyboard(ui);
  const active = mail && ui.focus === "list";
  const selecting = ui.selection.ids.length > 0;

  const onResize = useCallback(
    (width: number) => dispatch({ type: "listWidth", width }),
    [dispatch],
  );

  /*
   * Declaration order is the order the help sheet prints, so these read down
   * the page as tasks — move, open, leave; then act; then select — rather than
   * in whatever order they were added over time.
   */
  useKeyBindings([
    /* ------------------------------------------------------------- Mail --- */
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
    {
      // Gmail's second name for Enter. Free here — the calendar's `o` opens the
      // event in Google Calendar, and the two modes are never live together.
      keys: "o",
      when: () => active,
      handler: () => actions.openSelected(),
    },
    {
      // Gmail's "back to the threadlist". Escape does this too and always has;
      // `u` is the one a Gmail hand reaches for, and unlike Escape it does not
      // have to share the key with clearing a selection.
      keys: "u",
      group: "Mail",
      description: "Back to the list",
      when: () => active,
      handler: () => actions.closeThread(),
    },
    {
      // The rail and the list are one loop, not two worlds. Tab is unclaimed in
      // mail mode and is the idiom for exactly this.
      //
      // Not while the keyboard is in a composer, though: there ⇥ means "next
      // field", and this binding was firing on every stop that is not a text
      // field — cc / bcc, attach, discard, pop out — and throwing the keyboard
      // out of a half-written message into the rail. See `keyboardInComposer`.
      keys: "tab",
      group: "Mail",
      description: "Sidebar or list",
      when: () => mail && !keyboardInComposer(),
      handler: () => actions.toggleFocus(),
    },
    {
      keys: "shift+tab",
      when: () => mail && !keyboardInComposer(),
      handler: () => actions.toggleFocus(),
    },

    /* ---------------------------------------------------------- Actions --- */
    // `c`, `r`, `a` and `f` are registered by <ComposerDock/>, which owns the
    // draft they open. A binding here would have to reach into that state
    // through the shell to do anything useful.
    //
    // The rest live in `mail-bindings.ts` as data. They are spread in place, so
    // the help sheet still prints them here, between the movement keys and the
    // selection keys — see that file for why they were moved at all.
    ...mailActionBindings(
      { mail: () => mail, active: () => active },
      {
        archive: () => actions.archiveSelected(),
        openSnooze: () => actions.setSnooze(true),
        star: () => actions.starSelected(),
        trash: () => actions.trashSelected(),
        favorite: () => actions.toggleFavoriteFocused(),
        undo: () => actions.undo(),
      },
    ),

    /* -------------------------------------------------------- Selection --- */
    /* `x` is the Gmail/Superhuman verb and the one his hands already know;
       ⇧J/⇧K are the same movement keys with the range dragged along behind
       them. Gmail builds its bulk selection out of `*` sequences instead —
       `* a`, `* u` — which this registry cannot spend, because `*` is its
       match-anything token. ⌘A and a moving cursor cover the same ground. */
    {
      keys: "x",
      group: "Selection",
      description: "Select",
      when: () => active,
      handler: () => actions.toggleAtCursor(),
    },
    {
      keys: "shift+j",
      group: "Selection",
      description: "Extend down",
      when: () => active,
      handler: () => actions.extendCursor(1),
    },
    {
      keys: "shift+k",
      group: "Selection",
      description: "Extend up",
      when: () => active,
      handler: () => actions.extendCursor(-1),
    },
    {
      // Safe to claim: `globals.css` puts `user-select: none` on the chrome, so
      // the WebView's own select-all has nothing to select here. It is still
      // preventDefault-ed (the registry does that by default) so the WebView
      // does not try anyway.
      keys: "mod+a",
      group: "Selection",
      description: "Select everything loaded",
      when: () => active,
      handler: () => actions.selectAllThreads(),
    },
    /* Two Escapes, mutually exclusive so the registry sees no conflict: with a
       selection up it drops the selection, otherwise it closes the thread. */
    {
      keys: "escape",
      group: "Selection",
      description: "Clear the selection",
      priority: 10,
      when: () => active && selecting,
      handler: () => actions.clearSelection(),
    },
    {
      keys: "escape",
      priority: 10,
      when: () => active && !selecting && ui.threadId !== null,
      handler: () => actions.closeThread(),
    },

    /* --------------------------------------------------------- Accounts --- */
    /*
     * ⌃1–5 filters to an account, ⌃0 clears the filter.
     *
     * ⌘ picks the surface (⌘1 mail, ⌘2 calendar) and ⌃ picks the account —
     * one modifier per axis, which is the only version of this anyone can
     * remember. Not ⌥: Option is the text-entry modifier, and ⌥e in the
     * composer is the start of an accented character, not a shortcut. (It was
     * also silently broken until `tokenFromEvent` learned to read `event.code`,
     * because macOS turns ⌥1 into `¡` before the app ever sees it.)
     *
     * Gmail has no equivalent — it has no second account — so nothing here is
     * a divergence, it is a whole axis Gmail does not have.
     */
    {
      keys: "ctrl+0",
      group: "Accounts",
      description: "All accounts",
      when: () => mail,
      handler: () => dispatch({ type: "account", accountId: null }),
    },
    ...accounts.map((account, index) => ({
      keys: `ctrl+${index + 1}`,
      group: "Accounts",
      description: account.name,
      when: () => mail,
      handler: () => dispatch({ type: "account", accountId: account.id }),
    })),
  ]);

  return (
    <div className="flex h-full min-h-0">
      <AccountRail />
      <Pane width={ui.listWidth}>
        {/* Search is a mode of this pane, not a screen of its own: it takes the
            list over while a query is live and hands it straight back when the
            query goes away. Its own keys (`/`, ⌘F) live inside it. */}
        <SearchView>
          <ThreadList />
        </SearchView>
      </Pane>
      <Resizer
        size={ui.listWidth}
        onResize={onResize}
        min={LIST_WIDTH_BOUNDS.min}
        max={LIST_WIDTH_BOUNDS.max}
        label="Conversation list width"
      />
      {/* Marked because two boxes share this column and its height is the only
          number that settles how much each may have — see `composer-layout`. */}
      <FlexPane {...{ [READING_COLUMN]: "" }}>
        {/* The composer grows at the bottom of the conversation it answers, so
            the message being replied to never leaves the screen. */}
        <ReadingPane />
        <ComposerDock />
      </FlexPane>
      {/* Renders nothing until `b` opens it. Mounted here rather than beside
          the palette in `App` because it acts on the thread list's selection
          and has no meaning in the calendar. */}
      <SnoozePicker />
    </div>
  );
}
