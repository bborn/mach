import { useEffect, useRef, useState, type ReactNode } from "react";
import { TriangleAlert, X } from "lucide-react";
import { useMach, type StatusMessage } from "@/hooks/useMach";
import { describeRedo, describeUndo, peekRedo, peekUndo } from "@/lib/undo-stack";
import { cn } from "@/lib/utils";
import { Kbd } from "@/components/ui/kbd";

/**
 * The transient half of "something just happened".
 *
 * Undo worked long before this existed and had almost nothing to show for it.
 * Pressing ⌘Z reversed an action and reported it on a 24px rail at the bottom
 * of the window, in 11px type, next to the sync spinner — which is a fine place
 * for a fact and a terrible place for an acknowledgement. Archiving three
 * conversations moves three rows off screen; a person needs to be *told* that
 * happened, and told where the button is, in the place they were already
 * looking.
 *
 * # It is not a third status system
 *
 * There was already `ui.status` — one transient message, one undo offer, timed
 * out by `undoWindowSeconds` — and the status bar rendered it. This subsumes
 * that rather than sitting beside it. `ui.status` is still the only "what just
 * happened" in the app, still set from the same four places (`run`, the
 * calendar's write path, the plugin host, `setStatus`), still cleared by the
 * same timer in `useMach`. All that changed is where it is drawn: loudly, here,
 * with the offer attached. The status bar kept only the part that outlives the
 * message — the quiet ⌘Z line, which is about capability rather than about
 * news — and no longer renders the message at all.
 *
 * The practical consequence is that every surface that already reported through
 * the status bar got a toast for free, including ones this file has never heard
 * of: a plugin's `notify`, the composer's send failures, the agent.
 *
 * # Stacking: it collapses, it does not queue
 *
 * `ui.status` holds one message, so there is one toast, always. Archive three
 * times in three seconds and the third message replaces the second, restarts
 * the clock, and shows `×3` — the count is what makes the collapse legible
 * rather than making two messages silently vanish. A queue was the alternative
 * and it is the wrong shape for this: a queued toast is feedback about an
 * action you took five seconds ago, arriving after you have moved on, and three
 * of them stacked up cover the list they are reporting on. ⌘Z still reaches
 * every one of those three, one press at a time, which is what the count says.
 *
 * # What it must not do
 *
 * Not take the keyboard: nothing here autofocuses, and the layer is `fixed`, so
 * it cannot move the reading pane or reflow the list. Nor is it in the tab
 * order of the mailbox — mail and the calendar both claim ⇥ for their own
 * focus loop — which is why the buttons wear their bindings: ⌘Z and ⇧⌘Z are
 * the keyboard route to them, and the card is where you find that out.
 *
 * Not block the work either: the layer is `pointer-events-none` and only the
 * card takes clicks, and it is parked where the panes have least to lose — see
 * the note on the layer's own class list.
 *
 * Not vanish from under a hand, either. The clock belongs to `useMach`, so the
 * toast cannot extend it — what it can do is refuse to disappear while the
 * pointer is over it or the focus is inside it, which is the difference between
 * a button you can reach for and one that is gone by the time you get there.
 */

/* -------------------------------------------------------------------------- */
/* The pure parts                                                              */
/* -------------------------------------------------------------------------- */

export type ToastOffer = "undo" | "redo";

/**
 * Which traversal button belongs beside a message, if any.
 *
 * Two sources, and neither of them is the wording. A message that carries an
 * inverse is by definition a reversible thing that just happened, so it earns
 * ⌘Z; a message a traversal produced says so itself, because "Undid archived 3
 * conversations" has no inverse to carry and wants ⇧⌘Z. Everything else — "Open
 * a conversation first", "Copied “Lunch with Dana”" — gets no button, which is
 * the point: the status bar used to show `Undo` next to messages that had
 * nothing to do with the action it would undo.
 */
export function offerFor(status: StatusMessage | null): ToastOffer | null {
  if (!status) return null;
  if (status.offer) return status.offer;
  return status.undo ? "undo" : null;
}

/** What is on screen, and how many identical messages landed on it. */
export interface ToastRun {
  status: StatusMessage | null;
  /** 1 for a single message, 0 for none. Rendered as `×n` from 2 up. */
  repeat: number;
}

export const noToast: ToastRun = { status: null, repeat: 0 };

/**
 * Folds the next message into the one on screen.
 *
 * Same text, same toast, one higher count — which is what keeps a run of
 * archives from ever being more than one card, however fast they arrive.
 * Different text replaces outright, because the newest thing that happened is
 * the thing the offer belongs to.
 */
export function collapse(previous: ToastRun, next: StatusMessage | null): ToastRun {
  if (next === null) return noToast;
  const repeated = previous.status !== null && previous.status.message === next.message;
  return { status: next, repeat: repeated ? previous.repeat + 1 : 1 };
}

/* -------------------------------------------------------------------------- */
/* The surface                                                                 */
/* -------------------------------------------------------------------------- */

export interface ToastAction {
  /** The word on the button — "Undo" or "Redo". */
  word: string;
  /** The whole sentence: "Undo archived 3 conversations". */
  title: string;
  /** The binding, shown so the shortcut is learned by using the button. */
  keys: string;
  run: () => void;
}

export interface ToastCardProps {
  message: string;
  tone: StatusMessage["tone"];
  /** Rendered as `×n` from 2 up. */
  repeat?: number;
  action: ToastAction | null;
  onDismiss: () => void;
  /** Called with `true` while a pointer or the focus is inside the card. */
  onHold?: (holding: boolean) => void;
}

/**
 * One card: a line, an optional keyboard-bound button, a dismiss.
 *
 * Split out from the layer because it is now worn by two things. `ui.status`
 * fills it for everything the app did, and — in a development build only —
 * `chrome/HeldUpdate.tsx` fills it to offer the frontend code that arrived
 * while he was reading. A second surface for that would have been a second
 * thing to learn; the button and its binding are the shape he already knows.
 */
export function ToastCard({
  message,
  tone,
  repeat = 1,
  action,
  onDismiss,
  onHold,
}: ToastCardProps) {
  const error = tone === "error";

  return (
    <div
      // Only the card takes the pointer. The layer around it does not, so the
      // rows underneath stay clickable right up to the toast's own edges.
      className={cn(
        "pointer-events-auto flex w-fit max-w-[26rem] items-center gap-2",
        "rounded-[var(--radius)] border bg-surface-raised px-3 py-2",
        "shadow-[0_6px_20px_-4px_rgb(0_0_0/0.28)]",
        error ? "border-danger" : "border-border-strong",
        /*
         * The entrance, in CSS and not in JavaScript.
         *
         * `@starting-style` — Tailwind's `starting:` — means the resting state
         * is what the element *is*, and the fade is only how it got there. The
         * first version of this held the card at `opacity-0` until a
         * `requestAnimationFrame` promoted it, and a frame callback that never
         * runs leaves a toast that is in the DOM, has an accessible name, and
         * cannot be seen. There is no timing to lose here.
         *
         * `motion-reduce` drops the transition, which under a starting style
         * means the card is simply already in place — which is what somebody
         * who asked for less motion wants, rather than a faster slide.
         */
        "transition-[opacity,translate] duration-150 ease-out motion-reduce:transition-none",
        "translate-y-0 opacity-100 starting:translate-y-1 starting:opacity-0",
      )}
      onPointerEnter={() => onHold?.(true)}
      onPointerLeave={() => onHold?.(false)}
      onFocus={() => onHold?.(true)}
      onBlur={(event) => {
        // Moving between the Undo button and the dismiss button is still
        // inside the toast; only leaving it altogether releases the hold.
        if (!event.currentTarget.contains(event.relatedTarget)) onHold?.(false);
      }}
    >
      {error && (
        <TriangleAlert size={13} strokeWidth={2} className="shrink-0 text-danger" aria-hidden />
      )}

      <span className={cn("min-w-0 text-list leading-snug", error ? "text-danger" : "text-foreground")}>
        {message}
      </span>

      {/* The collapsed run, said out loud rather than left as three messages
          the user never saw. The title explains what the number buys, because
          "×3" on its own could be read as "one undo takes back all three". */}
      {repeat > 1 && (
        <span
          title={`${repeat} in a row · ⌘Z undoes one at a time`}
          className="shrink-0 font-mono text-micro tabular-nums text-faint-foreground"
        >
          ×{repeat}
        </span>
      )}

      {action && (
        <button
          type="button"
          onClick={action.run}
          // The visible word is "Undo"; the accessible name is the whole
          // sentence, so a screen reader says what it would undo.
          aria-label={action.title}
          title={action.title}
          className={cn(
            "ml-1 inline-flex shrink-0 items-center gap-1 rounded-[3px] px-2 py-1",
            "text-list text-accent transition-colors hover:bg-row-hover",
          )}
        >
          {action.word}
          <Kbd keys={action.keys} className="border-transparent bg-transparent text-accent" />
        </button>
      )}

      <button
        type="button"
        onClick={onDismiss}
        aria-label="Dismiss"
        title="Dismiss"
        className="shrink-0 rounded-[3px] p-1 text-faint-foreground transition-colors hover:text-foreground"
      >
        <X size={12} strokeWidth={2} aria-hidden />
      </button>
    </div>
  );
}

export interface ToastLayerProps {
  status: StatusMessage | null;
  repeat: number;
  action: ToastAction | null;
  onDismiss: () => void;
  /** Called with `true` while a pointer or the focus is inside the card. */
  onHold?: (holding: boolean) => void;
  /**
   * Cards that are not `ui.status`, stacked above it and outliving it.
   *
   * One thing uses this and only in development: the held-update offer. It
   * cannot be a status message because a status message is timed out by
   * `useMach` in a few seconds and replaced by the next thing he does, and an
   * offer he has not answered yet must still be there when he looks up.
   */
  children?: ReactNode;
}

/**
 * The fixed layer and its two live regions.
 *
 * Two, not one, because an error has to interrupt and a confirmation must not:
 * a screen reader hearing "Archived 3 conversations" over the top of whatever
 * it was reading would make the app unusable, and one that waits politely to
 * mention a failure has hidden it. Both regions are mounted for the life of the
 * window whether or not anything is in them — a live region added to the page
 * at the same moment as its content is not reliably announced.
 *
 * Split out from `Toast` below so it can be rendered and asserted on without a
 * provider or a DOM. Everything here is props.
 */
export function ToastLayer({
  status,
  repeat,
  action,
  onDismiss,
  onHold,
  children,
}: ToastLayerProps) {
  const error = status?.tone === "error";

  const card = status && (
    <ToastCard
      message={status.message}
      tone={status.tone}
      repeat={repeat}
      action={action}
      onDismiss={onDismiss}
      onHold={onHold}
    />
  );

  return (
    <div
      /*
       * Bottom left, clear of the rail — and z-30, above the panes and below
       * every dialog and the palette, which are z-50.
       *
       * Hard against the left edge was the first version and it was wrong in
       * the calendar: the sidebar's own display checkboxes live at the bottom
       * of it, and a toast parked on top of them for twenty seconds is exactly
       * the "covers the thing you are using" failure. Starting where the rail
       * ends puts it over the thread list in mail and the last hour of the grid
       * in the calendar, which are the rows the message is usually about, and
       * clear of the reading pane entirely — the composer's own undo-send strip
       * lives down there on the right and must never be sat on.
       */
      className="pointer-events-none fixed bottom-9 left-[calc(var(--rail-width)+0.75rem)] z-30 flex flex-col items-start gap-1"
    >
      {/* Above the status card, because it is the one that stays. */}
      {children}
      <div role="status" aria-live="polite" aria-atomic="true">
        {error ? null : card}
      </div>
      <div role="alert" aria-live="assertive" aria-atomic="true">
        {error ? card : null}
      </div>
    </div>
  );
}

/**
 * The wired toast. One per window; `App` mounts it beside the status bar.
 */
export function Toast({ children }: { children?: ReactNode }) {
  const { ui, undoState, actions, dispatch } = useMach();
  const [run, setRun] = useState<ToastRun>(noToast);

  /*
   * Held while the pointer or the focus is inside the card.
   *
   * A ref *and* a state, for the same reason the undo stack keeps both: the
   * effect below has to read the value that is true right now rather than the
   * one its render closed over, and the release has to cause a render.
   */
  const holding = useRef(false);
  const [held, setHeld] = useState(false);
  const hold = (value: boolean) => {
    holding.current = value;
    setHeld(value);
  };

  /*
   * Fold each new message in.
   *
   * The guard is on the *object*, not the text: React may run an effect again
   * for the same commit, and a repeat count that grew because of a double
   * invocation would be a lie. Every dispatched status is a fresh object, so
   * identity is exactly "this is a message I have not seen".
   */
  const seen = useRef<StatusMessage | null>(null);
  useEffect(() => {
    if (seen.current === ui.status) return;
    seen.current = ui.status;
    // The window closing while a hand is on the toast is not a reason to take
    // the button away; the release below finishes the job.
    if (ui.status === null && holding.current) return;
    setRun((previous) => collapse(previous, ui.status));
  }, [ui.status]);

  useEffect(() => {
    if (!held && ui.status === null) setRun(noToast);
  }, [held, ui.status]);

  const offer = offerFor(run.status);
  const undoLabel = describeUndo(peekUndo(undoState));
  const redoLabel = describeRedo(peekRedo(undoState));

  /*
   * The offer, named from the stack rather than from the message.
   *
   * Which means it is never on screen for a traversal that has already been
   * spent — undo the last entry from the keyboard and the button the toast is
   * holding out goes with it, instead of sitting there promising an action the
   * stack no longer has.
   */
  const action: ToastAction | null =
    offer === "undo" && undoLabel
      ? { word: "Undo", title: undoLabel, keys: "mod+z", run: actions.undo }
      : offer === "redo" && redoLabel
        ? { word: "Redo", title: redoLabel, keys: "shift+mod+z", run: actions.redo }
        : null;

  return (
    <ToastLayer
      status={run.status}
      repeat={run.repeat}
      action={action}
      onHold={hold}
      onDismiss={() => {
        // Both halves: the message may already have timed out and only be on
        // screen because a hand was on it.
        hold(false);
        setRun(noToast);
        dispatch({ type: "status", status: null });
      }}
    >
      {children}
    </ToastLayer>
  );
}
