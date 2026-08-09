/**
 * Preferences — the model, the defaults, and the two IPC calls.
 *
 * Everything the user can decide about how Mach behaves is one field of
 * {@link Preferences}, and every field has a default that is the behaviour the
 * app had before it was configurable. That is the property that makes this file
 * safe to grow: adding a preference cannot change what an existing install does
 * until somebody opens ⌘, and changes it.
 *
 * # Why parsing is paranoid
 *
 * The store hands back whatever is in SQLite, and SQLite is a file on a disk the
 * user owns. A row can be written by a newer build, hand-edited, half-migrated,
 * or simply the wrong type because something wrote to the key that had no
 * business doing so. So {@link parsePreferences} never trusts a value: every
 * field is checked against the shape it must have and falls back to its default
 * if it is not that shape, per field rather than per document. One bad row costs
 * one setting, not the whole surface — which is the same rule the Rust side
 * applies to unparsable JSON, one layer down.
 *
 * Numbers are clamped rather than rejected. A sync interval of zero is not a
 * lie about what the user wanted; it is a value that would spin the loop, and
 * the honest answer to it is the nearest legal one.
 *
 * # Where the values are read
 *
 * Nowhere in this file. A preference that only lives here would be a control
 * that renders and does nothing, which is the thing this whole surface exists
 * to not be. Each one is read at its point of use:
 *
 *   | preference | who reads it |
 *   |---|---|
 *   | `theme` | `useMach` — mirrors it into `ui.theme`, which paints `.dark` |
 *   | `defaultAccountId` | `ComposerDock`, when `c` has no account to infer |
 *   | `signatures` | `ComposerDock`, on every draft it opens |
 *   | `undoWindowSeconds` | `useMach` — how long the undo affordance lingers |
 *   | `sendDelaySeconds` | `ComposerDock` — the outbox's `sendAfter` |
 *   | `weekStartsOn` | `viewRange` in `useMach`, via `CalendarMode` |
 *   | `workingHours` | `TimeGrid` — the shaded band and the opening scroll |
 *   | `syncIntervalSeconds` | Rust: `ipc::prefs`, straight into the sync loop |
 *   | `notificationsEnabled` | Rust: `notify::plan`, before it decides anything |
 *   | `notificationAccounts` | Rust: `notify::plan`, per account |
 *   | `badgeEnabled` | Rust: `notify::badge`, on every recompute |
 *   | `agentBackend` | Rust: `agent::backend`, when a session starts |
 *   | `agentModel` | Rust: `agent::backend`, likewise |
 *   | `agentCommand` | Rust: `agent::backend`, likewise |
 *
 * The last three are read only in Rust, and that is not an inconsistency. A
 * notification is a decision about a message that has *just arrived*, which is
 * knowledge the sync loop has and the window does not — the window may not even
 * be open. So these three are settings the frontend writes and never reads,
 * which is exactly the shape `syncIntervalSeconds` already had.
 *
 * # Transport
 *
 * Two Tauri commands, called the way `compose.ts` calls its own: directly,
 * rather than through `MachDataSource`. Preferences are not mail — they are not
 * paged, not pushed, not part of the read model the data seam exists to
 * abstract — and putting them on that interface would mean the fixture source
 * had to implement a settings store it has no use for. Outside Tauri they fall
 * back to `localStorage`, so the browser window that `bun run dev` serves is a
 * real place to iterate on this dialog.
 */

import { isTauri } from "./ipc";

/* -------------------------------------------------------------------------- */
/* The model                                                                   */
/* -------------------------------------------------------------------------- */

export type Theme = "system" | "light" | "dark";

/**
 * Which brain answers ⌘K.
 *
 * `auto` is the default and must stay the default: it prefers the Claude Code
 * CLI when one is installed — which costs nothing extra, because the user is
 * already paying for it — and falls back to the Anthropic API when a key is
 * set. The three explicit values exist for the person who wants to override
 * that, and an explicit value is never quietly substituted: asking for a
 * backend that cannot run is an error with a sentence, not a silent fallback.
 *
 * The strings are the wire values `agent::backend::BackendChoice` parses.
 */
export type AgentBackend = "auto" | "claudeCli" | "anthropicApi" | "command";

/** Sunday, Monday or Saturday — the three starts anybody actually uses. */
export type WeekStart = 0 | 1 | 6;

/** Whole hours in local time, `start` inclusive and `end` exclusive. */
export interface WorkingHours {
  start: number;
  end: number;
}

export interface Preferences {
  /**
   * The account a message written from nothing is sent from.
   *
   * Only consulted when there is nothing better to go on: a reply already knows
   * its account, and a list filtered to one account has said which. `null`
   * means "no opinion", which lands on the first account.
   */
  defaultAccountId: number | null;
  /** Account id (as a string key) to plain-text signature. */
  signatures: Record<string, string>;
  theme: Theme;
  syncIntervalSeconds: number;
  /**
   * How long ⌘Z stays offered after a command.
   *
   * The old value was six seconds, hardcoded, and it was wrong for the gesture
   * it was guarding: one keystroke can archive fifty conversations, and six
   * seconds is not long enough to notice that is what happened, let alone to
   * decide about it.
   */
  undoWindowSeconds: number;
  /** How long a sent message sits in the outbox before it actually leaves. */
  sendDelaySeconds: number;
  weekStartsOn: WeekStart;
  workingHours: WorkingHours;
  /**
   * Whether new mail is allowed to interrupt.
   *
   * On by default, which is the one place this file breaks its own rule about
   * defaults matching the old behaviour. The old behaviour was silence, and
   * silence is the bug: a client you leave open all day that never says
   * anything is one you have to keep checking, which is the thing it was
   * supposed to replace. What makes "on" safe is that the rule behind it is
   * narrow — see `notify::rule` — so this is not a switch that turns 61,000
   * messages into 61,000 banners.
   */
  notificationsEnabled: boolean;
  /**
   * Account id (as a string key, like `signatures`) to `false` for the
   * mailboxes that should stay quiet.
   *
   * Absent means "notify", so adding an account does not require a visit to
   * this dialog before you hear from it — and removing one leaves a key nobody
   * reads rather than a mute that silently transfers to the next id.
   */
  notificationAccounts: Record<string, boolean>;
  /** The unread count on the Dock icon. */
  badgeEnabled: boolean;
  agentBackend: AgentBackend;
  /**
   * A model id or alias, or `""` for the backend's own default.
   *
   * Free text rather than a list, because the two backends do not agree on what
   * a model is called — `opus` is a fine answer for the CLI and a meaningless
   * one for the Messages API — and a list would go stale the week after it was
   * written.
   */
  agentModel: string;
  /** The command line for `agentBackend: "command"`. See `docs/agent-backends.md`. */
  agentCommand: string;
}

/**
 * The defaults, which are also the behaviour the app had before any of this
 * existed — with the one deliberate exception noted on `undoWindowSeconds`.
 */
export const DEFAULT_PREFERENCES: Preferences = {
  defaultAccountId: null,
  signatures: {},
  theme: "system",
  syncIntervalSeconds: 60,
  undoWindowSeconds: 20,
  sendDelaySeconds: 10,
  weekStartsOn: 1,
  workingHours: { start: 9, end: 17 },
  notificationsEnabled: true,
  notificationAccounts: {},
  badgeEnabled: true,
  agentBackend: "auto",
  agentModel: "",
  agentCommand: "",
};

export interface Bounds {
  min: number;
  max: number;
}

/**
 * The ranges the numeric preferences are held to.
 *
 * These are the same numbers the dialog offers, and they are enforced here as
 * well as there because the dialog is not the only writer — a hand-edited
 * database is, and so is a future agent tool.
 */
export const SYNC_INTERVAL_BOUNDS: Bounds = { min: 15, max: 6 * 60 * 60 };
export const UNDO_WINDOW_BOUNDS: Bounds = { min: 3, max: 300 };
export const SEND_DELAY_BOUNDS: Bounds = { min: 0, max: 300 };

/* -------------------------------------------------------------------------- */
/* Parsing                                                                     */
/* -------------------------------------------------------------------------- */

const AGENT_BACKENDS = new Set<string>(["auto", "claudeCli", "anthropicApi", "command"]);
const THEMES = new Set<string>(["system", "light", "dark"]);
const WEEK_STARTS = new Set<number>([0, 1, 6]);

function clamp(value: number, { min, max }: Bounds): number {
  return Math.min(max, Math.max(min, value));
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/** A finite number, clamped, or the default. Rounded: seconds are whole here. */
function seconds(value: unknown, bounds: Bounds, fallback: number): number {
  if (typeof value !== "number" || !Number.isFinite(value)) return fallback;
  return clamp(Math.round(value), bounds);
}

/**
 * Working hours, or the default pair.
 *
 * Rejected as a unit rather than per end, because the two only mean anything
 * together: a start of 17 and an end of 9 is not "one bad field", it is a band
 * that cannot be drawn. A day that is entirely working hours (0 to 24) is
 * legal — some people do want the grid unshaded.
 */
function workingHours(value: unknown, fallback: WorkingHours): WorkingHours {
  if (!isRecord(value)) return fallback;
  const start = value.start;
  const end = value.end;
  if (typeof start !== "number" || typeof end !== "number") return fallback;
  if (!Number.isFinite(start) || !Number.isFinite(end)) return fallback;
  const from = clamp(Math.round(start), { min: 0, max: 23 });
  const to = clamp(Math.round(end), { min: 1, max: 24 });
  return to > from ? { start: from, end: to } : fallback;
}

/**
 * Signatures, keyed by account id, with anything that is not a string dropped.
 *
 * Keys are kept as strings even though they name numeric account ids: that is
 * what JSON objects have, and round-tripping them through `Number` only creates
 * a way for `"3"` and `3` to disagree.
 */
function signatures(value: unknown): Record<string, string> {
  if (!isRecord(value)) return {};
  const out: Record<string, string> = {};
  for (const [key, text] of Object.entries(value)) {
    if (typeof text === "string" && text.length > 0) out[key] = text;
  }
  return out;
}

/**
 * Which accounts are muted, with anything that is not a boolean dropped.
 *
 * Only the `false` entries are kept. `true` is the default already, so storing
 * it would be a row that says nothing and a second way to spell the same state
 * — and two spellings is how a mute ends up depending on which one was written
 * last.
 */
function mutedAccounts(value: unknown): Record<string, boolean> {
  if (!isRecord(value)) return {};
  const out: Record<string, boolean> = {};
  for (const [key, muted] of Object.entries(value)) {
    if (muted === false) out[key] = false;
  }
  return out;
}

/** A stored boolean, or the default. */
function flag(value: unknown, fallback: boolean): boolean {
  return typeof value === "boolean" ? value : fallback;
}

/**
 * Whatever the store handed back, as a `Preferences` that is definitely a
 * `Preferences`. Unknown keys are ignored; missing and malformed ones default.
 *
 * Ignoring unknown keys is also what retires a preference: `density` was a row
 * in this table until the app settled on one display, and the stores that still
 * have that row need no migration — nothing reads it, so nothing sees it.
 */
export function parsePreferences(raw: unknown): Preferences {
  const source = isRecord(raw) ? raw : {};
  const d = DEFAULT_PREFERENCES;

  const accountId = source.defaultAccountId;

  return {
    defaultAccountId:
      typeof accountId === "number" && Number.isInteger(accountId) && accountId > 0
        ? accountId
        : null,
    signatures: signatures(source.signatures),
    theme: typeof source.theme === "string" && THEMES.has(source.theme)
      ? (source.theme as Theme)
      : d.theme,
    syncIntervalSeconds: seconds(
      source.syncIntervalSeconds,
      SYNC_INTERVAL_BOUNDS,
      d.syncIntervalSeconds,
    ),
    undoWindowSeconds: seconds(source.undoWindowSeconds, UNDO_WINDOW_BOUNDS, d.undoWindowSeconds),
    sendDelaySeconds: seconds(source.sendDelaySeconds, SEND_DELAY_BOUNDS, d.sendDelaySeconds),
    weekStartsOn:
      typeof source.weekStartsOn === "number" && WEEK_STARTS.has(source.weekStartsOn)
        ? (source.weekStartsOn as WeekStart)
        : d.weekStartsOn,
    workingHours: workingHours(source.workingHours, d.workingHours),
    notificationsEnabled: flag(source.notificationsEnabled, d.notificationsEnabled),
    notificationAccounts: mutedAccounts(source.notificationAccounts),
    badgeEnabled: flag(source.badgeEnabled, d.badgeEnabled),
    agentBackend:
      typeof source.agentBackend === "string" && AGENT_BACKENDS.has(source.agentBackend)
        ? (source.agentBackend as AgentBackend)
        : d.agentBackend,
    // Trimmed, because these are typed by hand and a trailing space in a model
    // id is a failure two layers away from where it was made.
    agentModel: text(source.agentModel),
    agentCommand: text(source.agentCommand),
  };
}

/** A trimmed string, or `""`. */
function text(value: unknown): string {
  return typeof value === "string" ? value.trim() : "";
}

/* -------------------------------------------------------------------------- */
/* Derived values — the small conversions every consumer would otherwise repeat */
/* -------------------------------------------------------------------------- */

export function undoWindowMs(prefs: Preferences): number {
  return prefs.undoWindowSeconds * 1000;
}

export function sendDelayMs(prefs: Preferences): number {
  return prefs.sendDelaySeconds * 1000;
}

/**
 * Whether one account is allowed to interrupt. Absent means yes — see
 * {@link Preferences.notificationAccounts}.
 */
export function notifiesAccount(prefs: Preferences, accountId: number): boolean {
  return prefs.notificationAccounts[String(accountId)] !== false;
}

/**
 * The map with one account switched on or off.
 *
 * Returns a new object rather than mutating, because it is written straight
 * into React state; and it deletes rather than storing `true`, so the stored
 * map only ever holds the exceptions.
 */
export function withAccountNotifying(
  prefs: Preferences,
  accountId: number,
  notifying: boolean,
): Record<string, boolean> {
  const next = { ...prefs.notificationAccounts };
  if (notifying) delete next[String(accountId)];
  else next[String(accountId)] = false;
  return next;
}

/** The signature for an account, or `""` when there is none. */
export function signatureFor(prefs: Preferences, accountId: number | null | undefined): string {
  if (accountId == null) return "";
  return prefs.signatures[String(accountId)] ?? "";
}

/**
 * The RFC 3676 signature delimiter: a line of exactly `-- `, trailing space and
 * all. Every mail client on earth uses it to find the signature and grey it out
 * or trim it from a quote, and getting the space wrong means none of them do.
 */
export const SIGNATURE_DELIMITER = "\n\n-- \n";

/**
 * Put the signature at the bottom of a body, once.
 *
 * Idempotent, because the composer opens a draft that may already have been
 * saved with one — reopening a half-written reply must not stack two copies.
 * An empty signature, or a body that already ends with this exact text, is
 * returned untouched.
 */
export function withSignature(body: string, signature: string): string {
  const trimmed = signature.trim();
  if (!trimmed) return body;
  const block = SIGNATURE_DELIMITER + trimmed;
  return body.includes(block) ? body : body + block;
}

/**
 * Which account a brand-new message should be sent from.
 *
 * `scoped` is the account the list is filtered to, and it wins: if the user is
 * looking at one mailbox, that is the answer, whatever the preference says.
 * Then the preference, but only if it still names an account that exists —
 * removing an account must not leave the composer pointing at a ghost. Then the
 * first account, which is what the code did before any of this.
 */
export function composeAccountId(
  prefs: Preferences,
  accounts: readonly { id: number }[],
  scoped: number | null,
): number | undefined {
  if (scoped !== null && accounts.some((a) => a.id === scoped)) return scoped;
  const preferred = prefs.defaultAccountId;
  if (preferred !== null && accounts.some((a) => a.id === preferred)) return preferred;
  return accounts[0]?.id;
}

/* -------------------------------------------------------------------------- */
/* Transport                                                                   */
/* -------------------------------------------------------------------------- */

/** Where the browser fallback keeps them. One blob, because there is no store. */
export const LOCAL_STORAGE_KEY = "mach.preferences";

async function invoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  const { invoke: call } = await import("@tauri-apps/api/core");
  return call<T>(command, args);
}

function readLocal(): Record<string, unknown> {
  if (typeof window === "undefined") return {};
  try {
    const raw = window.localStorage.getItem(LOCAL_STORAGE_KEY);
    return raw ? (JSON.parse(raw) as Record<string, unknown>) : {};
  } catch {
    // A quota error, private browsing, a half-written value — none of which is
    // a reason to render nothing.
    return {};
  }
}

function writeLocal(key: string, value: unknown): void {
  if (typeof window === "undefined") return;
  try {
    const all = { ...readLocal(), [key]: value };
    window.localStorage.setItem(LOCAL_STORAGE_KEY, JSON.stringify(all));
  } catch {
    /* see readLocal */
  }
}

/**
 * Every preference, in one round trip.
 *
 * Never rejects. A backend that is not there, a command that is not registered
 * in this build, a store that will not open — all of them mean "the user has
 * not configured anything", and the defaults are a correct answer to that.
 */
export async function loadPreferences(): Promise<Preferences> {
  if (!isTauri()) return parsePreferences(readLocal());
  try {
    return parsePreferences(await invoke<Record<string, unknown>>("get_preferences"));
  } catch {
    return DEFAULT_PREFERENCES;
  }
}

/**
 * Write one preference.
 *
 * One key per call rather than the whole document, so two windows editing two
 * different settings cannot overwrite each other's — the row is the unit of
 * change on both sides of the wire.
 */
export async function writePreference<K extends keyof Preferences>(
  key: K,
  value: Preferences[K],
): Promise<void> {
  if (!isTauri()) {
    writeLocal(key, value);
    return;
  }
  await invoke("set_preference", { key, value });
}

/* -------------------------------------------------------------------------- */
/* Notification permission — not a preference, but the dialog has to show it   */
/* -------------------------------------------------------------------------- */

/**
 * What macOS will let Mach do, which is not the same question as what the user
 * asked for.
 *
 * `permission` is the operating system's answer; `available` is whether there
 * is anything to deliver through at all — false in a QA instance, and in the
 * browser that `bun run dev` serves. Both exist so that ⌘, can explain a switch
 * that is on and silent, rather than leaving the user to wonder.
 *
 * It lives beside the preferences rather than in `lib/ipc.ts` for the reason
 * the top of this file gives for `get_preferences`: this is settings-surface
 * plumbing, not part of the read model the data seam abstracts, and the fixture
 * source has no business implementing it.
 */
export interface NotificationStatus {
  permission: "granted" | "denied" | "prompt";
  available: boolean;
}

/** What a place with no notifications looks like. Never an error. */
export const NO_NOTIFICATIONS: NotificationStatus = {
  permission: "prompt",
  available: false,
};

function parseNotificationStatus(raw: unknown): NotificationStatus {
  if (!isRecord(raw)) return NO_NOTIFICATIONS;
  const permission = raw.permission;
  return {
    permission:
      permission === "granted" || permission === "denied" ? permission : "prompt",
    available: raw.available === true,
  };
}

/** Ask without asking macOS anything it has not been asked already. */
export async function loadNotificationStatus(): Promise<NotificationStatus> {
  if (!isTauri()) return NO_NOTIFICATIONS;
  try {
    return parseNotificationStatus(await invoke<unknown>("notification_state"));
  } catch {
    return NO_NOTIFICATIONS;
  }
}

/**
 * Ask macOS for permission, now.
 *
 * Called when the switch is turned on and at no other time. A prompt that
 * arrives at launch — before there is any mail, and before the user has any
 * idea what the app wants — is the one people refuse out of reflex.
 */
export async function requestNotificationPermission(): Promise<NotificationStatus> {
  if (!isTauri()) return NO_NOTIFICATIONS;
  try {
    return parseNotificationStatus(
      await invoke<unknown>("notification_request_permission"),
    );
  } catch {
    return NO_NOTIFICATIONS;
  }
}

/* -------------------------------------------------------------------------- */
/* Session — where the window was, which is not a setting                      */
/* -------------------------------------------------------------------------- */

/**
 * What the app remembers about where it was, as opposed to what the user chose.
 *
 * The distinction is worth keeping even though both live in the same table. A
 * *setting* is a decision somebody made in a dialog and can find again; this is
 * the app not being stupid about the divider you just dragged. Session state is
 * therefore deliberately **not** part of {@link Preferences} and never renders
 * as a row in ⌘, — a "reading pane width: 520" control would be an absurd thing
 * to put in front of a person who has a mouse.
 *
 * It is one key rather than five for a practical reason: it changes far more
 * often than any preference does, so it is written debounced, as a unit, and one
 * row is one write.
 *
 * Every field is optional on the way in. A session written by a build with more
 * fields, or none at all on first launch, has to mean "use the defaults", not
 * "fail to boot".
 */
export interface UiSession {
  mode: "mail" | "calendar";
  calendarView: "day" | "week" | "month";
  accountId: number | null;
  labelId: string;
  listWidth: number;
  /** Account ids whose calendar group is folded up in the sidebar. */
  collapsedCalendarAccounts: number[];
  /** Mail rail sections folded up — `"inbox"`, `"folders"`, `"favorites"`. */
  collapsedRailSections: string[];
}

/** The key the session blob lives under. Alphanumeric, like every other key. */
export const SESSION_KEY = "uiSession";

/**
 * The width bounds `uiReducer` clamps to.
 *
 * Duplicated here on purpose rather than imported: the restore path must not be
 * able to put a value on screen that a dispatch could not, and a stored 4000 —
 * from a hand edit, or from a build with a different maximum — would otherwise
 * paint one pane at full width before the first drag corrected it.
 */
export const LIST_WIDTH_BOUNDS: Bounds = { min: 280, max: 640 };

const MODES = new Set<string>(["mail", "calendar"]);
const CALENDAR_VIEWS = new Set<string>(["day", "week", "month"]);

/**
 * Whatever was stored, as much of a session as can be believed.
 *
 * Returns a *partial*: a field that was absent or malformed is simply not
 * there, so the caller restores only what it actually knows and leaves its own
 * defaults standing for the rest. That is a better shape than filling the gaps
 * with a copy of `initialUi` here, which would mean this file had to know the
 * shell's defaults and go stale when they changed.
 */
export function parseSession(raw: unknown): Partial<UiSession> {
  if (!isRecord(raw)) return {};
  const out: Partial<UiSession> = {};

  if (typeof raw.mode === "string" && MODES.has(raw.mode)) {
    out.mode = raw.mode as UiSession["mode"];
  }
  if (typeof raw.calendarView === "string" && CALENDAR_VIEWS.has(raw.calendarView)) {
    out.calendarView = raw.calendarView as UiSession["calendarView"];
  }
  if (raw.accountId === null) out.accountId = null;
  else if (typeof raw.accountId === "number" && Number.isInteger(raw.accountId)) {
    out.accountId = raw.accountId;
  }
  if (typeof raw.labelId === "string" && raw.labelId.length > 0) out.labelId = raw.labelId;
  if (typeof raw.listWidth === "number" && Number.isFinite(raw.listWidth)) {
    out.listWidth = clamp(Math.round(raw.listWidth), LIST_WIDTH_BOUNDS);
  }
  if (Array.isArray(raw.collapsedCalendarAccounts)) {
    out.collapsedCalendarAccounts = raw.collapsedCalendarAccounts.filter(
      (id): id is number => typeof id === "number" && Number.isInteger(id),
    );
  }
  // Section ids are not validated against the rail's own list on purpose: a
  // section that no longer exists is inert, and dropping unknown ids here would
  // silently forget a section belonging to a newer build the user also runs.
  if (Array.isArray(raw.collapsedRailSections)) {
    out.collapsedRailSections = raw.collapsedRailSections.filter(
      (id): id is string => typeof id === "string" && id.length > 0,
    );
  }

  return out;
}

export async function loadSession(): Promise<Partial<UiSession>> {
  if (!isTauri()) return parseSession(readLocal()[SESSION_KEY]);
  try {
    const all = await invoke<Record<string, unknown>>("get_preferences");
    return parseSession(all?.[SESSION_KEY]);
  } catch {
    return {};
  }
}

export async function saveSession(session: Partial<UiSession>): Promise<void> {
  if (!isTauri()) {
    writeLocal(SESSION_KEY, session);
    return;
  }
  await invoke("set_preference", { key: SESSION_KEY, value: session });
}

/**
 * How long the window waits after the last change before writing.
 *
 * The dominant writer is the reading-pane divider, which fires on every
 * `pointermove` of a drag — a few hundred events for one gesture, each of which
 * would otherwise be a transaction. A drag is over well inside this, so a
 * whole resize costs exactly one write.
 */
export const SESSION_WRITE_DEBOUNCE_MS = 500;
