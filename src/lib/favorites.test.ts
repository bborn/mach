import { describe, expect, it } from "vitest";
import {
  FAVORITES_STORAGE_KEY,
  addFavorite,
  favoriteKey,
  isFavorited,
  loadFavorites,
  parseFavorites,
  removeFavorite,
  saveFavorites,
  toggleFavorite,
  type Favorite,
  type FavoriteStore,
} from "./favorites";

const inbox: Favorite = { kind: "mailbox", labelId: "INBOX", accountId: null, name: "Inbox" };
const inboxPersonal: Favorite = {
  kind: "mailbox",
  labelId: "INBOX",
  accountId: 4,
  name: "Inbox · Personal",
};
const thread: Favorite = { kind: "thread", threadId: 12, accountId: 1, name: "Current Rent Roll" };

function store(initial?: string): FavoriteStore & { value: string | null } {
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

describe("favoriteKey", () => {
  it("separates the same mailbox under different account scopes", () => {
    expect(favoriteKey(inbox)).not.toBe(favoriteKey(inboxPersonal));
  });

  it("ignores the display name, so a renamed label stays the same favorite", () => {
    expect(favoriteKey({ ...inbox, name: "Something else" })).toBe(favoriteKey(inbox));
  });
});

describe("add / remove / toggle", () => {
  it("appends in pin order", () => {
    const list = addFavorite(addFavorite([], inbox), thread);
    expect(list.map((f) => f.name)).toEqual(["Inbox", "Current Rent Roll"]);
  });

  it("refreshes the name instead of duplicating an existing favorite", () => {
    const list = addFavorite([inbox], { ...inbox, name: "Inbox (renamed)" });
    expect(list).toHaveLength(1);
    expect(list[0]!.name).toBe("Inbox (renamed)");
  });

  it("toggles off what is already pinned, and leaves the rest alone", () => {
    const list = toggleFavorite([inbox, thread], inbox);
    expect(list).toEqual([thread]);
    expect(isFavorited(list, favoriteKey(inbox))).toBe(false);
  });

  it("toggles on what is not", () => {
    expect(toggleFavorite([inbox], thread)).toEqual([inbox, thread]);
  });

  it("removes by key", () => {
    expect(removeFavorite([inbox, inboxPersonal], favoriteKey(inboxPersonal))).toEqual([inbox]);
  });
});

describe("parseFavorites", () => {
  it("round-trips what was written", () => {
    expect(parseFavorites(JSON.stringify([inbox, thread]))).toEqual([inbox, thread]);
  });

  it("survives junk rather than throwing", () => {
    expect(parseFavorites(null)).toEqual([]);
    expect(parseFavorites("not json")).toEqual([]);
    expect(parseFavorites('{"kind":"mailbox"}')).toEqual([]);
  });

  it("drops entries an older build would not recognise", () => {
    const raw = JSON.stringify([
      inbox,
      { kind: "mailbox", labelId: "INBOX" }, // no name, no account scope
      { kind: "thread", threadId: "12", accountId: 1, name: "string id" },
      { kind: "person", email: "someone@example.com", name: "Someone" },
      null,
    ]);
    expect(parseFavorites(raw)).toEqual([inbox]);
  });

  it("collapses duplicates written by a buggy writer", () => {
    expect(parseFavorites(JSON.stringify([inbox, inbox]))).toEqual([inbox]);
  });
});

describe("storage", () => {
  it("writes and reads back through a store", () => {
    const backing = store();
    saveFavorites([inbox, thread], backing);
    expect(backing.value).toBe(JSON.stringify([inbox, thread]));
    expect(loadFavorites(backing)).toEqual([inbox, thread]);
  });

  it("is a no-op without a store — a private window is not an error", () => {
    expect(loadFavorites(null)).toEqual([]);
    expect(() => saveFavorites([inbox], null)).not.toThrow();
  });

  it("shrugs off a store that refuses to write", () => {
    const hostile: FavoriteStore = {
      getItem() {
        throw new Error("blocked");
      },
      setItem() {
        throw new Error("quota");
      },
    };
    expect(loadFavorites(hostile)).toEqual([]);
    expect(() => saveFavorites([inbox], hostile)).not.toThrow();
  });

  it("keys its storage slot by version", () => {
    expect(FAVORITES_STORAGE_KEY).toBe("mach.favorites.v1");
  });
});
