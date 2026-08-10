import {
  ArrowUpRight,
  Camera,
  CircleCheck,
  LoaderCircle,
  PenLine,
  Square,
  Trash2,
  TriangleAlert,
  Undo2,
} from "lucide-react";
import {
  useCallback,
  useEffect,
  useReducer,
  useRef,
  useState,
  useSyncExternalStore,
  type ReactNode,
} from "react";
import { useKeyBindings } from "@/hooks/useKeymap";
import { useMach } from "@/hooks/useMach";
import {
  annotationReducer,
  captureWindow,
  closeFeedback,
  emptyAnnotation,
  feedbackRequest,
  hasInk,
  submitFeedback,
  subscribeFeedback,
  type AnnotationTool,
  type FeedbackContextInfo,
  type FeedbackReceipt,
} from "@/lib/feedback";
import { errorMessage } from "@/lib/ipc";
import { cn } from "@/lib/utils";
import { Overlay } from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { Textarea } from "@/components/ui/textarea";
import { AnnotationCanvas } from "./AnnotationCanvas";

/**
 * How long to wait after the palette closes before photographing the window.
 *
 * The whole point is a picture of *what he was looking at*, so the ⌘K overlay
 * must be off the screen and repainted before the shutter. One frame is enough
 * in practice; this is several, and still under the threshold where the wait is
 * noticed.
 */
const SETTLE_MS = 120;

type Phase = "capturing" | "editing" | "sending" | "sent" | "failed";

const TOOLS: { id: AnnotationTool; label: string; icon: typeof PenLine }[] = [
  { id: "arrow", label: "Arrow", icon: ArrowUpRight },
  { id: "rect", label: "Box", icon: Square },
  { id: "pen", label: "Draw", icon: PenLine },
];

/**
 * Notice something, ⌘K, scribble on it, one line of text, and an agent is
 * working on it — without leaving the app.
 *
 * The capture happens the moment this opens, before anything is rendered: the
 * context is almost always "this thing, right here", and asking him to take a
 * screenshot first would cost more than the fix.
 */
export function FeedbackDialog() {
  const request = useSyncExternalStore(subscribeFeedback, feedbackRequest);
  const { ui, accounts, allThreads, actions } = useMach();

  const [phase, setPhase] = useState<Phase>("capturing");
  const [shot, setShot] = useState<string | null>(null);
  const [captureError, setCaptureError] = useState<string | null>(null);
  const [text, setText] = useState("");
  const [tool, setTool] = useState<AnnotationTool>("arrow");
  const [annotation, annotate] = useReducer(annotationReducer, emptyAnnotation);
  const [receipt, setReceipt] = useState<FeedbackReceipt | null>(null);
  const [failure, setFailure] = useState<string | null>(null);
  const canvasRef = useRef<HTMLCanvasElement | null>(null);

  const nonce = request?.nonce ?? 0;
  const seed = request?.seed ?? "";

  useEffect(() => {
    if (!nonce) return;
    // The palette is what he opened this from; it must not be in the picture.
    actions.setPalette(false);
    setPhase("capturing");
    setShot(null);
    setCaptureError(null);
    setReceipt(null);
    setFailure(null);
    setText(seed);
    setTool("arrow");
    annotate({ type: "clear" });

    let cancelled = false;
    const timer = window.setTimeout(() => {
      void captureWindow()
        .then((data) => {
          if (!cancelled) setShot(data);
        })
        .catch((error: unknown) => {
          // A failed capture is not a failed session: he can still say the
          // thing in words.
          if (!cancelled) setCaptureError(errorMessage(error));
        })
        .finally(() => {
          if (!cancelled) setPhase("editing");
        });
    }, SETTLE_MS);

    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
    // Reopening re-captures; nothing else may retrigger the shutter.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [nonce]);

  const open = request !== null;
  const canSend = text.trim().length > 0 && phase !== "sending";

  const send = useCallback(async () => {
    if (!text.trim() || phase === "sending") return;
    setPhase("sending");
    setFailure(null);
    try {
      const image = shot ? (canvasRef.current?.toDataURL("image/png") ?? shot) : null;
      const filed = await submitFeedback({
        text: text.trim(),
        imagePngBase64: image,
        context: describeContext(),
      });
      setReceipt(filed);
      setPhase("sent");
    } catch (error) {
      setFailure(errorMessage(error));
      setPhase("failed");
    }

    function describeContext(): FeedbackContextInfo {
      return {
        mode: ui.mode,
        view: ui.mode === "calendar" ? ui.calendarView : undefined,
        label: ui.labelId,
        account:
          ui.accountId == null
            ? "all accounts (unified)"
            : accounts.find((a) => a.id === ui.accountId)?.email,
        thread:
          ui.threadId == null
            ? undefined
            : allThreads.find((t) => t.id === ui.threadId)?.subject,
      };
    }
  }, [text, phase, shot, ui, accounts, allThreads]);

  useKeyBindings([
    {
      keys: "escape",
      group: "Global",
      description: "Close feedback",
      allowInInput: true,
      // Above the palette: this dialog is the frontmost thing on screen.
      priority: 300,
      when: () => open,
      handler: () => closeFeedback(),
    },
    {
      keys: "mod+enter",
      group: "Global",
      description: "Send feedback to an agent",
      allowInInput: true,
      priority: 300,
      when: () => open && phase === "editing",
      handler: () => void send(),
    },
    {
      keys: "mod+z",
      allowInInput: true,
      priority: 300,
      when: () => open && phase === "editing" && hasInk(annotation),
      handler: () => annotate({ type: "undo" }),
    },
  ]);

  // Nothing is rendered while the shutter is open — a dialog in its own
  // screenshot would be the one thing he never means.
  if (!open || phase === "capturing") return null;

  return (
    <Overlay
      open
      onClose={closeFeedback}
      labelledBy="feedback-title"
      className="max-h-[88vh] max-w-[52rem]"
    >
      <header className="flex h-10 shrink-0 items-center gap-2 border-b border-border px-3">
        <Camera size={14} strokeWidth={1.75} className="shrink-0 text-faint-foreground" />
        <span id="feedback-title" className="text-body text-foreground">
          Send feedback
        </span>
        <span className="truncate text-micro text-faint-foreground">
          {phase === "sent" ? "filed" : "to an agent in this repo"}
        </span>
      </header>

      <div className="min-h-0 flex-1 overflow-y-auto">
        {phase === "sent" && receipt ? (
          <Filed receipt={receipt} />
        ) : (
          <div className="flex flex-col gap-2 p-3">
            {shot ? (
              <>
                <div className="flex items-center gap-1">
                  {TOOLS.map(({ id, label, icon: Icon }) => (
                    <Button
                      key={id}
                      size="sm"
                      variant={tool === id ? "subtle" : "ghost"}
                      aria-pressed={tool === id}
                      onClick={() => setTool(id)}
                      className={cn(tool === id && "text-foreground")}
                    >
                      <Icon size={12} strokeWidth={1.75} />
                      {label}
                    </Button>
                  ))}
                  <span className="mx-1 h-3.5 w-px bg-border" />
                  <Button
                    size="sm"
                    onClick={() => annotate({ type: "undo" })}
                    disabled={!hasInk(annotation)}
                  >
                    <Undo2 size={12} strokeWidth={1.75} />
                    Undo
                  </Button>
                  <Button
                    size="sm"
                    onClick={() => annotate({ type: "clear" })}
                    disabled={!hasInk(annotation)}
                  >
                    <Trash2 size={12} strokeWidth={1.75} />
                    Clear
                  </Button>
                  <span className="ml-auto text-micro text-faint-foreground">
                    {/*
                      The empty state used to coach ("an arrow says 'that one'
                      better than a sentence can"). The toolbar is right there;
                      what it needed was the verb, not the argument for it.
                    */}
                    {hasInk(annotation) ? null : "drag to mark, or skip"}
                  </span>
                </div>

                <div className="max-h-[42vh] overflow-auto rounded-[var(--radius)] bg-surface-raised">
                  <AnnotationCanvas
                    src={shot}
                    tool={tool}
                    state={annotation}
                    dispatch={annotate}
                    canvasRef={canvasRef}
                  />
                </div>
              </>
            ) : (
              <Notice tone="warn">
                No screenshot — {captureError ?? "the window could not be captured"}.
              </Notice>
            )}

            <Textarea
              autoFocus
              rows={3}
              value={text}
              onChange={(event) => setText(event.target.value)}
              placeholder="What should change?"
              className="py-1.5"
            />

            {phase === "failed" && failure && (
              <Notice tone="error">Not filed — {failure}</Notice>
            )}
          </div>
        )}
      </div>

      <footer className="flex h-9 shrink-0 items-center gap-2 border-t border-border px-3">
        {phase === "sent" ? (
          <>
            {/*
              "Frontend fixes hot-reload into this window; Rust changes need a
              relaunch" used to sit here — the build system explaining itself
              to the only person who already knows how it works.
            */}
            <Button size="sm" variant="subtle" className="ml-auto" onClick={closeFeedback}>
              Done
            </Button>
          </>
        ) : (
          <>
            {/* No `⌘⏎ send` chip: the button that sends is right there, and it
                is the only thing this footer is for. */}
            <span className="ml-auto flex items-center gap-1.5">
              <Button size="sm" onClick={closeFeedback}>
                Cancel
              </Button>
              <Button size="sm" variant="default" disabled={!canSend} onClick={() => void send()}>
                {phase === "sending" ? (
                  <>
                    <LoaderCircle size={12} strokeWidth={2} className="animate-spin" />
                    Filing
                  </>
                ) : phase === "failed" ? (
                  "Try again"
                ) : (
                  "Send to agent"
                )}
              </Button>
            </span>
          </>
        )}
      </footer>
    </Overlay>
  );
}

/** The confirmation. Never just "closed" — he has to see that it landed. */
function Filed({ receipt }: { receipt: FeedbackReceipt }) {
  return (
    <div className="flex flex-col gap-3 p-4">
      <div className="flex items-start gap-2">
        <CircleCheck size={15} strokeWidth={1.75} className="mt-px shrink-0 text-accent" />
        <div className="flex flex-col gap-1">
          <span className="text-body text-foreground">
            {receipt.taskId ? `Task #${receipt.taskId}` : "Filed"}
          </span>
          <span className="text-list text-muted-foreground">
            Queued in <span className="font-mono">mach</span>.
          </span>
        </div>
      </div>

      <dl className="grid grid-cols-[6rem_1fr] gap-x-3 gap-y-1 text-micro">
        <dt className="text-faint-foreground">What it says</dt>
        <dd className="text-muted-foreground">{receipt.message}</dd>
        {receipt.screenshotPath && (
          <>
            <dt className="text-faint-foreground">Screenshot</dt>
            <dd className="break-all font-mono text-muted-foreground">
              {receipt.screenshotPath}
            </dd>
          </>
        )}
        {receipt.output && receipt.output !== receipt.message && (
          <>
            <dt className="text-faint-foreground">ty said</dt>
            <dd className="whitespace-pre-wrap font-mono text-muted-foreground">
              {receipt.output}
            </dd>
          </>
        )}
      </dl>
    </div>
  );
}

function Notice({ tone, children }: { tone: "warn" | "error"; children: ReactNode }) {
  return (
    <div
      className={cn(
        "flex items-start gap-2 rounded-[var(--radius)] border px-2 py-1.5 text-list",
        tone === "error"
          ? "border-danger/40 text-danger"
          : "border-border text-muted-foreground",
      )}
    >
      <TriangleAlert size={13} strokeWidth={1.75} className="mt-0.5 shrink-0" />
      <span className="min-w-0 break-words">{children}</span>
    </div>
  );
}
