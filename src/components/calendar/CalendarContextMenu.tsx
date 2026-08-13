import { useCallback, useEffect, useRef, useState, type MouseEvent, type ReactNode } from "react";
import type { CalendarEvent, EventId, Rsvp } from "@/types";
import { useKeyBindings, useKeymap } from "@/hooks/useKeymap";
import { overlayOwnsKeyboard, useMach } from "@/hooks/useMach";
import type { KeyBinding } from "@/lib/keymap";
import {
  DEFAULT_EVENT_MINUTES,
  HOUR_HEIGHT,
  clamp,
  snapTimeDown,
  timeForOffset,
} from "@/lib/calendar-geometry";
import { nextSlot } from "@/lib/calendar-edit";
import { mapsUrl } from "@/lib/calendar-links";
import { MINUTE, shortTime, startOfDay } from "@/lib/time";
import {
  ContextMenu,
  ContextMenuItem,
  ContextMenuSeparator,
} from "@/components/ui/context-menu";

/**
 * Right-click on the calendar — on an event, or on the empty grid.
 *
 * # It is the mail menu's idiom, not a second one
 *
 * `ThreadContextMenu` is the reference and this follows it line for line: the
 * same `ui/context-menu` primitive, the same anchor-or-pointer trick, the same
 * ⇧F10 and Menu-key pair opening it on whatever the keyboard is already on, the
 * same rule that **an item whose binding is not live does not appear**. The menu
 * is built from a snapshot of `keymap.active()` taken at the moment of the
 * gesture, so it can only ever offer what the keyboard could have done from the
 * same position, and the keys printed beside the items are the bindings' own.
 *
 * # The one place it diverges, and why
 *
 * The mail menu runs `binding.handler` itself — the keystroke by another route.
 * That works there because every mail command resolves its targets when it is
 * *called*, out of `commandTargets`. The calendar's handlers do not: they close
 * over `selectedEvent` from the render they were built in (see `withEvent` in
 * `CalendarMode`), and the snapshot here is taken in a pointer handler, before
 * the selection this menu is about has been committed by React. Running the
 * binding's handler would therefore act on whatever was selected *before* the
 * right-click — deleting the wrong meeting, which is the one failure this
 * surface may not have.
 *
 * So an item is still *gated* on a live binding and still prints that binding's
 * keys, but it calls the verb with the event in its hand. The verbs come from
 * `CalendarMode` and are the same functions its bindings call, so there is still
 * exactly one implementation of each; only the target is stated rather than
 * inferred.
 *
 * # What it is about
 *
 * A right-click on a block makes that block the selection, the way the mail menu
 * makes a row the selection, so the menu and the keyboard agree about what the
 * next command acts on. Dismissing it without choosing anything puts the
 * previous cursor back.
 *
 * A right-click on empty grid offers the one thing empty grid is for: a new
 * event at the time under the pointer. The time comes from the same
 * `timeForOffset`/`snapTimeDown` pair the drag-to-create gesture uses, off the
 * column's own `data-day-start`.
 *
 * # What a read-only calendar does not get
 *
 * `canEditEvent` is the whole decision, and `CalendarMode` has it already —
 * a `reader` or `freeBusyReader` subscription refuses every write whoever
 * organized the event. Delete is absent on one. So is Duplicate, which is a
 * *create* on the same calendar and would 403 a round trip later. Open, Copy
 * (which only fills Mach's own clipboard) and the Google Calendar link stay:
 * none of them writes anything.
 */

/** Where the popup hangs: a block element, or the point the pointer was at. */
type Anchor = Element | { getBoundingClientRect: () => DOMRect };

/** A stretch of empty grid, as the create path wants it. */
export interface Slot {
  start: number;
  end: number;
}

/**
 * The verbs, as `CalendarMode` performs them.
 *
 * Every one of these is the function that surface's own key bindings call. The
 * menu is a second way to reach them and never a second copy.
 */
export interface CalendarVerbs {
  open: (event: CalendarEvent) => void;
  /** Delete, asking about the series first when the event repeats. */
  remove: (event: CalendarEvent) => void;
  duplicate: (event: CalendarEvent) => void;
  /** Mach's own clipboard, for ⌘V onto another day. */
  copy: (event: CalendarEvent) => void;
  openInGoogle: (event: CalendarEvent) => void;
  /** The address on the event, on a map. Says so when there is not one. */
  openMap: (event: CalendarEvent) => void;
  rsvp: (event: CalendarEvent, response: Rsvp) => void;
  createAt: (slot: Slot) => void;
}

interface OpenMenu {
  anchor: Anchor;
  /** Resolved once, at the moment it was asked for — see the note above. */
  items: Item[];
  label: string;
  /**
   * The cursor to put back if the menu is dismissed without a choice, or null
   * when the right-click did not move it.
   */
  restore: { to: EventId | null } | null;
}

/**
 * A menu that has been asked for but not yet built.
 *
 * The gesture and the snapshot are deliberately one render apart. A right-click
 * *while a menu is already up* arrives with that menu's keyboard claim still in
 * force — the outside-press has closed it in React's eyes, but the release runs
 * in a layout effect on the next commit — and `keymap.active()` asked in that
 * window answers "almost nothing", so the second menu comes up holding only the
 * items that need no binding. Parking the request and resolving it once `menu`
 * is back to null puts the snapshot after the release, every time, with no
 * timer to get wrong.
 */
type Request =
  | { kind: "event"; event: CalendarEvent; anchor: Anchor }
  | { kind: "slot"; slot: Slot; anchor: Anchor };

export interface CalendarContextMenuProps {
  /** `CalendarMode`'s own gate: calendar mode, no palette, no dialog. */
  active: boolean;
  eventById: (id: EventId) => CalendarEvent | undefined;
  /** `canEditEvent`, as `CalendarMode` already computes it. */
  canEdit: (event: CalendarEvent) => boolean;
  /** Whether the event's calendar would accept a new event on it. */
  canCreateOn: (event: CalendarEvent) => boolean;
  verbs: CalendarVerbs;
  children: ReactNode;
}

export function CalendarContextMenu({
  active,
  eventById,
  canEdit,
  canCreateOn,
  verbs,
  children,
}: CalendarContextMenuProps) {
  const { ui, dispatch } = useMach();
  const keymap = useKeymap();
  const [menu, setMenu] = useState<OpenMenu | null>(null);
  const [pending, setPending] = useState<Request | null>(null);
  const chosen = useRef(false);
  const returnTo = useRef<HTMLElement | null>(null);

  /** Ask for a menu. Whatever is up yields, and its keyboard claim with it. */
  const request = useCallback((next: Request) => {
    setMenu(null);
    setPending(next);
  }, []);

  /**
   * Build the menu that was asked for, now that nothing is claiming the
   * keyboard, and put the cursor where the menu is about to be about.
   */
  useEffect(() => {
    if (pending === null || menu !== null) return;
    setPending(null);

    const bindings = keymap.active();
    if (pending.kind === "slot") {
      const items = buildSlotItems(bindings, pending.slot, verbs);
      if (items.length === 0) return;
      returnTo.current = document.activeElement as HTMLElement | null;
      chosen.current = false;
      setMenu({ anchor: pending.anchor, items, label: "Empty time", restore: null });
      return;
    }

    const { event } = pending;
    const items = buildEventItems(bindings, event, {
      canEdit: canEdit(event),
      canCreate: canCreateOn(event),
      verbs,
    });
    // An empty menu is not a menu. Bail before anything is dispatched, so a
    // right-click with nothing to offer also has no side effect.
    if (items.length === 0) return;

    const restore = ui.eventId === event.id ? null : { to: ui.eventId };
    if (restore) dispatch({ type: "event", eventId: event.id });

    returnTo.current = document.activeElement as HTMLElement | null;
    chosen.current = false;
    setMenu({ anchor: pending.anchor, items, label: event.title, restore });
  }, [pending, menu, keymap, verbs, canEdit, canCreateOn, dispatch, ui.eventId]);

  const onContextMenu = useCallback(
    (pointer: MouseEvent) => {
      if (!active) return;
      const target = pointer.target as Element | null;
      const { clientX, clientY } = pointer;
      const anchor = { getBoundingClientRect: () => new DOMRect(clientX, clientY, 0, 0) };

      const block = target?.closest?.("[data-event-id]");
      if (block) {
        const id = Number(block.getAttribute("data-event-id"));
        const event = Number.isFinite(id) ? eventById(id) : undefined;
        if (!event) return;
        pointer.preventDefault();
        request({ kind: "event", event, anchor });
        return;
      }

      const slot = slotUnder(target, clientY);
      if (!slot) return;
      pointer.preventDefault();
      request({ kind: "slot", slot, anchor });
    },
    [active, eventById, request],
  );

  /* ------------------------------------------------------------- keyboard --- */

  const target = () =>
    keyboardTarget({
      mode: ui.mode,
      eventId: ui.eventId,
      active,
      menuOpen: menu !== null || pending !== null,
      overlays: ui.overlays,
    });

  const openAtCursor = () => {
    const id = target();
    if (id === null) return;
    const event = eventById(id);
    // Anchored to the block itself rather than to a pointer that was never
    // there — the same thing ⇧F10 does in the conversation list.
    const block = document.querySelector(`[data-event-id="${id}"]`);
    if (!event || !block) return;
    request({ kind: "event", event, anchor: block });
  };

  useKeyBindings([
    {
      keys: "shift+f10",
      group: "Event",
      description: "Menu for the event",
      when: () => target() !== null,
      handler: openAtCursor,
    },
    {
      // The dedicated Menu key, where a keyboard has one. Undocumented for the
      // same reason the mail menu leaves it out: `formatBinding` has no glyph
      // for it, and "CONTEXTMENU" in the key column would be worse than nothing.
      keys: "contextmenu",
      when: () => target() !== null,
      handler: openAtCursor,
    },
  ]);

  /* ------------------------------------------------------------------ menu --- */

  const close = useCallback(
    (next: boolean) => {
      if (next) return;
      // Dismissed rather than used: the cursor move the menu made to aim itself
      // was never something the user asked for, so it goes away with it.
      if (menu?.restore && !chosen.current) {
        dispatch({ type: "event", eventId: menu.restore.to });
      }
      setMenu(null);
    },
    [dispatch, menu],
  );

  const items = menu?.items ?? [];

  return (
    <>
      <div className="flex min-h-0 min-w-0 flex-1" onContextMenu={onContextMenu}>
        {children}
      </div>
      <ContextMenu
        open={menu !== null}
        onOpenChange={close}
        anchor={menu?.anchor ?? null}
        finalFocus={returnTo}
        label={menu?.label}
      >
        {items.map((item) =>
          item.kind === "separator" ? (
            <ContextMenuSeparator key={item.key} />
          ) : (
            <ContextMenuItem
              key={item.key}
              shortcut={item.shortcut}
              tone={item.tone}
              disabled={item.disabled}
              onClick={() => {
                chosen.current = true;
                item.run();
              }}
            >
              {item.label}
            </ContextMenuItem>
          ),
        )}
      </ContextMenu>
    </>
  );
}

/* -------------------------------------------------------------------------- */
/* What the keyboard can open                                                  */
/* -------------------------------------------------------------------------- */

export interface KeyboardState {
  mode: string;
  eventId: EventId | null;
  /** `CalendarMode`'s gate: no palette, no modal, no quick-create, no finder. */
  active: boolean;
  menuOpen: boolean;
  /** `ui.overlays` — a dialog above the calendar owns the keyboard. */
  overlays: number;
}

/**
 * The event ⇧F10 would open the menu on, or null for "not this key's business".
 *
 * A gate rather than a boolean so the handler cannot disagree with the `when`
 * that let it run. Null with nothing selected is the point: a menu about no
 * event is a menu about nothing, and the key should fall through rather than
 * pop an empty popup at the top-left of the grid.
 */
export function keyboardTarget(state: KeyboardState): EventId | null {
  if (state.mode !== "calendar") return null;
  if (!state.active) return null;
  if (state.menuOpen) return null;
  if (overlayOwnsKeyboard({ overlays: state.overlays })) return null;
  return state.eventId;
}

/* -------------------------------------------------------------------------- */
/* The empty grid, as a time                                                   */
/* -------------------------------------------------------------------------- */

/**
 * The slot under the pointer, or null if the pointer is not over open grid.
 *
 * `data-day-start` is the time grid's own drag-to-create surface — the same
 * element, and the same arithmetic, that a drag on it would use. `data-day-cell`
 * is a month cell, which has a day but no time: it takes the hour
 * `defaultSlot` picks for a keyboard-created event, which is the next clean half
 * hour on today and nine in the morning on any other day.
 */
export function slotUnder(target: Element | null | undefined, clientY: number): Slot | null {
  const length = DEFAULT_EVENT_MINUTES * MINUTE;

  const column = target?.closest?.("[data-day-start]");
  if (column) {
    const dayStart = Number(column.getAttribute("data-day-start"));
    if (!Number.isFinite(dayStart)) return null;
    const rect = column.getBoundingClientRect();
    const offset = clamp(clientY - rect.top, 0, 24 * HOUR_HEIGHT);
    const start = snapTimeDown(timeForOffset(offset, dayStart));
    return { start, end: start + length };
  }

  const cell = target?.closest?.("[data-day-cell]");
  if (cell) {
    const dayStart = Number(cell.getAttribute("data-day-cell"));
    if (!Number.isFinite(dayStart)) return null;
    const now = Date.now();
    const start =
      startOfDay(dayStart).getTime() === startOfDay(now).getTime()
        ? nextSlot(now)
        : dayStart + 9 * 60 * MINUTE;
    return { start, end: start + length };
  }

  return null;
}

/* -------------------------------------------------------------------------- */
/* Turning the registry into a menu                                            */
/* -------------------------------------------------------------------------- */

export type Item =
  | { kind: "separator"; key: string }
  | {
      kind: "item";
      key: string;
      label: string;
      shortcut?: string;
      tone?: "default" | "danger";
      /** Offered, but nothing to do — the RSVP you have already given. */
      disabled?: boolean;
      run: () => void;
    };

/**
 * The one place a group-and-description pair is written down.
 *
 * A lookup into the registry, not a description of an action: if the pair stops
 * matching a live binding — renamed, regrouped, deleted — the item simply does
 * not appear, which is the failure mode worth having. The alternative is a menu
 * that offers something and does nothing.
 */
function find(
  bindings: readonly KeyBinding[],
  group: string,
  description: string,
): KeyBinding | undefined {
  return bindings.find((b) => b.group === group && b.description === description);
}

export interface EventMenuOptions {
  /** `canEditEvent` — false for a read-only calendar or a stranger's invite. */
  canEdit: boolean;
  /** Whether the event's calendar would accept a copy of it. */
  canCreate: boolean;
  verbs: CalendarVerbs;
}

/** What the three RSVP items say, in the order Google offers them. */
const RSVP_LABELS: [Rsvp, string][] = [
  ["accepted", "Going"],
  ["tentative", "Maybe"],
  ["declined", "Not going"],
];

export function buildEventItems(
  bindings: readonly KeyBinding[],
  event: CalendarEvent,
  { canEdit, canCreate, verbs }: EventMenuOptions,
): Item[] {
  const items: Item[] = [];

  const push = (
    group: string,
    description: string,
    label: string,
    run: () => void,
    tone?: "default" | "danger",
  ) => {
    const binding = find(bindings, group, description);
    if (!binding) return;
    items.push({ kind: "item", key: `${group}/${description}`, label, shortcut: binding.keys, tone, run });
  };

  const separate = () => {
    if (items.length > 0 && items[items.length - 1]?.kind !== "separator") {
      items.push({ kind: "separator", key: `sep-${items.length}` });
    }
  };

  push("Event", "Open the event", "Open", () => verbs.open(event));

  /*
   * Answering an invitation.
   *
   * No binding to hang these off — there is no RSVP key — so they are the
   * calendar's "Search by sender": an item routed through a seam that already
   * exists, `Command::Rsvp`, rather than a new command invented for a menu.
   * They appear only on an event that *is* an invitation; an event with no
   * `rsvp` at all is one you own, and there is nothing to answer.
   *
   * The answer already given is disabled rather than hidden, so the menu says
   * which one it is instead of quietly offering two of three.
   */
  if (event.rsvp !== undefined) {
    separate();
    for (const [response, label] of RSVP_LABELS) {
      items.push({
        kind: "item",
        key: `rsvp/${response}`,
        label,
        disabled: event.rsvp === response,
        run: () => verbs.rsvp(event, response),
      });
    }
  }

  separate();
  push("Event", "Copy the event", "Copy", () => verbs.copy(event));
  // A duplicate lands on the same calendar, so it is a *create* there and a
  // read-only subscription refuses it — a round trip later, and in worse words.
  if (canCreate) {
    push("Event", "Duplicate the event", "Duplicate", () => verbs.duplicate(event));
  }

  separate();
  // Gated on the location as well as on the binding, the way the RSVP items are
  // gated on there being an invitation to answer. The key stays live on every
  // event so it can be found in the sheet and say what is missing; a menu has
  // no such duty, and an item that can only report "no address" is noise on the
  // three events in four that have none.
  if (mapsUrl(event.location)) {
    push("Event", "Map the address", "Map", () => verbs.openMap(event));
  }
  push("Event", "Open in Google Calendar", "Open in Google Calendar", () =>
    verbs.openInGoogle(event),
  );

  if (canEdit) {
    separate();
    push("Event", "Delete the event", "Delete", () => verbs.remove(event), "danger");
  }

  // A leading or trailing rule is a rule around nothing.
  while (items[0]?.kind === "separator") items.shift();
  while (items[items.length - 1]?.kind === "separator") items.pop();
  return items;
}

/**
 * The empty grid's one item: an event at the time under the pointer.
 *
 * Gated on `Create — full editor` being live, and printing that binding's keys,
 * so the menu cannot offer a create the keyboard could not have done. It hands
 * the slot to the same `openCreate` the drag-to-create gesture hands its draft
 * to when the draft is expanded with Tab.
 */
export function buildSlotItems(
  bindings: readonly KeyBinding[],
  slot: Slot,
  verbs: CalendarVerbs,
): Item[] {
  const binding = find(bindings, "Event", "Create — full editor");
  if (!binding) return [];
  return [
    {
      kind: "item",
      key: "slot/create",
      label: `New event at ${shortTime(slot.start)}`,
      shortcut: binding.keys,
      run: () => verbs.createAt(slot),
    },
  ];
}
