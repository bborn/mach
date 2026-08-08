import { describe, expect, it } from "vitest";
import {
  DEFAULT_PREFERENCES,
  LIST_WIDTH_BOUNDS,
  SEND_DELAY_BOUNDS,
  SIGNATURE_DELIMITER,
  SYNC_INTERVAL_BOUNDS,
  UNDO_WINDOW_BOUNDS,
  composeAccountId,
  loadPreferences,
  loadSession,
  parsePreferences,
  parseSession,
  sendDelayMs,
  signatureFor,
  undoWindowMs,
  withSignature,
  type Preferences,
} from "./prefs";
import { initialUi, uiReducer } from "@/hooks/useMach";

const accounts = [{ id: 4 }, { id: 7 }, { id: 9 }];

function prefs(overrides: Partial<Preferences> = {}): Preferences {
  return { ...DEFAULT_PREFERENCES, ...overrides };
}

describe("parsePreferences", () => {
  it("returns the defaults for an empty store", () => {
    expect(parsePreferences({})).toEqual(DEFAULT_PREFERENCES);
  });

  it("returns the defaults for anything that is not an object", () => {
    for (const raw of [null, undefined, 7, "theme", [], true]) {
      expect(parsePreferences(raw)).toEqual(DEFAULT_PREFERENCES);
    }
  });

  it("reads a fully populated store", () => {
    expect(
      parsePreferences({
        defaultAccountId: 7,
        signatures: { "7": "— Bruno" },
        density: "compact",
        theme: "dark",
        syncIntervalSeconds: 300,
        undoWindowSeconds: 45,
        sendDelaySeconds: 30,
        weekStartsOn: 0,
        workingHours: { start: 8, end: 18 },
      }),
    ).toEqual({
      defaultAccountId: 7,
      signatures: { "7": "— Bruno" },
      density: "compact",
      theme: "dark",
      syncIntervalSeconds: 300,
      undoWindowSeconds: 45,
      sendDelaySeconds: 30,
      weekStartsOn: 0,
      workingHours: { start: 8, end: 18 },
    });
  });

  it("keeps the good fields when one of them is rubbish", () => {
    // Per field, not per document: one bad row costs one setting.
    const parsed = parsePreferences({ theme: "chartreuse", density: "compact" });
    expect(parsed.theme).toBe(DEFAULT_PREFERENCES.theme);
    expect(parsed.density).toBe("compact");
  });

  it("ignores keys it has never heard of", () => {
    const parsed = parsePreferences({ theme: "light", somethingFromTheFuture: [1, 2] });
    expect(parsed).toEqual(prefs({ theme: "light" }));
  });

  describe("enumerations", () => {
    it("accepts only the three themes", () => {
      for (const theme of ["system", "light", "dark"] as const) {
        expect(parsePreferences({ theme }).theme).toBe(theme);
      }
      for (const bad of ["Dark", "", 1, null]) {
        expect(parsePreferences({ theme: bad }).theme).toBe("system");
      }
    });

    it("accepts only the two densities", () => {
      expect(parsePreferences({ density: "compact" }).density).toBe("compact");
      expect(parsePreferences({ density: "cosy" }).density).toBe("comfortable");
    });

    it("accepts only Sunday, Monday and Saturday as week starts", () => {
      expect(parsePreferences({ weekStartsOn: 0 }).weekStartsOn).toBe(0);
      expect(parsePreferences({ weekStartsOn: 6 }).weekStartsOn).toBe(6);
      // Not a start anybody uses, and the grid has no story for it.
      expect(parsePreferences({ weekStartsOn: 3 }).weekStartsOn).toBe(1);
      expect(parsePreferences({ weekStartsOn: "1" }).weekStartsOn).toBe(1);
    });
  });

  describe("numbers", () => {
    it("clamps rather than rejects", () => {
      expect(parsePreferences({ syncIntervalSeconds: 0 }).syncIntervalSeconds).toBe(
        SYNC_INTERVAL_BOUNDS.min,
      );
      expect(parsePreferences({ syncIntervalSeconds: 1e9 }).syncIntervalSeconds).toBe(
        SYNC_INTERVAL_BOUNDS.max,
      );
      expect(parsePreferences({ undoWindowSeconds: -4 }).undoWindowSeconds).toBe(
        UNDO_WINDOW_BOUNDS.min,
      );
      expect(parsePreferences({ sendDelaySeconds: 9999 }).sendDelaySeconds).toBe(
        SEND_DELAY_BOUNDS.max,
      );
    });

    it("allows a send delay of zero — sending immediately is a real choice", () => {
      expect(SEND_DELAY_BOUNDS.min).toBe(0);
      expect(parsePreferences({ sendDelaySeconds: 0 }).sendDelaySeconds).toBe(0);
    });

    it("rounds to whole seconds", () => {
      expect(parsePreferences({ undoWindowSeconds: 20.6 }).undoWindowSeconds).toBe(21);
    });

    it("falls back for anything that is not a finite number", () => {
      for (const bad of [Number.NaN, Number.POSITIVE_INFINITY, "30", null, {}]) {
        expect(parsePreferences({ undoWindowSeconds: bad }).undoWindowSeconds).toBe(
          DEFAULT_PREFERENCES.undoWindowSeconds,
        );
      }
    });

    it("defaults the undo window to something longer than the old six seconds", () => {
      // The reason this preference exists: one keystroke can archive fifty
      // conversations, and six seconds is not long enough to notice.
      expect(DEFAULT_PREFERENCES.undoWindowSeconds).toBeGreaterThan(6);
    });
  });

  describe("the default account", () => {
    it("keeps a positive integer id", () => {
      expect(parsePreferences({ defaultAccountId: 7 }).defaultAccountId).toBe(7);
    });

    it("treats anything else as no opinion", () => {
      for (const bad of [0, -1, 2.5, "7", null, undefined]) {
        expect(parsePreferences({ defaultAccountId: bad }).defaultAccountId).toBeNull();
      }
    });
  });

  describe("signatures", () => {
    it("drops entries that are not strings", () => {
      expect(
        parsePreferences({ signatures: { "4": "— B", "7": 12, "9": null, "11": "" } }).signatures,
      ).toEqual({ "4": "— B" });
    });

    it("falls back to none when the whole map is the wrong shape", () => {
      expect(parsePreferences({ signatures: "— B" }).signatures).toEqual({});
      expect(parsePreferences({ signatures: ["— B"] }).signatures).toEqual({});
    });
  });

  describe("working hours", () => {
    it("clamps each end into the day", () => {
      expect(parsePreferences({ workingHours: { start: -3, end: 40 } }).workingHours).toEqual({
        start: 0,
        end: 24,
      });
    });

    it("rejects an inverted or empty band as a unit", () => {
      // A start after its end is not one bad field; it is a band that cannot be
      // drawn, so both ends go back to the default together.
      for (const band of [{ start: 17, end: 9 }, { start: 9, end: 9 }]) {
        expect(parsePreferences({ workingHours: band }).workingHours).toEqual(
          DEFAULT_PREFERENCES.workingHours,
        );
      }
    });

    it("allows a whole day of working hours", () => {
      expect(parsePreferences({ workingHours: { start: 0, end: 24 } }).workingHours).toEqual({
        start: 0,
        end: 24,
      });
    });

    it("falls back when either end is missing or not a number", () => {
      for (const band of [{ start: 9 }, { start: "9", end: 17 }, {}, 9]) {
        expect(parsePreferences({ workingHours: band }).workingHours).toEqual(
          DEFAULT_PREFERENCES.workingHours,
        );
      }
    });
  });
});

describe("derived values", () => {
  it("converts the two windows to milliseconds", () => {
    expect(undoWindowMs(prefs({ undoWindowSeconds: 20 }))).toBe(20_000);
    expect(sendDelayMs(prefs({ sendDelaySeconds: 10 }))).toBe(10_000);
  });

  it("finds a signature by account id, and answers empty when there is none", () => {
    const p = prefs({ signatures: { "7": "— Bruno" } });
    expect(signatureFor(p, 7)).toBe("— Bruno");
    expect(signatureFor(p, 4)).toBe("");
    expect(signatureFor(p, null)).toBe("");
    expect(signatureFor(p, undefined)).toBe("");
  });
});

describe("withSignature", () => {
  it("appends after the RFC 3676 delimiter", () => {
    expect(withSignature("Sounds good.", "— Bruno")).toBe(
      `Sounds good.${SIGNATURE_DELIMITER}— Bruno`,
    );
  });

  it("keeps the delimiter's trailing space, which is what makes it one", () => {
    expect(SIGNATURE_DELIMITER).toBe("\n\n-- \n");
  });

  it("is idempotent, so reopening a saved draft does not stack copies", () => {
    const once = withSignature("Sounds good.", "— Bruno");
    expect(withSignature(once, "— Bruno")).toBe(once);
  });

  it("leaves the body alone when there is no signature", () => {
    expect(withSignature("Sounds good.", "")).toBe("Sounds good.");
    expect(withSignature("Sounds good.", "   \n ")).toBe("Sounds good.");
  });

  it("works on an empty body — a fresh reply is signature only", () => {
    expect(withSignature("", "— Bruno")).toBe(`${SIGNATURE_DELIMITER}— Bruno`);
  });
});

describe("composeAccountId", () => {
  it("prefers the account the list is scoped to", () => {
    expect(composeAccountId(prefs({ defaultAccountId: 7 }), accounts, 9)).toBe(9);
  });

  it("falls back to the preference when nothing is scoped", () => {
    expect(composeAccountId(prefs({ defaultAccountId: 7 }), accounts, null)).toBe(7);
  });

  it("ignores a preference naming an account that has been removed", () => {
    expect(composeAccountId(prefs({ defaultAccountId: 99 }), accounts, null)).toBe(4);
  });

  it("ignores a scope naming an account that has been removed", () => {
    expect(composeAccountId(prefs({ defaultAccountId: 7 }), accounts, 99)).toBe(7);
  });

  it("falls back to the first account, which is what the code did before", () => {
    expect(composeAccountId(DEFAULT_PREFERENCES, accounts, null)).toBe(4);
  });

  it("answers undefined when there are no accounts at all", () => {
    expect(composeAccountId(prefs({ defaultAccountId: 7 }), [], null)).toBeUndefined();
  });
});

describe("loadPreferences", () => {
  it("answers the defaults when there is no backend and no storage", async () => {
    // Node, no window: the same situation as a first launch, and the defaults
    // are a correct answer to it rather than an error to render.
    await expect(loadPreferences()).resolves.toEqual(DEFAULT_PREFERENCES);
  });
});

/* -------------------------------------------------------------------------- */
/* Session — where the window was, which is not a setting                      */
/* -------------------------------------------------------------------------- */

describe("parseSession", () => {
  it("is empty for a first launch, and for garbage", () => {
    for (const raw of [undefined, null, {}, 7, "mail", []]) {
      expect(parseSession(raw)).toEqual({});
    }
  });

  it("reads a whole session back", () => {
    expect(
      parseSession({
        mode: "calendar",
        calendarView: "day",
        accountId: 7,
        labelId: "STARRED",
        listWidth: 480,
        collapsedCalendarAccounts: [1, 4],
      }),
    ).toEqual({
      mode: "calendar",
      calendarView: "day",
      accountId: 7,
      labelId: "STARRED",
      listWidth: 480,
      collapsedCalendarAccounts: [1, 4],
    });
  });

  it("omits a field it cannot believe rather than inventing one", () => {
    // A partial is the point: the caller keeps its own default for anything
    // missing, so this file never has to know what `initialUi` says.
    const parsed = parseSession({ mode: "sideways", labelId: "INBOX" });
    expect(parsed).toEqual({ labelId: "INBOX" });
    expect("mode" in parsed).toBe(false);
  });

  it("keeps the unified stream, which is a null account and not a missing one", () => {
    expect(parseSession({ accountId: null })).toEqual({ accountId: null });
    expect(parseSession({ accountId: "7" })).toEqual({});
  });

  it("clamps a stored width to the same range the reducer allows", () => {
    expect(parseSession({ listWidth: 4000 }).listWidth).toBe(LIST_WIDTH_BOUNDS.max);
    expect(parseSession({ listWidth: 10 }).listWidth).toBe(LIST_WIDTH_BOUNDS.min);
    expect(parseSession({ listWidth: 481.7 }).listWidth).toBe(482);
    expect(parseSession({ listWidth: Number.NaN }).listWidth).toBeUndefined();
  });

  it("drops non-integers out of the collapsed list rather than the whole list", () => {
    expect(
      parseSession({ collapsedCalendarAccounts: [1, "2", null, 4.5, 7] })
        .collapsedCalendarAccounts,
    ).toEqual([1, 7]);
    expect(parseSession({ collapsedCalendarAccounts: 3 }).collapsedCalendarAccounts).toBeUndefined();
  });

  it("rejects an empty label id — 'no mailbox' is not a mailbox", () => {
    expect(parseSession({ labelId: "" }).labelId).toBeUndefined();
  });
});

describe("restoring a session through the reducer", () => {
  // The restore path dispatches ordinary actions rather than a bespoke
  // `restore` case, so the stored values go through exactly the rules a click
  // would. These pin that, without rendering anything.
  it("applies a believable session", () => {
    const session = parseSession({
      mode: "calendar",
      accountId: 7,
      labelId: "STARRED",
      listWidth: 480,
    });

    let ui = initialUi;
    ui = uiReducer(ui, { type: "mode", mode: session.mode! });
    ui = uiReducer(ui, { type: "account", accountId: session.accountId! });
    ui = uiReducer(ui, { type: "label", labelId: session.labelId! });
    ui = uiReducer(ui, { type: "listWidth", width: session.listWidth! });

    expect(ui.mode).toBe("calendar");
    expect(ui.accountId).toBe(7);
    expect(ui.labelId).toBe("STARRED");
    expect(ui.listWidth).toBe(480);
  });

  it("cannot put a width on screen that a drag could not", () => {
    // Belt and braces: `parseSession` clamps, and the reducer clamps again.
    // Either alone would do; both means a future caller that skips one is
    // still safe.
    expect(uiReducer(initialUi, { type: "listWidth", width: 4000 }).listWidth).toBe(
      LIST_WIDTH_BOUNDS.max,
    );
    expect(uiReducer(initialUi, { type: "listWidth", width: -1 }).listWidth).toBe(
      LIST_WIDTH_BOUNDS.min,
    );
  });

  it("leaves the defaults standing for everything the session did not name", () => {
    const session = parseSession({ mode: "calendar" });
    const ui = uiReducer(initialUi, { type: "mode", mode: session.mode! });
    expect(ui.labelId).toBe(initialUi.labelId);
    expect(ui.listWidth).toBe(initialUi.listWidth);
  });
});

describe("loadSession", () => {
  it("answers nothing stored when there is no backend and no storage", async () => {
    await expect(loadSession()).resolves.toEqual({});
  });
});
