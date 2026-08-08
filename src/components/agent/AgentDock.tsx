import { Sparkles } from "lucide-react";
import {
  useCallback,
  useEffect,
  useMemo,
  useReducer,
  useRef,
  useState,
  useSyncExternalStore,
} from "react";
import { useKeyBindings } from "@/hooks/useKeymap";
import { useMach } from "@/hooks/useMach";
import {
  askRequest,
  clearAsk,
  closeSession,
  contextFor,
  listSessions,
  listenToSessions,
  paletteQuery,
  reduceSessions,
  requestAsk,
  startSession,
  subscribeAsk,
  type AgentEvent,
  type AgentSession,
} from "@/lib/agent";
import { errorMessage } from "@/lib/ipc";
import { AgentDrawer } from "./AgentDrawer";
import { AgentPill } from "./AgentPill";

/**
 * The bottom bar, and everything that owns session state.
 *
 * Asking is not modal — that is the design of this unit. So the dock is a strip
 * of pills above the status bar, each one a session, each expanding into a
 * drawer. The mail list keeps its focus, its scroll and its keybindings the
 * whole time.
 *
 * Three things live here and nowhere else:
 *
 * 1. **the sessions**, reduced from the `agent-session` event stream. Pushed,
 *    never polled — `agent_sessions` is read once on mount, for the case where
 *    the window reloaded while a session was still running;
 * 2. **the ⌘K handoff.** `lib/agent.ts` records the ask; this closes the
 *    palette, gathers what is on screen, and starts the session. The same shape
 *    as `FeedbackDialog`, for the same reason: a palette resolver is a plain
 *    function with no access to `useMach`;
 * 3. **⇥.** The palette owns its input as local state, so the keystroke is
 *    claimed here at a higher priority and reads the query the resolver saw.
 */
export function AgentDock() {
  const mach = useMach();
  const { ui, actions } = mach;

  const [sessions, apply] = useReducer(reduceSessions, [] as AgentSession[]);
  const [openId, setOpenId] = useState<string | null>(null);
  const [starting, setStarting] = useState(false);

  /* ------------------------------------------------------------- the stream */

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let cancelled = false;

    void listenToSessions((event: AgentEvent) => apply(event)).then((off) => {
      if (cancelled) off();
      else unlisten = off;
    });

    // A reload while a session was running must not orphan it: the engine
    // outlives the webview.
    void listSessions().then((existing) => {
      if (cancelled) return;
      for (const session of existing) {
        apply({ type: "created", sessionId: session.id, session });
      }
    });

    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  /* --------------------------------------------------------------- the ask */

  const ask = useSyncExternalStore(subscribeAsk, askRequest);
  const nonce = ask?.nonce ?? 0;
  const prompt = ask?.prompt ?? "";

  // The whole shell, in a ref, so starting a session reads the *current* view
  // without the effect re-running every time a thread finishes loading.
  const shell = useRef(mach);
  shell.current = mach;

  useEffect(() => {
    if (!nonce || !prompt) return;
    clearAsk();
    // The palette is what he asked from; it must not sit over the answer.
    actions.setPalette(false);

    const now = shell.current;
    const context = contextFor({
      mode: now.ui.mode,
      calendarView: now.ui.calendarView,
      anchor: now.ui.anchor,
      labelId: now.ui.labelId,
      mailboxName: now.labels.find((label) => label.id === now.ui.labelId)?.name,
      threadId: now.ui.threadId,
      eventId: now.ui.eventId,
      selectedThread: now.visibleThreads[now.selectedIndex] ?? null,
      openThread: now.detail,
      selectedEvent: now.selectedEvent,
    });

    setStarting(true);
    void startSession(prompt, context)
      .then((session) => {
        apply({ type: "created", sessionId: session.id, session });
        setOpenId(session.id);
      })
      .catch((error: unknown) => {
        // A missing ANTHROPIC_API_KEY lands here, saying exactly what to set.
        actions.setStatus(errorMessage(error), "error");
      })
      .finally(() => setStarting(false));
    // Only a new ask may start a session.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [nonce]);

  /* ------------------------------------------------------------- keystrokes */

  useKeyBindings([
    {
      // The palette's own ⇥ is a placeholder from before this unit existed;
      // this claims the keystroke at a higher priority while the palette is
      // open. The round trip is always an explicit keystroke — never a pause.
      keys: "tab",
      group: "Global",
      description: "Ask the agent",
      allowInInput: true,
      priority: 300,
      when: () => ui.paletteOpen,
      handler: () => {
        const query = paletteQuery().trim();
        if (!query) {
          actions.setStatus("Type a question first, then ⇥ hands it to the agent");
          return;
        }
        requestAsk(query);
      },
    },
    {
      keys: "escape",
      allowInInput: true,
      priority: 150,
      when: () => openId !== null && !ui.paletteOpen,
      handler: () => setOpenId(null),
    },
  ]);

  /* ----------------------------------------------------------------- render */

  const open = useMemo(
    () => sessions.find((session) => session.id === openId) ?? null,
    [sessions, openId],
  );

  const close = useCallback((id: string) => {
    setOpenId((current) => (current === id ? null : current));
    void closeSession(id).catch(() => {});
    apply({ type: "closed", sessionId: id });
  }, []);

  if (sessions.length === 0 && !starting) return null;

  return (
    <>
      {open && (
        <AgentDrawer
          session={open}
          onMinimise={() => setOpenId(null)}
          onClose={() => close(open.id)}
        />
      )}

      <div
        aria-label="Agent sessions"
        className="flex h-8 shrink-0 items-center gap-1.5 overflow-x-auto border-t border-border bg-surface px-3"
      >
        <Sparkles size={12} strokeWidth={1.75} className="shrink-0 text-faint-foreground" />
        {starting && sessions.length === 0 && (
          <span className="text-micro text-faint-foreground">Starting…</span>
        )}
        {sessions.map((session) => (
          <AgentPill
            key={session.id}
            session={session}
            active={session.id === openId}
            onOpen={() => setOpenId(session.id === openId ? null : session.id)}
            onClose={() => close(session.id)}
          />
        ))}
      </div>
    </>
  );
}
