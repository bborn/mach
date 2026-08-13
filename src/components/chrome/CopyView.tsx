import { useCallback, useEffect, useRef, useSyncExternalStore } from "react";
import { useKeyBindings } from "@/hooks/useKeymap";
import { useMach } from "@/hooks/useMach";
import {
  clearCopyRequest,
  copyContextText,
  copyRequest,
  copyableContext,
  describeCopy,
  subscribeCopyView,
} from "@/lib/copy-view";
import { errorMessage } from "@/lib/ipc";
import { mailboxName } from "@/lib/mailboxes";

/**
 * ⌘⌥C — put what is on screen on the clipboard, as text.
 *
 * Renders nothing. It exists because the copy needs three things that live in
 * three places: the keystroke (the registry), the shell (`useMach`), and the
 * ⌘K row (a resolver, which is a plain function with no hooks). A component is
 * the only thing that can hold all three, and this is the smallest one that
 * does — the same shape `AgentDock` uses for its own ⌘K handoff.
 *
 * # Why ⌘⌥C
 *
 * ⌘C is the copy the selection already does and must stay that. ⇧⌘C is
 * `composeAnother` in the composer, so it is not free everywhere. ⌘⌥C is free
 * across the registry, and reads as what it is: a variant of copy. Gmail and
 * Google Calendar have no key for this, so there is no vocabulary to match —
 * which is exactly when inventing one is allowed.
 *
 * It answers from inside a text field as well, on the same grounds ⇧⌘R does:
 * a modified letter is not an editing key, and the moment you most want the
 * conversation on your clipboard is half-way through replying to it.
 */
export function CopyView() {
  const mach = useMach();
  const { actions } = mach;

  // The whole shell in a ref, so the handler reads the *current* view rather
  // than whatever it closed over. `AgentDock` does the same, for the same
  // reason: a thread finishing its load must not have to re-register a key.
  const shell = useRef(mach);
  shell.current = mach;

  const copy = useCallback(() => {
    const now = shell.current;
    const label = now.labels.find((row) => row.id === now.ui.labelId);
    const items = copyableContext({
      mode: now.ui.mode,
      calendarView: now.ui.calendarView,
      anchor: now.ui.anchor,
      labelId: now.ui.labelId,
      // `mailboxName` and not `label.name`: Gmail's system labels are named
      // INBOX and CATEGORY_PROMOTIONS on the wire, and the rail has always
      // drawn them as Inbox and Promotions. The copy says what he can see.
      mailboxName: label ? mailboxName(label) : undefined,
      threadId: now.ui.threadId,
      eventId: now.ui.eventId,
      selectedThread: now.visibleThreads[now.selectedIndex] ?? null,
      openThread: now.detail,
      selectedEvent: now.selectedEvent,
      visibleThreads: now.visibleThreads,
      visibleEvents: now.visibleEvents,
    });

    if (items.length === 0) {
      actions.setStatus("Nothing on screen to copy");
      return;
    }

    // A copy that silently succeeds and one that silently failed look the same,
    // so both outcomes say which happened.
    void copyContextText(items)
      .then(({ chars, truncated }) => {
        if (chars === 0) {
          actions.setStatus("Nothing on screen to copy");
          return;
        }
        actions.setStatus(describeCopy(items, truncated));
      })
      .catch((error: unknown) => actions.setStatus(errorMessage(error), "error"));
  }, [actions]);

  useKeyBindings([
    {
      keys: "mod+alt+c",
      group: "Global",
      description: "Copy what's on screen as text",
      allowInInput: true,
      handler: copy,
    },
  ]);

  /* ⌘K's route in. The resolver records a request; this fulfils it. */
  const request = useSyncExternalStore(subscribeCopyView, copyRequest);
  const nonce = request?.nonce ?? 0;

  useEffect(() => {
    if (!nonce) return;
    clearCopyRequest();
    // The palette is what he asked from, and it is covering the thing he asked
    // about — close it before reading the shell.
    actions.setPalette(false);
    copy();
    // Only a new request may copy.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [nonce]);

  return null;
}
