/**
 * Which message of a conversation carries the unsubscribe offer, and what the
 * button says.
 *
 * A conversation is a list of messages and the offer is a property of one of
 * them — a mailing list changes its headers, a thread can hold a reply from a
 * person as well as the newsletter it started as — so "does this conversation
 * offer an unsubscribe" is a question with two answers in it: whether, and
 * about which message. Answering it inside the reading pane's JSX would make it
 * unaskable from a test, and it is the half of this feature most likely to be
 * wrong.
 *
 * Newest wins. The header on the most recent message is the one the sender is
 * using now; an address that appeared in a mail from 2023 has had two years to
 * stop working.
 */

import type { Message, MessageId, UnsubscribeOffer } from "@/types";

/** The button the conversation earns, or nothing. */
export interface UnsubscribeAction {
  /** The message whose header the command will use. */
  messageId: MessageId;
  offer: UnsubscribeOffer;
  /**
   * The word on the button, which is also its `title`.
   *
   * "Report spam" is not a softer way of saying unsubscribe — it is the
   * different thing that happens. Rust reached for it because nothing vouches
   * for the sender, and offering "Unsubscribe" there would tell the user the
   * app was about to confirm their address to somebody it does not trust.
   */
  label: string;
  /**
   * Who the confirmation names — the sender's name, or their address.
   *
   * "Unsubscribed" on its own is the message that makes a person go and check
   * what they just did. The list is what they were trying to get away from, so
   * the line has to say which one it was.
   */
  sender: string;
}

export const UNSUBSCRIBE_LABEL = "Unsubscribe";
export const REPORT_SPAM_LABEL = "Report spam";

/**
 * The offer this conversation makes, if any.
 *
 * `null` for the overwhelming majority of conversations, and the caller renders
 * no button at all rather than a disabled one: there is nothing to explain to
 * somebody reading a reply from a colleague.
 */
export function unsubscribeAction(
  messages: readonly Message[],
): UnsubscribeAction | null {
  let best: Message | null = null;
  for (const message of messages) {
    if (!message.unsubscribe) continue;
    // `>=` rather than `>`: two messages sharing a timestamp is common enough
    // in machine-generated mail, and later in the list is the later message —
    // `get_thread` returns them in send order.
    if (best === null || message.timestamp >= best.timestamp) best = message;
  }
  if (!best?.unsubscribe) return null;

  return {
    messageId: best.id,
    offer: best.unsubscribe,
    label: best.unsubscribe.offer === "unsubscribe" ? UNSUBSCRIBE_LABEL : REPORT_SPAM_LABEL,
    // Gmail returns senders with a name and no address, and senders with an
    // address and no name. "the sender" is the last resort and covers neither
    // being there, which the store does allow.
    sender: best.from.name || best.from.email || "the sender",
  };
}
