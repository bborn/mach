import { useMemo } from "react";
import { useMach } from "./useMach";
import { contactsFrom, loadSent, type Contact } from "@/lib/contacts";

/**
 * The address book, derived from the store.
 *
 * One hook so every field that completes people completes the *same* people:
 * the composer, the event modal's guest list, and ⌘K all read this.
 *
 * Two kinds of source, merged rather than chosen between. `addressBook` is the
 * store's own index — every address in every message, read once at boot — and
 * it is the body of the book. The rest is whatever happens to be in memory: the
 * loaded list, the open conversation, the calendar window. Those are a few
 * dozen people, and they are the ones most likely to be wanted next, so they
 * fold on top and can move somebody's recency forward past a stale index entry.
 *
 * Nothing here waits on anything. `addressBook` is `[]` until the scan lands,
 * and an empty one still leaves the conversation on screen completable, which
 * is what lets a composer open and take typing during the read.
 *
 * `loadSent()` is read once per rebuild rather than subscribed to: the list it
 * returns only changes when a message is sent, and sending closes the composer.
 */
export function useContacts(): Contact[] {
  const { allThreads, events, detail, accounts, addressBook } = useMach();

  return useMemo(
    () =>
      contactsFrom({
        indexed: addressBook,
        threads: allThreads,
        events,
        detail,
        accounts,
        sent: loadSent(),
      }),
    [addressBook, allThreads, events, detail, accounts],
  );
}
