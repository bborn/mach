/**
 * What the reading pane is allowed to do with a calendar invitation.
 *
 * The decisions live here rather than in the component because each of them is
 * a rule about correctness — which messages get a control, which of several
 * invitations a keystroke means, what "already answered" is called — and every
 * one of them is testable without a DOM.
 *
 * Recognition itself is not here. Whether a message *is* an invitation was
 * settled in Rust at sync time, from its `text/calendar` part, and arrives as
 * `message.invitation`; see `src-tauri/src/invite/`. This file only decides
 * what to do about one.
 */

import type { Invitation, Message, MessageId, Rsvp } from "@/types";
import { fullDate, timeRangeLabel } from "@/lib/time";

/** The one `METHOD` that is an organiser asking a question. */
export const REQUEST = "REQUEST";

/**
 * The three answers, in the order they are shown, with Google Calendar's own
 * words for them.
 *
 * "Yes / No / Maybe" is what the invitation email itself says and what the
 * calendar's event detail says (`EventModal`), so it is what this says. The
 * order differs from the mail's on purpose — yes, maybe, no runs from most to
 * least committal, and it is the order the calendar already uses.
 */
export const ANSWERS: { response: Rsvp; label: string; keys: string }[] = [
  { response: "accepted", label: "Yes", keys: "i y" },
  { response: "tentative", label: "Maybe", keys: "i m" },
  { response: "declined", label: "No", keys: "i n" },
];

/**
 * The invitation on a message, if it is one worth showing a control for.
 *
 * `METHOD` is the whole of the test and Rust has already applied it — only a
 * `REQUEST` reaches here — but it is checked again rather than assumed, because
 * the field is data crossing a boundary and "the other side promised" is how a
 * cancellation ends up with an Accept button on it.
 */
export function invitationOn(message: Message): Invitation | null {
  const invitation = message.invitation;
  if (!invitation || invitation.method !== REQUEST) return null;
  return invitation;
}

/**
 * Whether there is anything to answer.
 *
 * `Rsvp` addresses an event row by id. No id, no command — so an invitation
 * whose event is not in the local store gets a note saying so and no buttons.
 * The alternative was a control that dispatched nothing, and a control that
 * silently does nothing is the specific failure this app has paid for most.
 */
export function isAnswerable(invitation: Invitation | null): boolean {
  return invitation?.eventId !== undefined;
}

/**
 * Which invitation a keystroke means, when a conversation holds more than one.
 *
 * The one the reader is on, and otherwise the last — an invitation thread that
 * has been rescheduled twice holds three of these, and the newest is the one
 * being looked at. `focused` comes from the message cursor (`n` / `p`), so
 * moving to a message and pressing the chord answers *that* one.
 */
export function activeInvitation(
  ids: readonly MessageId[],
  focused: MessageId | null,
): MessageId | null {
  if (ids.length === 0) return null;
  if (focused !== null && ids.includes(focused)) return focused;
  return ids[ids.length - 1] ?? null;
}

/** The word for an answer already on the calendar. `null` when there is none. */
export function answerLabel(response: Rsvp | undefined): string | null {
  if (!response || response === "needsAction") return null;
  return ANSWERS.find((answer) => answer.response === response)?.label ?? null;
}

/**
 * When the meeting is, from the event rather than from the mail.
 *
 * Empty when the event is not in the store, which is the case where there is
 * nothing to say — the message's own body is still on screen and says it.
 */
export function whenLabel(invitation: Invitation): string {
  if (invitation.start === undefined) return "";
  const day = fullDate(invitation.start);
  if (invitation.allDay || invitation.end === undefined) return day;
  return `${day} · ${timeRangeLabel(invitation.start, invitation.end)}`;
}
