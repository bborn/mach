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
import { AGENT_DRAWER_HEIGHT_BOUNDS } from "@/lib/prefs";
import { useUiSession } from "@/components/prefs/PreferencesProvider";
import { useViewportHeight } from "@/hooks/useViewportHeight";
import { RESIZE_STEP, Resizer } from "@/components/ui/split";
import { AgentDrawer } from "./AgentDrawer";
import { AgentPill } from "./AgentPill";
import { DEFAULT_AGENT_DRAWER_HEIGHT, clampDrawerHeight } from "./drawer-height";

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
 *
 * …and, since the drawer became draggable, **how tall it stands**. That is not
 * session state about mail, so it does not go near `useMach`'s reducer: it is
 * one component remembering one thing about itself through `uiSession`, which
 * is what the rail's folded sections and the calendar sidebar already do.
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

  /* ------------------------------------------------------------ the height */

  const { session: where, remember } = useUiSession();
  const [chosen, setChosen] = useState(DEFAULT_AGENT_DRAWER_HEIGHT);
  const restoredHeight = useRef(false);
  const viewport = useViewportHeight();

  // The stored height lands a tick after mount, like the rest of the session,
  // and only the first one counts: `remember` writes back into the same object,
  // so a second restore would fight every drag.
  const stored = where.agentDrawerHeight;
  useEffect(() => {
    if (restoredHeight.current || stored === undefined) return;
    restoredHeight.current = true;
    setChosen(stored);
  }, [stored]);

  /*
   * What he chose and what fits are two different numbers, and only the second
   * one is a height.
   *
   * Keeping the choice unclamped is what makes a narrow window a temporary
   * condition rather than a decision: shrink the window and the drawer gives
   * way, and the height he dragged to is still what comes back on a display
   * that can hold it.
   */
  const height = clampDrawerHeight(chosen, viewport);
  const maxHeight = clampDrawerHeight(AGENT_DRAWER_HEIGHT_BOUNDS.max, viewport);

  const resize = useCallback(
    (next: number) => {
      const clamped = clampDrawerHeight(next, viewport);
      setChosen(clamped);
      remember({ agentDrawerHeight: clamped });
    },
    [remember, viewport],
  );

  /* ------------------------------------------------------------- keystrokes */

  const open = useMemo(
    () => sessions.find((session) => session.id === openId) ?? null,
    [sessions, openId],
  );

  /** The newest thing the open session surfaced — what ⌘⇧O opens. */
  const latestArtifact = useMemo(() => {
    if (!open) return null;
    for (let i = open.entries.length - 1; i >= 0; i--) {
      const entry = open.entries[i];
      if (entry.role === "tool" && entry.artifact) return entry.artifact;
    }
    return null;
  }, [open]);

  useKeyBindings([
    /*
     * The keyboard's own route to the divider.
     *
     * The handle is a focus stop and answers ↑ ↓ once it has focus, but ⇥ is
     * claimed by both modes for their rail-and-list loop, so focus is not
     * always a keystroke away. These are: they work from anywhere the drawer
     * is open, including from inside its own input, and `?` lists them.
     */
    {
      keys: "mod+alt+up",
      group: "Agent",
      description: "Taller",
      allowInInput: true,
      when: () => openId !== null,
      handler: () => resize(height + RESIZE_STEP),
    },
    {
      keys: "mod+alt+down",
      group: "Agent",
      description: "Shorter",
      allowInInput: true,
      when: () => openId !== null,
      handler: () => resize(height - RESIZE_STEP),
    },
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
      /*
       * The keyboard's own route to the card.
       *
       * The card is an ordinary button in the drawer's tab order, which is how
       * minimise, close and the approval buttons are reached — and that order
       * is only enterable from inside the drawer's own input, because ⇥ is
       * claimed by both modes everywhere else. A thing the agent put on screen
       * should not need the pointer *or* a detour through a text field, so it
       * gets a binding, exactly as the drawer's divider did.
       *
       * The newest artifact, because it is the one the answer is about.
       */
      keys: "shift+mod+o",
      group: "Agent",
      description: "Open what it found",
      allowInInput: true,
      when: () => latestArtifact !== null,
      handler: () => {
        if (!latestArtifact) return;
        setOpenId(null);
        actions.openArtifact(latestArtifact);
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

  const close = useCallback((id: string) => {
    setOpenId((current) => (current === id ? null : current));
    void closeSession(id).catch(() => {});
    apply({ type: "closed", sessionId: id });
  }, []);

  if (sessions.length === 0 && !starting) return null;

  return (
    <>
      {open && (
        <Resizer
          axis="y"
          size={height}
          onResize={resize}
          min={AGENT_DRAWER_HEIGHT_BOUNDS.min}
          max={maxHeight}
          label="Agent panel height"
          // Full width, and pulled up over the border the bottom furniture
          // already draws — see `App.tsx`. The line it lights while it moves
          // is that same edge, not a second one.
          className="w-full"
        />
      )}
      {open && (
        <AgentDrawer
          session={open}
          height={height}
          onMinimise={() => setOpenId(null)}
          onClose={() => close(open.id)}
          // Opening what the agent made is navigation, and navigation belongs
          // to the shell. The drawer knows what was made; this knows where it
          // lives. Minimising first, because the point of the button is to put
          // the thing on screen, not the drawer that mentioned it.
          onOpenArtifact={(artifact) => {
            setOpenId(null);
            actions.openArtifact(artifact);
          }}
        />
      )}

      {/* No rule above the pills: the drawer's own background already ends
          where they begin, and when there is no drawer the container in
          `App.tsx` draws the one edge the bottom of the window needs. */}
      <div
        aria-label="Agent sessions"
        className="flex h-7 shrink-0 items-center gap-1 overflow-x-auto px-2"
      >
        {starting && sessions.length === 0 && (
          <span className="px-1 text-micro text-faint-foreground">Starting…</span>
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
