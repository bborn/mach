import { describe, expect, it } from "vitest";
import type { Account, CalendarEvent, Thread, ThreadDetail } from "@/types";
import {
  CONTACTS_STORAGE_KEY,
  MAX_SENT_CONTACTS,
  contactValue,
  contactsFrom,
  loadSent,
  matchScore,
  parseSent,
  rankContacts,
  recordSent,
  saveSent,
  type Contact,
  type ContactStore,
  type SentContact,
} from "./contacts";

const DAY = 86_400_000;

function thread(partial: Partial<Thread> & Pick<Thread, "id" | "participants">): Thread {
  return {
    accountId: 1,
    subject: "Subject",
    snippet: "",
    timestamp: 1_000,
    unread: false,
    starred: false,
    hasAttachment: false,
    messageCount: 1,
    labelIds: ["INBOX"],
    ...partial,
  };
}

function contact(partial: Partial<Contact> & Pick<Contact, "email">): Contact {
  return { lastSeen: 0, sends: 0, self: false, ...partial };
}

function store(initial?: string): ContactStore & { value: string | null } {
  return {
    value: initial ?? null,
    getItem() {
      return this.value;
    },
    setItem(_key, value) {
      this.value = value;
    },
  };
}

describe("contactsFrom", () => {
  it("deduplicates on the address, case-insensitively, keeping the newest sighting", () => {
    const contacts = contactsFrom({
      threads: [
        thread({ id: 1, timestamp: 5 * DAY, participants: [{ name: "Ada", email: "Ada@X.com" }] }),
        thread({ id: 2, timestamp: 9 * DAY, participants: [{ name: "Ada", email: "ada@x.com" }] }),
      ],
    });
    expect(contacts).toHaveLength(1);
    expect(contacts[0].email).toBe("ada@x.com");
    expect(contacts[0].lastSeen).toBe(9 * DAY);
  });

  it("learns a name from any sighting that has one and never unlearns it", () => {
    const contacts = contactsFrom({
      threads: [
        thread({ id: 1, timestamp: 2, participants: [{ name: "Ada Lovelace", email: "a@x.com" }] }),
        thread({ id: 2, timestamp: 3, participants: [{ name: "", email: "a@x.com" }] }),
      ],
    });
    expect(contacts[0].name).toBe("Ada Lovelace");
  });

  it("does not treat the address repeated as a name", () => {
    const contacts = contactsFrom({
      threads: [thread({ id: 1, participants: [{ name: "a@x.com", email: "a@x.com" }] })],
    });
    expect(contacts[0].name).toBeUndefined();
    expect(contactValue(contacts[0])).toBe("a@x.com");
  });

  it("reads the open conversation's to and cc, which no row shows", () => {
    const detail: ThreadDetail = {
      thread: thread({ id: 1, participants: [] }),
      messages: [
        {
          id: 1,
          threadId: 1,
          accountId: 1,
          from: { name: "Ada", email: "ada@x.com" },
          to: [{ name: "Bob", email: "bob@y.com" }],
          cc: [{ name: "Cy", email: "cy@z.com" }],
          timestamp: 7,
          bodyText: "",
          attachments: [],
          isDraft: false,
        },
      ],
    };
    expect(contactsFrom({ detail }).map((c) => c.email)).toEqual(
      expect.arrayContaining(["ada@x.com", "bob@y.com", "cy@z.com"]),
    );
  });

  it("takes calendar organisers and attendees too", () => {
    const event: CalendarEvent = {
      id: 1,
      calendarId: "primary",
      accountId: 1,
      title: "Standup",
      start: 4 * DAY,
      end: 4 * DAY + 3600_000,
      allDay: false,
      organizer: { name: "Ada", email: "ada@x.com" },
      attendees: [{ name: "Bob", email: "bob@y.com" }],
    };
    expect(contactsFrom({ events: [event] }).map((c) => c.email)).toEqual([
      "ada@x.com",
      "bob@y.com",
    ]);
  });

  it("orders by who you write to before who you have merely seen", () => {
    const contacts = contactsFrom({
      threads: [
        thread({ id: 1, timestamp: 9 * DAY, participants: [{ name: "Seen", email: "seen@x.com" }] }),
        thread({ id: 2, timestamp: 1 * DAY, participants: [{ name: "Sent", email: "sent@x.com" }] }),
      ],
      sent: [{ email: "sent@x.com", sends: 3, lastSentAt: 1 * DAY }],
    });
    expect(contacts.map((c) => c.email)).toEqual(["sent@x.com", "seen@x.com"]);
  });

  it("marks your own accounts and sorts them last without dropping them", () => {
    const accounts: Account[] = [
      { id: 1, email: "me@x.com", name: "Me", colorIndex: 1, kind: "personal" },
    ];
    const contacts = contactsFrom({
      threads: [thread({ id: 1, participants: [{ name: "Ada", email: "ada@x.com" }] })],
      accounts,
    });
    expect(contacts.map((c) => c.email)).toEqual(["ada@x.com", "me@x.com"]);
    expect(contacts[1].self).toBe(true);
  });
});

describe("contactsFrom — merging the store's index with what is on screen", () => {
  it("keeps everyone the store knows about, not only who is loaded", () => {
    const contacts = contactsFrom({
      indexed: [
        contact({ email: "ada@x.com", name: "Ada", sends: 12, lastSeen: 4 * DAY }),
        contact({ email: "bob@y.com", name: "Bob", sends: 0, lastSeen: 2 * DAY }),
      ],
      threads: [thread({ id: 1, participants: [{ name: "Cy", email: "cy@z.com" }] })],
    });
    expect(contacts.map((c) => c.email).sort()).toEqual(["ada@x.com", "bob@y.com", "cy@z.com"]);
  });

  it("ranks someone you write to often above someone in a loaded thread", () => {
    const contacts = contactsFrom({
      // The index is a snapshot from boot, so its recency is behind the list's.
      indexed: [contact({ email: "often@x.com", name: "Often", sends: 40, lastSeen: 1 * DAY })],
      threads: [
        thread({ id: 1, timestamp: 9 * DAY, participants: [{ name: "Seen", email: "seen@x.com" }] }),
      ],
    });
    expect(contacts.map((c) => c.email)).toEqual(["often@x.com", "seen@x.com"]);
  });

  it("lets a sighting on screen outrank a stale index entry", () => {
    const contacts = contactsFrom({
      indexed: [
        contact({ email: "old@x.com", lastSeen: 1 * DAY }),
        contact({ email: "ada@x.com", lastSeen: 2 * DAY }),
      ],
      // Ada was in the index at two days old; she is in the open conversation
      // now, and that is the one the next message is most likely going to.
      threads: [
        thread({ id: 1, timestamp: 9 * DAY, participants: [{ name: "Ada", email: "ada@x.com" }] }),
      ],
    });
    expect(contacts.map((c) => c.email)).toEqual(["ada@x.com", "old@x.com"]);
    expect(contacts[0].lastSeen).toBe(9 * DAY);
  });

  it("does not let a sighting cost someone the sends the store counted", () => {
    const contacts = contactsFrom({
      indexed: [contact({ email: "ada@x.com", name: "Ada", sends: 12, lastSeen: 1 })],
      threads: [thread({ id: 1, timestamp: 5, participants: [{ name: "", email: "ada@x.com" }] })],
    });
    expect(contacts).toHaveLength(1);
    expect(contacts[0].sends).toBe(12);
    expect(contacts[0].lastSeen).toBe(5);
  });

  it("takes the higher send count when the index and localStorage disagree", () => {
    // The store counts every send ever; `mach.contacts.v1` only knows the ones
    // made from Mach. Neither is wrong, and the larger one is the true one.
    const contacts = contactsFrom({
      indexed: [contact({ email: "ada@x.com", sends: 40, lastSeen: 1 })],
      sent: [
        { email: "ada@x.com", sends: 3, lastSentAt: 2 },
        // Sent a minute ago, so the next sync has not filed it yet: this is
        // the whole reason the localStorage list is still here.
        { email: "fresh@x.com", sends: 1, lastSentAt: 9 * DAY },
      ],
    });
    expect(contacts.map((c) => [c.email, c.sends])).toEqual([
      ["ada@x.com", 40],
      ["fresh@x.com", 1],
    ]);
  });

  it("deduplicates the index against the screen, case-insensitively", () => {
    const contacts = contactsFrom({
      indexed: [contact({ email: "Ada@X.com", name: "Ada Lovelace", sends: 2, lastSeen: 1 })],
      threads: [thread({ id: 1, timestamp: 5, participants: [{ name: "", email: "ADA@x.COM" }] })],
    });
    expect(contacts).toHaveLength(1);
    expect(contacts[0]).toMatchObject({ email: "ada@x.com", name: "Ada Lovelace", sends: 2 });
  });

  it("lets a sighting teach a name the index never saw", () => {
    const contacts = contactsFrom({
      indexed: [contact({ email: "ada@x.com", sends: 2, lastSeen: 1 })],
      threads: [thread({ id: 1, timestamp: 5, participants: [{ name: "Ada", email: "ada@x.com" }] })],
    });
    expect(contacts[0].name).toBe("Ada");
  });

  it("carries the index's self flag, and the accounts still mark their own", () => {
    const contacts = contactsFrom({
      indexed: [
        contact({ email: "me@x.com", name: "Me", self: true, lastSeen: 9 * DAY, sends: 5 }),
        contact({ email: "ada@x.com", name: "Ada", lastSeen: 1 }),
      ],
      accounts: [{ id: 1, email: "me@x.com", name: "Me", colorIndex: 1, kind: "personal" }],
    });
    expect(contacts.map((c) => c.email)).toEqual(["ada@x.com", "me@x.com"]);
    expect(contacts[1].self).toBe(true);
  });

  it("is the old behaviour when the index has not arrived yet", () => {
    const sources = {
      threads: [thread({ id: 1, timestamp: 5, participants: [{ name: "Ada", email: "ada@x.com" }] })],
    };
    expect(contactsFrom({ indexed: [], ...sources })).toEqual(contactsFrom(sources));
  });
});

describe("matchScore", () => {
  const ada = contact({ email: "ada@northwind.com", name: "Ada Lovelace" });

  it("scores an exact address above everything", () => {
    expect(matchScore(ada, "ada@northwind.com")).toBeGreaterThan(matchScore(ada, "ada"));
  });

  it("prefers the local part over a name, and a name over a substring", () => {
    expect(matchScore(ada, "ad")).toBeGreaterThan(matchScore(ada, "Ada Lov"));
    expect(matchScore(ada, "Ada Lov")).toBeGreaterThan(matchScore(ada, "north"));
  });

  it("finds a surname, because a word start is what people type", () => {
    expect(matchScore(ada, "lovelace")).toBeGreaterThan(0);
  });

  it("finds a company by domain", () => {
    expect(matchScore(ada, "@northwind")).toBeGreaterThan(0);
  });

  it("refuses anything the contact does not contain", () => {
    expect(matchScore(ada, "zzz")).toBe(0);
  });

  it("matches everyone when nothing has been typed", () => {
    expect(matchScore(ada, "  ")).toBeGreaterThan(0);
  });
});

describe("rankContacts", () => {
  const people = [
    contact({ email: "ada@x.com", name: "Ada Lovelace", lastSeen: 1, sends: 0 }),
    contact({ email: "adam@x.com", name: "Adam Smith", lastSeen: 2, sends: 4 }),
    contact({ email: "bob@y.com", name: "Bob", lastSeen: 3, sends: 9 }),
  ];

  it("returns only what matches", () => {
    expect(rankContacts(people, "ad").map((c) => c.email).sort()).toEqual([
      "ada@x.com",
      "adam@x.com",
    ]);
  });

  it("breaks a tie on match quality with who you write to", () => {
    expect(rankContacts(people, "a").map((c) => c.email)).toEqual(["adam@x.com", "ada@x.com"]);
  });

  it("never offers an address already in the field", () => {
    expect(rankContacts(people, "ad", { exclude: ["ADA@x.com"] }).map((c) => c.email)).toEqual([
      "adam@x.com",
    ]);
  });

  it("honours the limit", () => {
    expect(rankContacts(people, "", { limit: 2 })).toHaveLength(2);
  });
});

describe("recordSent", () => {
  it("counts a first send and increments a repeat", () => {
    const once = recordSent([], [{ name: "Ada", email: "Ada@X.com" }], 10);
    expect(once).toEqual([{ email: "ada@x.com", name: "Ada", sends: 1, lastSentAt: 10 }]);
    const twice = recordSent(once, [{ email: "ada@x.com" }], 20);
    expect(twice[0]).toEqual({ email: "ada@x.com", name: "Ada", sends: 2, lastSentAt: 20 });
  });

  it("never moves a timestamp backwards", () => {
    const list = recordSent([], [{ email: "a@x.com" }], 100);
    expect(recordSent(list, [{ email: "a@x.com" }], 50)[0].lastSentAt).toBe(100);
  });

  it("trims to the people you actually write to", () => {
    const many: SentContact[] = Array.from({ length: MAX_SENT_CONTACTS }, (_, i) => ({
      email: `p${i}@x.com`,
      sends: 1,
      lastSentAt: i,
    }));
    const kept = recordSent(many, [{ email: "new@x.com" }], 1);
    expect(kept).toHaveLength(MAX_SENT_CONTACTS);
    expect(kept.some((row) => row.email === "p0@x.com")).toBe(false);
  });
});

describe("parseSent", () => {
  it("survives anything that is not a list of contacts", () => {
    expect(parseSent(null)).toEqual([]);
    expect(parseSent("{oh no")).toEqual([]);
    expect(parseSent('{"email":"a@x.com"}')).toEqual([]);
    expect(parseSent('[3, null, {"nope": 1}]')).toEqual([]);
  });

  it("keeps the good rows out of a mixed list, deduplicated", () => {
    const raw = JSON.stringify([
      { email: "A@x.com", name: " Ada ", sends: 3, lastSentAt: 9 },
      { email: "a@x.com", sends: 1, lastSentAt: 1 },
      { nope: true },
    ]);
    expect(parseSent(raw)).toEqual([{ email: "a@x.com", name: "Ada", sends: 3, lastSentAt: 9 }]);
  });
});

describe("persistence", () => {
  it("round-trips through a store", () => {
    const target = store();
    saveSent([{ email: "a@x.com", sends: 2, lastSentAt: 5 }], target);
    expect(loadSent(target)).toEqual([{ email: "a@x.com", name: undefined, sends: 2, lastSentAt: 5 }]);
  });

  it("uses the versioned key", () => {
    const target = store();
    saveSent([], target);
    expect(CONTACTS_STORAGE_KEY).toBe("mach.contacts.v1");
    expect(target.value).toBe("[]");
  });
});
