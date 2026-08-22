import { useCallback } from "react";
import { overlayOwnsKeyboard, useMach } from "@/hooks/useMach";
import { useKeyBindings } from "@/hooks/useKeymap";
import { keyboardInComposer } from "@/lib/compose";
import { LIST_WIDTH_BOUNDS } from "@/lib/prefs";
import { FlexPane, Pane, Resizer } from "@/components/ui/split";
import { AccountRail } from "./AccountRail";
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
      alsoKeys: ["down"],
      group: "Mail",
      description: "Next conversation",
      when: () => active,
      handler: () => actions.moveCursor(1),
    },
    {
      keys: "k",
      alsoKeys: ["up"],
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
      alsoKeys: ["o"],
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
      alsoKeys: ["shift+tab"],
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
        reportSpam: () => actions.reportSpamSelected(),
        unsubscribe: () => actions.unsubscribe(),
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
      alsoKeys: ["shift+k"],
      group: "Selection",
      description: "Extend the selection",
      when: () => active,
      handler: () => actions.extendCursor(1),
    },
    {
      keys: "shift+k",
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
     * `mod2`1–5 filters to an account, `mod2`0 clears the filter — ⌃ on macOS,
     * Alt everywhere else.
     *
     * `mod` picks the surface (⌘1 mail, ⌘2 calendar) and `mod2` picks the
     * account — one modifier per axis, which is the only version of this anyone
     * can remember. The axes are the same on both platforms; which key carries
     * the second one is not, and cannot be.
     *
     * # macOS: ⌃, and not ⌥
     *
     * Option is the text-entry modifier there, and ⌥e in the composer is the
     * start of an accented character, not a shortcut. (⌥ was also silently
     * broken until `tokenFromEvent` learned to read `event.code`, because macOS
     * turns ⌥1 into `¡` before the app ever sees it.) ⌘ and ⌃ are unrelated
     * chords, so ⌃ costs nothing.
     *
     * # Linux: ⌥, because ⌃ is already `mod`
     *
     * `detectModKey` answers `ctrl` off a Mac, so there ⌃1 is `mod+1` — the two
     * axes collapse onto one chord. They did: both bindings registered, the
     * account one registered later, ties go to the last registration, and ⌃1
     * filtered to the first account instead of showing Mail. ⌘1 and ⌃1 being
     * different keys is a fact about Macs, not about this app.
     *
     * Alt is free of the reason it was refused above. It is not a compose
     * modifier under a default xkb layout — Alt+1 on `us` produces no character
     * at all, where ⌥1 on a Mac produces `¡` — so nothing in the composer is
     * spent by taking it. It is also free of the compositor: Hyprland's own
     * bindings are Super's, and its only bare-Alt chord is Alt+Tab.
     *
     * What Alt+1–5 does cost on Linux is the composer's tab strip, which binds
     * the same digits at priority 110 while two or more drafts are open. That
     * precedence stands and is the same one the calendar's solo keys already
     * accept: while you are writing a message the number row is about
     * composers. See `ComposerDock`, and `CalendarMode`'s ⌥ block for the other
     * half of it — the calendar's are gated on calendar mode and these on mail,
     * so those two never meet.
     *
     * Gmail has no equivalent — it has no second account — so nothing here is
     * a divergence, it is a whole axis Gmail does not have.
     */
    {
      keys: "mod2+0",
      group: "Accounts",
      description: "All accounts",
      when: () => mail,
      handler: () => dispatch({ type: "account", accountId: null }),
    },
    ...accounts.map((account, index) => ({
      keys: `mod2+${index + 1}`,
      group: "Accounts",
      description: account.name,
      when: () => mail,
      handler: () => dispatch({ type: "account", accountId: account.id }),
    })),
  ]);

  return (
    <div className="flex h-full min-h-0">
      <AccountRail />
      <Pane width={ui.listWidth} className="bg-background">
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
      <FlexPane>
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
