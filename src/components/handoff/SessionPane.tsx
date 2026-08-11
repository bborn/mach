import { ChevronDown, ChevronRight, TriangleAlert, Wrench, X } from "lucide-react";
import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  useSyncExternalStore,
} from "react";
import type { IDisposable, ITheme, Terminal } from "@xterm/xterm";
import { useKeyBindings } from "@/hooks/useKeymap";
import { useMach } from "@/hooks/useMach";
import { useViewportHeight } from "@/hooks/useViewportHeight";
import {
  activeSessionId,
  adoptSessions,
  closeSession,
  consumeSession,
  decodeChunk,
  resizeSession,
  selectSession,
  sessionsSnapshot,
  stepSession,
  subscribeSession,
  writeSession,
  type HandoffSession,
} from "@/lib/handoff";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { RESIZE_STEP, Resizer } from "@/components/ui/split";
import { clampDrawerHeight } from "@/components/agent/drawer-height";
import { useIsDark } from "@/components/calendar/use-is-dark";
import { sessionBindings } from "./session-keys";

/**
 * Handoffs that stayed in the window: a tab per process, each on its own pty.
 *
 * `claude "{{prompt}}"` is the command the owner configured, and it is an
 * interactive session — so the two modes that existed could either leave the
 * app (terminal) or refuse it outright (inline, which says so in its own error
 * text). This is the third mode.
 *
 * # A tab is a whole session
 *
 * Everything a session has, it has by itself: its own pty, its own emulator and
 * therefore its own scrollback, its own resolved prompt to disclose, its own
 * exit status, its own dropped-bytes count, and its own reaping. The strip
 * across the top is the only shared thing, and it holds at most
 * `MAX_SESSIONS` — Rust refuses the one past that by number.
 *
 * The tabs that are not in front stay mounted and stay laid out; they are
 * `invisible` rather than unmounted or `hidden`. Unmounting would throw away
 * the scrollback the process is still writing into, and `display: none` would
 * leave the emulator measuring a zero-height cell — which draws every row of
 * the next fit on top of row one. Keeping the layout and taking away the paint
 * is the one arrangement where a background tab both keeps its history and
 * comes back to the front correctly sized.
 *
 * # The keyboard contract is next door
 *
 * ⌘ belongs to the application and everything else belongs to the process, and
 * the whole of it — including why Escape means one thing while a process is
 * running and another once it has stopped, and why the tabs are ⌥⌘←/→ — is
 * written down in `session-keys.ts`, which is also where the tests press keys
 * at it.
 */

/** How tall the pane stands before it is dragged. */
const DEFAULT_HEIGHT = 380;
const MIN_HEIGHT = 120;

/** What each emulator keeps above the top of the pane. */
const SCROLLBACK = 5000;

/**
 * How much output may wait for one tab's emulator to arrive.
 *
 * Rust caps a chunk at a share of one frame and sends at most one per frame per
 * session, so the ordinary wait — one dynamic import — holds a few of these.
 * The number is here for the case where the emulator never arrives at all.
 */
const MAX_QUEUED_CHUNKS = 64;

export function SessionPane() {
  const sessions = useSyncExternalStore(subscribeSession, sessionsSnapshot);
  const activeId = useSyncExternalStore(subscribeSession, activeSessionId);
  const { ui } = useMach();
  const viewport = useViewportHeight();

  const [focused, setFocused] = useState(false);
  /** Which tabs have exited, and with what. One entry per session that has. */
  const [exits, setExits] = useState<Record<string, number | null>>({});
  const [showPrompt, setShowPrompt] = useState(false);
  const [chosenHeight, setChosenHeight] = useState(DEFAULT_HEIGHT);

  /** Where focus was when ⇧⌘T took it, so the same key can give it back. */
  const cameFrom = useRef<HTMLElement | null>(null);
  /** The front tab's emulator, for ⇧⌘T. Registered by the tab itself. */
  const focusActive = useRef<(() => void) | null>(null);
  const host = useRef<HTMLDivElement>(null);

  const height = clampDrawerHeight(chosenHeight, viewport);
  const active = useMemo(
    () => sessions.find((session) => session.sessionId === activeId) ?? null,
    [sessions, activeId],
  );
  const exited = active ? active.sessionId in exits : false;

  // A window that reloaded while sessions were running must not leave the
  // processes on their ptys with nothing on screen pointing at them.
  useEffect(() => {
    void adoptSessions();
  }, []);

  // A tab that has gone stops being one that exited, so reopening the same
  // target does not come back already looking finished.
  useEffect(() => {
    setExits((was) => {
      const ids = new Set(sessions.map((session) => session.sessionId));
      const next = Object.fromEntries(Object.entries(was).filter(([id]) => ids.has(id)));
      return Object.keys(next).length === Object.keys(was).length ? was : next;
    });
  }, [sessions]);

  /* ----------------------------------------------------------------- the keys */

  const releaseFocus = useCallback(() => {
    const back = cameFrom.current;
    cameFrom.current = null;
    if (back && back.isConnected && !host.current?.contains(back)) back.focus();
    else {
      (document.activeElement as HTMLElement | null)?.blur();
      document.body.focus?.();
    }
  }, []);

  const focusTerminal = useCallback(() => {
    cameFrom.current = document.activeElement as HTMLElement | null;
    focusActive.current?.();
  }, []);

  const end = useCallback(() => {
    if (!activeId) return;
    // Only give focus back when the pane is about to be empty; closing one of
    // several should leave the caret in the strip.
    if (sessions.length <= 1) releaseFocus();
    void closeSession(activeId);
  }, [activeId, sessions.length, releaseFocus]);

  useKeyBindings(
    sessionBindings(
      {
        live: () => active !== null && !exited,
        focused: () => focused,
        present: () => active !== null,
        finished: () => active !== null && exited,
        tabs: () => sessions.length,
        paletteOpen: () => ui.paletteOpen,
      },
      {
        toggleFocus: () => (focused ? releaseFocus() : focusTerminal()),
        end,
        nextTab: () => stepSession(1),
        previousTab: () => stepSession(-1),
        taller: () => setChosenHeight((h) => clampDrawerHeight(h + RESIZE_STEP, viewport)),
        shorter: () => setChosenHeight((h) => clampDrawerHeight(h - RESIZE_STEP, viewport)),
      },
    ),
  );

  if (sessions.length === 0) return null;

  return (
    <>
      <Resizer
        axis="y"
        size={height}
        onResize={(next) => setChosenHeight(clampDrawerHeight(next, viewport))}
        min={MIN_HEIGHT}
        max={clampDrawerHeight(Number.MAX_SAFE_INTEGER, viewport)}
        label="Session pane height"
        className="w-full"
      />
      <section
        ref={host}
        aria-label="Terminal sessions"
        style={{ height }}
        className="flex shrink-0 flex-col overflow-hidden bg-background"
      >
        <Tabs
          sessions={sessions}
          activeId={activeId}
          exits={exits}
          onSelect={selectSession}
          onClose={(id) => {
            if (sessions.length <= 1) releaseFocus();
            void closeSession(id);
          }}
        />

        {active && (
          <Header
            session={active}
            focused={focused}
            status={exited ? { status: exits[active.sessionId] } : null}
            showPrompt={showPrompt}
            onTogglePrompt={() => setShowPrompt((was) => !was)}
            onClose={end}
          />
        )}

        {showPrompt && active && (
          <pre
            id="session-prompt"
            aria-label="What was handed over"
            tabIndex={0}
            className={cn(
              "mx-3 mb-2 max-h-40 shrink-0 overflow-auto whitespace-pre-wrap break-words",
              "rounded-[var(--radius)] bg-surface-raised p-2",
              "font-mono text-micro text-muted-foreground focus:outline-none",
            )}
          >
            {active.prompt}
          </pre>
        )}

        {/*
          Every session draws here at once, stacked. The inactive ones keep
          their box — see the file's doc — so their emulators stay correctly
          measured and their scrollback survives being in the background.
        */}
        <div className="relative min-h-0 flex-1">
          {sessions.map((session) => (
            <SessionTerminal
              key={session.sessionId}
              session={session}
              active={session.sessionId === activeId}
              onFocusChange={(next) => {
                if (session.sessionId === activeId) setFocused(next);
              }}
              onExit={(status) =>
                setExits((was) => ({ ...was, [session.sessionId]: status }))
              }
              registerFocus={(focus) => {
                if (session.sessionId === activeId) focusActive.current = focus;
              }}
              onPointerEnter={() => {
                if (!focused) cameFrom.current = document.activeElement as HTMLElement | null;
              }}
            />
          ))}
        </div>
      </section>
    </>
  );
}

/* -------------------------------------------------------------------------- */
/* The strip                                                                   */
/* -------------------------------------------------------------------------- */

/**
 * One tab per session: which target it is, and whether it is still running.
 *
 * Rendered even for a single session, because appearing only at two would mean
 * the row under the mail list jumps by its own height the moment a second
 * handoff starts.
 */
function Tabs({
  sessions,
  activeId,
  exits,
  onSelect,
  onClose,
}: {
  sessions: HandoffSession[];
  activeId: string | null;
  exits: Record<string, number | null>;
  onSelect: (id: string) => void;
  onClose: (id: string) => void;
}) {
  return (
    <div role="tablist" aria-label="Sessions" className="flex h-7 shrink-0 items-stretch gap-px px-2 pt-1">
      {sessions.map((session) => {
        const selected = session.sessionId === activeId;
        const exited = session.sessionId in exits;
        return (
          <div
            key={session.sessionId}
            className={cn(
              "group flex min-w-0 max-w-[12rem] flex-1 items-center gap-1 rounded-t-[var(--radius)] px-2",
              selected ? "bg-surface-raised" : "bg-transparent hover:bg-surface-raised/50",
            )}
          >
            <button
              type="button"
              role="tab"
              aria-selected={selected}
              onClick={() => onSelect(session.sessionId)}
              className="min-w-0 flex-1 truncate text-left text-micro focus:outline-none focus-visible:underline"
            >
              <span
                aria-hidden
                className={cn(
                  "mr-1.5 inline-block size-1.5 rounded-full align-middle",
                  exited ? "bg-faint-foreground" : "bg-accent",
                )}
              />
              <span className={selected ? "text-foreground" : "text-muted-foreground"}>
                {session.targetName}
              </span>
            </button>
            <button
              type="button"
              aria-label={`Close ${session.targetName}`}
              onClick={() => onClose(session.sessionId)}
              className={cn(
                "shrink-0 rounded-[2px] text-faint-foreground opacity-0 hover:text-foreground",
                "group-hover:opacity-100 focus:opacity-100 focus:outline-none",
                selected && "opacity-60",
              )}
            >
              <X size={11} strokeWidth={2} />
            </button>
          </div>
        );
      })}
    </div>
  );
}

/* -------------------------------------------------------------------------- */
/* One session                                                                 */
/* -------------------------------------------------------------------------- */

function Header({
  session,
  focused,
  status,
  showPrompt,
  onTogglePrompt,
  onClose,
}: {
  session: HandoffSession;
  focused: boolean;
  status: { status: number | null } | null;
  showPrompt: boolean;
  onTogglePrompt: () => void;
  onClose: () => void;
}) {
  return (
    <header className="flex h-9 shrink-0 items-center gap-2 px-3">
      {/* The command is the part that can be any length, so it is the part that
          gives way. `min-w-0` on the flexible item, or the flex box refuses to
          shrink it and every fixed label after it is squeezed instead. */}
      <span className="min-w-0 flex-1 truncate font-mono text-micro text-faint-foreground">
        {session.command}
      </span>

      {/* Which sessions can reach the mailbox is not a detail. A `claude`
          session is started against Mach's own tools and can search, read,
          label, archive, draft and send; every other target has no such
          endpoint and no token. */}
      {session.resources.length > 0 && (
        <span
          className="flex shrink-0 items-center gap-1 text-micro text-muted-foreground"
          title="This session can search, read, label, archive, snooze and draft. Sending asks you first."
        >
          <Wrench size={11} strokeWidth={1.75} />
          {session.resources.join(", ")}
        </span>
      )}

      {status ? (
        <span className="shrink-0 text-micro text-muted-foreground">
          {status.status === 0 || status.status === null ? "exited" : `exited ${status.status}`}
        </span>
      ) : (
        <span
          className={cn("shrink-0 text-micro", focused ? "text-accent" : "text-faint-foreground")}
        >
          {focused ? "⇧⌘T leaves" : "⇧⌘T types here"}
        </span>
      )}

      <div className="flex shrink-0 items-center gap-0.5">
        <Button
          size="sm"
          variant="ghost"
          onClick={onTogglePrompt}
          aria-expanded={showPrompt}
          aria-controls="session-prompt"
          className="gap-1 text-micro text-muted-foreground"
        >
          {showPrompt ? (
            <ChevronDown size={12} strokeWidth={1.75} />
          ) : (
            <ChevronRight size={12} strokeWidth={1.75} />
          )}
          Prompt
        </Button>
        <Button size="icon" variant="ghost" onClick={onClose} aria-label="End the session">
          <X size={13} strokeWidth={1.75} />
        </Button>
      </div>
    </header>
  );
}

/**
 * One session's emulator, and the stream that feeds it.
 *
 * Mounted for every tab, visible for one. Everything in here is scoped to a
 * single `sessionId`: the dynamic import is shared by the module cache, but the
 * `Terminal`, the listener, the queue and the byte count are this session's.
 */
function SessionTerminal({
  session,
  active,
  onFocusChange,
  onExit,
  registerFocus,
  onPointerEnter,
}: {
  session: HandoffSession;
  active: boolean;
  onFocusChange: (focused: boolean) => void;
  onExit: (status: number | null) => void;
  registerFocus: (focus: () => void) => void;
  onPointerEnter: () => void;
}) {
  const dark = useIsDark();
  const [dropped, setDropped] = useState(0);
  /** Why there is no emulator, when there is no emulator. */
  const [broken, setBroken] = useState<string | null>(null);
  /** The emulator is open and can be measured. See the shape effects. */
  const [ready, setReady] = useState(false);

  const host = useRef<HTMLDivElement>(null);
  const term = useRef<Terminal | null>(null);
  const fit = useRef<{ fit: () => void } | null>(null);
  /** Output that arrived before the emulator did. */
  const unwritten = useRef<Uint8Array[]>([]);

  const sessionId = session.sessionId;

  /* ------------------------------------------------------------ the emulator */

  /*
   * The emulator and its stylesheet are loaded when the first session starts,
   * not when the app does. It is 330 kB for a surface that is empty almost
   * always, and a dynamic import puts it in a chunk of its own — so a launch
   * that never opens a session never pays for it. A second tab pays nothing:
   * the module is already resolved.
   */
  useEffect(() => {
    if (!host.current) return;
    let disposed = false;
    let terminal: Terminal | null = null;
    let listeners: IDisposable[] = [];

    void (async () => {
      const [{ Terminal: Xterm }, { FitAddon }] = await Promise.all([
        import("@xterm/xterm"),
        import("@xterm/addon-fit"),
        import("@xterm/xterm/css/xterm.css"),
      ]);
      if (disposed || !host.current) return;

      terminal = new Xterm({
        allowProposedApi: true,
        convertEol: false,
        cursorBlink: true,
        fontFamily: 'ui-monospace, "SF Mono", "JetBrains Mono", "Menlo", monospace',
        fontSize: 12,
        scrollback: SCROLLBACK,
        theme: readTheme(),
      });
      /*
       * The emulator must not try to make bytes out of a ⌘ chord. Returning
       * false here means "not for you": the browser's own default still runs,
       * which is what makes ⌘C and ⌘V the clipboard rather than two control
       * codes, and the app's binding has already had the key by this point.
       */
      terminal.attachCustomKeyEventHandler((event) => !event.metaKey);

      const addon = new FitAddon();
      terminal.loadAddon(addon);
      terminal.open(host.current);
      addon.fit();

      listeners = [
        terminal.onData((data) => {
          void writeSession(sessionId, data).catch(() => undefined);
        }),
        terminal.onResize(({ cols, rows }) => {
          void resizeSession(sessionId, cols, rows).catch(() => undefined);
        }),
      ];

      term.current = terminal;
      fit.current = addon;
      // Whatever this session said while its emulator was loading, in order.
      for (const bytes of unwritten.current) terminal.write(bytes);
      unwritten.current = [];
      // The pane was measured before the emulator existed; tell the pty what it
      // actually got, since `open` had to guess.
      void resizeSession(sessionId, terminal.cols, terminal.rows).catch(() => undefined);
      // And paint it. A replay written straight after `open` lands in the
      // buffer without reaching the screen — the renderer has not had a frame
      // yet — which is a tab that comes up blank while its process is talking
      // to it. Costs one repaint per session, once.
      addon.fit();
      terminal.refresh(0, Math.max(0, terminal.rows - 1));
      setReady(true);
    })().catch((error: unknown) => {
      // A pane that stayed blank while a process ran into it is the silent
      // failure this project has paid for repeatedly. If the emulator will not
      // load there is still a live pty on the other side, so say which half
      // broke rather than leaving an empty rectangle.
      if (!disposed) setBroken(String(error));
    });

    return () => {
      disposed = true;
      setReady(false);
      for (const listener of listeners) listener.dispose();
      terminal?.dispose();
      term.current = null;
      fit.current = null;
    };
  }, [sessionId]);

  /* --------------------------------------------------------------- the stream */

  useEffect(() => {
    return consumeSession(sessionId, (event) => {
      if (event.type === "output") {
        if (event.dropped > 0) setDropped((total) => total + event.dropped);
        const bytes = decodeChunk(event.base64);
        // The emulator arrives over a dynamic import, and a command that says
        // something immediately says it before that lands. Held here until
        // there is something to draw on, then written in order.
        if (term.current) term.current.write(bytes);
        else if (unwritten.current.length < MAX_QUEUED_CHUNKS) unwritten.current.push(bytes);
      } else {
        onExit(event.status);
        // Nothing takes keystrokes any more, so this tab stops holding them:
        // Escape means "dismiss this" from here on, and it can only mean that
        // if the app is the one hearing it.
        if (document.activeElement && host.current?.contains(document.activeElement)) {
          (document.activeElement as HTMLElement).blur();
        }
      }
    });
    // `onExit` is a fresh closure on every render and must not re-attach the
    // stream, which would replay this session's queue into the emulator again.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sessionId]);

  /* ---------------------------------------------------------------- the shape */

  /*
   * A tab that changed size and did not tell its pty renders every full-screen
   * program wrong — the process is drawing to a window it thinks is another
   * shape. `fit` recomputes the cell grid; `onResize` above forwards it.
   *
   * `ready` is in the dependencies because of what happens without it, which is
   * a pane that stays blank: the emulator arrives over a dynamic import, so on
   * the render that starts a session this runs while `fit.current` is still
   * null — including the ResizeObserver's first callback, which fires on
   * `observe` and is the only one a pane that never changes shape ever gets.
   */
  useEffect(() => {
    if (!ready || typeof ResizeObserver === "undefined" || !host.current) return;
    fit.current?.fit();
    const observer = new ResizeObserver(() => fit.current?.fit());
    observer.observe(host.current);
    return () => observer.disconnect();
  }, [ready]);

  // The emulator holds its own copy of the colours, so it has to be told when
  // they change. `useIsDark` watches the `.dark` class rather than the theme
  // preference, which is the only thing that is true in all three cases: the
  // preference changing, the system palette changing under `system`, and this
  // effect running *before* the provider's — children's effects run first, so
  // reading the tokens off `ui.theme` would read the outgoing palette.
  useEffect(() => {
    if (term.current) term.current.options.theme = readTheme();
  }, [dark, ready]);

  /* ----------------------------------------------------------------- focus */

  useEffect(() => {
    registerFocus(() => term.current?.focus());
  }, [registerFocus, ready]);

  /*
   * Coming to the front means being measured, repainted and given the caret.
   *
   * A background tab is laid out but not on screen, and whatever its process
   * wrote while it was back there is in the buffer rather than on the canvas.
   * Refreshing on the way forward is what makes ⌥⌘→ show the output that
   * arrived while you were looking at another session; the caret is so that the
   * tab you switched to is the one you are typing in.
   */
  useEffect(() => {
    if (!active || !ready) return;
    const terminal = term.current;
    if (!terminal) return;
    fit.current?.fit();
    terminal.refresh(0, Math.max(0, terminal.rows - 1));
    terminal.focus();
  }, [active, ready]);

  return (
    <div
      aria-hidden={!active}
      className={cn(
        "absolute inset-0 flex flex-col",
        // Laid out either way — an emulator measured at zero height draws every
        // row on top of row one when it comes back. See the file's doc.
        active ? "visible" : "invisible",
      )}
    >
      {broken && (
        <p className="mx-3 mb-2 shrink-0 break-words text-micro text-danger">
          The terminal did not load: {broken}
        </p>
      )}
      {dropped > 0 && (
        <p
          className="mx-3 flex shrink-0 items-center gap-1 text-micro text-warning"
          title="Output arrived faster than the pane could draw it"
        >
          <TriangleAlert size={11} strokeWidth={1.75} />
          {formatBytes(dropped)} dropped
        </p>
      )}
      <div
        ref={host}
        onFocusCapture={() => onFocusChange(true)}
        onBlurCapture={() => onFocusChange(false)}
        onMouseDown={onPointerEnter}
        className="min-h-0 flex-1 overflow-hidden px-2 pb-1"
      />
    </div>
  );
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} kB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

/**
 * The app's colours, as the emulator wants them.
 *
 * Only the four the escape sequences leave alone: a program picks its own
 * foreground and background whenever it says so, and overriding the sixteen
 * ANSI colours would be Mach deciding what red means in somebody else's output.
 * The paper, the ink, the caret and the selection are the window's, and those
 * are the ones that would look wrong against the rest of the app.
 *
 * The tokens are `oklch(…)`, which the emulator's colour parser does not read.
 * A canvas context does — assigning any CSS colour to `fillStyle` and reading
 * it back is the browser's own conversion to `#rrggbb`, done by the same engine
 * that painted the rest of the window.
 */
function readTheme(): ITheme {
  const style = getComputedStyle(document.documentElement);
  const token = (name: string, fallback: string) =>
    toHex(style.getPropertyValue(name).trim(), fallback);
  return {
    background: token("--background", "#ffffff"),
    foreground: token("--foreground", "#111111"),
    cursor: token("--accent", "#3b82f6"),
    cursorAccent: token("--background", "#ffffff"),
    selectionBackground: token("--accent-subtle", "#c7d9f5"),
  };
}

function toHex(value: string, fallback: string): string {
  if (!value) return fallback;
  if (/^#[0-9a-f]{3,8}$/i.test(value)) return value;
  try {
    const context = document.createElement("canvas").getContext("2d");
    if (!context) return fallback;
    context.fillStyle = fallback;
    context.fillStyle = value;
    const resolved = context.fillStyle;
    return typeof resolved === "string" ? resolved : fallback;
  } catch {
    return fallback;
  }
}
