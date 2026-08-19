/**
 * Which message a conversation opens on.
 *
 * `ReadingPane` draws every message in the thread and unfolds exactly one of
 * them without being asked. The rule reads like it should be "the newest", and
 * for almost every conversation it is.
 *
 * The exception is a draft. A draft is mirrored into the conversation it
 * answers, so it really is the newest thing there — and `ThreadMessage` refuses
 * to unfold one, because activating a draft row opens the composer, which is
 * the only thing anybody wants from it. Naming a draft as the message to open
 * therefore opens nothing: three collapsed headers and a screenful of white
 * where the mail should be. A thread you have half-answered is exactly the
 * thread you opened in order to re-read, so the answer falls back to the newest
 * message that can actually show itself.
 *
 * A thread that is *only* a draft — one you started from scratch and have not
 * sent — has no such message, and returns `null` rather than pretending.
 */

/** Just enough of a message for this decision. */
interface Openable {
  id: number;
  isDraft?: boolean;
}

/** The id the reading pane should unfold, or `null` when there is nothing to. */
export function openableLatestId(messages: readonly Openable[]): number | null {
  for (let i = messages.length - 1; i >= 0; i--) {
    const message = messages[i]!;
    if (!message.isDraft) return message.id;
  }
  return null;
}
