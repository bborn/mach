import { ChevronDown, ChevronRight, X } from "lucide-react";
import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { ThreadId } from "@/types";
import { overlayOwnsKeyboard, useMach } from "@/hooks/useMach";
import { useKeyBindings } from "@/hooks/useKeymap";
import { useUiSession } from "@/components/prefs/PreferencesProvider";
import { inboxLabelId, railMailboxes } from "@/lib/mailboxes";
import { RAIL_WIDTH_BOUNDS } from "@/lib/prefs";
import { suppressedIds } from "@/lib/projection";
import { cn } from "@/lib/utils";
import { ScrollArea } from "@/components/ui/scroll-area";
import { ShortcutTooltip } from "@/components/ui/tooltip";
import { RESIZE_STEP, Resizer } from "@/components/ui/split";
import { DEFAULT_RAIL_WIDTH, clampRailWidth } from "./rail-layout";
import { railItems, railStep, type RailItem, type RailSection } from "./rail-model";
import { useInboxUnread } from "./use-inbox-unread";
import { useUnsent } from "./use-unsent";

/**
 * The rail is a tree, and trees are navigable.
 *
 * Every row here is reachable with the keyboard: Tab moves the keyboard into
 * the rail, `j`/`k` walk it, `←`/`→` fold and unfold the sections and step
 * between a heading and its children, Enter picks a mailbox and hands the
 * keyboard back to the list, Escape hands it back without changing anything.
 * ⌘K still jumps to any mailbox by name, and ⌃1–5 (Alt+1–5 off a Mac; see the
 * Accounts block in `MailMode`) still filters to an account
 * without leaving the mailbox you are in — the rail's account rows are the
 * other gesture, "that account's inbox", which is what nesting them promises.
 *
 * The rows themselves are built in `rail-model.tsx`; this is the wiring.
 *
 * The handle on its right edge is the same `Resizer` the conversation-list
 * divider, the agent drawer and the composer use, sized by the same shape:
 * an unclamped choice in state, `rail-layout.ts` deciding what it means, and
 * `uiSession` remembering it. It is returned as a sibling of the rail rather
 * than drawn inside it, so it lands between the two columns of `MailMode`'s
 * flex row.
 */
export function AccountRail() {
  const { accounts, labels, ui, favorites, dispatch, actions } = useMach();
  const scroller = useRef<HTMLDivElement>(null);

  /*
   * Which sections are folded — remembered across launches.
   *
   * Session state, not a setting: nobody would look for it in ⌘,, and the app
   * simply ought to be where you left it. The calendar sidebar already keeps
   * its folded groups here; this is the same mechanism, not a second one.
   */
  const { session, remember } = useUiSession();
  const collapsed = session.collapsedRailSections ?? [];
  const toggleSection = (section: RailSection) =>
    remember({
      collapsedRailSections: collapsed.includes(section)
        ? collapsed.filter((id) => id !== section)
        : [...collapsed, section],
    });

  /* ------------------------------------------------------------- the width */

  const [chosenWidth, setChosenWidth] = useState(DEFAULT_RAIL_WIDTH);
  const restoredWidth = useRef(false);

  // The stored width lands a tick after mount, like the rest of the session,
  // and only the first one counts: `remember` writes back into the same object,
  // so a second restore would fight every drag.
  const storedWidth = session.railWidth;
  useEffect(() => {
    if (restoredWidth.current || storedWidth === undefined) return;
    restoredWidth.current = true;
    setChosenWidth(storedWidth);
  }, [storedWidth]);

  const width = clampRailWidth(chosenWidth);

  /*
   * `--rail-width` is the rail's width, so the rail is what sets it.
   *
   * The token was a constant in `globals.css` and is now the live value, which
   * is what keeps the toast where it belongs: it is `fixed`, mounted in `App`
   * outside this tree, and parks itself where the rail ends. A prop could not
   * reach it. `w-rail` on the scroller below reads the same token, so this is
   * also what paints the rail, and there is one copy of the number rather than
   * two that can disagree.
   */
  useLayoutEffect(() => {
    document.documentElement.style.setProperty("--rail-width", `${width}px`);
  }, [width]);

  /** Mid-drag: move it, and leave the store alone until the pointer is up. */
  const resize = useCallback((next: number) => setChosenWidth(clampRailWidth(next)), []);

  /**
   * A width somebody has finished choosing — the pointer released, a key
   * pressed, the divider double-clicked. The only one of the two that writes.
   */
  const setWidth = useCallback(
    (next: number) => {
      const clamped = clampRailWidth(next);
      setChosenWidth(clamped);
      remember({ railWidth: clamped });
    },
    [remember],
  );

  /*
   * Anything the UI has already taken out of the inbox or marked read, but that
   * the backend has not confirmed yet. The badge has to fall with the rows, not
   * a round trip later — an archive-everything gesture that empties the list
   * while the rail still claims fifty unread is the exact lie this guards.
   */
  const suppressed = useMemo<ReadonlySet<ThreadId>>(
    () => suppressedIds(ui.guesses, "INBOX"),
    [ui.guesses],
  );
  const inboxId = inboxLabelId();
  const unread = useInboxUnread(suppressed, inboxId);
  const { failed } = useUnsent();

  // The rail carries the mailboxes you navigate to, not every label Gmail has.
  // Labels live in ⌘K, and the ones worth a permanent row you favorite. Inbox
  // is not among them any more: it is the section these sit under. When Gmail
  // has tabs, that heading is Primary and `INBOX` stays in the folders as All.
  const mailboxes = useMemo(
    () => railMailboxes(labels).filter((label) => label.id !== inboxId),
    [labels, inboxId],
  );

  const items = useMemo<RailItem[]>(
    () =>
      railItems(
        {
          accounts,
          mailboxes,
          favorites,
          accountId: ui.accountId,
          labelId: ui.labelId,
          inboxId,
          threadId: ui.threadId,
          unread,
          unsent: failed.length,
          collapsed,
        },
        {
          open: (accountId, labelId) => {
            dispatch({ type: "account", accountId });
            dispatch({ type: "label", labelId });
          },
          openLabel: (labelId) => dispatch({ type: "label", labelId }),
          openFavorite: (favorite) => actions.openFavorite(favorite),
          unfavorite: (key) => actions.unfavorite(key),
          toggle: toggleSection,
          openUnsent: () => actions.setUnsent(true),
        },
      ),
    // `toggleSection` is rebuilt every render but closes over nothing except
    // `collapsed` and the store's own setter, so `collapsed` is the dependency
    // that actually decides both which rows exist and what folding one does.
    [
      accounts,
      mailboxes,
      favorites,
      unread,
      failed.length,
      collapsed,
      ui.accountId,
      ui.labelId,
      inboxId,
      ui.threadId,
      dispatch,
      actions,
    ],
  );

  const railActive = ui.mode === "mail" && !ui.paletteOpen && ui.focus === "rail";
  // Wider than `railActive`: the rail's width is a property of the mail
  // surface, not of where the cursor happens to be standing in it.
  const railVisible = ui.mode === "mail" && !overlayOwnsKeyboard(ui);

  // `-1` in state means "wherever the current mailbox is", so arriving in the
  // rail puts the cursor on the row you are already looking at rather than on
  // whatever it happened to be three mailboxes ago.
  const activeIndex = Math.max(
    items.findIndex((item) => item.active),
    0,
  );
  const focusedIndex = railActive
    ? ui.railIndex === -1
      ? activeIndex
      : Math.min(Math.max(ui.railIndex, 0), items.length - 1)
    : -1;

  /*
   * The cursor is real DOM focus, not a painted imitation.
   *
   * That is what makes the row announce itself to VoiceOver, what gets the
   * scroll-into-view for free, and what keeps Enter working if the keymap ever
   * declines the key. Leaving the rail blurs it, so a focus ring is never left
   * glowing on a pane the keyboard has walked away from.
   */
  useEffect(() => {
    const root = scroller.current;
    if (!root) return;
    if (focusedIndex < 0) {
      const focused = document.activeElement;
      if (focused instanceof HTMLElement && root.contains(focused)) focused.blur();
      return;
    }
    const row = root.querySelector<HTMLElement>(`[data-rail-index="${focusedIndex}"]`);
    row?.focus({ preventScroll: true });
    row?.scrollIntoView({ block: "nearest" });
  }, [focusedIndex]);

  const move = (delta: number) => {
    if (items.length === 0) return;
    const next = Math.min(Math.max(focusedIndex + delta, 0), items.length - 1);
    dispatch({ type: "railIndex", index: next });
  };

  const step = (direction: "in" | "out") => {
    const outcome = railStep(items, focusedIndex, direction);
    if (outcome.kind === "toggle") toggleSection(outcome.section);
    else if (outcome.kind === "move") dispatch({ type: "railIndex", index: outcome.index });
  };

  const focused = items[focusedIndex];

  useKeyBindings([
    {
      keys: "j",
      alsoKeys: ["down"],
      group: "Sidebar",
      description: "Next mailbox",
      when: () => railActive,
      handler: () => move(1),
    },
    {
      keys: "k",
      alsoKeys: ["up"],
      group: "Sidebar",
      description: "Previous mailbox",
      when: () => railActive,
      handler: () => move(-1),
    },
    { keys: "down", when: () => railActive, handler: () => move(1) },
    { keys: "up", when: () => railActive, handler: () => move(-1) },
    {
      keys: "left",
      group: "Sidebar",
      description: "Fold, or step out",
      when: () => railActive,
      handler: () => step("out"),
    },
    {
      keys: "right",
      group: "Sidebar",
      description: "Unfold, or step in",
      when: () => railActive,
      handler: () => step("in"),
    },
    {
      // Space is the platform's "operate the thing under the cursor" and costs
      // nothing here: the rail is not a text surface and does not scroll by page.
      // Declined on a row with nothing to fold, so it falls through rather than
      // being swallowed.
      keys: "space",
      when: () => railActive,
      handler: () => {
        if (!focused?.section) return false;
        toggleSection(focused.section);
      },
    },
    {
      keys: "enter",
      group: "Sidebar",
      description: "Open, and back to the list",
      when: () => railActive,
      handler: () => {
        // A heading that only folds has nowhere to send you, so Enter folds it
        // and the keyboard stays where it is rather than being handed to a list
        // that did not change.
        if (!focused) return;
        if (!focused.activate) {
          if (focused.section) toggleSection(focused.section);
          return;
        }
        focused.activate();
        actions.setFocus("list");
      },
    },
    {
      keys: "escape",
      group: "Sidebar",
      description: "Back to the list",
      priority: 10,
      when: () => railActive,
      handler: () => actions.setFocus("list"),
    },
    /*
     * The keyboard's own route to the divider.
     *
     * The handle is a focus stop and answers ← → once it has focus, but ⇥ is
     * spent on the rail-and-list loop, so focus is not a keystroke away from
     * most places in mail. These are the same two keys the agent drawer and
     * the composer use for their own dividers, turned through ninety degrees —
     * one gesture for "give this pane more room", learned once. `←`/`→` on
     * their own already fold and step the tree and cannot be spent twice.
     */
    {
      keys: "mod+alt+right",
      group: "Sidebar",
      description: "Wider",
      allowInInput: true,
      when: () => railVisible,
      handler: () => setWidth(width + RESIZE_STEP),
    },
    {
      keys: "mod+alt+left",
      group: "Sidebar",
      description: "Narrower",
      allowInInput: true,
      when: () => railVisible,
      handler: () => setWidth(width - RESIZE_STEP),
    },
  ]);

  // `flex-none` overrides ScrollArea's own `flex-1`: the rail is a column of a
  // width somebody chose, not a stretching one. `w-rail` reads `--rail-width`,
  // which the layout effect above keeps at that width.
  return (
    <>
      <ScrollArea
        ref={scroller}
        role="tree"
        aria-label="Mailboxes"
        className="w-rail flex-none border-r border-border bg-surface"
      >
        {items.map((item, index) => {
          const section = item.section;
          return (
            <RailRow
              key={item.key}
              item={item}
              index={index}
              focused={index === focusedIndex}
              onToggle={section ? () => toggleSection(section) : undefined}
            />
          );
        })}
        <div className="h-3" />
      </ScrollArea>
      {/* The pane is to its left, so it straddles the border the rail already
          draws rather than adding a second line beside it. */}
      <Resizer
        size={width}
        onResize={resize}
        onCommit={setWidth}
        onReset={() => setWidth(DEFAULT_RAIL_WIDTH)}
        min={RAIL_WIDTH_BOUNDS.min}
        max={RAIL_WIDTH_BOUNDS.max}
        label="Sidebar width"
      />
    </>
  );
}

export function RailRow({
  item,
  index,
  focused,
  onToggle,
}: {
  item: RailItem;
  index: number;
  focused: boolean;
  onToggle?: () => void;
}) {
  const { active, label, count, countSuffix, title, shortcut, leading, onRemove, removeTitle } =
    item;
  const surface = item.surface === true;
  const warning = item.tone === "warning";
  const rowButton = (
      <button
        type="button"
        data-rail-index={index}
        data-rail-key={item.key}
        role="treeitem"
        aria-level={item.level}
        aria-selected={active}
        aria-expanded={item.section ? item.expanded : undefined}
        tabIndex={focused ? 0 : -1}
        onClick={item.activate ?? onToggle}
        className={cn(
          // Every row is a row, including the ones that head a section. A
          // section heading drawn as a tiny uppercase label reads as a caption,
          // and a *folded* one reads as a caption with nothing under it — which
          // looks broken rather than folded. Weight carries the distinction
          // instead: surfaces heavy, groupings quiet.
          "z-10 flex h-full min-w-0 flex-1 items-center gap-1.5 pl-1 pr-1 text-left text-list outline-none",
          item.level === 2 && "pl-3.5",
          active || surface
            ? "font-medium text-foreground"
            : "text-muted-foreground group-hover:text-foreground",
          // An unread count is the reason to look at the row, so the row leans
          // into it the way Gmail's mailbox list does.
          !active && count ? "font-medium text-foreground" : undefined,
        )}
      >
        <span
          className={cn(
            "flex w-3.5 shrink-0 items-center justify-center",
            warning
              ? "text-warning"
              : active
                ? "text-accent"
                : surface
                  ? "text-muted-foreground"
                  : undefined,
          )}
        >
          {leading}
        </span>
        <span className="min-w-0 flex-1 truncate">{label}</span>
        {count ? (
          <span
            className={cn(
              "shrink-0 font-mono text-micro tabular-nums",
              warning ? "text-warning" : active ? "text-foreground" : "text-muted-foreground",
            )}
          >
            {count}
            {countSuffix}
          </span>
        ) : null}
      </button>
  );
  return (
    // The row is a button, so the unpin control cannot be nested inside it —
    // it sits alongside, and the group hover is what ties them together. The
    // disclosure is a third button for the same reason, and because folding a
    // section and navigating into it are different intents.
    <div
      className={cn(
        "group relative flex w-full items-center px-1",
        // Inbox sits on the same 32px band as the list header, so the two
        // columns share a top edge under the title bar. Folder rows stay a
        // step tighter.
        surface ? "h-8" : "h-7",
        item.spaced && "mt-2",
      )}
    >
      <div
        className={cn(
          // Edge to edge, not an inset pill: the rail is a column of places and
          // the selected one is the whole width of the column. Selected and
          // focused are different facts — which mailbox you are in, and where
          // the keyboard is — so they get different marks.
          "pointer-events-none absolute inset-0",
          active && "bg-row-selected",
          focused && "ring-1 ring-inset ring-accent",
        )}
      />
      {onToggle ? (
        <button
          type="button"
          // Not in the tab order and not a rail index: `←`/`→` operate it, and a
          // second stop per section would double the length of the walk.
          tabIndex={-1}
          aria-label={`${item.expanded ? "Collapse" : "Expand"} ${label}`}
          onClick={onToggle}
          className="z-10 flex h-full w-3.5 shrink-0 items-center justify-center text-muted-foreground hover:text-foreground"
        >
          {item.expanded ? (
            <ChevronDown size={11} strokeWidth={2} />
          ) : (
            <ChevronRight size={11} strokeWidth={2} />
          )}
        </button>
      ) : (
        <span className="w-3.5 shrink-0" />
      )}
      {shortcut || title ? (
        <ShortcutTooltip label={title ?? label} keys={shortcut} side="right">
          {rowButton}
        </ShortcutTooltip>
      ) : (
        rowButton
      )}
      {onRemove && (
        <button
          type="button"
          onClick={onRemove}
          title={removeTitle}
          aria-label={removeTitle}
          tabIndex={-1}
          className="z-10 mr-1 hidden h-4 w-4 shrink-0 items-center justify-center rounded-[var(--radius)] text-faint-foreground hover:text-foreground group-hover:flex"
        >
          <X size={11} strokeWidth={2} />
        </button>
      )}
    </div>
  );
}
