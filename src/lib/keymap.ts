/**
 * The keyboard system.
 *
 * One registry, one window listener. Features register bindings and get an
 * unregister function back; nothing attaches its own `keydown` handler. That
 * makes the whole keymap enumerable (the ⌘K palette and a future help sheet
 * both read it) and makes precedence a property of the registry rather than
 * an accident of DOM bubbling.
 *
 * Binding syntax:
 *   "j"          a single key
 *   "mod+k"      ⌘ on macOS, Ctrl elsewhere
 *   "shift+a"    shift only qualifies alphabetic keys; "?" is bound as "?"
 *   "g i"        a sequence — press g, then i, within SEQUENCE_TIMEOUT_MS
 *
 * Precedence: highest `priority` wins; ties break in favour of the most
 * recently registered binding, so a dialog's Escape beats the shell's Escape
 * for as long as the dialog is mounted. A handler returning `false` declines
 * and the next candidate is tried.
 *
 * Modal surfaces go further than precedence: they *claim* the keyboard, and
 * everything below the claim's floor stops being live at all. See
 * `claimKeyboard`.
 */

export const SEQUENCE_TIMEOUT_MS = 900;

/**
 * The priority at which a binding is saying "I belong to a modal surface".
 *
 * A claim on the keyboard silences every binding below this line, which is how
 * one fix covers every dialog instead of each dialog gating each mode by hand.
 * The number is not new — the convention that shell bindings sit at 0 and
 * overlays at 100 predates this and every dialog in the app already follows it,
 * which is what makes a single floor able to tell the two apart.
 *
 * The cost of the convention is that a binding which borrowed a high priority
 * to win one local fight now reads as overlay-class and survives the claim.
 * Three do, and none of them is a dialog: the calendar's `/` and the Escape
 * that closes the finder it opens (220 and 230, above the palette's `/` on
 * purpose), and the agent drawer's Escape at 150. All three still answer from
 * behind an open dialog. None of them writes anything, and the fix in each case
 * is one clause — `&& !overlayOwnsKeyboard(ui)` on the surface's own `active` —
 * in a file this change was not free to edit.
 */
export const OVERLAY_KEY_FLOOR = 100;

/** The subset of KeyboardEvent the dispatcher touches — so it is testable. */
export interface KeyEventLike {
  key: string;
  /**
   * The physical key, e.g. "Digit1". Needed because `key` is what the layout
   * *produced*: on macOS Option+1 yields "¡", so a binding written "alt+1"
   * could never match. Optional so synthetic events in tests stay terse.
   */
  code?: string;
  metaKey: boolean;
  ctrlKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
  target?: EventTargetLike | null;
  preventDefault?: () => void;
  stopPropagation?: () => void;
}

export interface EventTargetLike {
  tagName?: string;
  isContentEditable?: boolean;
}

export interface KeyBinding {
  /** One token, or space-separated tokens for a sequence. */
  keys: string;
  handler: (event: KeyEventLike) => void | boolean;
  /** Shown in the palette and the help sheet. Omit to hide the binding. */
  description?: string;
  /** Grouping label for the help sheet: "Mail", "Calendar", "Global". */
  group?: string;
  /** Gate on mode, focus, dialog state. Absent means always live. */
  when?: () => boolean;
  /** Bindings are dead while typing unless this is set. */
  allowInInput?: boolean;
  /** Higher wins. Shell bindings sit at 0, overlays at 100. */
  priority?: number;
  /** Set false for bindings that should not swallow the browser default. */
  preventDefault?: boolean;
  /**
   * Take the key away from every other binding, then leave the event alone.
   *
   * For one situation, and it is the terminal session pane. A pty wants every
   * keystroke, and the emulator that encodes a keystroke into bytes — dead
   * keys, composition, ⌃C, the arrow keys' escape sequences — is a DOM listener
   * on its own textarea. So the pane needs both halves of a thing the registry
   * could not previously say: *no app binding may answer this key*, and *the
   * event must still reach the element under the caret*.
   *
   * `preventDefault: false` is only the first half of that. The dispatcher
   * calls `stopPropagation` on every consumed key, which it must, or a dialog's
   * Escape would also reach whatever is behind it — and stopping propagation in
   * the capture phase means the textarea never sees the key at all.
   *
   * A binding marked this way therefore consumes the *lookup* and nothing else:
   * no `preventDefault`, no `stopPropagation`, and no further binding tried.
   * Nothing but the pane should need it; see `TerminalPane` for the contract it
   * implements.
   */
  passthrough?: boolean;
  /**
   * Registration order, stamped by the registry — not something a caller sets.
   *
   * `active()` hands bindings back in *precedence* order, which is priority
   * first and recency second, so a list read straight off it comes out
   * backwards and with the odd binding jumped up the page because it happens to
   * carry a priority. The help sheet wants neither: it wants the order the
   * feature declared its keys in, because that is the order somebody chose on
   * purpose. This is how it gets it.
   */
  readonly order?: number;
}

interface Registered extends KeyBinding {
  seq: string[];
  order: number;
}

const NAMED_KEYS: Record<string, string> = {
  escape: "escape",
  esc: "escape",
  enter: "enter",
  return: "enter",
  tab: "tab",
  backspace: "backspace",
  delete: "delete",
  " ": "space",
  space: "space",
  spacebar: "space",
  arrowup: "up",
  arrowdown: "down",
  arrowleft: "left",
  arrowright: "right",
  up: "up",
  down: "down",
  left: "left",
  right: "right",
  home: "home",
  end: "end",
  pageup: "pageup",
  pagedown: "pagedown",
};

const MODIFIER_KEYS = new Set(["shift", "control", "ctrl", "alt", "meta", "capslock"]);

export type ModKey = "meta" | "ctrl";

export function detectModKey(): ModKey {
  if (typeof navigator === "undefined") return "ctrl";
  const ua = `${navigator.platform ?? ""} ${navigator.userAgent ?? ""}`;
  return /mac|iphone|ipad/i.test(ua) ? "meta" : "ctrl";
}

function baseKey(key: string): string {
  const lower = key.toLowerCase();
  return NAMED_KEYS[lower] ?? lower;
}

/**
 * Canonical token for an event. Shift is only recorded for alphabetic keys —
 * for everything else the shifted character *is* the key ("?" not "shift+/").
 */
export function tokenFromEvent(event: KeyEventLike): string | null {
  const raw = event.key;
  if (!raw || MODIFIER_KEYS.has(raw.toLowerCase())) return null;

  /*
   * Prefer the physical digit when Alt is held.
   *
   * `event.key` is what the keyboard layout produced, and on macOS Option
   * remaps the number row to ¡™£¢∞§¶•ª. A binding written "alt+1" therefore
   * never fired — the token came out as "alt+¡". Reading the digit off
   * `event.code` fixes that without affecting any other key.
   */
  const digit =
    event.altKey && /^Digit[0-9]$/.test(event.code ?? "")
      ? event.code!.slice(-1)
      : null;

  const key = digit ?? baseKey(raw);
  const parts: string[] = [];
  if (event.metaKey) parts.push("meta");
  if (event.ctrlKey) parts.push("ctrl");
  if (event.altKey) parts.push("alt");
  if (event.shiftKey && (key.length > 1 || /^[a-z]$/.test(key))) parts.push("shift");
  parts.push(key);
  return parts.join("+");
}

/** Canonical token for a binding string, resolving `mod` for this platform. */
export function normalizeToken(token: string, mod: ModKey): string {
  const pieces = token.split("+").filter(Boolean);
  const key = baseKey(pieces.pop() ?? "");
  const mods = new Set<string>();
  for (const piece of pieces) {
    const m = piece.toLowerCase();
    mods.add(m === "mod" || m === "cmd" || m === "command" ? mod : m === "control" ? "ctrl" : m);
  }
  const parts: string[] = [];
  if (mods.has("meta")) parts.push("meta");
  if (mods.has("ctrl")) parts.push("ctrl");
  if (mods.has("alt") || mods.has("option")) parts.push("alt");
  if (mods.has("shift")) parts.push("shift");
  parts.push(key);
  return parts.join("+");
}

export function isTypingTarget(target: EventTargetLike | null | undefined): boolean {
  if (!target) return false;
  if (target.isContentEditable) return true;
  const tag = target.tagName?.toUpperCase();
  return tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT";
}

export interface Keymap {
  register(binding: KeyBinding): () => void;
  /** Returns true when a binding consumed the event. */
  handle(event: KeyEventLike, now?: number): boolean;
  /** Everything currently registered and live, in precedence order. */
  active(): KeyBinding[];
  /** The sequence prefix awaiting completion, e.g. "g". */
  pending(): string | null;
  /**
   * Key sequences that more than one *currently live* binding answers to.
   *
   * Ties resolve by most-recently-registered, so a conflict does not throw —
   * it silently gives the key to whichever component mounted last. That is how
   * Cmd-1 came to toggle the first calendar instead of switching to mail: App
   * registers it globally, CalendarMode registers it per calendar, and the
   * calendar mounts second.
   *
   * Live means `when()` currently passes, so two bindings scoped to different
   * modes are not a conflict — only ones that can fire at the same moment.
   *
   * A *tie* is the thing being reported, so bindings that differ in `priority`
   * are not one: priority is an explicit statement about which wins, and the
   * outcome is the same however the two components happen to mount. Reporting
   * those made Escape look broken every time the shortcut sheet opened over a
   * calendar with an event selected — a real pair, deliberately ordered, and
   * behaving exactly as written.
   */
  conflicts(): KeyConflict[];
  clear(): void;
  /**
   * Hand the keyboard to a modal surface until the returned function is called.
   *
   * While a claim is held, only bindings at or above `floor` are live — they do
   * not merely lose a tie, they stop being candidates, so an unclaimed key
   * reaches the DOM untouched and the dialog's own inputs, buttons and popup
   * menus behave exactly as they would with no registry at all.
   *
   * This is the answer to a whole class of bug rather than to one key: with
   * Preferences open, `e` still archived the conversation behind it, because
   * every mode gate had been written against the one overlay anybody remembered
   * to gate on. Nothing is more likely to be forgotten than the next dialog, so
   * the surface every dialog already renders through claims the keyboard for
   * all of them — see `Overlay` in `components/ui/dialog.tsx`.
   *
   * Claims nest: a select menu inside preferences, a confirmation inside the
   * plugins panel. The floor in force is the highest one claimed.
   */
  claimKeyboard(floor?: number): () => void;
  /** How many modal surfaces hold a claim. Zero means the app has the keys. */
  claims(): number;
  /** Notified when the claim count changes, for gates that render off it. */
  subscribe(listener: () => void): () => void;
}

export interface KeyConflict {
  /** The normalised sequence, e.g. "mod+1". */
  keys: string;
  /** The competing bindings, winner first. */
  bindings: KeyBinding[];
}

export function createKeymap(mod: ModKey = detectModKey()): Keymap {
  const bindings = new Set<Registered>();
  let order = 0;
  let pendingSeq: string[] = [];
  let pendingAt = 0;
  /* One entry per modal surface currently up; the value is its floor. */
  const held = new Set<{ floor: number }>();
  const listeners = new Set<() => void>();

  function floor(): number {
    let highest = Number.NEGATIVE_INFINITY;
    for (const claim of held) highest = Math.max(highest, claim.floor);
    return highest;
  }

  function candidates(): Registered[] {
    const cutoff = floor();
    return [...bindings]
      .filter((b) => (b.priority ?? 0) >= cutoff)
      .filter((b) => (b.when ? b.when() : true))
      .sort((a, b) => (b.priority ?? 0) - (a.priority ?? 0) || b.order - a.order);
  }

  function run(binding: Registered, event: KeyEventLike): boolean {
    const result = binding.handler(event);
    if (result === false) return false;
    // Claimed, and then handed straight back to the page. See `passthrough`.
    if (binding.passthrough) return true;
    if (binding.preventDefault !== false) event.preventDefault?.();
    event.stopPropagation?.();
    return true;
  }

  return {
    register(binding) {
      const registered: Registered = {
        ...binding,
        seq: binding.keys.trim().split(/\s+/).map((t) => normalizeToken(t, mod)),
        order: order++,
      };
      bindings.add(registered);
      return () => {
        bindings.delete(registered);
      };
    },

    conflicts() {
      const live = candidates();
      // Grouped by sequence *and* priority: only same-priority bindings are
      // ties, and a tie is what this reports. See the doc on `conflicts`.
      const groups = new Map<string, KeyConflict>();
      for (const b of live) {
        const keys = b.seq.join(" ");
        const slot = keys + " @" + String(b.priority ?? 0);
        const group = groups.get(slot);
        if (group) group.bindings.push(b);
        else groups.set(slot, { keys, bindings: [b] });
      }
      return [...groups.values()].filter((group) => group.bindings.length > 1);
    },

    handle(event, now = Date.now()) {
      const token = tokenFromEvent(event);
      if (token === null) return false;

      const typing = isTypingTarget(event.target);
      const live = candidates().filter((b) => (typing ? b.allowInInput === true : true));

      // A live sequence takes precedence over anything else the token matches.
      if (pendingSeq.length > 0) {
        const expired = now - pendingAt > SEQUENCE_TIMEOUT_MS;
        const attempt = [...pendingSeq, token];
        pendingSeq = [];
        if (!expired) {
          for (const binding of live) {
            if (sequenceEquals(binding.seq, attempt) && run(binding, event)) return true;
          }
          // A started sequence swallows its follow-up key even when it misses,
          // so `g` then a stray key never archives anything.
          event.preventDefault?.();
          return true;
        }
      }

      for (const binding of live) {
        if (binding.seq.length !== 1) continue;
        /*
         * `*` matches any key. It exists for modal surfaces that must swallow
         * everything while they are up — a reference sheet you read with `j`
         * should not also scroll the list behind it. Give it a priority below
         * the Escape that closes the surface, or you cannot get out.
         */
        const matches = binding.seq[0] === token || binding.seq[0] === "*";
        if (matches && run(binding, event)) return true;
      }

      // No exact match — does this key open a sequence?
      if (live.some((b) => b.seq.length > 1 && b.seq[0] === token)) {
        pendingSeq = [token];
        pendingAt = now;
        event.preventDefault?.();
        return true;
      }

      return false;
    },

    active() {
      return candidates();
    },

    pending() {
      return pendingSeq.length > 0 ? pendingSeq.join(" ") : null;
    },

    clear() {
      pendingSeq = [];
    },

    claimKeyboard(floor = OVERLAY_KEY_FLOOR) {
      const claim = { floor };
      held.add(claim);
      /*
       * A half-typed `g` belongs to the surface that was on screen when it was
       * pressed. Dropping it here means the sequence cannot complete against a
       * dialog it was never aimed at.
       */
      pendingSeq = [];
      for (const listener of listeners) listener();
      return () => {
        if (!held.delete(claim)) return;
        for (const listener of listeners) listener();
      };
    },

    claims() {
      return held.size;
    },

    subscribe(listener) {
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    },
  };
}

function sequenceEquals(a: readonly string[], b: readonly string[]): boolean {
  return a.length === b.length && a.every((token, i) => token === b[i]);
}

/** Pretty-print a binding for the UI: "mod+k" → "⌘K", "g i" → "G then I". */
export function formatBinding(keys: string, mod: ModKey = detectModKey()): string {
  const tokens = keys.trim().split(/\s+/);
  const rendered = tokens.map((token) => {
    const parts = normalizeToken(token, mod).split("+");
    const key = parts.pop() ?? "";
    const glyphs = parts
      .map((p) => (p === "meta" ? "⌘" : p === "ctrl" ? "⌃" : p === "alt" ? "⌥" : "⇧"))
      .join("");
    return glyphs + (KEY_GLYPHS[key] ?? key.toUpperCase());
  });
  return rendered.join(" then ");
}

const KEY_GLYPHS: Record<string, string> = {
  enter: "↩",
  escape: "Esc",
  space: "Space",
  up: "↑",
  down: "↓",
  left: "←",
  right: "→",
  tab: "⇥",
  backspace: "⌫",
};
