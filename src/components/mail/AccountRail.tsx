import {
  Archive,
  Bookmark,
  Clock,
  FileText,
  Inbox,
  Mail,
  Plus,
  Send,
  Star,
  Tag,
  Trash2,
  TriangleAlert,
  X,
  type LucideIcon,
} from "lucide-react";
import { Fragment, useEffect, useMemo, useRef } from "react";
import type { AccountId } from "@/types";
import { useMach } from "@/hooks/useMach";
import { useKeyBindings } from "@/hooks/useKeymap";
import { ACCOUNT_BG } from "@/lib/colors";
import { favoriteKey } from "@/lib/favorites";
import { railMailboxes } from "@/lib/mailboxes";
import { cn } from "@/lib/utils";
import { ScrollArea } from "@/components/ui/scroll-area";

const SYSTEM_ICONS: Record<string, LucideIcon> = {
  INBOX: Inbox,
  STARRED: Star,
  SNOOZED: Clock,
  DRAFT: FileText,
  SENT: Send,
  ARCHIVE: Archive,
  SPAM: TriangleAlert,
  TRASH: Trash2,
};

interface RailItem {
  key: string;
  /** Heading rendered immediately above this row, if it opens a section. */
  section?: string;
  active: boolean;
  activate: () => void;
  leading: React.ReactNode;
  label: string;
  count?: number;
  title?: string;
  onRemove?: () => void;
  removeTitle?: string;
}

/**
 * The rail is a list, and lists are navigable.
 *
 * Every row here is reachable with the keyboard: Tab moves the keyboard into
 * the rail, `j`/`k` walk it, Enter picks a mailbox and hands the keyboard back
 * to the list, Escape hands it back without changing anything. ⌘K still jumps
 * to any mailbox by name — this is the path for the times you know where the
 * row is on screen and do not want to open a dialog to reach it.
 *
 * The rows are built as data first so the keyboard and the pointer walk exactly
 * the same list, in exactly the same order, including favorites and anything
 * below the fold.
 */
export function AccountRail() {
  const { accounts, labels, allThreads, ui, favorites, dispatch, actions, isUnread } = useMach();
  const scroller = useRef<HTMLDivElement>(null);

  const unreadByAccount = useMemo(() => {
    const counts = new Map<AccountId, number>();
    for (const thread of allThreads) {
      if (!isUnread(thread) || !thread.labelIds.includes("INBOX")) continue;
      counts.set(thread.accountId, (counts.get(thread.accountId) ?? 0) + 1);
    }
    return counts;
  }, [allThreads, isUnread]);

  const totalUnread = useMemo(
    () => [...unreadByAccount.values()].reduce((sum, n) => sum + n, 0),
    [unreadByAccount],
  );

  // The rail carries the mailboxes you navigate to, not every label Gmail has.
  // Labels live in ⌘K, and the ones worth a permanent row you favorite.
  const mailboxes = useMemo(() => railMailboxes(labels), [labels]);

  const items = useMemo<RailItem[]>(() => {
    const rows: RailItem[] = [
      {
        key: "account:all",
        section: "Accounts",
        active: ui.accountId === null,
        activate: () => dispatch({ type: "account", accountId: null }),
        leading: <span className="h-1.5 w-1.5 rounded-full bg-foreground" />,
        label: "All accounts",
        count: totalUnread,
      },
      ...accounts.map((account) => ({
        key: `account:${account.id}`,
        active: ui.accountId === account.id,
        activate: () => dispatch({ type: "account", accountId: account.id }),
        leading: (
          <span className={cn("h-1.5 w-1.5 rounded-full", ACCOUNT_BG[account.colorIndex])} />
        ),
        // The full address, not the local part. With five accounts the bit
        // before the @ is often identical across them, which makes the rail
        // ambiguous exactly when it matters most.
        label: account.email,
        count: unreadByAccount.get(account.id) ?? 0,
        title: account.email,
      })),
      ...favorites.map((favorite, index) => {
        const key = favoriteKey(favorite);
        return {
          key: `favorite:${key}`,
          section: index === 0 ? "Favorites" : undefined,
          active:
            favorite.kind === "mailbox"
              ? ui.labelId === favorite.labelId && ui.accountId === favorite.accountId
              : ui.threadId === favorite.threadId,
          activate: () => actions.openFavorite(favorite),
          leading:
            favorite.kind === "thread" ? (
              <Mail size={13} strokeWidth={1.75} className="text-faint-foreground" />
            ) : (
              <Bookmark size={13} strokeWidth={1.75} className="text-faint-foreground" />
            ),
          label: favorite.name,
          title: favorite.name,
          onRemove: () => actions.unfavorite(key),
          removeTitle: `Remove ${favorite.name} from favorites`,
        };
      }),
      ...mailboxes.map((label, index) => {
        const Icon = SYSTEM_ICONS[label.id] ?? Tag;
        return {
          key: `mailbox:${label.id}`,
          section: index === 0 ? "Mailboxes" : undefined,
          active: ui.labelId === label.id,
          activate: () => dispatch({ type: "label", labelId: label.id }),
          leading: <Icon size={13} strokeWidth={1.75} className="text-faint-foreground" />,
          label: label.name,
        };
      }),
      {
        key: "add-account",
        section: "Account",
        active: false,
        activate: () => dispatch({ type: "addAccount", open: true }),
        leading: <Plus size={13} strokeWidth={1.75} className="text-faint-foreground" />,
        label: "Add account",
      },
    ];
    return rows;
  }, [
    accounts,
    favorites,
    mailboxes,
    totalUnread,
    unreadByAccount,
    ui.accountId,
    ui.labelId,
    ui.threadId,
    dispatch,
    actions,
  ]);

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
      keys: "enter",
      group: "Sidebar",
      description: "Open, and hand the keyboard back to the list",
      when: () => railActive,
      handler: () => {
        items[focusedIndex]?.activate();
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
      role="listbox"
      aria-label="Mailboxes"
      className="w-rail flex-none border-r border-border bg-surface"
    >
      {items.map((item, index) => (
        <Fragment key={item.key}>
          {item.section && (
            <SectionLabel className={index === 0 ? undefined : "mt-3"}>
              {item.section}
            </SectionLabel>
          )}
          <RailRow item={item} index={index} focused={index === focusedIndex} />
        </Fragment>
      ))}
      <div className="h-3" />
    </ScrollArea>
  );
}

function SectionLabel({ children, className }: { children: React.ReactNode; className?: string }) {
  return (
    <div
      className={cn(
        "px-3 pb-1 pt-2 text-micro font-medium uppercase tracking-[0.06em] text-faint-foreground",
        className,
      )}
    >
      {children}
    </div>
  );
}

function RailRow({
  item,
  index,
  focused,
}: {
  item: RailItem;
  index: number;
  focused: boolean;
}) {
  const { active, label, count, title, leading, onRemove, removeTitle } = item;
  return (
    // The row is a button, so the unpin control cannot be nested inside it —
    // it sits alongside, and the group hover is what ties them together.
    <div
      className={cn(
        "group relative flex h-7 w-full items-center",
        active ? "bg-surface-raised" : "hover:bg-row-hover",
        // Focused and active are different facts — where the keyboard is, and
        // which mailbox you are in — so they get different marks: an outline
        // for the one, a fill and an edge for the other.
        focused && "ring-1 ring-inset ring-accent",
      )}
    >
      {active && <span className="absolute inset-y-0 left-0 w-[2px] bg-accent" />}
      <button
        type="button"
        data-rail-index={index}
        role="option"
        aria-selected={active}
        tabIndex={focused ? 0 : -1}
        onClick={item.activate}
        title={title}
        className={cn(
          "flex h-full min-w-0 flex-1 items-center gap-2 pl-3 pr-2 text-list outline-none",
          active ? "text-foreground" : "text-muted-foreground group-hover:text-foreground",
        )}
      >
        <span className="flex w-3.5 shrink-0 justify-center">{leading}</span>
        <span className="min-w-0 flex-1 truncate text-left">{label}</span>
        {count ? (
          <span className="shrink-0 font-mono text-micro tabular-nums text-faint-foreground">
            {count}
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
          className="mr-1.5 hidden h-4 w-4 shrink-0 items-center justify-center rounded-[var(--radius)] text-faint-foreground hover:text-foreground group-hover:flex"
        >
          <X size={11} strokeWidth={2} />
        </button>
      )}
    </div>
  );
}
