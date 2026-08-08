import { useEffect, useRef, useState } from "react";
import { useMach } from "@/hooks/useMach";
import type { LabelId } from "@/types";
import { usePreferencesStore } from "./PreferencesProvider";

/**
 * The app remembering where it was: mode, mailbox, and the divider.
 *
 * Renders nothing. It exists as a component rather than as a block inside
 * `MachProvider` because it needs both of two contexts that sit on either side
 * of it — the preferences store above, the shell's `ui` below — and this is the
 * one place both are in scope. It also keeps `useMach` free of a concern that
 * is not about mail: the reducer stays a pure function of actions, and the
 * restore is expressed as the same actions a user would have dispatched.
 *
 * # Why the restore is a burst of ordinary actions
 *
 * There is no `restore` case in `uiReducer`, deliberately. Dispatching `mode`,
 * `account`, `label` and `listWidth` means the restored state goes through
 * exactly the transitions a person's clicks would — including `listWidth`'s
 * 280..640 clamp, which is the one number a stored value could plausibly be
 * outside of. A bespoke action would be a second way into the state with its
 * own copy of those rules to get wrong.
 *
 * Order matters once: `account` and `label` each clear the thread selection, so
 * they run before anything that could have set one. At boot nothing has.
 *
 * # One flash, accepted
 *
 * The store is SQLite and the read is a promise, so the first paint is the
 * default mailbox and the restore lands a tick later. The alternative is a
 * synchronous read at module load, which would mean a second storage mechanism
 * for the sake of one frame.
 */
export function SessionMemory() {
  const { ui, dispatch, actions } = useMach();
  const { session, loaded, remember } = usePreferencesStore();
  /*
   * Two guards, and they are not redundant.
   *
   * The ref stops the restore running twice. The state is what the *recorder*
   * waits on, and it has to be state rather than the ref: React batches the
   * dispatches below with this `setState`, so the render that first sees
   * `restored === true` is also the first render that sees the restored `ui`.
   * Gating the recorder on the ref instead would let it run once with the
   * defaults still in hand and write them straight back over the session it had
   * just read.
   */
  const started = useRef(false);
  const [restored, setRestored] = useState(false);

  useEffect(() => {
    if (!loaded || started.current) return;
    started.current = true;
    setRestored(true);

    if (session.mode) actions.setMode(session.mode);
    if (session.calendarView) actions.setCalendarView(session.calendarView);
    if (session.accountId !== undefined) {
      dispatch({ type: "account", accountId: session.accountId });
    }
    if (session.labelId) dispatch({ type: "label", labelId: session.labelId as LabelId });
    if (session.listWidth !== undefined) {
      dispatch({ type: "listWidth", width: session.listWidth });
    }
  }, [loaded, session, dispatch, actions]);

  // Record, but never before the restore has landed — otherwise the defaults
  // this component was mounted with would be written over the session it is
  // about to read, and the first launch after a quit would look like the memory
  // failed.
  useEffect(() => {
    if (!restored) return;
    remember({
      mode: ui.mode,
      calendarView: ui.calendarView,
      accountId: ui.accountId,
      labelId: ui.labelId,
      listWidth: ui.listWidth,
    });
  }, [restored, ui.mode, ui.calendarView, ui.accountId, ui.labelId, ui.listWidth, remember]);

  return null;
}
