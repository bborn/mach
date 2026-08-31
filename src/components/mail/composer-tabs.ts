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
    const lead = who ?? about ?? kindName(draft);
    const trail = who === null ? null : about;
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
function recipient(draft: Draft): string | null {
  const people: Mailbox[] = draft.to.length > 0 ? draft.to : draft.cc;
  const first = people[0];
  if (!first) return null;
  const name = first.name?.trim() || first.email.trim();
  if (name === "") return null;
  return people.length > 1 ? `${name} +${people.length - 1}` : name;
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
