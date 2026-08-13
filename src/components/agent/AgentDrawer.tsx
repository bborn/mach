import {
  Check,
  ChevronDown,
  CircleAlert,
  LoaderCircle,
  Paperclip,
  Send,
  Wrench,
  X,
} from "lucide-react";
import { Fragment, useEffect, useRef, useState } from "react";
import {
  approve,
  deny,
  removeContext,
  sendMessage,
  type AgentSession,
  type Artifact,
  type ContextItem,
  type Entry,
  type PendingApproval,
} from "@/lib/agent";
import { errorMessage } from "@/lib/ipc";
import { openExternal } from "@/lib/message-body";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { ArtifactCard } from "./ArtifactCard";
import { parseMarkdown, type MarkdownLine, type Segment } from "./markdown";

/**
 * One session, expanded.
 *
 * Deliberately **not** a modal: it is a panel anchored above the dock, the rest
 * of the window stays live, and Escape only minimises it. He keeps reading mail
 * while it works — that is the point of the whole unit.
 *
 * Three regions, top to bottom:
 *
 * - **what is attached**, as removable lines. Implicit attachment is what makes
 *   it feel like talking about the screen; showing it is what stops the agent
 *   silently working from the wrong thing;
 * - **the conversation**, with each tool call as its own line so "what is it
 *   doing" never needs a guess — and, where the call surfaced an object, a card
 *   drawn as that object which opens it. The agent used to say it had drafted a
 *   reply and leave the draft unreachable from anywhere in the app; then it
 *   answered "show me the event" with the calendar row retyped as bullet points.
 *   A sentence is not an affordance, and `ArtifactCard` is what fixes both;
 * - **the approval bar**, when an outbound action is waiting. It names the
 *   consequence — who, and when — because approving a sentence you cannot read
 *   is not approving anything.
 *
 * # Why it is nearly ruleless now
 *
 * Every one of those regions used to be fenced off with a one-pixel line, and
 * stacked in a panel a few hundred pixels tall they read as a stack of nothing
 * in particular: a rule under the title, a rule under the context chips, a rule
 * over the dock, a rule over the status bar. A rule earns its place by
 * separating two things that could be confused for each other, and a title and
 * the conversation it titles are not two such things — space says it. What is
 * left is the boundary between the drawer and the app behind it (drawn by the
 * container in `App.tsx`, and doubling as the drag handle), and the one between
 * the record and the box you type into, which are genuinely different kinds of
 * thing. The approval bar keeps its warning-coloured edge because that one is
 * an alarm, not a fence.
 */
export function AgentDrawer({
  session,
  height,
  onMinimise,
  onClose,
  onOpenArtifact,
}: {
  session: AgentSession;
  /** What the divider above it says. See `drawer-height.ts` for the clamp. */
  height: number;
  onMinimise: () => void;
  onClose: () => void;
  /** Put the owner in front of something a tool made. See `Artifact`. */
  onOpenArtifact: (artifact: Artifact) => void;
}) {
  const [draft, setDraft] = useState("");
  const [error, setError] = useState<string | null>(null);
  /**
   * The approval this drawer has answered, before the agent has said so.
   *
   * The highest-stakes pair of buttons in the app had no same-frame feedback at
   * all: "Send it" dispatched an IPC call and then everything sat exactly as it
   * was until Rust pushed a `status` back, which is long enough to press it
   * twice and not know which press counted.
   *
   * Keyed by the tool call rather than held as a bare flag, so it can only ever
   * describe the approval it was given for: the next one arrives with a
   * different `toolUseId` and this stops applying on its own, with nothing to
   * remember to clear.
   */
  const [answered, setAnswered] = useState<{
    toolUseId: string;
    choice: "approve" | "deny";
  } | null>(null);
  const scroller = useRef<HTMLDivElement>(null);

  // Follow the conversation as it streams.
  useEffect(() => {
    const node = scroller.current;
    if (node) node.scrollTop = node.scrollHeight;
  }, [session.entries.length, session.streaming, session.pending]);

  const send = () => {
    const text = draft.trim();
    if (!text) return;
    setDraft("");
    void sendMessage(session.id, text).catch((e: unknown) => setError(errorMessage(e)));
  };

  return (
    <section
      aria-label={`Agent session: ${session.title}`}
      style={{ height }}
      className="flex shrink-0 flex-col overflow-hidden bg-background"
    >
      <header className="flex h-9 shrink-0 items-center gap-2 px-3">
        <span className="truncate text-list text-foreground">{session.title}</span>
        <StatusLabel session={session} />
        {/*
          Which brain answered. Worth a few characters of chrome because the
          answer can differ between two sessions open at once — the preference
          is read when a session starts, not when it is read back — and because
          "why is this one slower / better" is otherwise unanswerable.
        */}
        {session.backend && (
          <span className="hidden truncate text-micro text-faint-foreground sm:inline">
            {session.backend}
          </span>
        )}
        <div className="ml-auto flex items-center gap-0.5">
          <Button size="icon" variant="ghost" onClick={onMinimise} aria-label="Minimise">
            <ChevronDown size={13} strokeWidth={1.75} />
          </Button>
          <Button size="icon" variant="ghost" onClick={onClose} aria-label="Close session">
            <X size={13} strokeWidth={1.75} />
          </Button>
        </div>
      </header>

      {session.context.length > 0 && (
        <div className="flex shrink-0 flex-wrap items-center gap-1.5 px-3 pb-1.5">
          {session.context.map((item) => (
            <ContextChip
              key={item.id}
              sessionId={session.id}
              item={item}
              onError={setError}
            />
          ))}
        </div>
      )}

      <div ref={scroller} className="min-h-0 flex-1 overflow-y-auto px-3 py-2">
        {session.entries.map((entry, index) => (
          <EntryRow
            key={rowKey(entry, index)}
            entry={entry}
            onOpenArtifact={onOpenArtifact}
          />
        ))}
        {session.streaming && (
          <p className="whitespace-pre-wrap py-1 text-list text-foreground">
            <Prose text={session.streaming} />
            <span className="ml-0.5 inline-block h-3 w-1 animate-pulse bg-accent align-middle motion-reduce:animate-none" />
          </p>
        )}
        {session.error && (
          <p className="flex items-start gap-1.5 py-1 text-list text-danger">
            <CircleAlert size={13} strokeWidth={1.75} className="mt-0.5 shrink-0" />
            {session.error}
          </p>
        )}
      </div>

      {session.pending && (
        <ApprovalBar
          pending={session.pending}
          answered={answered?.toolUseId === session.pending.toolUseId ? answered.choice : null}
          onAnswer={(choice) => {
            const toolUseId = session.pending!.toolUseId;
            // Before the first `await`, so the bar says which button was pressed
            // in the frame it was pressed in.
            setAnswered({ toolUseId, choice });
            setError(null);
            const call =
              choice === "approve"
                ? approve(session.id, toolUseId)
                : deny(session.id, toolUseId, "Denied from the drawer.");
            void call.catch((e: unknown) => {
              // The exact inverse: the answer never left, so the buttons come
              // back and the reason goes in the footer.
              setAnswered((current) => (current?.toolUseId === toolUseId ? null : current));
              setError(errorMessage(e));
            });
          }}
        />
      )}

      <footer className="flex h-9 shrink-0 items-center gap-2 border-t border-border px-3">
        <input
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey) {
              e.preventDefault();
              e.stopPropagation();
              send();
            }
          }}
          placeholder="Say something else…"
          className="min-w-0 flex-1 bg-transparent text-list text-foreground outline-none placeholder:text-faint-foreground"
        />
        {/* An `Esc minimise` chip used to fill this corner whenever there was no
            error to put in it. The drawer's own header draws minimise and close
            as buttons; a hint that exists to occupy space is furniture. */}
        {error && <span className="shrink-0 text-micro text-danger">{error}</span>}
      </footer>
    </section>
  );
}

/**
 * The approval bar, and what it looks like once you have answered it.
 *
 * The answer stays on screen rather than the bar vanishing, because the bar is
 * also the record of what was asked: replacing it with nothing would leave the
 * drawer looking exactly as it did before the agent asked, and "did I press
 * that?" is the question this whole change is about. Both buttons go quiet and
 * the one that was chosen keeps its label with a spinner beside it, so the bar
 * says which of the two was pressed and that it has not finished yet.
 */
function ApprovalBar({
  pending,
  answered,
  onAnswer,
}: {
  pending: PendingApproval;
  answered: "approve" | "deny" | null;
  onAnswer: (choice: "approve" | "deny") => void;
}) {
  return (
    <div className="flex shrink-0 items-center gap-2 border-t border-warning bg-surface-raised px-3 py-2">
      <span className="min-w-0 flex-1 text-list text-foreground">{pending.summary}</span>
      <Button
        size="sm"
        variant="subtle"
        disabled={answered !== null}
        onClick={() => onAnswer("deny")}
      >
        {answered === "deny" && (
          <LoaderCircle size={11} strokeWidth={2} className="animate-spin" />
        )}
        Don&apos;t
      </Button>
      <Button
        size="sm"
        variant="default"
        disabled={answered !== null}
        onClick={() => onAnswer("approve")}
      >
        {answered === "approve" ? (
          <LoaderCircle size={11} strokeWidth={2} className="animate-spin" />
        ) : (
          <Send size={11} strokeWidth={2} />
        )}
        Send it
      </Button>
    </div>
  );
}

function StatusLabel({ session }: { session: AgentSession }) {
  if (session.status === "running") {
    return (
      <span className="flex shrink-0 items-center gap-1 text-micro text-faint-foreground">
        <LoaderCircle size={11} strokeWidth={2} className="animate-spin" />
        working
      </span>
    );
  }
  if (session.status === "awaitingApproval") {
    return <span className="shrink-0 text-micro text-warning">waiting for you</span>;
  }
  if (session.status === "failed") {
    return <span className="shrink-0 text-micro text-danger">failed</span>;
  }
  return (
    <span className="flex shrink-0 items-center gap-1 text-micro text-faint-foreground">
      <Check size={11} strokeWidth={2} className="text-success" />
      done
    </span>
  );
}

/** The removable line: `Re: Series A data room · added to context`. */
function ContextChip({
  sessionId,
  item,
  onError,
}: {
  sessionId: string;
  item: ContextItem;
  onError: (message: string) => void;
}) {
  /*
   * The chip goes now, and comes back if the call is refused.
   *
   * It used to wait for a `context` event to arrive from Rust, so clicking the
   * × on a chip did nothing you could see — and the `.catch(() => {})` on the
   * end meant a refusal did nothing you could see either, permanently.
   */
  const [removed, setRemoved] = useState(false);
  if (removed) return null;
  return (
    <span className="group flex h-5 items-center gap-1 rounded-[var(--radius)] border border-border bg-surface pl-1.5 pr-1 text-micro text-muted-foreground">
      <Paperclip size={9} strokeWidth={2} className="shrink-0 text-faint-foreground" />
      <span className="max-w-[18rem] truncate text-foreground">{item.label}</span>
      <span className="text-faint-foreground">· added to context</span>
      <button
        type="button"
        aria-label={`Remove ${item.label} from context`}
        onClick={() => {
          setRemoved(true);
          void removeContext(sessionId, item.id).catch((e: unknown) => {
            setRemoved(false);
            onError(errorMessage(e));
          });
        }}
        className="rounded-[var(--radius)] p-0.5 text-faint-foreground opacity-0 hover:text-foreground group-hover:opacity-100"
      >
        <X size={9} strokeWidth={2} />
      </button>
    </span>
  );
}

function EntryRow({
  entry,
  onOpenArtifact,
}: {
  entry: Entry;
  onOpenArtifact: (artifact: Artifact) => void;
}) {
  if (entry.role === "user") {
    return (
      <p className="border-l-2 border-accent py-1 pl-2 text-list text-muted-foreground">
        {entry.text}
      </p>
    );
  }
  if (entry.role === "agent") {
    return (
      <p className="whitespace-pre-wrap py-1 text-list text-foreground">
        <Prose text={entry.text} />
      </p>
    );
  }
  return (
    <div className="py-0.5">
      <p
        className={cn(
          "flex items-center gap-1.5 font-mono text-micro",
          entry.state === "error" && "text-danger",
          entry.state === "denied" && "text-warning",
          entry.state !== "error" && entry.state !== "denied" && "text-faint-foreground",
        )}
      >
        {entry.state === "running" ? (
          <LoaderCircle size={10} strokeWidth={2} className="shrink-0 animate-spin" />
        ) : (
          <Wrench size={10} strokeWidth={2} className="shrink-0" />
        )}
        <span className="truncate">{entry.summary}</span>
        <span className="shrink-0 text-faint-foreground opacity-60">{entry.name}</span>
      </p>
      {/*
        The object the call surfaced, drawn as itself.

        It used to be a button on the line above saying "Show event", with
        everything the event *was* left to the model to retype underneath. The
        card is the affordance and the description at once — see `ArtifactCard`.
      */}
      {entry.artifact && (
        <ArtifactCard
          artifact={entry.artifact}
          onOpen={() => onOpenArtifact(entry.artifact!)}
          onOpenExternal={(url) => void openExternal(url)}
        />
      )}
    </div>
  );
}

/**
 * What the agent said, with its Markdown honoured rather than printed.
 *
 * Spans only, and no layout of its own: the paragraph around it is
 * `whitespace-pre-wrap`, so the answer's own line breaks are the layout and
 * this decides nothing but weight. See `markdown.ts` for what is covered.
 */
function Prose({ text }: { text: string }) {
  return (
    <>
      {parseMarkdown(text).map((line, index) => (
        <Fragment key={index}>
          {index > 0 && "\n"}
          <Line line={line} />
        </Fragment>
      ))}
    </>
  );
}

function Line({ line }: { line: MarkdownLine }) {
  const runs = line.segments.map((segment, index) => <Run key={index} segment={segment} />);
  // A heading in three sentences of chat is a label, not a title: the weight
  // is the whole of it, and a type scale here would only make the drawer
  // louder than the mail behind it.
  return line.kind === "heading" ? <span className="font-medium">{runs}</span> : <>{runs}</>;
}

function Run({ segment }: { segment: Segment }) {
  switch (segment.kind) {
    case "strong":
      return <strong className="font-medium text-foreground">{segment.text}</strong>;
    case "em":
      return <em>{segment.text}</em>;
    case "code":
      return (
        <code className="rounded-[3px] bg-surface-raised px-1 font-mono text-micro">
          {segment.text}
        </code>
      );
    default:
      return <>{segment.text}</>;
  }
}

/** Tool rows are identified by their tool-use id; prose rows by position. */
function rowKey(entry: Entry, index: number): string {
  return entry.role === "tool" ? `tool:${entry.id}` : `${entry.role}:${index}`;
}
