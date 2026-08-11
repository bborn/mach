/**
 * The address book Mach never asked you to fill in.
 *
 * There is no contacts API in this app and there should not be one: the
 * addresses worth completing are the addresses you have actually corresponded
 * with, and every one of those is already in the store.
 *
 * So the index is derived, from two kinds of source that are merged rather than
 * chosen between:
 *
 *  * **The store**, read once through `list_contacts` — every address in every
 *    message, folded and ranked by `db::queries::address_book`. This is the
 *    body of the book: thousands of people, including everyone the owner has
 *    ever replied to.
 *  * **Whatever is in memory** — the threads currently in the list, the open
 *    conversation's `to`/`cc` lines, calendar organisers and attendees. A few
 *    dozen people, and the ones most likely to be wanted next, which is why
 *    they are folded on top of the index rather than behind it: a sighting a
 *    second ago is what makes someone the freshest address in the book.
 *
 * The whole index used to be the second half alone, which is how a composer
 * opened in Drafts could offer completions from eight conversations and nothing
 * else.
 *
 * Three decisions worth knowing:
 *
 *  * **Sends outrank sightings.** Being cc'd on a thread with forty people puts
 *    forty addresses in the index; typing two letters should not offer you all
 *    of them ahead of the person you write to every day. `sends` is the only
 *    signal that says "you chose this address", so it dominates the tie-break.
 *  * **Your own addresses sort last, and are never dropped.** Mailing yourself
 *    is a real thing people do; having your own address as the top hit for `b`
 *    is not.
 *  * **Matching is prefix-first, substring-second, and never fuzzy.** ⌘K is
 *    fuzzy because you are searching. An address field is not a search: you
 *    know the name, you are typing it, and a fuzzy match that reorders under
 *    your fingers is how the wrong person gets a message.
 */

import type { Account, CalendarEvent, Participant, Thread, ThreadDetail } from "@/types";

export interface Contact {
  /** Lowercased — the identity of a contact, and what gets inserted. */
  email: string;
  /** The best display name seen for this address, if any was. */
  name?: string;
  /** The most recent moment this address appeared anywhere, epoch ms. */
  lastSeen: number;
  /** How many messages you have addressed to them. */
  sends: number;
  /** One of your own accounts. Kept, but sorted last. */
  self: boolean;
}

/* -------------------------------------------------------------------------- */
/* Building the index                                                          */
/* -------------------------------------------------------------------------- */

export interface ContactSources {
  /**
   * The store's own address book, as `list_contacts` returned it.
   *
   * Folded first, so everything below is allowed to improve on it: a sighting
   * in the open conversation moves `lastSeen` forward and can teach a name the
   * index does not have, while `sends` — which only the store knows — survives
   * untouched.
   */
  indexed?: readonly Contact[];
  threads?: readonly Thread[];
  /** The open conversation — its `to`/`cc` lines hold addresses no row shows. */
  detail?: ThreadDetail | null;
  events?: readonly CalendarEvent[];
  /** What `mach.contacts.v1` remembers about who you write to. */
  sent?: readonly SentContact[];
  /** Your own accounts, so they can be marked rather than guessed at. */
  accounts?: readonly Account[];
}

/**
 * Every address the app has seen, deduplicated, best name kept, ordered.
 *
 * The order is the "no query yet" order: who you write to most, most recently.
 * `rankContacts` re-sorts once there is something to match against.
 */
export function contactsFrom(sources: ContactSources): Contact[] {
  const byEmail = new Map<string, Contact>();

  const note = (person: Participant | undefined, at: number) => {
    if (!person?.email) return;
    const email = normalizeEmail(person.email);
    if (!email) return;
    const name = cleanName(person.name, email);
    const existing = byEmail.get(email);
    if (!existing) {
      byEmail.set(email, { email, name, lastSeen: at, sends: 0, self: false });
      return;
    }
    if (at > existing.lastSeen) existing.lastSeen = at;
    // A later sighting is allowed to teach us a name, never to unlearn one:
    // plenty of headers carry a bare address where an earlier one had "Ada
    // Lovelace <…>".
    if (!existing.name && name) existing.name = name;
  };

  for (const row of sources.indexed ?? []) {
    const email = normalizeEmail(row.email);
    if (!email) continue;
    byEmail.set(email, {
      email,
      name: cleanName(row.name, email),
      lastSeen: row.lastSeen,
      sends: row.sends,
      self: row.self,
    });
  }

  for (const thread of sources.threads ?? []) {
    for (const participant of thread.participants) note(participant, thread.timestamp);
  }

  for (const message of sources.detail?.messages ?? []) {
    note(message.from, message.timestamp);
    for (const person of message.to) note(person, message.timestamp);
    for (const person of message.cc) note(person, message.timestamp);
  }

  for (const event of sources.events ?? []) {
    note(event.organizer, event.start);
    for (const attendee of event.attendees) note(attendee, event.start);
  }

  for (const row of sources.sent ?? []) {
    note({ name: row.name ?? "", email: row.email }, row.lastSentAt);
    const contact = byEmail.get(normalizeEmail(row.email));
    if (contact) contact.sends = Math.max(contact.sends, row.sends);
  }

  for (const account of sources.accounts ?? []) {
    const email = normalizeEmail(account.email);
    if (!email) continue;
    const existing = byEmail.get(email);
    if (existing) existing.self = true;
    else byEmail.set(email, { email, name: account.name, lastSeen: 0, sends: 0, self: true });
  }

  return [...byEmail.values()].sort(byUsefulness);
}

/** Sends, then recency, then the address — total, so the order is stable. */
function byUsefulness(a: Contact, b: Contact): number {
  if (a.self !== b.self) return a.self ? 1 : -1;
  if (a.sends !== b.sends) return b.sends - a.sends;
  if (a.lastSeen !== b.lastSeen) return b.lastSeen - a.lastSeen;
  return a.email < b.email ? -1 : a.email > b.email ? 1 : 0;
}

export function normalizeEmail(email: string): string {
  return email.trim().toLowerCase();
}

/** A "name" that is just the address again is not a name. */
function cleanName(name: string | undefined, email: string): string | undefined {
  const trimmed = name?.trim().replace(/^"|"$/g, "").trim();
  if (!trimmed) return undefined;
  return normalizeEmail(trimmed) === email ? undefined : trimmed;
}

/** `Ada Lovelace <ada@x.com>` — what accepting a suggestion inserts. */
export function contactValue(contact: Contact): string {
  return contact.name ? `${contact.name} <${contact.email}>` : contact.email;
}

/* -------------------------------------------------------------------------- */
/* Matching                                                                    */
/* -------------------------------------------------------------------------- */

/**
 * How well a contact answers what has been typed so far.
 *
 * Zero means "do not offer this at all" — an address field showing a row that
 * does not contain what you typed is worse than showing nothing, because the
 * row under the cursor is one Enter away from being the recipient.
 */
export function matchScore(contact: Contact, query: string): number {
  const q = query.trim().toLowerCase();
  if (!q) return 1;

  const email = contact.email;
  const local = email.split("@")[0] ?? "";
  const name = (contact.name ?? "").toLowerCase();

  if (email === q) return 1000;
  if (email.startsWith(q)) return 220;
  if (local.startsWith(q)) return 200;
  if (name.startsWith(q)) return 180;
  // "lov" should find "Ada Lovelace", and "lovelace" should find her by
  // surname — a word start is what people mean when they type part of a name.
  if (startsAWord(name, q)) return 160;
  // The domain is a real thing to type: "@northwind" narrows a company.
  if (q.startsWith("@") && email.includes(q)) return 120;
  if (email.includes(q)) return 60;
  if (name.includes(q)) return 40;
  return 0;
}

function startsAWord(haystack: string, needle: string): boolean {
  let from = 0;
  for (;;) {
    const at = haystack.indexOf(needle, from);
    if (at < 0) return false;
    if (at > 0 && /[\s.\-_']/.test(haystack[at - 1])) return true;
    from = at + 1;
  }
}

export interface RankOptions {
  limit?: number;
  /** Addresses already in the field — offering them again is a duplicate. */
  exclude?: readonly string[];
}

/**
 * The rows to show under the caret, best first.
 *
 * Ties on match quality fall through to `byUsefulness`, so "two people called
 * Ada" is decided by which one you actually write to.
 */
export function rankContacts(
  contacts: readonly Contact[],
  query: string,
  options: RankOptions = {},
): Contact[] {
  const limit = options.limit ?? 6;
  const excluded = new Set((options.exclude ?? []).map(normalizeEmail));

  const scored: { contact: Contact; score: number }[] = [];
  for (const contact of contacts) {
    if (excluded.has(contact.email)) continue;
    const score = matchScore(contact, query);
    if (score <= 0) continue;
    scored.push({ contact, score });
  }

  scored.sort((a, b) => (b.score - a.score) || byUsefulness(a.contact, b.contact));
  return scored.slice(0, limit).map((row) => row.contact);
}

/* -------------------------------------------------------------------------- */
/* Who you write to — the persisted half                                       */
/* -------------------------------------------------------------------------- */

/**
 * This used to be the *only* record of who you write to. It is not any more —
 * `db::queries::address_book` counts every send in the store, going back as far
 * as the mail does, where this list only ever knew about messages sent from
 * Mach since the code shipped.
 *
 * It is kept for one thing: the gap. The store's book is read once at boot, and
 * a message sent since then is not in it — the send has to reach Google, come
 * back through a sync, and land in `messages` before it counts. Writing to
 * someone for the first time and having them vanish from completion for the
 * rest of the session is exactly the complaint this whole index exists to
 * answer, so the localStorage list covers the interval.
 *
 * The overlap is harmless because it is resolved rather than merged:
 * `contactsFrom` takes `Math.max` of the two counts, so a store that says forty
 * and a local list that says three still says forty.
 */
export const CONTACTS_STORAGE_KEY = "mach.contacts.v1";

/** Past this the list is trimmed by usefulness. Nobody has 500 correspondents. */
export const MAX_SENT_CONTACTS = 500;

export interface SentContact {
  email: string;
  name?: string;
  sends: number;
  lastSentAt: number;
}

/** The slice of `Storage` this needs, so a test can pass a `Map`-backed fake. */
export interface ContactStore {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
}

/**
 * Parse whatever is in storage, discarding anything that is not a contact.
 *
 * Same rule as `parseFavorites`: this reads data written by an older build, so
 * a wrong shape is an ordinary event and bad rows are dropped rather than
 * thrown over.
 */
export function parseSent(raw: string | null | undefined): SentContact[] {
  if (!raw) return [];
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return [];
  }
  if (!Array.isArray(parsed)) return [];

  const out: SentContact[] = [];
  const seen = new Set<string>();
  for (const entry of parsed) {
    if (typeof entry !== "object" || entry === null) continue;
    const row = entry as Record<string, unknown>;
    if (typeof row.email !== "string") continue;
    const email = normalizeEmail(row.email);
    if (!email || seen.has(email)) continue;
    seen.add(email);
    out.push({
      email,
      name: typeof row.name === "string" && row.name.trim() ? row.name.trim() : undefined,
      sends: typeof row.sends === "number" && row.sends > 0 ? Math.floor(row.sends) : 1,
      lastSentAt: typeof row.lastSentAt === "number" ? row.lastSentAt : 0,
    });
  }
  return out;
}

/** Fold one message's recipients into the list. Pure — the caller persists. */
export function recordSent(
  list: readonly SentContact[],
  recipients: readonly { name?: string; email: string }[],
  at: number,
): SentContact[] {
  const byEmail = new Map(list.map((row) => [row.email, { ...row }]));

  for (const recipient of recipients) {
    const email = normalizeEmail(recipient.email);
    if (!email) continue;
    const name = cleanName(recipient.name, email);
    const existing = byEmail.get(email);
    if (existing) {
      existing.sends += 1;
      existing.lastSentAt = Math.max(existing.lastSentAt, at);
      if (name) existing.name = name;
    } else {
      byEmail.set(email, { email, name, sends: 1, lastSentAt: at });
    }
  }

  const rows = [...byEmail.values()];
  if (rows.length <= MAX_SENT_CONTACTS) return rows;
  // Trimming keeps the people you write to, not the people you wrote to once
  // in 2019 — the same order the index itself is built in.
  rows.sort((a, b) => b.sends - a.sends || b.lastSentAt - a.lastSentAt);
  return rows.slice(0, MAX_SENT_CONTACTS);
}

function storage(store?: ContactStore): ContactStore | null {
  if (store) return store;
  try {
    return typeof localStorage === "undefined" ? null : localStorage;
  } catch {
    return null;
  }
}

export function loadSent(store?: ContactStore): SentContact[] {
  const target = storage(store);
  if (!target) return [];
  try {
    return parseSent(target.getItem(CONTACTS_STORAGE_KEY));
  } catch {
    return [];
  }
}

export function saveSent(list: readonly SentContact[], store?: ContactStore): void {
  const target = storage(store);
  if (!target) return;
  try {
    target.setItem(CONTACTS_STORAGE_KEY, JSON.stringify(list));
  } catch {
    /* a full or disabled localStorage must not fail a send */
  }
}

/**
 * Note that a message went to these people.
 *
 * Called on send rather than on save: a draft you abandoned is not evidence of
 * anything, and half-typed addresses would poison the index.
 */
export function noteSent(
  recipients: readonly { name?: string; email: string }[],
  at: number = Date.now(),
  store?: ContactStore,
): SentContact[] {
  const next = recordSent(loadSent(store), recipients, at);
  saveSent(next, store);
  return next;
}
