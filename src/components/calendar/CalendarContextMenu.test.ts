/**
 * The calendar's context menu, tested where it is decided.
 *
 * The same split as `ThreadContextMenu.test.ts`: everything this menu promises
 * is a property of three pure functions — which items exist for an event, which
 * exist for empty grid, and whether ⇧F10 is this key's business at all — and
 * none of it needs a DOM. What is checked here is that no item exists without a
 * live binding behind it, that the keys printed beside one are that binding's
 * own, that choosing one calls the verb `CalendarMode` already has rather than
 * a copy of it, and that a calendar you can only read is never offered a write.
 */

import { describe, expect, it, vi } from "vitest";
import type { CalendarEvent } from "@/types";
import type { KeyBinding } from "@/lib/keymap";
import {
  buildEventItems,
  buildSlotItems,
  keyboardTarget,
  type CalendarVerbs,
  type Item,
} from "./CalendarContextMenu";

function event(over: Partial<CalendarEvent> = {}): CalendarEvent {
  return {
    id: 1,
    calendarId: "work@example.com",
    accountId: 1,
    title: "Scope review",
    start: Date.UTC(2026, 7, 12, 16, 0),
    end: Date.UTC(2026, 7, 12, 16, 30),
    allDay: false,
    attendees: [],
    ...over,
  };
}

/** The bindings `CalendarMode` registers, as the registry has them. */
function registry(over: Partial<Record<string, boolean>> = {}): KeyBinding[] {
  const all: KeyBinding[] = [
    { keys: "e", group: "Event", description: "Open the event", handler: () => {} },
    { keys: "backspace", group: "Event", description: "Delete the event", handler: () => {} },
    { keys: "mod+c", group: "Event", description: "Copy the event", handler: () => {} },
    { keys: "shift+d", group: "Event", description: "Duplicate the event", handler: () => {} },
    { keys: "o", group: "Event", description: "Open in Google Calendar", handler: () => {} },
    { keys: "shift+m", group: "Event", description: "Map the address", handler: () => {} },
    { keys: "shift+c", group: "Event", description: "Create — full editor", handler: () => {} },
  ];
  return all.filter((b) => over[b.description!] !== false);
}

function verbs(): CalendarVerbs {
  return {
    open: vi.fn(),
    remove: vi.fn(),
    duplicate: vi.fn(),
    copy: vi.fn(),
    openInGoogle: vi.fn(),
    openMap: vi.fn(),
    rsvp: vi.fn(),
    createAt: vi.fn(),
  };
}

function labels(items: Item[]): string[] {
  return items.filter((i) => i.kind === "item").map((i) => (i as { label: string }).label);
}

function byLabel(items: Item[], label: string) {
  const found = items.find((i) => i.kind === "item" && i.label === label);
  return found as Extract<Item, { kind: "item" }> | undefined;
}

const writable = (over: Partial<Parameters<typeof buildEventItems>[2]> = {}) => ({
  canEdit: true,
  canCreate: true,
  verbs: verbs(),
  ...over,
});

describe("buildEventItems", () => {
  it("offers the things the calendar already does to an event", () => {
    const items = buildEventItems(registry(), event(), writable());

    expect(labels(items)).toEqual([
      "Open",
      "Copy",
      "Duplicate",
      "Open in Google Calendar",
      "Delete",
    ]);
  });

  it("withholds every write on a calendar that can only be read", () => {
    // `canEditEvent` says no for `reader`/`freeBusyReader`, and nothing may be
    // created on such a calendar either — so the copy that would land back on
    // it goes too. What is left writes nothing.
    const items = buildEventItems(
      registry(),
      event(),
      writable({ canEdit: false, canCreate: false }),
    );

    expect(labels(items)).toEqual(["Open", "Copy", "Open in Google Calendar"]);
    expect(labels(items)).not.toContain("Delete");
    expect(labels(items)).not.toContain("Duplicate");
  });

  it("separates the two halves of read-only: a stranger's invite can still be copied", () => {
    // Not the same question. `canEditEvent` is about this event; whether a
    // duplicate can land is about the calendar under it.
    const items = buildEventItems(registry(), event(), writable({ canEdit: false }));

    expect(labels(items)).toContain("Duplicate");
    expect(labels(items)).not.toContain("Delete");
  });

  it("answers an invitation, and only an invitation", () => {
    const invited = buildEventItems(registry(), event({ rsvp: "needsAction" }), writable());
    expect(labels(invited)).toEqual([
      "Open",
      "Going",
      "Maybe",
      "Not going",
      "Copy",
      "Duplicate",
      "Open in Google Calendar",
      "Delete",
    ]);

    // No `rsvp` at all is an event you own. There is nothing to answer.
    const own = buildEventItems(registry(), event(), writable());
    expect(labels(own)).not.toContain("Going");
  });

  it("disables the answer already given rather than hiding it", () => {
    const items = buildEventItems(registry(), event({ rsvp: "accepted" }), writable());

    expect(byLabel(items, "Going")?.disabled).toBe(true);
    expect(byLabel(items, "Maybe")?.disabled).toBe(false);
    expect(byLabel(items, "Not going")?.disabled).toBe(false);
  });

  it("omits an item whose binding is not live rather than offering a dead one", () => {
    const items = buildEventItems(registry({ "Delete the event": false }), event(), writable());

    expect(labels(items)).not.toContain("Delete");
    expect(labels(items)).toContain("Open");
  });

  it("prints the binding's own keys, so the menu cannot drift from the keymap", () => {
    const moved: KeyBinding[] = [
      { keys: "mod+shift+o", group: "Event", description: "Open the event", handler: () => {} },
    ];
    const items = buildEventItems(moved, event(), writable());

    expect(byLabel(items, "Open")?.shortcut).toBe("mod+shift+o");
  });

  it("runs the verb the keyboard runs, on the event the menu is about", () => {
    const on = verbs();
    const target = event({ id: 7 });
    const items = buildEventItems(registry(), target, writable({ verbs: on }));

    byLabel(items, "Delete")?.run();
    byLabel(items, "Open")?.run();
    byLabel(items, "Duplicate")?.run();
    byLabel(items, "Open in Google Calendar")?.run();

    expect(on.remove).toHaveBeenCalledWith(target);
    expect(on.open).toHaveBeenCalledWith(target);
    expect(on.duplicate).toHaveBeenCalledWith(target);
    expect(on.openInGoogle).toHaveBeenCalledWith(target);
  });

  it("offers a map only where there is an address to map", () => {
    const withAddress = event({
      location: "Twin Ignition Startup Garage, 1317 Marshall St NE, Minneapolis, MN 55413, USA",
    });
    expect(labels(buildEventItems(registry(), withAddress, writable()))).toContain("Map");

    // A room, a call, and nothing at all — three locations a map cannot take.
    for (const location of [undefined, "Room 2", "https://meet.google.com/abc-defg-hij"]) {
      expect(labels(buildEventItems(registry(), event({ location }), writable()))).not.toContain(
        "Map",
      );
    }
  });

  it("hands the map the event the menu is about, through the verb", () => {
    const on = verbs();
    const target = event({ id: 9, location: "1317 Marshall St NE, Minneapolis, MN 55413" });
    const items = buildEventItems(registry(), target, writable({ verbs: on }));

    byLabel(items, "Map")?.run();
    expect(on.openMap).toHaveBeenCalledWith(target);
    expect(byLabel(items, "Map")?.shortcut).toBe("shift+m");
  });

  it("hands the RSVP the response its label names", () => {
    const on = verbs();
    const target = event({ rsvp: "needsAction" });
    const items = buildEventItems(registry(), target, writable({ verbs: on }));

    byLabel(items, "Not going")?.run();
    expect(on.rsvp).toHaveBeenCalledWith(target, "declined");
  });

  it("never rules off nothing", () => {
    for (const items of [
      buildEventItems(registry(), event({ rsvp: "tentative" }), writable()),
      buildEventItems(registry(), event(), writable({ canEdit: false, canCreate: false })),
      buildEventItems(
        registry({ "Open the event": false, "Copy the event": false }),
        event(),
        writable(),
      ),
    ]) {
      expect(items[0]?.kind).toBe("item");
      expect(items[items.length - 1]?.kind).toBe("item");
      for (let i = 1; i < items.length; i++) {
        expect(items[i]?.kind === "separator" && items[i - 1]?.kind === "separator").toBe(false);
      }
    }
  });
});

describe("buildSlotItems", () => {
  const slot = { start: new Date(2026, 7, 12, 10, 30).getTime(), end: 0 };

  it("offers a new event at the time under the pointer", () => {
    const items = buildSlotItems(registry(), slot, verbs());

    expect(labels(items)).toEqual(["New event at 10:30a"]);
    expect(byLabel(items, "New event at 10:30a")?.shortcut).toBe("shift+c");
  });

  it("creates into that slot, through the path drag-to-create already uses", () => {
    const on = verbs();
    byLabel(buildSlotItems(registry(), slot, on), "New event at 10:30a")?.run();

    expect(on.createAt).toHaveBeenCalledWith(slot);
  });

  it("offers nothing when creating is not a live binding", () => {
    expect(buildSlotItems(registry({ "Create — full editor": false }), slot, verbs())).toEqual([]);
  });
});

describe("keyboardTarget", () => {
  const state = {
    mode: "calendar",
    eventId: 3 as number | null,
    active: true,
    menuOpen: false,
    overlays: 0,
  };

  it("opens on the selected event", () => {
    expect(keyboardTarget(state)).toBe(3);
  });

  it("does nothing without a selection", () => {
    expect(keyboardTarget({ ...state, eventId: null })).toBeNull();
  });

  it("yields the key while a dialog owns the keyboard", () => {
    expect(keyboardTarget({ ...state, overlays: 1 })).toBeNull();
  });

  it("yields the key to whatever else is up", () => {
    // `active` is `CalendarMode`'s own gate — the palette, the modal, the
    // quick-create bar and the type-to-select finder all take it away.
    expect(keyboardTarget({ ...state, active: false })).toBeNull();
    expect(keyboardTarget({ ...state, mode: "mail" })).toBeNull();
    // And a menu that is already up is not asked to open a second one.
    expect(keyboardTarget({ ...state, menuOpen: true })).toBeNull();
  });
});
