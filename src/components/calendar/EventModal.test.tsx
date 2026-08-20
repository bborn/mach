// @vitest-environment jsdom

/**
 * The two things the editor owes the guests.
 *
 * Both come out of the same discovery: Google's calendar API tells nobody
 * anything unless the request says `sendUpdates`, and Mach never said it. The
 * command layer is where that is fixed, and the assertions about the request
 * that reaches Google live beside it in `src-tauri/tests/commands.rs`. What only
 * the interface can promise is here.
 *
 * **The question gets asked.** Google Calendar puts a dialog in front of a
 * change to a meeting, because both answers are things people want: a moved
 * meeting has to reach the room, a typo fixed in the notes of a thirty-person
 * standup does not. Picking either one silently would be the software making a
 * decision that costs somebody else's inbox.
 *
 * **The call can be asked for.** Google mints the Meet code, so "add a Meet
 * link" has to be a request rather than a field you paste into — and until this
 * row existed, making one meant leaving for Google Calendar.
 *
 * Driven in jsdom rather than rendered as static markup, which this modal cannot
 * be: the form is built in an effect, so `renderToStaticMarkup` returns an empty
 * string. Nothing here depends on layout.
 */

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { anyPopupOpen, resetPopups, setPopupOpen } from "@/lib/popups";
import type { Account, Calendar, CalendarEvent } from "@/types";
import { KeymapProvider } from "@/hooks/useKeymap";
import type { EventForm } from "@/lib/calendar-edit";
import type { EventScope } from "@/lib/data";
import { EventModal, type EventModalProps } from "./EventModal";

declare global {
  var IS_REACT_ACT_ENVIRONMENT: boolean | undefined;
}

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  globalThis.IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
  resetPopups();
  globalThis.IS_REACT_ACT_ENVIRONMENT = undefined;
});

const NINE = new Date(2026, 7, 7, 9, 0, 0, 0).getTime();
const HALF_HOUR = 30 * 60_000;

const ACCOUNT = { id: 1, email: "bruno@example.com", name: "Bruno" } as Account;
const CALENDAR: Calendar = { id: "primary", accountId: 1, name: "Bruno", colorIndex: 1 };

const MEETING: CalendarEvent = {
  id: 1,
  calendarId: "primary",
  accountId: 1,
  title: "Board call",
  start: NINE,
  end: NINE + HALF_HOUR,
  allDay: false,
  attendees: [
    { name: "Ada", email: "ada@example.com" },
    { name: "Bob", email: "bob@example.com" },
  ],
};

function mount(over: Partial<EventModalProps> = {}) {
  const props: EventModalProps = {
    target: { mode: "view", event: MEETING },
    calendars: [CALENDAR],
    accounts: [ACCOUNT],
    colorFor: () => "#4f8ef7",
    dark: false,
    merged: null,
    defaultCalendarId: "primary",
    recurring: false,
    canEdit: true,
    error: null,
    busy: false,
    onClose: () => {},
    onSave: () => {},
    onDelete: () => {},
    onDuplicate: () => {},
    onRsvp: () => {},
    onOpenExternal: () => {},
    ...over,
  };
  act(() => {
    root.render(
      <KeymapProvider>
        <EventModal {...props} />
      </KeymapProvider>,
    );
  });
}

/**
 * Every control whose label starts with this text, in document order.
 *
 * `startsWith` rather than equality because Save carries its own ⌘⏎ chip, which
 * is part of the button's text and not part of its name.
 */
function buttons(label: string): HTMLButtonElement[] {
  return [...document.querySelectorAll("button")].filter((b) =>
    (b.textContent ?? "").trim().startsWith(label),
  ) as HTMLButtonElement[];
}

/**
 * The checkboxes on screen — the visible ones.
 *
 * Base UI pairs each with a hidden native input for form submission; the thing a
 * keyboard and a screen reader meet is the `<button role="checkbox">`.
 */
function checkboxes(): HTMLButtonElement[] {
  return [...document.querySelectorAll('[role="checkbox"]')] as HTMLButtonElement[];
}

function click(label: string) {
  const [button] = buttons(label);
  if (!button) throw new Error(`no button labelled “${label}”`);
  act(() => button.click());
}

function type(selector: string, value: string) {
  const field = document.querySelector(selector) as HTMLInputElement | HTMLTextAreaElement;
  if (!field) throw new Error(`no field matching ${selector}`);
  act(() => {
    const setter = Object.getOwnPropertyDescriptor(
      field instanceof HTMLTextAreaElement
        ? HTMLTextAreaElement.prototype
        : HTMLInputElement.prototype,
      "value",
    )!.set!;
    setter.call(field, value);
    field.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

function text(): string {
  return document.body.textContent ?? "";
}

function press(key: string, at: EventTarget = document.activeElement ?? window) {
  act(() => {
    at.dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true }));
  });
}

describe("Escape", () => {
  it("does not think a popup is open just because the form has selects", () => {
    mount();
    expect(anyPopupOpen()).toBe(false);
  });

  it("closes from the title field", () => {
    const onClose = vi.fn();
    mount({ onClose });
    const title = document.querySelector('input[aria-label="Title"]') as HTMLInputElement;
    expect(title).toBeTruthy();
    act(() => title.focus());
    press("Escape", title);
    expect(onClose).toHaveBeenCalled();
  });

  it("closes even if a popup claimed itself open without being on screen", () => {
    const onClose = vi.fn();
    mount({ onClose });
    setPopupOpen("leaked", true);
    const title = document.querySelector('input[aria-label="Title"]') as HTMLInputElement;
    act(() => title.focus());
    press("Escape", title);
    expect(onClose).toHaveBeenCalled();
  });
});

describe("telling the guests", () => {
  it("asks before emailing them, and sends what was asked for", () => {
    const saved: { form: EventForm; scope: EventScope }[] = [];
    mount({ onSave: (form, scope) => saved.push({ form, scope }) });

    type('input[aria-label="Title"]', "Board call — moved");
    click("Save");

    // Nothing has gone anywhere yet: the question is the gate.
    expect(saved).toHaveLength(0);
    expect(text()).toContain("Email 2 guests about this change?");

    click("Send");
    expect(saved).toHaveLength(1);
    expect(saved[0].form.notify).toBe("guests");
    expect(saved[0].form.title).toBe("Board call — moved");
  });

  it("takes “Don't send” for an answer", () => {
    const saved: EventForm[] = [];
    mount({ onSave: (form) => saved.push(form) });

    type('input[aria-label="Title"]', "Board call (short)");
    click("Save");
    click("Don't send");

    expect(saved).toHaveLength(1);
    expect(saved[0].notify).toBe("nobody");
  });

  it("goes back to the form rather than saving when the answer is neither", () => {
    const saved: EventForm[] = [];
    mount({ onSave: (form) => saved.push(form) });

    type('input[aria-label="Title"]', "Board call (short)");
    click("Save");
    click("Back");

    expect(saved).toHaveLength(0);
    expect(text()).not.toContain("Email 2 guests");
    // Still holding what was typed.
    expect((document.querySelector('input[aria-label="Title"]') as HTMLInputElement).value).toBe(
      "Board call (short)",
    );
  });

  it("does not ask about an event nobody else is in", () => {
    const saved: EventForm[] = [];
    mount({
      target: { mode: "view", event: { ...MEETING, title: "Gym", attendees: [] } },
      onSave: (form) => saved.push(form),
    });

    type('input[aria-label="Title"]', "Gym, later");
    click("Save");

    expect(saved).toHaveLength(1);
    expect(text()).not.toContain("about this change?");
  });

  it("says what deleting a meeting does to its guests, before it happens", () => {
    mount();
    click("Delete");
    // First press arms it; the count is what makes the second press informed.
    expect(text()).toContain("Really delete");
    expect(text()).toContain("Cancels with 2 guests");
  });
});

describe("the call", () => {
  it("offers a Meet request as a real, labelled control", () => {
    mount();
    expect(text()).toContain("Google Meet");
    // Reachable and announced: the primitive draws `role="checkbox"` with a tab
    // stop rather than a styled div you can only reach with a mouse. That is the
    // whole reason `components/ui` exists, and the easiest thing to lose here —
    // a `<div onClick>` renders identically.
    const boxes = checkboxes();
    expect(boxes.length).toBeGreaterThan(0);
    expect(boxes.every((b) => b.tabIndex === 0)).toBe(true);
    // The event has no call, so nothing is asking for one yet.
    expect(boxes.some((b) => b.getAttribute("aria-checked") === "true")).toBe(false);
  });

  it("is not offered on somebody else's event, where it could only 403", () => {
    mount({ canEdit: false });
    expect(text()).not.toContain("Google Meet");
  });

  it("says what removing an existing call costs, since Google cannot reissue it", () => {
    mount({
      target: {
        mode: "view",
        event: {
          ...MEETING,
          conference: {
            id: "abc-defg-hij",
            name: "Google Meet",
            entryPoints: [{ kind: "video", uri: "https://meet.google.com/abc-defg-hij" }],
          },
        },
      },
    });
    expect(text()).toContain("Join Google Meet");

    const [box] = checkboxes().filter((b) => b.getAttribute("aria-checked") === "true");
    expect(box, "the Meet tick starts on for an event that has a call").toBeDefined();
    act(() => box.click());
    expect(text()).toContain("Removing the call retires this link");
  });
});

/**
 * The address, as somewhere you can be shown.
 *
 * The location was inert text: select it, copy it, paste it into Maps. What is
 * checked here is that the control appears exactly where an address is and
 * nowhere else, that it is a real button in the tab order rather than a click
 * handler on a span, and that what it opens is a URL this app built rather than
 * the attacker-controlled text it was built from.
 */
describe("the address", () => {
  const ADDRESS = "Twin Ignition Startup Garage, 1317 Marshall St NE, Minneapolis, MN 55413, USA";

  it("offers a map on an event with a street address", () => {
    const opened: string[] = [];
    mount({
      target: { mode: "view", event: { ...MEETING, location: ADDRESS } },
      onOpenExternal: (url) => opened.push(url),
    });

    const [map] = buttons("Map");
    expect(map, "an address gets a Map control").toBeDefined();
    // Keyboard-reachable by being an ordinary button: no negative tabindex, no
    // `<div onClick>`. ⇥ out of the location field lands on it.
    expect(map!.tagName).toBe("BUTTON");
    expect(map!.tabIndex).toBeGreaterThanOrEqual(0);

    click("Map");
    expect(opened).toEqual([
      "https://www.google.com/maps/search/?api=1&query=" +
        "Twin%20Ignition%20Startup%20Garage%2C%201317%20Marshall%20St%20NE%2C" +
        "%20Minneapolis%2C%20MN%2055413%2C%20USA",
    ]);
  });

  it("offers nothing on a location no map can take", () => {
    // A fresh id each time, or the modal keeps the form it already built —
    // it keys on the event so a background sync cannot clear a field mid-edit.
    for (const [id, location] of [undefined, "", "Room 2"].entries()) {
      mount({ target: { mode: "view", event: { ...MEETING, id: id + 1, location } } });
      expect(buttons("Map")).toHaveLength(0);
      // The location is still in the field — it is the answer to "where", just
      // not one a map can show.
      const field = document.querySelector('input[aria-label="Location"]') as HTMLInputElement;
      expect(field.value).toBe(location ?? "");
    }
  });

  it("leaves a conference link in the location to the Join button it already has", () => {
    mount({
      target: {
        mode: "view",
        event: { ...MEETING, location: "https://meet.google.com/abc-defg-hij" },
      },
    });
    expect(buttons("Map")).toHaveLength(0);
    expect(text()).toContain("Join Google Meet");
  });

  it("never navigates to the location text, however it is shaped", () => {
    const opened: string[] = [];
    // Both halves: a bare scheme gets no control at all, and a payload smuggled
    // into something address-shaped still comes out as an https Maps search.
    mount({
      target: { mode: "view", event: { ...MEETING, location: "javascript:alert(1)" } },
      onOpenExternal: (url) => opened.push(url),
    });
    expect(buttons("Map")).toHaveLength(0);

    // A different id, or the form is not rebuilt: the modal keys its state on
    // the event it points at, so that a background sync cannot clear a field
    // mid-edit.
    mount({
      target: {
        mode: "view",
        event: { ...MEETING, id: 2, location: "123 Main Street, javascript:alert(1)" },
      },
      onOpenExternal: (url) => opened.push(url),
    });
    click("Map");
    expect(opened).toHaveLength(1);
    const parsed = new URL(opened[0]!);
    expect(parsed.protocol).toBe("https:");
    expect(parsed.hostname).toBe("www.google.com");
    expect(opened[0]).not.toContain("javascript:");
  });

  it("finds the map as the address is typed, without waiting for a save", () => {
    mount({ target: { mode: "create", start: NINE, end: NINE + HALF_HOUR } });
    expect(buttons("Map")).toHaveLength(0);

    type('input[aria-label="Location"]', ADDRESS);
    expect(buttons("Map")).toHaveLength(1);
  });
});
