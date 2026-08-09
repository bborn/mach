import { ChevronDown, ChevronRight, X } from "lucide-react";
import { useEffect, useMemo, useRef } from "react";
import type { ThreadId } from "@/types";
import { useMach } from "@/hooks/useMach";
import { useKeyBindings } from "@/hooks/useKeymap";
import { useUiSession } from "@/components/prefs/PreferencesProvider";
import { railMailboxes } from "@/lib/mailboxes";
import { cn } from "@/lib/utils";
import { ScrollArea } from "@/components/ui/scroll-area";
import { railItems, railStep, type RailItem, type RailSection } from "./rail-model";
import { useInboxUnread } from "./use-inbox-unread";

/**
 * The rail is a tree, and trees are navigable.
 *
 * Every row here is reachable with the keyboard: Tab moves the keyboard into
 * the rail, `j`/`k` walk it, `←`/`→` fold and unfold the sections and step
 * between a heading and its children, Enter picks a mailbox and hands the
 * keyboard back to the list, Escape hands it back without changing anything.
 * ⌘K still jumps to any mailbox by name, and ⌃1–5 still filters to an account
 * without leaving the mailbox you are in — the rail's account rows are the
 * other gesture, "that account's inbox", which is what nesting them promises.
 *
 * The rows themselves are built in `rail-model.tsx`; this is the wiring.
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

  /*
   * Anything the UI has already taken out of the inbox or marked read, but that
   * the backend has not confirmed yet. The badge has to fall with the rows, not
   * a round trip later — an archive-everything gesture that empties the list
   * while the rail still claims fifty unread is the exact lie this guards.
   */
  const suppressed = useMemo<ReadonlySet<ThreadId>>(
    () => new Set<ThreadId>([...ui.archived, ...ui.readExtra]),
    [ui.archived, ui.readExtra],
  );
  const unread = useInboxUnread(suppressed);

  // The rail carries the mailboxes you navigate to, not every label Gmail has.
  // Labels live in ⌘K, and the ones worth a permanent row you favorite. Inbox
  // is not among them any more: it is the section these sit under.
  const mailboxes = useMemo(
    () => railMailboxes(labels).filter((label) => label.id !== "INBOX"),
    [labels],
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
          threadId: ui.threadId,
          unread,
          collapsed,
        },
        {
          open: (accountId, labelId) => {
            dispatch({ type: "account", accountId });
            dispatch({ type: "label", labelId });
          },
          openLabel: (labelId) => dispatch({ type: "label", labelId }),
          openCalendar: () => actions.setMode("calendar"),
          openFavorite: (favorite) => actions.openFavorite(favorite),
          unfavorite: (key) => actions.unfavorite(key),
          toggle: toggleSection,
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
      collapsed,
      ui.accountId,
      ui.labelId,
      ui.threadId,
      dispatch,
      actions,
    ],
  );

  const railActive = ui.mode === "mail" && !ui.paletteOpen && ui.focus === "rail";

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
      group: "Sidebar",
      description: "Next mailbox",
      when: () => railActive,
      handler: () => move(1),
    },
    {
      keys: "k",
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
      description: "Fold the section, or step out to it",
      when: () => railActive,
      handler: () => step("out"),
    },
    {
      keys: "right",
      group: "Sidebar",
      description: "Unfold the section, or step into it",
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
      description: "Open, and hand the keyboard back to the list",
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
  ]);

  // `flex-none` overrides ScrollArea's own `flex-1`: the rail is a fixed
  // column, not a stretching one.
  return (
    <ScrollArea
      ref={scroller}
      role="tree"
      aria-label="Mailboxes"
      className="w-rail flex-none border-r border-border bg-surface py-1"
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
  const { active, label, count, countSuffix, title, leading, onRemove, removeTitle } = item;
  const surface = item.surface === true;
  return (
    // The row is a button, so the unpin control cannot be nested inside it —
    // it sits alongside, and the group hover is what ties them together. The
    // disclosure is a third button for the same reason, and because folding a
    // section and navigating into it are different intents.
    <div
      className={cn("group relative flex h-7 w-full items-center px-1", item.spaced && "mt-2")}
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
        title={title}
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
            active ? "text-accent" : surface ? "text-muted-foreground" : undefined,
          )}
        >
          {leading}
        </span>
        <span className="min-w-0 flex-1 truncate">{label}</span>
        {count ? (
          <span
            className={cn(
              "shrink-0 font-mono text-micro tabular-nums",
              active ? "text-foreground" : "text-muted-foreground",
            )}
          >
            {count}
            {countSuffix}
          </span>
        ) : null}
      </button>
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
