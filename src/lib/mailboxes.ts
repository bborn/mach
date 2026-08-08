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
];

/**
 * Gmail's system label ids are their names too, and they shout. These are the
 * ones worth a hand-written translation; anything else falls back to
 * `humanize`.
 */
const DISPLAY_NAMES: Record<string, string> = {
  INBOX: "Inbox",
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
  return RAIL_ORDER.flatMap((id) => {
    const label = system.get(id);
    return label ? [{ ...label, name: mailboxName(label) }] : [];
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
