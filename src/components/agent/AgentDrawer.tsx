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
import { useEffect, useRef, useState } from "react";
import {
  approve,
  deny,
  removeContext,
  sendMessage,
  type AgentSession,
  type ContextItem,
  type Entry,
} from "@/lib/agent";
import { errorMessage } from "@/lib/ipc";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Kbd } from "@/components/ui/kbd";

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
 *   doing" never needs a guess;
 * - **the approval bar**, when an outbound action is waiting. It names the
 *   consequence — who, and when — because approving a sentence you cannot read
 *   is not approving anything.
 */
export function AgentDrawer({
  session,
  onMinimise,
  onClose,
}: {
  session: AgentSession;
  onMinimise: () => void;
  onClose: () => void;
}) {
  const [draft, setDraft] = useState("");
  const [error, setError] = useState<string | null>(null);
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
      className="flex h-80 shrink-0 flex-col overflow-hidden border-t border-border bg-background"
    >
      <header className="flex h-8 shrink-0 items-center gap-2 border-b border-border px-3">
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
        <div className="flex shrink-0 flex-wrap items-center gap-1.5 border-b border-border px-3 py-1.5">
          {session.context.map((item) => (
            <ContextChip key={item.id} sessionId={session.id} item={item} />
          ))}
        </div>
      )}

      <div ref={scroller} className="min-h-0 flex-1 overflow-y-auto px-3 py-2">
        {session.entries.map((entry, index) => (
          <EntryRow key={rowKey(entry, index)} entry={entry} />
        ))}
        {session.streaming && (
          <p className="whitespace-pre-wrap py-1 text-list text-foreground">
            {session.streaming}
            <span className="ml-0.5 inline-block h-3 w-1 animate-pulse bg-accent align-middle" />
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
        <div className="flex shrink-0 items-center gap-2 border-t border-warning bg-surface-raised px-3 py-2">
          <span className="min-w-0 flex-1 text-list text-foreground">
            {session.pending.summary}
          </span>
          <Button
            size="sm"
            variant="subtle"
            onClick={() =>
              void deny(session.id, session.pending!.toolUseId, "Denied from the drawer.").catch(
                (e: unknown) => setError(errorMessage(e)),
              )
            }
          >
            Don&apos;t
          </Button>
          <Button
            size="sm"
            variant="default"
            onClick={() =>
              void approve(session.id, session.pending!.toolUseId).catch((e: unknown) =>
                setError(errorMessage(e)),
              )
            }
          >
            <Send size={11} strokeWidth={2} />
            Send it
          </Button>
        </div>
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
        {error ? (
          <span className="shrink-0 text-micro text-danger">{error}</span>
        ) : (
          <span className="flex shrink-0 items-center gap-1">
            <Kbd keys="escape" />
            <span className="text-micro text-faint-foreground">minimise</span>
          </span>
        )}
      </footer>
    </section>
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
function ContextChip({ sessionId, item }: { sessionId: string; item: ContextItem }) {
  return (
    <span className="group flex h-5 items-center gap-1 rounded-[var(--radius)] border border-border bg-surface pl-1.5 pr-1 text-micro text-muted-foreground">
      <Paperclip size={9} strokeWidth={2} className="shrink-0 text-faint-foreground" />
      <span className="max-w-[18rem] truncate text-foreground">{item.label}</span>
      <span className="text-faint-foreground">· added to context</span>
      <button
        type="button"
        aria-label={`Remove ${item.label} from context`}
        onClick={() => void removeContext(sessionId, item.id).catch(() => {})}
        className="rounded-[var(--radius)] p-0.5 text-faint-foreground opacity-0 hover:text-foreground group-hover:opacity-100"
      >
        <X size={9} strokeWidth={2} />
      </button>
    </span>
  );
}

function EntryRow({ entry }: { entry: Entry }) {
  if (entry.role === "user") {
    return (
      <p className="border-l-2 border-accent py-1 pl-2 text-list text-muted-foreground">
        {entry.text}
      </p>
    );
  }
  if (entry.role === "agent") {
    return <p className="whitespace-pre-wrap py-1 text-list text-foreground">{entry.text}</p>;
  }
  return (
    <p
      className={cn(
        "flex items-center gap-1.5 py-0.5 font-mono text-micro",
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
  );
}

/** Tool rows are identified by their tool-use id; prose rows by position. */
function rowKey(entry: Entry, index: number): string {
  return entry.role === "tool" ? `tool:${entry.id}` : `${entry.role}:${index}`;
}
