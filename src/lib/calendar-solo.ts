import type { Account, AccountId, Calendar, CalendarEvent, CalendarId } from "@/types";

/**
 * "Just show me this one" — the rail's solo, for an account or for a calendar.
 *
 * # It is a lens, not an edit
 *
 * Solo writes nothing. It is a filter laid over the events the rail has already
 * decided to show, and dropping it puts the array it was given back, unchanged
 * and by reference. That is the whole of "un-soloing restores exactly what was
 * there before": there is no previous visibility set to remember, because none
 * was ever disturbed.
 *
 * The alternative — untick every other calendar, then tick them all back — is
 * what Google Calendar's "Display this only" does, and it is the version that
 * loses a configuration. Five accounts with five calendars taken out of the list
 * and a dozen more unticked is not a state that survives being reconstructed
 * from memory, and "restore" quietly meaning "everything on" is exactly how it
 * would be lost. Nothing here can lose it, because nothing here can write it.
 *
 * The same reasoning covers the unlisted rows at the foot of the rail. `hidden`
 * and `unlisted` are two different axes (see `CalendarSidebar`), and a solo is
 * about neither: it does not touch `ui.hiddenCalendars` and it does not touch
 * `UiSession.calendarVisibility`, so a calendar that was out of the list before
 * a solo is out of the list during it and after it.
 *
 * # One solo at a time
 *
 * Account solo and calendar solo are the same state, so asking for one clears
 * the other. Two simultaneous solos have a state where they disagree — calendar
 * X soloed while account B is soloed, and X belongs to A — and it draws an empty
 * grid. A gesture whose entire promise is "show me this" may not answer with
 * nothing.
 *
 * # Why a calendar solo ignores the hidden set and an account solo does not
 *
 * An account solo narrows to a group of calendars, and inside that group the
 * per-calendar ticks still mean something: he has calendars unticked in the
 * account he is soloing, and showing them would be the solo turning things on he
 * had turned off. So it filters what is already shown.
 *
 * A calendar solo names one calendar. There is nothing left for the tick to say,
 * and a solo that drew an empty grid because the calendar you just named happens
 * to be unticked would be the gesture failing at the only thing it does. So it
 * filters the whole projection.
 */
export type Solo =
  | { kind: "account"; id: AccountId }
  | { kind: "calendar"; id: CalendarId };

export function sameSolo(a: Solo | null, b: Solo | null): boolean {
  if (a === null || b === null) return a === b;
  return a.kind === b.kind && a.id === b.id;
}

/**
 * What pressing solo on `target` leaves behind.
 *
 * Pressing it on what is already soloed clears it; pressing it on anything else
 * replaces whatever was soloed before. One function, so the ⌥-click on the row,
 * the `solo` chip beside it and the ⌥-digit binding cannot drift apart — the
 * account solo's two halves had already drifted once, with the toggle written
 * out in the sidebar's `onClick` and again in `CalendarMode`'s key handler.
 */
export function nextSolo(current: Solo | null, target: Solo): Solo | null {
  return sameSolo(current, target) ? null : target;
}

/** The calendar `⌥<digit>` addresses — the same index `v <digit>` counts. */
export function calendarSoloAt(calendars: readonly Calendar[], index: number): Solo | null {
  const calendar = calendars[index];
  return calendar ? { kind: "calendar", id: calendar.id } : null;
}

/** The account `s <digit>` addresses. */
export function accountSoloAt(accounts: readonly Account[], index: number): Solo | null {
  const account = accounts[index];
  return account ? { kind: "account", id: account.id } : null;
}

/**
 * The events a solo lets through.
 *
 * `shown` is what the sidebar's hidden set has already left standing; `all` is
 * the whole projection, unticked calendars included. With no solo the answer is
 * `shown` itself — the same array, so a render that was not soloing pays
 * nothing for the feature existing and un-soloing is provably a no-op.
 */
export function soloEvents(
  solo: Solo | null,
  shown: readonly CalendarEvent[],
  all: readonly CalendarEvent[],
): readonly CalendarEvent[] {
  if (solo === null) return shown;
  if (solo.kind === "account") return shown.filter((event) => event.accountId === solo.id);
  return all.filter((event) => event.calendarId === solo.id);
}
