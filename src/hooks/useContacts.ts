import { useMemo } from "react";
import { useMach } from "./useMach";
import { contactsFrom, loadSent, type Contact } from "@/lib/contacts";

/**
 * The address book, derived from the store.
 *
 * One hook so every field that completes people completes the *same* people:
 * the composer, the event modal's guest list, and ⌘K all read this. The
 * expensive part is the fold over every loaded thread, and it is memoised on
 * the two things that change it.
 *
 * `loadSent()` is read once per rebuild rather than subscribed to: the list it
 * returns only changes when a message is sent, and sending closes the composer.
 */
export function useContacts(): Contact[] {
  const { allThreads, events, detail, accounts } = useMach();

  return useMemo(
    () => contactsFrom({ threads: allThreads, events, detail, accounts, sent: loadSent() }),
    [allThreads, events, detail, accounts],
  );
}
