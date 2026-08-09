/**
 * The rail, as data.
 *
 * The rows are built here rather than in the JSX so the keyboard and the
 * pointer walk exactly the same list, in exactly the same order — and so that
 * folding a section removes rows from both at once rather than from one of
 * them. It also makes the shape of the rail something a test can assert on
 * without a DOM, which matters more than usual here: the standing rule is that
 * everything is keyboard navigable, and a `<div onClick>` that renders
 * identically is the easiest way in the world to lose that quietly.
 *
 * The structure is Spark's. Spark answered the question Mach was getting
 * wrong: an inbox is a *place*, and the accounts are filters within it. The old
 * rail listed ACCOUNTS and MAILBOXES as two flat, unrelated groups, which made
 * "alex@lumen.example" look like a destination of the same kind as "Sent" and
 * left the reader to work out that picking one changed what the other meant.
 * Nesting the accounts under Inbox says it outright.
 */

import {
  Archive,
  Bookmark,
  Calendar,
  Clock,
  FileText,
  Folder,
  Inbox,
  Mail,
  Send,
  Star,
  Tag,
  Trash2,
  TriangleAlert,
  type LucideIcon,
} from "lucide-react";
import type { Account, AccountId, Label, LabelId, ThreadId } from "@/types";
import type { Favorite } from "@/lib/favorites";
import { favoriteKey } from "@/lib/favorites";
import { ACCOUNT_BG } from "@/lib/colors";
import { cn } from "@/lib/utils";
import type { InboxUnread } from "./use-inbox-unread";

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

/**
 * The sections that fold. Their ids are persisted, so they are strings that
 * mean something rather than indices into a list that will be reordered.
 */
export type RailSection = "inbox" | "folders" | "favorites";

export interface RailItem {
  key: string;
  label: string;
  /** 1 is a section heading or a top-level row; 2 is nested under one. */
  level: 1 | 2;
  /** Heads a section: carries the disclosure. */
  heading?: boolean;
  /** A surface — Inbox, Calendar — rather than a grouping. Heavier. */
  surface?: boolean;
  /** The section this row folds, when it has one. */
  section?: RailSection;
  expanded?: boolean;
  active: boolean;
  /** Absent on a row that can only fold. */
  activate?: () => void;
  leading: React.ReactNode;
  count?: number;
  /** `"+"` when the count is a floor rather than a total. */
  countSuffix?: string;
  title?: string;
  onRemove?: () => void;
  removeTitle?: string;
  /** Air above, so the sections read as sections. */
  spaced?: boolean;
}

export interface RailInput {
  accounts: readonly Account[];
  /** Already filtered and named by `railMailboxes`, minus Inbox. */
  mailboxes: readonly Label[];
  favorites: readonly Favorite[];
  accountId: AccountId | null;
  labelId: LabelId;
  threadId: ThreadId | null;
  unread: InboxUnread;
  collapsed: readonly string[];
}

export interface RailHandlers {
  /** Go to a mailbox, optionally narrowing to one account. */
  open: (accountId: AccountId | null, labelId: LabelId) => void;
  openLabel: (labelId: LabelId) => void;
  openCalendar: () => void;
  openFavorite: (favorite: Favorite) => void;
  unfavorite: (key: string) => void;
  toggle: (section: RailSection) => void;
}

export function railItems(input: RailInput, on: RailHandlers): RailItem[] {
  const { accounts, mailboxes, favorites, accountId, labelId, threadId, unread } = input;
  const open = (section: RailSection) => !input.collapsed.includes(section);
  const atInbox = labelId === "INBOX";

  const accountRows: RailItem[] = accounts.map((account) => ({
    key: `account:${account.id}`,
    level: 2,
    // Picking an account under Inbox means that account's inbox, which is what
    // the nesting says it means. ⌃1–5 is the other reading — keep this mailbox,
    // change the account — and both are worth having.
    active: atInbox && accountId === account.id,
    activate: () => on.open(account.id, "INBOX"),
    leading: <span className={cn("h-3.5 w-[3px] rounded-full", ACCOUNT_BG[account.colorIndex])} />,
    // The full address, not the local part. With five accounts the bit before
    // the @ is often identical across them, which makes the rail ambiguous
    // exactly when it matters most.
    label: account.email,
    count: unread.byAccount.get(account.id) ?? 0,
    countSuffix: unread.capped ? "+" : undefined,
    title: account.email,
  }));

  const mailboxRows: RailItem[] = mailboxes.map((label) => {
    const Icon = SYSTEM_ICONS[label.id] ?? Tag;
    return {
      key: `mailbox:${label.id}`,
      level: 2,
      active: labelId === label.id,
      activate: () => on.openLabel(label.id),
      leading: <Icon size={13} strokeWidth={1.75} className="text-faint-foreground" />,
      label: label.name,
    };
  });

  const favoriteRows: RailItem[] = favorites.map((favorite) => {
    const key = favoriteKey(favorite);
    return {
      key: `favorite:${key}`,
      level: 2,
      active:
        favorite.kind === "mailbox"
          ? labelId === favorite.labelId && accountId === favorite.accountId
          : threadId === favorite.threadId,
      activate: () => on.openFavorite(favorite),
      leading:
        favorite.kind === "thread" ? (
          <Mail size={13} strokeWidth={1.75} className="text-faint-foreground" />
        ) : (
          <Bookmark size={13} strokeWidth={1.75} className="text-faint-foreground" />
        ),
      label: favorite.name,
      title: favorite.name,
      onRemove: () => on.unfavorite(key),
      removeTitle: `Remove ${favorite.name} from favorites`,
    };
  });

  /*
   * A folded section still says whether you are inside it.
   *
   * Folding Folders away while sitting in Sent would otherwise leave the rail
   * with no lit row at all and no answer to "where am I" — so the heading
   * inherits the mark from the child it is hiding.
   */
  const carries = (section: RailSection, rows: RailItem[]) =>
    !open(section) && rows.some((row) => row.active);

  return [
    {
      key: "section:inbox",
      level: 1,
      heading: true,
      surface: true,
      section: "inbox",
      expanded: open("inbox"),
      active: (atInbox && accountId === null) || carries("inbox", accountRows),
      activate: () => on.open(null, "INBOX"),
      leading: <Inbox size={13} strokeWidth={1.75} />,
      label: "Inbox",
      count: unread.total,
      countSuffix: unread.capped ? "+" : undefined,
      title: "All accounts",
    },
    ...(open("inbox") ? accountRows : []),
    /*
     * Calendar, a peer of Inbox rather than a tab in the window chrome.
     *
     * This is the one place Spark's structure and Mach's disagreed about what
     * kind of thing the calendar is. Spark's own shortcuts settle it — ⌘⇧1 is
     * Inbox and ⌘⇧2 is Calendar, a matched pair — so the two are surfaces of
     * equal standing, and the rail is where an app states what its surfaces
     * are. Mach spelled the same pair ⌘1/⌘2 but drew it as a segmented control
     * in the title bar, which put the calendar in the furniture rather than in
     * the navigation.
     *
     * It never shows as selected, because the rail is inert in calendar mode —
     * so the way back out is a Mail button in the calendar's own header rather
     * than a row here.
     */
    {
      key: "surface:calendar",
      level: 1,
      surface: true,
      active: false,
      activate: on.openCalendar,
      leading: <Calendar size={13} strokeWidth={1.75} />,
      label: "Calendar",
      title: "Calendar — ⌘2",
    },
    {
      key: "section:folders",
      level: 1,
      heading: true,
      spaced: true,
      section: "folders",
      expanded: open("folders"),
      active: carries("folders", mailboxRows),
      leading: <Folder size={13} strokeWidth={1.75} />,
      label: "Folders",
    },
    ...(open("folders") ? mailboxRows : []),
    ...(favorites.length > 0
      ? [
          {
            key: "section:favorites",
            level: 1 as const,
            heading: true,
            spaced: true,
            section: "favorites" as const,
            expanded: open("favorites"),
            active: carries("favorites", favoriteRows),
            leading: <Bookmark size={13} strokeWidth={1.75} />,
            label: "Favorites",
          },
          ...(open("favorites") ? favoriteRows : []),
        ]
      : []),
  ];
  /*
   * No "Add account" row.
   *
   * The rail is navigation — the places mail lives — and connecting an account
   * is a settings action somebody performs about five times ever. It sat here
   * because it had nowhere else to go; it belongs in Preferences, and ⌘K has
   * carried "Add a Google account" the whole time, so the gesture survives the
   * row leaving. `ui.addAccountOpen` and `<AddAccountDialog/>` are untouched —
   * this removed a row, not a feature.
   */
}

/** What a `←` or `→` press should do, decided without touching the DOM. */
export type RailStep =
  | { kind: "toggle"; section: RailSection }
  | { kind: "move"; index: number }
  | { kind: "none" };

/**
 * `←`/`→` are the tree keys, and they do the two-step every tree does: the
 * first press acts on the node you are on, and only a press that would be a
 * no-op moves you. So `←` folds an open section, and folds *again* by walking
 * out to the heading of the section you are in; `→` unfolds a closed one and
 * steps into it once it is open.
 */
export function railStep(
  items: readonly RailItem[],
  index: number,
  direction: "in" | "out",
): RailStep {
  const item = items[index];
  if (!item) return { kind: "none" };

  if (direction === "out") {
    if (item.section && item.expanded) return { kind: "toggle", section: item.section };
    for (let i = index - 1; i >= 0; i--) {
      if (items[i]?.heading) return { kind: "move", index: i };
    }
    return { kind: "none" };
  }

  if (item.section && !item.expanded) return { kind: "toggle", section: item.section };
  if (item.heading && index + 1 < items.length) return { kind: "move", index: index + 1 };
  return { kind: "none" };
}
