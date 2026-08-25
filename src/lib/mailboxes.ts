/**
 * Which mailboxes belong in the rail, and what they are called.
 *
 * Gmail hands back forty-odd labels per account — `CATEGORY_FORUMS`,
 * `YELLOW_STAR`, `UNREAD`, plus every folder the user ever made — and a rail
 * that lists all of them is a filing cabinet, not a mailbox. The rail carries
 * the handful you navigate to by muscle memory; everything else is reachable
 * from ⌘K, which is the right surface for a long list you search by name
 * rather than scan.
 *
 * Both halves need the same two things — a display name, and a way to tell two
 * labels called "Receipts" apart — so they live here rather than in a
 * component.
 */

import type { AccountId, Label, LabelId, LabelKind } from "@/types";

/**
 * Gmail's Primary tab. When the store has it, Inbox means this rather than
 * every message that happens to carry `INBOX` — Promotions included.
 */
/**
 * Virtual, like Archive. Not Gmail's `CATEGORY_PERSONAL` — asking for that
 * label returns every conversation Google ever filed under Primary, including
 * archived login codes. This id is unknown to an older binary, so it cannot
 * accidentally do that; the current one answers it as inbox minus bulk.
 */
export const PRIMARY_LABEL: LabelId = "PRIMARY";
export const PROMOTIONS_LABEL: LabelId = "CATEGORY_PROMOTIONS";

/** The mailboxes the rail shows, in the order it shows them. */
export const RAIL_ORDER: readonly LabelId[] = [
  "INBOX",
  "STARRED",
  "SNOOZED",
  "DRAFT",
  "SENT",
  "ARCHIVE",
  "SPAM",
  "TRASH",
  PROMOTIONS_LABEL,
];

/**
 * Gmail's bulk tabs. "Move to Inbox" strips these. They are not the same cut
 * as Inbox itself: Gmail's Primary tab still shows Updates- and Forums-stamped
 * mail (Honeybadger, GitHub, the lot) while parking Promotions and Social in
 * their own tabs.
 */
export const BULK_CATEGORIES: readonly LabelId[] = [
  "CATEGORY_PROMOTIONS",
  "CATEGORY_SOCIAL",
  "CATEGORY_UPDATES",
  "CATEGORY_FORUMS",
];

/**
 * What Inbox subtracts. Promotions and Social are the tabs Gmail actually
 * splits off Primary; Updates and Forums stay in that list even when the API
 * still stamps the category.
 */
export const PRIMARY_EXCLUDED: readonly LabelId[] = [
  "CATEGORY_PROMOTIONS",
  "CATEGORY_SOCIAL",
];

export function isBulk(labelIds: readonly string[]): boolean {
  return labelIds.some((id) => (BULK_CATEGORIES as readonly string[]).includes(id));
}

/** True when the conversation would not appear in Inbox. */
export function isOutsidePrimary(labelIds: readonly string[]): boolean {
  return labelIds.some((id) => (PRIMARY_EXCLUDED as readonly string[]).includes(id));
}

/** True when moving this conversation to Inbox would change its labels. */
export function needsInbox(labelIds: readonly string[]): boolean {
  return !labelIds.includes("INBOX") || isOutsidePrimary(labelIds);
}

export function hasBulkTabs(labels: readonly Label[]): boolean {
  return labels.some((label) => (BULK_CATEGORIES as readonly string[]).includes(label.id));
}

/** Where Inbox, `g i`, and a new window land. Always Primary, not the catch-all. */
export function inboxLabelId(): LabelId {
  return PRIMARY_LABEL;
}

/**
 * Gmail's category tabs: still in the inbox.
 *
 * Primary is inbox minus Promotions and Social. Promotions is inbox ∩ promotions. Archive
 * takes `INBOX` off and leaves the category, so a query that only asked for
 * the category would keep showing mail you had filed away.
 */
export function isInboxTab(labelId: LabelId): boolean {
  return labelId === "INBOX" || labelId === PRIMARY_LABEL || labelId === PROMOTIONS_LABEL;
}

/**
 * The two mailboxes Gmail has no label for, and which the store answers itself.
 *
 * Both were in `RAIL_ORDER` from the start and in no rail on any real mailbox,
 * because `railMailboxes` shows the labels the store actually has and neither
 * of these is one. `g a` and `g b` still offered them, so the keyboard reached
 * a place the rail could not name and the list came back empty — which is the
 * bug as it was reported.
 *
 * Archived means the *absence* of `INBOX`; snoozed is a local wake row plus a
 * per-account user label. `db::queries::mailbox_clause` is where each becomes a
 * query. Synthesizing them as labels here is what keeps the rail, ⌘K, the
 * favorites and the list header reading from one list rather than from two that
 * can disagree.
 *
 * `accountId: null` because neither belongs to an account — the archive spans
 * every mailbox, and narrowing it to one is the account rail's job, exactly as
 * it is for Sent.
 */
export const VIRTUAL_MAILBOXES: readonly Label[] = [
  { id: "SNOOZED", accountId: null, name: "Snoozed", kind: "system" },
  { id: "ARCHIVE", accountId: null, name: "Archive", kind: "system" },
  { id: PRIMARY_LABEL, accountId: null, name: "Inbox", kind: "system" },
];

/**
 * The store's labels plus the mailboxes it answers without one.
 *
 * Applied once, where the label list arrives, so that everything downstream
 * sees the same set. An empty list stays empty: before the first sync there is
 * no mailbox to have an archive of, and a rail carrying nothing but "Archive"
 * would be worse than a rail carrying nothing.
 */
export function withVirtualMailboxes(labels: readonly Label[]): Label[] {
  if (labels.length === 0) return [...labels];
  const present = new Set(labels.map((label) => label.id));
  return [...labels, ...VIRTUAL_MAILBOXES.filter((mailbox) => !present.has(mailbox.id))];
}

/**
 * Gmail's system label ids are their names too, and they shout. These are the
 * ones worth a hand-written translation; anything else falls back to
 * `humanize`.
 */
const DISPLAY_NAMES: Record<string, string> = {
  INBOX: "All",
  STARRED: "Starred",
  SNOOZED: "Snoozed",
  DRAFT: "Drafts",
  DRAFTS: "Drafts",
  SENT: "Sent",
  ARCHIVE: "Archive",
  SPAM: "Spam",
  TRASH: "Trash",
  UNREAD: "Unread",
  IMPORTANT: "Important",
  CHAT: "Chat",
  PRIMARY: "Inbox",
  CATEGORY_PERSONAL: "Primary",
  CATEGORY_SOCIAL: "Social",
  CATEGORY_PROMOTIONS: "Promotions",
  CATEGORY_UPDATES: "Updates",
  CATEGORY_FORUMS: "Forums",
};

/** What to call a label on screen. User labels already read fine. */
export function mailboxName(label: Label): string {
  if (label.kind !== "system") return label.name;
  return DISPLAY_NAMES[label.id] ?? DISPLAY_NAMES[label.name] ?? humanize(label.name);
}

/** `YELLOW_STAR` → "Yellow star". Leaves anything already human alone. */
function humanize(raw: string): string {
  if (!/^[A-Z0-9_]+$/.test(raw)) return raw;
  const words = raw.toLowerCase().split("_").filter(Boolean);
  // `CATEGORY_` is Gmail's namespace, not part of the name.
  if (words.length > 1 && words[0] === "category") words.shift();
  const text = words.join(" ");
  return text.charAt(0).toUpperCase() + text.slice(1);
}

/**
 * The rail's mailbox rows: the canonical set, in canonical order, named for
 * humans, and only the ones this mailbox actually has.
 */
export function railMailboxes(labels: readonly Label[]): Label[] {
  const system = new Map<LabelId, Label>();
  for (const label of labels) {
    if (label.kind === "system" && !system.has(label.id)) system.set(label.id, label);
  }
  const tabs = hasBulkTabs(labels);
  return RAIL_ORDER.flatMap((id) => {
    const label = system.get(id);
    if (!label) return [];
    // Inbox is the heading, and the heading is Primary. The full INBOX stays
    // reachable as "All" so promotions are one row away rather than mixed
    // into the thing you triage. Hidden when Google has no bulk tabs, because
    // then Primary and All are the same query.
    if (id === "INBOX") return tabs ? [{ ...label, name: "All" }] : [];
    return [{ ...label, name: mailboxName(label) }];
  });
}

/** One jumpable place: a mailbox or a label, named unambiguously. */
export interface MailboxTarget {
  id: LabelId;
  /** Display name, suffixed with the account when the name alone is ambiguous. */
  name: string;
  accountId: AccountId | null;
  kind: LabelKind;
}

/**
 * Everything ⌘K can jump to.
 *
 * User labels have per-account Gmail ids, so "Receipts" can appear five times —
 * once per mailbox that has one. When a name is ambiguous, say which account it
 * belongs to rather than showing the same word twice.
 */
export function mailboxTargets(
  labels: readonly Label[],
  accountName: (id: AccountId) => string | undefined,
): MailboxTarget[] {
  const named = labels.map((label) => ({ label, name: mailboxName(label) }));

  const counts = new Map<string, number>();
  for (const { name } of named) counts.set(name, (counts.get(name) ?? 0) + 1);

  return named.map(({ label, name }) => {
    const suffix =
      (counts.get(name) ?? 0) > 1 && label.accountId !== null
        ? accountName(label.accountId)
        : undefined;
    return {
      id: label.id,
      name: suffix ? `${name} · ${suffix}` : name,
      accountId: label.accountId,
      kind: label.kind,
    };
  });
}
