import { useCallback, useState } from "react";
import { CalendarDays, LoaderCircle, Repeat, TriangleAlert } from "lucide-react";
import type { Invitation as InvitationData, MessageId, Rsvp } from "@/types";
import { useKeyBindings } from "@/hooks/useKeymap";
import { overlayOwnsKeyboard, useMach } from "@/hooks/useMach";
import { keyboardInComposer } from "@/lib/compose";
import { ANSWERS, activeInvitation, answerLabel, isAnswerable, whenLabel } from "@/lib/invitation";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { focusedMessageId } from "./thread-cursor";

/** Marks a card in the DOM so the keyboard can find which one is live. */
export const INVITATION_CARD = "data-invitation";

export interface InvitationProps {
  messageId: MessageId;
  invitation: InvitationData;
}

/**
 * Answer an invitation without leaving Mach.
 *
 * # What this replaces
 *
 * A Google invitation renders its own Yes / No / Maybe inside the message, and
 * those are links to google.com: answering meant a browser, a tab, and the
 * calendar's web UI. This card sits above the message and dispatches the
 * `Rsvp` command that already existed for the calendar's event detail — the
 * same command, the same inverse, the same undo entry — so an answer is a local
 * write with a network call behind it rather than a trip out of the app.
 *
 * # Google's own buttons are collapsed, not rewritten
 *
 * Two identical sets of Yes / No / Maybe, one of which quietly leaves the app,
 * is worse than either alone. So when there is something to answer, the
 * message's body starts collapsed behind a disclosure (`ThreadMessage`) and
 * this card is what the reader sees. The HTML itself is untouched — one click
 * or one Tab brings the whole invitation back exactly as it was sent, which
 * matters because the message can carry a note from the organiser that the
 * event row does not.
 *
 * When there is *nothing* to answer, the body is left open. Google's buttons
 * are then the only working affordance and hiding them would be taking away
 * the reader's last route to answering at all.
 *
 * # The keyboard
 *
 * `i` then `y` / `m` / `n` — invitation, then the answer's own initial. A
 * chord rather than three bare letters because mail mode's single keys are
 * Gmail's and this app does not invent a second vocabulary for them; `i` is
 * unused, and `g i` (go to inbox) is a different first key, so nothing is
 * shadowed. The chord is dead while the caret is in a composer, dead behind any
 * dialog, and dead when there is no invitation to answer — see
 * `src/lib/composer-keys.test.ts` for the invariant it has to satisfy, which it
 * does trivially: none of these is a key a text editor owns.
 *
 * The buttons are real `<button>`s in the tab order, so the pointer route and
 * the Tab route need no code at all.
 *
 * # Optimistic, like every other write
 *
 * The answer is on screen in the frame the key was pressed. Rust writes SQLite
 * before it calls Google and rolls the row back if Google refuses; when that
 * happens the button goes back to where it was and the reason is printed under
 * it, because a silent failure here means telling an organiser you are coming
 * when you are not.
 */
export function Invitation({ messageId, invitation }: InvitationProps) {
  const { ui, actions } = useMach();
  /** The answer this session has given, before the store has been re-read. */
  const [optimistic, setOptimistic] = useState<Rsvp | null>(null);
  const [busy, setBusy] = useState<Rsvp | null>(null);
  const [error, setError] = useState<string | null>(null);

  const answerable = isAnswerable(invitation);
  const current = optimistic ?? invitation.response ?? undefined;

  const respond = useCallback(
    (response: Rsvp) => {
      const eventId = invitation.eventId;
      if (eventId === undefined || busy !== null) return;
      const previous = optimistic;
      setOptimistic(response);
      setBusy(response);
      setError(null);
      /*
       * Through `actions.execute` rather than the data source directly.
       *
       * The button above is already optimistic and always was, and that was the
       * whole of it: this card executed the command itself, so the *event* — the
       * block on the calendar, the tick in its right-click menu — went on saying
       * the old answer until the next refetch. One write path means the answer
       * lands in both places at once, and it is also what puts a proper entry on
       * the undo stack, which a status message carrying only the inverse cannot.
       */
      void actions
        .execute(
          { kind: "rsvp", eventId, response },
          {
            // Rust has already put the row back; this puts the button back.
            onRefused: ({ message }) => {
              setOptimistic(previous);
              setError(message);
            },
          },
        )
        .finally(() => setBusy(null));
    },
    [busy, actions, invitation.eventId, optimistic],
  );

  /*
   * Live only when this is the card a keystroke means: mail mode, no dialog
   * over it, the caret not in a composer, and this card the one the message
   * cursor is on (or the last one, when the cursor is elsewhere).
   */
  const live = () =>
    answerable &&
    ui.mode === "mail" &&
    !overlayOwnsKeyboard(ui) &&
    !keyboardInComposer() &&
    activeInvitation(cardIds(), focusedMessageId()) === messageId;

  useKeyBindings(
    ANSWERS.map((answer) => ({
      keys: answer.keys,
      group: "Mail",
      description: `${answer.label} to this invitation`,
      when: live,
      handler: () => respond(answer.response),
    })),
  );

  return (
    <section
      {...{ [INVITATION_CARD]: messageId }}
      aria-label="Invitation"
      className="mt-3 rounded-[var(--radius)] border border-border bg-surface px-3 py-2"
    >
      <div className="flex min-w-0 items-baseline gap-2">
        <CalendarDays
          size={12}
          strokeWidth={1.75}
          className="shrink-0 self-center text-faint-foreground"
        />
        <span className="min-w-0 flex-1 truncate text-list text-foreground">
          {invitation.title ?? "Invitation"}
        </span>
        {invitation.recurring && (
          <Repeat
            size={11}
            strokeWidth={1.75}
            aria-label="Repeats — this answers one occurrence"
            className="shrink-0 self-center text-faint-foreground"
          />
        )}
      </div>

      {(whenLabel(invitation) || invitation.location) && (
        <div className="mt-0.5 truncate text-micro text-faint-foreground">
          {[whenLabel(invitation), invitation.location].filter(Boolean).join(" · ")}
        </div>
      )}

      {answerable ? (
        <div className="mt-2 flex flex-wrap items-center gap-1">
          <span className="mr-1 text-micro text-muted-foreground">Going?</span>
          {ANSWERS.map((answer) => {
            const chosen = current === answer.response;
            return (
              <Button
                key={answer.response}
                size="sm"
                variant={chosen ? "default" : "subtle"}
                aria-pressed={chosen}
                disabled={busy !== null}
                title={`${answer.label} (${answer.keys.replace(" ", " then ").toUpperCase()})`}
                onClick={() => respond(answer.response)}
                className={cn(chosen && "font-medium")}
              >
                {busy === answer.response && (
                  <LoaderCircle size={11} strokeWidth={1.75} className="animate-spin" />
                )}
                {answer.label}
              </Button>
            );
          })}
          {/*
            The answer already on the calendar, in words as well as in the
            filled button — colour alone is not a signal a reader can be asked
            to decode, and this is the sentence a screen reader gets too.
          */}
          {answerLabel(current) && (
            <span className="text-micro text-muted-foreground">· you said {answerLabel(current)}</span>
          )}
        </div>
      ) : (
        /*
          No event, no id, no command. Saying which of the three reasons it is
          would be a guess — an unrun calendar sync, an address whose calendar
          Mach does not hold, and a forwarded invitation all arrive here
          identically — so it says only the fact, and Google's own buttons are
          left where they are, below, working.
        */
        <div className="mt-1 text-micro text-muted-foreground">Not on this calendar</div>
      )}

      {error && (
        /*
          Wrapped rather than truncated. The status bar truncates because it is
          24px tall and transient; this is where the button was pressed, it
          stays until the next attempt, and Google's refusals carry the
          recovery in the tail of the sentence ("the account must be
          authorized again") — which is the half a single line loses.
        */
        <div role="status" className="mt-1.5 flex items-start gap-1 text-micro text-danger">
          <TriangleAlert size={12} strokeWidth={1.75} className="mt-[2px] shrink-0" />
          <span className="min-w-0">{error}</span>
        </div>
      )}
    </section>
  );
}

/**
 * The invitations currently on screen, in reading order.
 *
 * Read from the DOM rather than passed down: the cards are rendered one per
 * message by `ThreadMessage`, and each one needs to know about the others only
 * to decide whether a keystroke is its own. Threading a list through the pane
 * to answer that would be a prop nobody else wants.
 */
function cardIds(): MessageId[] {
  if (typeof document === "undefined") return [];
  return [...document.querySelectorAll(`[${INVITATION_CARD}]`)]
    .map((node) => Number(node.getAttribute(INVITATION_CARD)))
    .filter((id) => Number.isFinite(id));
}
