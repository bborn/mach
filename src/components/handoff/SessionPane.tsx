import { ChevronDown, ChevronRight, TriangleAlert, X } from "lucide-react";
import { useCallback, useEffect, useRef, useState, useSyncExternalStore } from "react";
import type { IDisposable, ITheme, Terminal } from "@xterm/xterm";
import { useKeyBindings } from "@/hooks/useKeymap";
import { useMach } from "@/hooks/useMach";
import { useViewportHeight } from "@/hooks/useViewportHeight";
import {
  adoptSession,
  closeSession,
  consumeSession,
  decodeChunk,
  resizeSession,
  sessionSnapshot,
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
 * A handoff that stayed in the window: one process on a pty, and the pane it
 * draws in.
 *
 * `claude "{{prompt}}"` is the command the owner configured, and it is an
 * interactive session — so the two modes that existed could either leave the
 * app (terminal) or refuse it outright (inline, which says so in its own error
 * text). This is the third mode. It is a session pane for a handoff target and
 * nothing else: no shell prompt, no command entry, no tabs, one session at a
 * time, and it goes away when the process does.
 *
 * # The keyboard contract is next door
 *
 * ⌘ belongs to the application and everything else belongs to the process, and
 * the whole of it — including why Escape means one thing while a process is
 * running and another once it has stopped — is written down in
 * `session-keys.ts`, which is also where the tests press keys at it.
 */

/** How tall the pane stands before it is dragged. */
const DEFAULT_HEIGHT = 380;
const MIN_HEIGHT = 120;

/** What the emulator keeps above the top of the pane. */
const SCROLLBACK = 5000;

/**
 * How much output may wait for the emulator to arrive.
 *
 * Rust caps a chunk at 64 kB and sends at most one per frame, so the ordinary
 * wait — one dynamic import — holds a few of these. The number is here for the
 * case where the emulator never arrives at all: without a limit a process that
 * keeps talking to a pane that failed to load would grow this array forever.
 */
const MAX_QUEUED_CHUNKS = 64;

export function SessionPane() {
  const session = useSyncExternalStore(subscribeSession, sessionSnapshot);
  const { ui } = useMach();
  const dark = useIsDark();
  const viewport = useViewportHeight();

  const [focused, setFocused] = useState(false);
  const [exit, setExit] = useState<{ status: number | null } | null>(null);
  const [dropped, setDropped] = useState(0);
  const [showPrompt, setShowPrompt] = useState(false);
  /** Why there is no emulator, when there is no emulator. */
  const [broken, setBroken] = useState<string | null>(null);
  /** The emulator is open and can be measured. See the shape effects. */
  const [ready, setReady] = useState(false);
  const [chosenHeight, setChosenHeight] = useState(DEFAULT_HEIGHT);

  const host = useRef<HTMLDivElement>(null);
  const term = useRef<Terminal | null>(null);
  const fit = useRef<{ fit: () => void } | null>(null);
  /** Output that arrived before the emulator did. */
  const unwritten = useRef<Uint8Array[]>([]);
  /** Where focus was when ⇧⌘T took it, so the same key can give it back. */
  const cameFrom = useRef<HTMLElement | null>(null);

  const sessionId = session?.sessionId ?? null;
  const height = clampDrawerHeight(chosenHeight, viewport);
  const live = sessionId !== null && exit === null;

  // A window that reloaded while a session was running must not leave the
  // process on its pty with nothing on screen pointing at it.
  useEffect(() => {
    void adoptSession();
  }, []);

  /* ------------------------------------------------------------ the emulator */

  /*
   * The emulator and its stylesheet are loaded when a session starts, not when
   * the app does. It is 330 kB for a surface that is empty almost always, and a
   * dynamic import puts it in a chunk of its own — so a launch that never opens
   * a session never pays for it.
   */
  useEffect(() => {
    if (!sessionId || !host.current) return;
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
      terminal.focus();

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
      // Whatever the process said while this was loading, in order, first.
      for (const bytes of unwritten.current) terminal.write(bytes);
      unwritten.current = [];
      // The pane was measured before the emulator existed; tell the pty what it
      // actually got, since `open` had to guess.
      void resizeSession(sessionId, terminal.cols, terminal.rows).catch(() => undefined);
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
    if (!sessionId) return;
    setExit(null);
    setDropped(0);
    setBroken(null);

    return consumeSession((event) => {
      if (event.sessionId !== sessionId) return;
      if (event.type === "output") {
        if (event.dropped > 0) setDropped((total) => total + event.dropped);
        const bytes = decodeChunk(event.base64);
        // The emulator arrives over a dynamic import, and a command that says
        // something immediately says it before that lands. Held here until
        // there is something to draw on, then written in order.
        if (term.current) term.current.write(bytes);
        else if (unwritten.current.length < MAX_QUEUED_CHUNKS) unwritten.current.push(bytes);
      } else {
        setExit({ status: event.status });
        // Nothing takes keystrokes any more, so the pane stops holding them:
        // Escape means "dismiss this" from here on, and it can only mean that
        // if the app is the one hearing it.
        if (document.activeElement && host.current?.contains(document.activeElement)) {
          (document.activeElement as HTMLElement).blur();
        }
        setFocused(false);
      }
    });
  }, [sessionId]);

  /* ---------------------------------------------------------------- the shape */

  /*
   * A pane that changed size and did not tell the pty renders every full-screen
   * program wrong — the process is drawing to a window it thinks is another
   * shape. `fit` recomputes the cell grid; `onResize` above forwards it.
   *
   * `ready` is in the dependencies because of what happens without it, which is
   * a pane that stays blank. The emulator arrives over a dynamic import, so on
   * the render that starts a session both of these run while `fit.current` is
   * still null — including the ResizeObserver's first callback, which fires on
   * `observe` and is the only one a pane that never changes shape ever gets.
   * The `fit()` inside the loader is not a substitute: it measures a cell
   * before the layout it is measuring has settled, and a grid of zero-height
   * cells draws every row on top of row one. Measuring again once the emulator
   * exists is what puts text on the screen.
   */
  useEffect(() => {
    if (!sessionId || !ready) return;
    fit.current?.fit();
  }, [sessionId, ready, height, viewport]);

  useEffect(() => {
    if (!sessionId || !ready || typeof ResizeObserver === "undefined" || !host.current) return;
    const observer = new ResizeObserver(() => fit.current?.fit());
    observer.observe(host.current);
    return () => observer.disconnect();
  }, [sessionId, ready]);

  // The emulator holds its own copy of the colours, so it has to be told when
  // they change. `useIsDark` watches the `.dark` class rather than the theme
  // preference, which is the only thing that is true in all three cases: the
  // preference changing, the system palette changing under `system`, and this
  // effect running *before* the provider's — children's effects run first, so
  // reading the tokens off `ui.theme` would read the outgoing palette.
  useEffect(() => {
    if (term.current) term.current.options.theme = readTheme();
  }, [dark, sessionId]);

  /* ----------------------------------------------------------------- the keys */

  const focusTerminal = useCallback(() => {
    cameFrom.current = document.activeElement as HTMLElement | null;
    term.current?.focus();
  }, []);

  const releaseFocus = useCallback(() => {
    const back = cameFrom.current;
    cameFrom.current = null;
    if (back && back.isConnected && !host.current?.contains(back)) back.focus();
    else {
      (document.activeElement as HTMLElement | null)?.blur();
      document.body.focus?.();
    }
  }, []);

  const end = useCallback(() => {
    if (!sessionId) return;
    releaseFocus();
    void closeSession(sessionId);
  }, [sessionId, releaseFocus]);

  useKeyBindings(
    sessionBindings(
      {
        live: () => live,
        focused: () => focused,
        present: () => sessionId !== null,
        finished: () => sessionId !== null && exit !== null,
        paletteOpen: () => ui.paletteOpen,
      },
      {
        toggleFocus: () => (focused ? releaseFocus() : focusTerminal()),
        end,
        taller: () => setChosenHeight((h) => clampDrawerHeight(h + RESIZE_STEP, viewport)),
        shorter: () => setChosenHeight((h) => clampDrawerHeight(h - RESIZE_STEP, viewport)),
      },
    ),
  );

  if (!session) return null;

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
        aria-label={`Terminal session: ${session.targetName}`}
        style={{ height }}
        className="flex shrink-0 flex-col overflow-hidden bg-background"
      >
        <Header
          session={session}
          focused={focused}
          exit={exit}
          dropped={dropped}
          showPrompt={showPrompt}
          onTogglePrompt={() => setShowPrompt((was) => !was)}
          onClose={end}
        />

        {showPrompt && (
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
            {session.prompt}
          </pre>
        )}

        {broken && (
          <p className="mx-3 mb-2 shrink-0 break-words text-micro text-danger">
            The terminal did not load: {broken}
          </p>
        )}

        {/*
          The emulator draws here. `onFocusCapture` rather than a listener on the
          textarea: xterm owns that element and replaces it, and focus events
          bubble.
        */}
        <div
          ref={host}
          onFocusCapture={() => setFocused(true)}
          onBlurCapture={() => setFocused(false)}
          onMouseDown={() => {
            if (!focused) cameFrom.current = document.activeElement as HTMLElement | null;
          }}
          className="min-h-0 flex-1 overflow-hidden px-2 pb-1"
        />
      </section>
    </>
  );
}

function Header({
  session,
  focused,
  exit,
  dropped,
  showPrompt,
  onTogglePrompt,
  onClose,
}: {
  session: HandoffSession;
  focused: boolean;
  exit: { status: number | null } | null;
  dropped: number;
  showPrompt: boolean;
  onTogglePrompt: () => void;
  onClose: () => void;
}) {
  return (
    <header className="flex h-9 shrink-0 items-center gap-2 px-3">
      <span className="shrink-0 text-list text-foreground">{session.targetName}</span>
      {/* The command is the part that can be any length, so it is the part that
          gives way. `min-w-0` on the flexible item, or the flex box refuses to
          shrink it and every fixed label after it is squeezed instead — which
          is how the target's own name came out as "Sta…". */}
      <span className="min-w-0 flex-1 truncate font-mono text-micro text-faint-foreground">
        {session.command}
      </span>

      {exit ? (
        <span className="shrink-0 text-micro text-muted-foreground">
          {exit.status === 0 || exit.status === null ? "exited" : `exited ${exit.status}`}
        </span>
      ) : (
        <span
          className={cn(
            "shrink-0 text-micro",
            focused ? "text-accent" : "text-faint-foreground",
          )}
        >
          {focused ? "⇧⌘T leaves" : "⇧⌘T types here"}
        </span>
      )}

      {dropped > 0 && (
        <span
          className="flex shrink-0 items-center gap-1 text-micro text-warning"
          title="Output arrived faster than the pane could draw it"
        >
          <TriangleAlert size={11} strokeWidth={1.75} />
          {formatBytes(dropped)} dropped
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
