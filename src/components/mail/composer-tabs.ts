/**
 * What the composer strip says, and which key answers for it.
 *
 * # One list, two consumers
 *
 * The strip is drawn from this and `⌥1`…`⌥9` are registered from this, so the
 * tab labelled ⌥2 and the composer ⌥2 switches to cannot be two different
 * drafts. The keys used to be a `drafts.slice(0, 9)` beside a `drafts.map()`;
 * they agreed, but only because the two expressions happened to match.
 *
 * # What a tab is allowed to say
 *
 * Twenty characters, and a subject spends the first eight of them on `Fwd: `
 * and a shared noun. The owner saw `⌥1 Getting into your Influ…` beside
 * `⌥2 Fwd: Your Deposit Rec…` and could not tell which draft was which — both
 * labels were true and neither was an answer.
 *
 * So the **recipient leads**. Who a message is going to is what separates one
 * open draft from another in practice: a reply already knows its conversation,
 * two forwards of the same receipt differ only by who is getting them, and the
 * name is short, concrete and the thing being decided about. The subject
 * follows it, stripped of `Re:` and `Fwd:` — those say what the draft *is*,
 * which the strip cannot use, where the rest says what it is about — and it is
 * what truncation eats first, which is the right order to lose them in.
 *
 * A draft with nobody in it yet leads with its subject instead, and a draft
 * with neither is named for its kind, so a composer opened a second ago is
 * "New message" rather than an empty chip.
 *
 * # Except when the recipient is a machine
 *
 * Replying to a Delta receipt produced `⌥1 Transactional Email Reply Inbox ·
 * Your…`. That name is boilerplate an email service printed, it identifies
 * nothing, and it ate the line the subject would have used. So a recipient
 * that is not a person steps aside and the subject leads instead — see
 * `automatedAddress`, which decides that from the address and never from the
 * name.
 */

import { subjectStem, type Draft, type Mailbox } from "@/lib/compose";

export interface ComposerTab {
  id: string;
  /** The chord that switches to it. Null past the ninth: there is no ⌥10. */
  keys: string | null;
  /**
   * The same chord in the spelling `aria-keyshortcuts` wants.
   *
   * Two spellings of one fact, produced together, because the alternative is
   * the consumer rewriting `keys` — and a consumer that can rewrite the chord
   * is a consumer that can get it wrong, which is the whole thing this file
   * exists to prevent.
   */
  ariaKeys: string | null;
  /** The first line of the label — a name where there is one. */
  lead: string;
  /** What it is about, where that is not already the lead. */
  trail: string | null;
  /** Both of the above as one string: the accessible name. */
  label: string;
}

/** How many of them a chord can reach. ⌥1…⌥9, and then no more digits. */
export const KEYED_COMPOSERS = 9;

export function composerTabs(drafts: readonly Draft[]): ComposerTab[] {
  return drafts.map((draft, index) => {
    const who = recipient(draft);
    const about = subjectStem(draft.subject) || null;
    /*
     * The one case where the recipient does not lead: it is a machine, and
     * there is a subject to lead in its place. The name still follows — it
     * says which service, which is worth the characters left over — and it is
     * now what truncation eats, which it has earned.
     */
    const stepAside = who !== null && who.automated && about !== null;
    const lead = stepAside ? about : (who?.text ?? about ?? kindName(draft));
    const trail = stepAside ? who.text : who === null ? null : about;
    const chord = index < KEYED_COMPOSERS ? index + 1 : null;
    return {
      id: draft.id,
      keys: chord === null ? null : `alt+${chord}`,
      ariaKeys: chord === null ? null : `Alt+${chord}`,
      lead,
      trail,
      label: trail === null ? lead : `${lead} — ${trail}`,
    };
  });
}

/**
 * Who the message is going to, in as few characters as identify them.
 *
 * The display name where the address book has one, because "Sarah Chen" is
 * read faster than "sarah.chen@northwind.example", and the whole address
 * otherwise — not the local part, which throws away the half that distinguishes
 * two people called `info`.
 *
 * Several recipients count rather than list. A tab wide enough for three names
 * is a tab wide enough for nothing else, and the first name plus a number is
 * enough to pick the draft out; the composer itself has the full list.
 *
 * `cc` stands in for an empty `to` so that a draft addressed only in copy is
 * still named after a person.
 */
function recipient(draft: Draft): { text: string; automated: boolean } | null {
  const people: Mailbox[] = draft.to.length > 0 ? draft.to : draft.cc;
  const first = people[0];
  if (!first) return null;
  const name = first.name?.trim() || first.email.trim();
  if (name === "") return null;
  return {
    text: people.length > 1 ? `${name} +${people.length - 1}` : name,
    automated: automatedAddress(first.email),
  };
}

/**
 * Whether an address belongs to a machine rather than to somebody.
 *
 * # The address decides this, and the display name never does
 *
 * "Transactional Email Reply Inbox" is boilerplate and "Delta Air Lines" is a
 * company the owner really does write to, and nothing about their shape
 * separates them: both are capitalised multi-word noun phrases, both are
 * descriptions rather than given names. A test written against display names
 * either misses the first or demotes the second, and demoting the second is
 * much the worse error — it fires on the mail that matters instead of on
 * receipts. So display names are not consulted.
 *
 * The address is a different kind of evidence, because the sending system wrote
 * it about itself. `no-reply@` and `notifications@` are declarations that
 * nobody is reading the mailbox, and
 * `reply-H3DFZJSV4PQEFIHGBKJDBP6IAA.10202@t.delta.com` carries a routing token
 * no person would be given. Both live in the local part; the domain says
 * nothing, since a machine and a colleague can share one.
 *
 * It errs towards under-firing. A staffed `support@` or `info@` is left alone,
 * and so is a routing token shorter than the bar in `isRoutingToken`. The one
 * way it fires wrongly is a person who really does read `notifications@` at
 * their own domain, and even then their name survives in the trail.
 */
function automatedAddress(email: string): boolean {
  const at = email.lastIndexOf("@");
  const local = (at === -1 ? email : email.slice(0, at)).trim().toLowerCase();
  const segments = local.split(/[.\-_+]+/).filter((part) => part !== "");
  if (segments.length === 0) return false;
  /*
   * The declaration runs at the front, and the separators inside it are the
   * sender's taste: `noreply`, `no-reply` and `do.not.reply` are one mailbox
   * spelled three ways. Joining the leading segments back up reads all three,
   * and reads them through whatever the sender appended after — `noreply-orders@`.
   */
  for (let take = 1; take <= Math.min(3, segments.length); take += 1) {
    if (NOBODY_READS_IT.has(segments.slice(0, take).join(""))) return true;
  }
  return segments.some(isRoutingToken);
}

/**
 * Local parts that say, in the sender's own words, that nobody is there.
 *
 * Role addresses a human answers — `support`, `info`, `sales`, `billing`,
 * `hello` — are absent from this list. Writing to a company's support desk is
 * writing to the company, and the company's name is the useful half of that
 * label.
 */
const NOBODY_READS_IT = new Set([
  "noreply",
  "noreplies",
  "noresponse",
  "donotreply",
  "dontreply",
  "nomail",
  "notify",
  "notification",
  "notifications",
  "autoreply",
  "automated",
  "mailerdaemon",
  "postmaster",
  "bounce",
  "bounces",
]);

/**
 * A segment that is an identifier a machine minted, not a name a person picked.
 *
 * Sixteen characters, and digits among the letters. The bar is set high on
 * purpose: `johnsmith1985` is thirteen, and catching one more ESP token is not
 * worth demoting somebody who put their birth year in their address.
 */
function isRoutingToken(segment: string): boolean {
  if (segment.length < 16 || !/^[a-z0-9]+$/.test(segment)) return false;
  return /^[0-9]+$/.test(segment) || (/[a-z]/.test(segment) && /[0-9]/.test(segment));
}

/** The last resort: a draft with no recipient and no subject is what it is. */
function kindName(draft: Draft): string {
  switch (draft.kind) {
    case "reply":
    case "replyAll":
    case "adopted":
      return "Reply";
    case "forward":
      return "Forward";
    default:
      return "New message";
  }
}
