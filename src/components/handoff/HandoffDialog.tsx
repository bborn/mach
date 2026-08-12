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
  targetSnapshot,
  type HandoffPreview,
  type HandoffReceipt,
  type HandoffSourceRef,
} from "@/lib/handoff";
import { errorMessage, isTauri } from "@/lib/ipc";
import { cn } from "@/lib/utils";
import { Overlay } from "@/components/ui/dialog";
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

type Phase = "idle" | "preparing" | "confirming" | "running" | "shown" | "failed";

/**
 * The runner. Renders nothing almost all of the time, and that is the point.
 *
 * A handoff to a target he has used before is one keystroke: ⌘K, the sentence,
 * enter. This component opens, launches, and closes without ever painting. It
 * paints in exactly three cases —
 *
 *  * **the first time a target is used**, where it asks (see below);
 *  * **an inline result**, for the few seconds it is worth reading;
 *  * **a failure**, because a handoff that silently did nothing is the worst
 *    outcome available and this codebase has paid for that lesson already.
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
  const { ui, actions } = useMach();

  const [phase, setPhase] = useState<Phase>("idle");
  const [preview, setPreview] = useState<HandoffPreview | null>(null);
  const [receipt, setReceipt] = useState<HandoffReceipt | null>(null);
  const [failure, setFailure] = useState<string | null>(null);
  const pending = useRef(0);

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

  useEffect(() => {
    if (!nonce || kind !== "run" || request?.kind !== "run") return;
    const { targetId, note } = request;
    const ticket = ++pending.current;

    setPreview(null);
    setReceipt(null);
    setFailure(null);

    const target = targetSnapshot().find((t) => t.id === targetId);
    if (target && target.lastRunAt && target.mode !== "session") {
      void launch(targetId, note, target.mode);
      return;
    }

    /*
     * Never run, or the list is stale, or it opens a session: ask, showing what
     * would actually happen.
     *
     * A session target asks *every* time, and the reason is the prompt rather
     * than the command. The other two modes throw a sentence at something and
     * forget it; this one puts a stranger's words in front of a running agent
     * with tools, in a pane, for as long as it takes. Prompt injection is not
     * solvable here and pretending otherwise would be worse than useless — but
     * a prompt he has read before it is sent is a different thing from one he
     * has not, and the whole prompt is on screen below.
     */
    setPhase("preparing");
    void previewHandoff({ targetId, note, source })
      .then((plan) => {
        if (ticket !== pending.current) return;
        if (!plan.unproven && plan.mode !== "session") {
          void launch(targetId, note, plan.mode);
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
    // Only a new request may retrigger this.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [nonce, kind]);

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
    (phase === "preparing" ||
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
      when: () => open && phase === "confirming" && preview !== null,
      handler: () => {
        if (request?.kind === "run" && preview) {
          void launch(preview.targetId, request.note, preview.mode);
        }
      },
    },
  ]);

  if (request?.kind === "edit") {
    return <HandoffTargetsDialog onClose={dismiss} />;
  }
  if (!open) return null;

  return (
    <Overlay open onClose={dismiss} labelledBy="handoff-title" className="max-w-[44rem]">
      <header className="flex h-10 shrink-0 items-center gap-2 border-b border-border px-3">
        <Send size={14} strokeWidth={1.75} className="shrink-0 text-faint-foreground" />
        <span id="handoff-title" className="truncate text-body text-foreground">
          {phase === "failed"
            ? "Nothing was launched"
            : preview
              ? `Hand off to ${preview.targetName}`
              : receipt
                ? receipt.targetName
                : "Hand off"}
        </span>
        {phase === "confirming" && preview?.unproven && (
          <span className="truncate text-micro text-faint-foreground">first run of this target</span>
        )}
      </header>

      <div className="min-h-0 overflow-y-auto p-3">
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
        {phase === "confirming" && preview ? (
          /* The `⏎ run it` chip that used to lead this row is what the button
             beside it already is. */
          <span className="ml-auto flex shrink-0 items-center gap-1.5">
            <Button size="sm" onClick={dismiss}>
              Cancel
            </Button>
            <Button
              size="sm"
              variant="default"
              onClick={() => {
                if (request?.kind === "run") {
                  void launch(preview.targetId, request.note, preview.mode);
                }
              }}
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
