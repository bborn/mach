import { CircleCheck, LoaderCircle, Send, TriangleAlert } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";
import { useKeyBindings } from "@/hooks/useKeymap";
import { useMach } from "@/hooks/useMach";
import { registerResolver } from "@/lib/palette/resolver";
import {
  closeHandoff,
  handoffRequest,
  handoffResolver,
  loadTargets,
  openSession,
  previewHandoff,
  runHandoff,
  subscribeHandoff,
  subscribeTargets,
  targetSnapshot,
  type HandoffPreview,
  type HandoffReceipt,
  type HandoffSourceRef,
} from "@/lib/handoff";
import { errorMessage, isTauri } from "@/lib/ipc";
import { cn } from "@/lib/utils";
import { Overlay } from "@/components/ui/dialog";
import { BareInput } from "@/components/ui/input";
import { Button } from "@/components/ui/button";
import { HandoffTargetsDialog } from "./HandoffTargets";

/**
 * How long an inline result stays up before it takes itself away.
 *
 * "Shows the result briefly" is the requirement, and briefly is the operative
 * word: `ty task create` prints one line saying it worked, and a dialog that
 * waits to be dismissed would turn a fire-and-forget gesture into a two-step
 * one. A failure is not on this timer — see the render.
 */
const RESULT_MS = 7_000;

type Phase = "idle" | "asking" | "preparing" | "confirming" | "running" | "shown" | "failed";

/**
 * The runner. Renders nothing almost all of the time, and that is the point.
 *
 * A handoff to a target he has used before is one keystroke: ⌘K, the sentence,
 * enter. This component opens, launches, and closes without ever painting. It
 * paints in exactly four cases —
 *
 *  * **no instruction yet**, where it puts up the field for one (see below);
 *  * **the first time a target is used**, where it asks (see below);
 *  * **an inline result**, for the few seconds it is worth reading;
 *  * **a failure**, because a handoff that silently did nothing is the worst
 *    outcome available and this codebase has paid for that lesson already.
 *
 * # Why an empty instruction opens a field instead of refusing
 *
 * Reaching for the feature before phrasing the ask is the ordinary way in, not
 * a mistake: the row is on top of the palette from the first letter of
 * "handoff", so ⏎ lands there while the query is still the keyword that found
 * it. `LaunchPlan::prepare` refuses an empty note and always will — nothing may
 * hand a whole thread to an agent under no instruction — but the refusal used
 * to be what he *saw*: a panel with a warning in it, a Done button, and no way
 * to supply the missing sentence without starting over. The dialog knew exactly
 * what was missing, so it asks for it.
 *
 * # Why the first use is confirmed and later ones are not
 *
 * Launching a configured command *is* a genuine "are you sure" moment, but only
 * once. Until a target has run, three things are unverified at the same time:
 * that the command line he typed parses into the argv he meant, that the
 * program exists on his machine, and that the directory is the one he thinks it
 * is. Getting any of those wrong runs the wrong thing with an email pasted into
 * it. The sheet shows the actual argv and the actual prompt — the ones about to
 * be executed, not a description of them — so the answer is visible rather than
 * guessed at.
 *
 * After that, asking again would be theatre. He configured it, he watched it
 * work once, and the whole feature is worth having only if it costs one
 * gesture. `lastRunAt` on the target is the record; there is nothing else to it.
 */
export function HandoffDialog() {
  const request = useSyncExternalStore(subscribeHandoff, handoffRequest);
  const targets = useSyncExternalStore(subscribeTargets, targetSnapshot);
  const { ui, actions } = useMach();

  const [phase, setPhase] = useState<Phase>("idle");
  /**
   * The instruction being handed off, from wherever it came.
   *
   * The request seeds it and the field edits it, which is why it is state here
   * rather than `request.note` read at each use: after the field, the request
   * still says what ⌘K knew, which is nothing.
   */
  const [note, setNote] = useState("");
  const [preview, setPreview] = useState<HandoffPreview | null>(null);
  const [receipt, setReceipt] = useState<HandoffReceipt | null>(null);
  const [failure, setFailure] = useState<string | null>(null);
  const pending = useRef(0);
  const field = useRef<HTMLInputElement>(null);

  /*
   * The palette layer, mounted here rather than registered at import time.
   *
   * `registerResolver` is the seam `resolver.ts` exposes precisely so that a
   * new layer needs no edit to the chain, and doing it in an effect means hot
   * reload cannot end up with two copies of this resolver answering every
   * keystroke.
   */
  useEffect(() => {
    const unregister = registerResolver(handoffResolver);
    if (isTauri()) void loadTargets().catch(() => undefined);
    return unregister;
  }, []);

  /** Row ids only. Rust reads the thread and strips the quoted history. */
  const source = useMemo<HandoffSourceRef>(() => {
    if (ui.mode === "mail" && ui.threadId != null) {
      return { kind: "mail", threadId: ui.threadId };
    }
    if (ui.mode === "calendar" && ui.eventId != null) {
      return { kind: "event", eventId: ui.eventId };
    }
    return { kind: "none" };
  }, [ui.mode, ui.threadId, ui.eventId]);

  /** The row ⌘K picked. Only the title needs it — everything else is on the plan. */
  const target = useMemo(
    () => (request?.kind === "run" ? targets.find((t) => t.id === request.targetId) ?? null : null),
    [request, targets],
  );

  const launch = useCallback(
    async (targetId: string, note: string, mode?: HandoffPreview["mode"]) => {
      setPhase("running");
      setFailure(null);
      try {
        if (mode === "session") {
          // The pane takes it from here. Nothing more is shown: a window full
          // of the process's own output has already said that it started.
          await openSession({ targetId, note, source, cols: 100, rows: 30 });
          closeHandoff();
          setPhase("idle");
          return;
        }
        const done = await runHandoff({ targetId, note, source });
        setReceipt(done);
        // Terminal mode has already shown itself — a window opened. Anything
        // more would be Mach talking about its own behaviour.
        if (done.mode === "terminal") {
          closeHandoff();
          actions.setStatus(done.message);
          setPhase("idle");
        } else {
          setPhase("shown");
        }
      } catch (error) {
        setFailure(errorMessage(error));
        setPhase("failed");
      }
    },
    [source, actions],
  );

  const nonce = request?.nonce ?? 0;
  const kind = request?.kind;

  /*
   * ⌘K is what he opened this from, and the palette does not close itself.
   *
   * `choose()` records the pick and calls `run()`; closing is left to whatever
   * ran, because most palette actions are navigations that close as a side
   * effect of arriving somewhere. A handoff does not navigate, so without this
   * the palette stays up over the dialog — or, for a target that has already
   * run, stays up over nothing at all with the sentence still in it.
   */
  useEffect(() => {
    if (nonce) actions.setPalette(false);
    // The palette closing is a consequence of the request, not of `actions`.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [nonce]);

  /**
   * Everything a handoff does once it has a sentence to hand off.
   *
   * The request effect enters here with what ⌘K carried; the field enters here
   * with what he typed. Nothing downstream can tell the two apart, which is the
   * whole point — a handoff typed into this dialog is the same handoff.
   */
  const begin = useCallback(
    (targetId: string, sentence: string) => {
      const ticket = ++pending.current;

      setPreview(null);
      setReceipt(null);
      setFailure(null);

      const target = targetSnapshot().find((t) => t.id === targetId);
      if (target && target.lastRunAt && target.mode !== "session") {
        void launch(targetId, sentence, target.mode);
        return;
      }

      /*
       * Never run, or the list is stale, or it opens a session: ask, showing
       * what would actually happen.
       *
       * A session target asks *every* time, and the reason is the prompt rather
       * than the command. The other two modes throw a sentence at something and
       * forget it; this one puts a stranger's words in front of a running agent
       * with tools, in a pane, for as long as it takes. Prompt injection is not
       * solvable here and pretending otherwise would be worse than useless —
       * but a prompt he has read before it is sent is a different thing from
       * one he has not, and the whole prompt is on screen below.
       */
      setPhase("preparing");
      void previewHandoff({ targetId, note: sentence, source })
        .then((plan) => {
          if (ticket !== pending.current) return;
          if (!plan.unproven && plan.mode !== "session") {
            void launch(targetId, sentence, plan.mode);
            return;
          }
          setPreview(plan);
          setPhase("confirming");
        })
        .catch((error: unknown) => {
          if (ticket !== pending.current) return;
          setFailure(errorMessage(error));
          setPhase("failed");
        });
    },
    [launch, source],
  );

  useEffect(() => {
    if (!nonce || kind !== "run" || request?.kind !== "run") return;
    const { targetId, note: carried } = request;
    setNote(carried);

    // No sentence yet. The field is the answer to that, not a warning about it.
    if (!carried.trim()) {
      pending.current += 1;
      setPreview(null);
      setReceipt(null);
      setFailure(null);
      setPhase("asking");
      return;
    }

    begin(targetId, carried);
    // Only a new request may retrigger this.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [nonce, kind]);

  /*
   * The caret, on the field, without him reaching for it.
   *
   * The overlay puts focus on the first field when it *opens*, which covers the
   * ordinary case. This covers the other one: a dialog already up — showing a
   * result, or a failure — asked to hand off again with nothing typed.
   */
  useEffect(() => {
    if (phase === "asking") field.current?.focus();
  }, [phase, nonce]);

  // The inline result takes itself away.
  useEffect(() => {
    if (phase !== "shown") return;
    const timer = window.setTimeout(() => {
      closeHandoff();
      setPhase("idle");
    }, RESULT_MS);
    return () => window.clearTimeout(timer);
  }, [phase]);

  const dismiss = useCallback(() => {
    pending.current += 1;
    closeHandoff();
    setPhase("idle");
  }, []);

  /**
   * The field's ⏎, and its button.
   *
   * An empty field stays put. Nothing is launched and nothing is said about it:
   * the field is still there, still has the caret, and is its own explanation.
   */
  const submit = useCallback(() => {
    if (request?.kind !== "run") return;
    const sentence = note.trim();
    if (!sentence) return;
    setNote(sentence);
    begin(request.targetId, sentence);
  }, [begin, note, request]);

  /*
   * `preparing` is on this list, and it was the bug.
   *
   * ⌘K closes the palette the moment a handoff is asked for, and for an
   * unproven target — or any session target — the next thing that happens is a
   * `previewHandoff` round trip. With `preparing` missing from here the dialog
   * did not exist for the length of it, so choosing a target emptied the screen
   * and left it empty: the one gesture in this surface with no answer at all.
   */
  const open =
    request?.kind === "run" &&
    (phase === "asking" ||
      phase === "preparing" ||
      phase === "confirming" ||
      phase === "running" ||
      phase === "shown" ||
      phase === "failed");

  useKeyBindings([
    {
      keys: "escape",
      group: "Global",
      description: "Close handoff",
      allowInInput: true,
      // The frontmost thing on screen while it is up.
      priority: 312,
      when: () => open,
      handler: dismiss,
    },
    {
      keys: "enter",
      group: "Global",
      description: "Hand off",
      allowInInput: true,
      priority: 312,
      // Live for the whole of `asking`, empty field included: ⏎ on an empty
      // field is his key to press, and letting it fall through to whatever is
      // behind the dialog would be worse than it doing nothing.
      when: () => open && (phase === "asking" || (phase === "confirming" && preview !== null)),
      handler: () => {
        if (phase === "asking") {
          submit();
          return;
        }
        if (preview) void launch(preview.targetId, note, preview.mode);
      },
    },
  ]);

  if (request?.kind === "edit") {
    return <HandoffTargetsDialog onClose={dismiss} />;
  }
  if (!open) return null;

  return (
    <Overlay
      open
      onClose={dismiss}
      labelledBy="handoff-title"
      initialFocus={field}
      // `self-start` so the panel hugs what is in it. The overlay stretches its
      // child to the full 68vh otherwise, which for one field — or for the
      // one-line result, or the failure — is a sentence at the top of an empty
      // box the height of the window.
      className="max-w-[44rem] self-start"
    >
      <header className="flex h-10 shrink-0 items-center gap-2 border-b border-border px-3">
        <Send size={14} strokeWidth={1.75} className="shrink-0 text-faint-foreground" />
        <span id="handoff-title" className="truncate text-body text-foreground">
          {phase === "failed"
            ? "Nothing was launched"
            : preview
              ? `Hand off to ${preview.targetName}`
              : receipt
                ? receipt.targetName
                : target
                  ? `Hand off to ${target.name}`
                  : "Hand off"}
        </span>
        {phase === "confirming" && preview?.unproven && (
          <span className="truncate text-micro text-faint-foreground">first run of this target</span>
        )}
      </header>

      <div className="min-h-0 overflow-y-auto p-3">
        {phase === "asking" && (
          <BareInput
            ref={field}
            id="handoff-note"
            aria-label="What to do"
            value={note}
            placeholder="What to do"
            onChange={(event) => setNote(event.target.value)}
          />
        )}
        {phase === "confirming" && preview && <Confirm preview={preview} />}
        {phase === "preparing" && (
          <span className="flex items-center gap-1.5 text-list text-muted-foreground">
            <LoaderCircle size={13} strokeWidth={2} className="animate-spin" />
            Working out what would run…
          </span>
        )}
        {phase === "running" && (
          <span className="flex items-center gap-1.5 text-list text-muted-foreground">
            <LoaderCircle size={13} strokeWidth={2} className="animate-spin" />
            Launching…
          </span>
        )}
        {phase === "shown" && receipt && <Result receipt={receipt} />}
        {phase === "failed" && failure && (
          <span className="flex items-start gap-2 text-list text-danger">
            <TriangleAlert size={13} strokeWidth={1.75} className="mt-0.5 shrink-0" />
            <span className="min-w-0 break-words">{failure}</span>
          </span>
        )}
      </div>

      <footer className="flex h-9 shrink-0 items-center gap-2 border-t border-border px-3">
        {phase === "asking" ? (
          <span className="ml-auto flex shrink-0 items-center gap-1.5">
            <Button size="sm" onClick={dismiss}>
              Cancel
            </Button>
            {/* Disabled rather than a button that answers nothing: an empty
                handoff is not a thing to launch, and saying so by staying grey
                costs no reading. */}
            <Button size="sm" variant="default" disabled={!note.trim()} onClick={submit}>
              Hand off
            </Button>
          </span>
        ) : phase === "confirming" && preview ? (
          /* The `⏎ run it` chip that used to lead this row is what the button
             beside it already is. */
          <span className="ml-auto flex shrink-0 items-center gap-1.5">
            <Button size="sm" onClick={dismiss}>
              Cancel
            </Button>
            <Button
              size="sm"
              variant="default"
              onClick={() => void launch(preview.targetId, note, preview.mode)}
            >
              {preview.mode === "session" ? "Start session" : "Hand off"}
            </Button>
          </span>
        ) : (
          <Button size="sm" className="ml-auto" onClick={dismiss}>
            Done
          </Button>
        )}
      </footer>
    </Overlay>
  );
}

/** The plan, exactly as it will run. */
function Confirm({ preview }: { preview: HandoffPreview }) {
  return (
    <div className="flex flex-col gap-2">
      <dl className="grid grid-cols-[5rem_minmax(0,1fr)] gap-x-3 gap-y-1 text-micro">
        <dt className="text-faint-foreground">Runs</dt>
        {/* argv joined, so for `claude "{{prompt}}"` this line *is* the prompt
            again — thousands of characters of somebody else's mail, above the
            block that shows the same text with a label on it. Bounded here so
            the row stays a row: the readable copy is below, and this one keeps
            saying exactly what will be executed for anyone who scrolls it. */}
        <dd className="max-h-16 overflow-auto break-all font-mono text-muted-foreground">
          {preview.command}
        </dd>
        <dt className="text-faint-foreground">In</dt>
        <dd className="break-all font-mono text-muted-foreground">{preview.dir}</dd>
        <dt className="text-faint-foreground">Mode</dt>
        <dd className="text-muted-foreground">
          {preview.mode === "terminal"
            ? "Terminal session"
            : preview.mode === "session"
              ? "Session in a pane"
              : "Inline, output shown here"}
        </dd>
        {preview.contextLabel && (
          <>
            <dt className="text-faint-foreground">Context</dt>
            <dd className="truncate text-muted-foreground">{preview.contextLabel}</dd>
          </>
        )}
      </dl>

      {/* The block below is the argument the command is about to receive, and
          most of it was written by whoever sent the mail. Naming it is the
          difference between a prompt he read and one he did not. */}
      <span className="text-micro text-faint-foreground">Prompt</span>
      <pre
        aria-label="What will be sent"
        tabIndex={0}
        className={cn(
          "max-h-[38vh] overflow-auto whitespace-pre-wrap break-words",
          "rounded-[var(--radius)] bg-surface-raised p-2",
          "font-mono text-micro text-muted-foreground focus:outline-none",
        )}
      >
        {preview.prompt}
      </pre>
    </div>
  );
}

/** What an inline command printed. */
function Result({ receipt }: { receipt: HandoffReceipt }) {
  const output = receipt.stdout || receipt.stderr;
  const ok = receipt.status === 0 || receipt.status === null;
  return (
    <div className="flex flex-col gap-2">
      <span className="flex items-start gap-2 text-body">
        {ok ? (
          <CircleCheck size={15} strokeWidth={1.75} className="mt-px shrink-0 text-accent" />
        ) : (
          <TriangleAlert size={15} strokeWidth={1.75} className="mt-px shrink-0 text-danger" />
        )}
        <span className={cn("min-w-0", ok ? "text-foreground" : "text-danger")}>
          {receipt.message}
        </span>
      </span>
      {output && (
        <pre className="max-h-[32vh] overflow-auto whitespace-pre-wrap break-words rounded-[var(--radius)] bg-surface-raised p-2 font-mono text-micro text-muted-foreground">
          {output}
        </pre>
      )}
    </div>
  );
}
