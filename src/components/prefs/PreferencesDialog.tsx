import { useEffect, useId, useRef, useState } from "react";
import type { Account } from "@/types";
import { useKeyBindings } from "@/hooks/useKeymap";
import { useMach } from "@/hooks/useMach";
import { getDataSource } from "@/lib/data";
import { registerResolver } from "@/lib/palette/resolver";
import { anyPopupOpen } from "@/lib/popups";
import { ACCOUNT_BG } from "@/lib/colors";
import { cn } from "@/lib/utils";
import {
  NO_NOTIFICATIONS,
  loadNotificationStatus,
  notifiesAccount,
  requestNotificationPermission,
  withAccountNotifying,
  type NotificationStatus,
  type Preferences,
  type WeekStart,
} from "@/lib/prefs";
import {
  UNKNOWN_BACKEND,
  loadBackendStatus,
  type AgentBackendStatus,
} from "@/lib/agent";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Overlay } from "@/components/ui/dialog";
import {
  Field,
  FieldContent,
  FieldDescription,
  FieldGroup,
  FieldLabel,
} from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Textarea } from "@/components/ui/textarea";
import { Filters } from "./Filters";
import { PREFERENCES_EVENT, preferencesResolver } from "./palette";
import { usePreferencesStore } from "./PreferencesProvider";

/**
 * Preferences: ⌘, and nothing else on screen, filling the window.
 *
 * # Why it takes the whole window
 *
 * It used to be a centred modal about the size of a business card, with the
 * settings in one column and a scrollbar down the side, which meant six of them
 * were visible at a time and the rest were somewhere below the fold. A settings
 * surface is a place you go to *find* something; a list you have to scroll to
 * see the shape of is the one thing that makes finding hard. Going full screen
 * buys a second axis: the sections are a list on the left and one section at a
 * time fills the right, so nothing scrolls and the whole surface is on screen
 * as a map of itself.
 *
 * It is still an overlay rather than a route or a third column. A settings pane
 * with a permanent home costs a piece of the window forever to serve a surface
 * opened once a month, and a modal costs nothing when it is closed.
 *
 * # No native controls, and no free text where a list will do
 *
 * Every control here is a Base UI primitive from `components/ui`. The reason is
 * the one `EventModal` records: WebKit draws `<select>` and `<input type=number>`
 * with its own steppers and metrics, and one panel wearing them makes the whole
 * app look like a web form. The second-order effect is nicer than the first —
 * because the numeric settings are `Select`s over a list of sensible values
 * rather than free-text fields, there is no invalid state to validate, no error
 * copy to write, and no way to type `0` into the sync interval and wonder why
 * the app is hammering Google.
 *
 * The one free-text control is the signature, because a signature is prose.
 *
 * # Saving
 *
 * There is no Save button and nothing on screen says so. Every control writes
 * on change, which is what a preferences surface should do and what every
 * native one does — a form that can be abandoned half-applied is a form that
 * needs a confirmation dialog, and this is a list of independent switches, not
 * a transaction. "Done" closes; it does not commit. A header that announced
 * "Saved as you change them" was the software talking about itself, which is a
 * line of text between the user and the control they came for.
 *
 * # Keyboard
 *
 * Tab walks the sections and then the controls, because they are in the DOM in
 * that order; `Overlay` traps focus and restores it, and focus opens on the
 * section list rather than on whichever control happens to be first. Escape
 * closes, unless a select menu is open — then the key belongs to that menu,
 * which is what `anyPopupOpen()` is checking (see `lib/popups.ts`). ⌘, toggles,
 * at overlay priority so it works from inside a text field, which is where the
 * caret will be if you are in the middle of typing a signature.
 *
 * Tab itself has to be taken back from the mail mode underneath, which binds it
 * to "sidebar or list" and, having no idea this surface exists, was swallowing
 * every Tab pressed inside it — the reason a dialog full of keyboard-navigable
 * controls could not be navigated by keyboard. The binding below wins on
 * priority and does nothing, deliberately with `preventDefault: false` so the
 * browser still moves focus. It is off while a popup is open, because the
 * registry listens in the capture phase and would otherwise take Tab away from
 * an open menu as well.
 */

/* -------------------------------------------------------------------------- */
/* Choices                                                                     */
/* -------------------------------------------------------------------------- */

interface Choice<T> {
  value: T;
  label: string;
}

const THEMES: Choice<Preferences["theme"]>[] = [
  { value: "system", label: "Match the system" },
  { value: "light", label: "Light" },
  { value: "dark", label: "Dark" },
];

const WEEK_STARTS: Choice<WeekStart>[] = [
  { value: 0, label: "Sunday" },
  { value: 1, label: "Monday" },
  { value: 6, label: "Saturday" },
];

/**
 * Long enough to notice a bulk archive and decide about it; short enough that
 * the status line is not permanent furniture. Six — the old hardcoded value —
 * is deliberately not on the list.
 */
const UNDO_WINDOWS: Choice<number>[] = [
  { value: 10, label: "10 seconds" },
  { value: 20, label: "20 seconds" },
  { value: 45, label: "45 seconds" },
  { value: 120, label: "2 minutes" },
  { value: 300, label: "5 minutes" },
];

const SEND_DELAYS: Choice<number>[] = [
  { value: 0, label: "Send immediately" },
  { value: 5, label: "5 seconds" },
  { value: 10, label: "10 seconds" },
  { value: 20, label: "20 seconds" },
  { value: 30, label: "30 seconds" },
];

const AGENT_BACKENDS: Choice<Preferences["agentBackend"]>[] = [
  { value: "auto", label: "Automatic" },
  { value: "claudeCli", label: "Claude Code" },
  { value: "anthropicApi", label: "Anthropic API" },
  { value: "command", label: "Custom command" },
];

const SYNC_INTERVALS: Choice<number>[] = [
  { value: 30, label: "Every 30 seconds" },
  { value: 60, label: "Every minute" },
  { value: 300, label: "Every 5 minutes" },
  { value: 900, label: "Every 15 minutes" },
  { value: 1800, label: "Every 30 minutes" },
  { value: 3600, label: "Every hour" },
];

/**
 * The clock labels for the two working-hour lists.
 *
 * Hour 24 is spelled "Midnight" rather than "12 AM", which is what the start of
 * the day is already called. Both are the same instant on a clock and opposite
 * ends of a band, and "9 AM to 12 AM" reads as a mistake — which it was, until
 * this said so.
 */
function hourLabel(hour: number): string {
  if (hour === 24) return "Midnight";
  const suffix = hour < 12 ? "AM" : "PM";
  const twelve = hour % 12 === 0 ? 12 : hour % 12;
  return `${twelve} ${suffix}`;
}

const DAY_STARTS: Choice<number>[] = Array.from({ length: 24 }, (_, hour) => ({
  value: hour,
  label: hourLabel(hour),
}));

const DAY_ENDS: Choice<number>[] = Array.from({ length: 24 }, (_, i) => ({
  value: i + 1,
  label: hourLabel(i + 1),
}));

/** The sentinel for "no default account", which is not a number. */
const NO_DEFAULT_ACCOUNT = "auto";

/* -------------------------------------------------------------------------- */
/* Sections                                                                    */
/* -------------------------------------------------------------------------- */

/**
 * The left-hand list, in the order it reads.
 *
 * Accounts first because it is the only way into the app: adding one used to be
 * a row at the bottom of the mail rail, which is navigation, and an account is
 * not a place to go. Agent last because it is the one thing here configured
 * once. The ids are also the panel's identity for the heading it is labelled by.
 */
const SECTIONS = [
  { id: "accounts", title: "Accounts" },
  { id: "appearance", title: "Appearance" },
  { id: "mail", title: "Mail" },
  { id: "calendar", title: "Calendar" },
  { id: "notifications", title: "Notifications" },
  { id: "sync", title: "Sync" },
  { id: "agent", title: "Agent" },
] as const;

type SectionId = (typeof SECTIONS)[number]["id"];

/* -------------------------------------------------------------------------- */
/* The surface                                                                 */
/* -------------------------------------------------------------------------- */

export function PreferencesDialog() {
  const { prefs, set } = usePreferencesStore();
  const { accounts, labels, actions, sync } = useMach();
  const [open, setOpen] = useState(false);
  const [section, setSection] = useState<SectionId>("accounts");
  // Which account is one keystroke from being deleted, if any. Held here rather
  // than in the section so that Escape — which is registered here — can take it
  // back before it starts closing surfaces.
  const [confirmRemove, setConfirmRemove] = useState<number | null>(null);
  const ids = useIds();
  const [notifications, setNotifications] = useState<NotificationStatus>(NO_NOTIFICATIONS);
  // The section the surface opens focused on — see `Overlay`'s `initialFocus`.
  const current = useRef<HTMLButtonElement>(null);

  // Read when the surface opens rather than once at mount: the answer can change
  // while the app is running — System Settings is a few clicks away — and this
  // is the only place that renders it.
  useEffect(() => {
    if (!open) return;
    let live = true;
    void loadNotificationStatus().then((status) => {
      if (live) setNotifications(status);
    });
    return () => {
      live = false;
    };
  }, [open]);

  useEffect(() => {
    const show = () => setOpen(true);
    window.addEventListener(PREFERENCES_EVENT, show);
    return () => window.removeEventListener(PREFERENCES_EVENT, show);
  }, []);

  // The ⌘K entries. Registered from here rather than at module scope so the
  // chain is only extended while the surface that answers it is mounted.
  useEffect(() => registerResolver(preferencesResolver), []);

  useKeyBindings([
    {
      /*
       * ⌘, — the macOS preferences key, and unclaimed by anything else in the
       * app (`keymap.conflicts()` agrees; nothing else binds a comma, with or
       * without a modifier). `allowInInput` because the caret is in a text
       * field whenever the composer is open, and a global shortcut that stops
       * working while you are typing is not a global shortcut.
       */
      keys: "mod+,",
      group: "Global",
      description: "Preferences",
      allowInInput: true,
      priority: 200,
      handler: () => setOpen((was) => !was),
    },
    {
      keys: "escape",
      priority: 125,
      allowInInput: true,
      // A select menu is open: the key is its, not ours. Without this the first
      // Escape out of a menu would also close the surface.
      when: () => open && !anyPopupOpen(),
      handler: () => {
        // A pending "are you sure" is the innermost thing on screen, so it is
        // the first thing Escape takes back. Only then does the key mean close.
        if (confirmRemove !== null) setConfirmRemove(null);
        else setOpen(false);
      },
    },
    // Tab belongs to the browser while this is open — see the note at the top.
    {
      keys: "tab",
      priority: 124,
      preventDefault: false,
      when: () => open && !anyPopupOpen(),
      handler: () => {},
    },
    {
      keys: "shift+tab",
      priority: 124,
      preventDefault: false,
      when: () => open && !anyPopupOpen(),
      handler: () => {},
    },
  ]);

  if (!open) return null;

  return (
    <Overlay
      open
      onClose={() => setOpen(false)}
      align="center"
      fullScreen
      initialFocus={current}
      labelledBy="preferences-title"
    >
      {/*
       * `pl-[5.5rem]`, matching `chrome/TitleBar.tsx`, and not a style choice.
       *
       * The window is `titleBarStyle: "Overlay"` with `hiddenTitle`, so macOS
       * draws the close/minimise/zoom buttons *over* the top-left of our own
       * content. A full-screen surface that starts its header at the left edge
       * puts its title underneath them — which is exactly what happened here,
       * and could not be seen in the fixture browser, because a browser tab has
       * no traffic lights. Anything that fills the window owes them this inset.
       */}
      <header className="flex h-11 shrink-0 items-center gap-3 border-b border-border pl-[5.5rem] pr-4">
        <h2 id="preferences-title" className="text-reading font-medium text-foreground">
          Preferences
        </h2>
        {/* An `Esc close` chip used to sit immediately left of this button,
            which does the same thing and says so in a word. */}
        <Button variant="subtle" className="ml-auto" onClick={() => setOpen(false)}>
          Done
        </Button>
      </header>

      <div className="flex min-h-0 flex-1">
        <nav
          aria-label="Sections"
          className="w-rail shrink-0 overflow-y-auto border-r border-border p-2"
        >
          {SECTIONS.map((entry) => (
            <button
              key={entry.id}
              type="button"
              ref={entry.id === section ? current : undefined}
              aria-current={entry.id === section}
              onClick={() => setSection(entry.id)}
              className={cn(
                "flex w-full cursor-default items-center rounded-[var(--radius)] px-2 py-1 text-left text-body",
                entry.id === section
                  ? "bg-row-selected text-foreground"
                  : "text-muted-foreground hover:bg-row-hover",
              )}
            >
              {entry.title}
            </button>
          ))}
        </nav>

        <div
          role="region"
          aria-labelledby={`${ids.section}-${section}`}
          className="min-h-0 flex-1 overflow-y-auto px-8 py-6"
        >
          {/* Left-aligned against the section list rather than centred in the
              window: a form floating in the middle of 1400px reads as a modal
              somebody stretched, and the eye has to travel from the section it
              just clicked to find it. */}
          <div className="flex w-full max-w-[40rem] flex-col gap-4">
            <h3
              id={`${ids.section}-${section}`}
              className="text-reading font-medium text-foreground"
            >
              {SECTIONS.find((entry) => entry.id === section)?.title}
            </h3>

            <FieldGroup>
              {section === "accounts" && (
                <Accounts
                  accounts={accounts}
                  needsAuthorization={sync?.needsReauthorization ?? []}
                  confirming={confirmRemove}
                  onConfirm={setConfirmRemove}
                  onRemoved={() => actions.reload()}
                  onAdd={() => {
                    // The authorization dialog is an overlay of its own, and two
                    // focus traps on screen fight over the keyboard — so this
                    // one steps aside rather than stacking. Signing in is a
                    // whole task with a browser in the middle of it; coming back
                    // to the mail is the right place to land.
                    setOpen(false);
                    actions.setAddAccount(true);
                  }}
                />
              )}

              {section === "appearance" && (
                <Field orientation="row">
                  <FieldLabel htmlFor={ids.theme}>Theme</FieldLabel>
                  <Choose
                    id={ids.theme}
                    label="Theme"
                    choices={THEMES}
                    value={prefs.theme}
                    onChange={(value) => set("theme", value)}
                  />
                </Field>
              )}

              {section === "mail" && (
                <>
                  <Field orientation="row">
                    <FieldLabel htmlFor={ids.account}>Write from</FieldLabel>
                    <Select
                      items={accountItems(accounts)}
                      value={
                        prefs.defaultAccountId === null
                          ? NO_DEFAULT_ACCOUNT
                          : String(prefs.defaultAccountId)
                      }
                      onValueChange={(value) => {
                        if (value === null) return;
                        set(
                          "defaultAccountId",
                          value === NO_DEFAULT_ACCOUNT ? null : Number(value),
                        );
                      }}
                    >
                      <SelectTrigger
                        id={ids.account}
                        aria-label="Default account for new messages"
                      >
                        <SelectValue />
                      </SelectTrigger>
                      <SelectContent>
                        {accountItems(accounts).map((item) => {
                          const account = accounts.find((a) => String(a.id) === item.value);
                          return (
                            <SelectItem key={item.value} value={item.value}>
                              <span className="flex min-w-0 items-center gap-1.5">
                                <span
                                  className={cn(
                                    "h-2 w-2 shrink-0 rounded-[2px]",
                                    account ? ACCOUNT_BG[account.colorIndex] : "bg-border",
                                  )}
                                />
                                <span className="min-w-0 truncate">{item.label}</span>
                              </span>
                            </SelectItem>
                          );
                        })}
                      </SelectContent>
                    </Select>
                    {/* Only consulted when there is nothing better to go on: a
                        reply already knows its account, and a list filtered to
                        one account has said which. */}
                    <FieldDescription>Used when the account can't be inferred</FieldDescription>
                  </Field>

                  <Field orientation="row">
                    <FieldLabel htmlFor={ids.undo}>Undo for</FieldLabel>
                    <Choose
                      id={ids.undo}
                      label="How long undo stays offered"
                      choices={UNDO_WINDOWS}
                      value={prefs.undoWindowSeconds}
                      onChange={(value) => set("undoWindowSeconds", value)}
                    />
                    {/* One keystroke can archive fifty conversations, which is
                        what this window is really sized against — long enough
                        to notice that is what happened. */}
                    <FieldDescription>How long ⌘Z stays available</FieldDescription>
                  </Field>

                  <Field orientation="row">
                    <FieldLabel htmlFor={ids.sendDelay}>Send after</FieldLabel>
                    <Choose
                      id={ids.sendDelay}
                      label="Send delay"
                      choices={SEND_DELAYS}
                      value={prefs.sendDelaySeconds}
                      onChange={(value) => set("sendDelaySeconds", value)}
                    />
                    {/* The message sits in the outbox for this long, recallable
                        with ⌘Z, before it leaves. */}
                    <FieldDescription>Delay before a message leaves</FieldDescription>
                  </Field>

                  <Signatures
                    accounts={accounts}
                    signatures={prefs.signatures}
                    onChange={(next) => set("signatures", next)}
                  />

                  {/* The one surface here that is not a preference: a filter
                      lives in Gmail. It is here because this is where somebody
                      looking for it would look, and because the alternative
                      was telling them to open Gmail's web settings. */}
                  <Filters
                    accounts={accounts}
                    labels={labels}
                    missingScope={sync?.missingScope ?? []}
                  />
                </>
              )}

              {section === "calendar" && (
                <>
                  <Field orientation="row">
                    <FieldLabel htmlFor={ids.weekStart}>Week starts</FieldLabel>
                    <Choose
                      id={ids.weekStart}
                      label="Week starts on"
                      choices={WEEK_STARTS}
                      value={prefs.weekStartsOn}
                      onChange={(value) => set("weekStartsOn", value)}
                    />
                  </Field>

                  <Field orientation="row">
                    <FieldLabel htmlFor={ids.workStart}>Working</FieldLabel>
                    <div className="flex min-w-0 items-center gap-2">
                      <Choose
                        id={ids.workStart}
                        label="Working hours start"
                        choices={DAY_STARTS}
                        value={prefs.workingHours.start}
                        onChange={(start) =>
                          set("workingHours", {
                            start,
                            // The band has to stay a band: dragging the start
                            // past the end pushes the end rather than inverting
                            // the pair.
                            end: Math.max(prefs.workingHours.end, start + 1),
                          })
                        }
                      />
                      <span className="shrink-0 text-micro text-faint-foreground">to</span>
                      <Choose
                        id={ids.workEnd}
                        label="Working hours end"
                        choices={DAY_ENDS}
                        value={prefs.workingHours.end}
                        onChange={(end) =>
                          set("workingHours", {
                            start: Math.min(prefs.workingHours.start, end - 1),
                            end,
                          })
                        }
                      />
                    </div>
                    {/* The day grid also opens scrolled to this band, which is
                        the half of it nobody has to be told. */}
                    <FieldDescription>The day grid shades the hours outside these</FieldDescription>
                  </Field>
                </>
              )}

              {section === "notifications" && (
                <>
                  <Field orientation="row">
                    <FieldLabel htmlFor={ids.notifications}>New mail</FieldLabel>
                    {/*
                      The rule behind the switch, which is deliberately narrow
                      and deliberately not written on screen: unread mail that
                      reaches an inbox, from somebody other than you.
                      Promotions, Social, Updates and Forums stay quiet unless
                      the message continues a conversation you have written to,
                      and several arriving together are one notification. See
                      `notify::rule` — this is why "on" is a safe default in a
                      mailbox with 61,000 messages in it.
                    */}
                    <Toggle
                      id={ids.notifications}
                      label="Show a notification when mail arrives"
                      checked={prefs.notificationsEnabled}
                      onChange={(on) => {
                        set("notificationsEnabled", on);
                        // The permission prompt belongs to this moment and no
                        // other — the user has just said they want this, so
                        // macOS asking why is a question with an obvious answer.
                        if (on) void requestNotificationPermission().then(setNotifications);
                      }}
                    />
                    {/* Kept, because it is the one thing here the user has to
                        act on and would otherwise never learn. */}
                    {prefs.notificationsEnabled && notifications.permission === "denied" && (
                      <FieldDescription className="text-danger">
                        macOS is not delivering Mach's notifications. Turn them on for Mach in
                        System Settings → Notifications.
                      </FieldDescription>
                    )}
                  </Field>

                  <NotifyingAccounts
                    accounts={accounts}
                    prefs={prefs}
                    onChange={(next) => set("notificationAccounts", next)}
                  />

                  {/* Counts unread conversations still in an inbox, across every
                      account, so archiving everything takes it to nothing —
                      which is the point of the gesture. */}
                  <Field orientation="row">
                    <FieldLabel htmlFor={ids.badge}>Dock</FieldLabel>
                    <Toggle
                      id={ids.badge}
                      label="Show the unread count on the Dock icon"
                      checked={prefs.badgeEnabled}
                      onChange={(on) => set("badgeEnabled", on)}
                    />
                  </Field>
                </>
              )}

              {section === "sync" && (
                <Field orientation="row">
                  <FieldLabel htmlFor={ids.sync}>Check mail</FieldLabel>
                  {/* Incoming mail also arrives on Gmail's own push; this is the
                      floor under it, and it takes effect on the next pass. */}
                  <Choose
                    id={ids.sync}
                    label="Sync interval"
                    choices={SYNC_INTERVALS}
                    value={prefs.syncIntervalSeconds}
                    onChange={(value) => set("syncIntervalSeconds", value)}
                  />
                </Field>
              )}

              {section === "agent" && <AgentSettings prefs={prefs} set={set} ids={ids} />}
            </FieldGroup>
          </div>
        </div>
      </div>
    </Overlay>
  );
}

/* -------------------------------------------------------------------------- */
/* Parts                                                                       */
/* -------------------------------------------------------------------------- */

/**
 * The connected accounts: which they are, what colour each one is, and the two
 * things you can do to the list.
 *
 * This is where adding an account lives now. It was a row at the bottom of the
 * mail rail, next to the mailboxes — which made it navigation, and made a
 * once-ever action sit permanently beside the ones performed every minute.
 *
 * The colour is the point of showing the list at all. Mach assigns each account
 * a hue and then uses it everywhere — the bar down the left of every thread row,
 * the calendar, the account picker — and until now there was nowhere that said
 * which was which.
 *
 * # Removal
 *
 * Two steps, and the second one gets a real sentence rather than a label. The
 * house rule is that help text names an effect and stops; this is the exception
 * the rule allows, because the effect is destructive and cannot be inferred
 * from the word "Remove". `remove_account` deletes the account row and lets the
 * cascades take its threads, messages and events with it, then drops the
 * Keychain credential — and it does none of that to Google, which is the half
 * people are actually frightened of.
 *
 * The confirmation is inline rather than a second dialog, because a modal over
 * a modal is two focus traps arguing, and because the sentence belongs beside
 * the row it is about.
 */
function Accounts({
  accounts,
  needsAuthorization,
  confirming,
  onConfirm,
  onRemoved,
  onAdd,
}: {
  accounts: readonly Account[];
  /** Emails whose refresh token is gone from the Keychain. */
  needsAuthorization: readonly string[];
  confirming: number | null;
  onConfirm: (accountId: number | null) => void;
  onRemoved: () => void;
  onAdd: () => void;
}) {
  const [failed, setFailed] = useState<string | null>(null);

  const remove = (account: Account) => {
    onConfirm(null);
    setFailed(null);
    void getDataSource()
      .removeAccount(account.id)
      // Every list in the window loses rows, so the whole read model reloads.
      .then(onRemoved)
      .catch((caught: unknown) => {
        // Silent failure is the thing this project has paid most for: an
        // account that is still there after you removed it has to say why.
        setFailed(caught instanceof Error ? caught.message : String(caught));
      });
  };

  return (
    <Field orientation="row">
      <FieldLabel>Connected</FieldLabel>
      <FieldContent>
        {accounts.map((account) => (
          <div key={account.id} className="flex min-w-0 flex-col gap-1.5">
            <div className="flex min-w-0 items-center gap-2">
              {/* The same stripe the thread list draws down its left edge. */}
              <span
                className={cn(
                  "h-4 w-[3px] shrink-0 rounded-full",
                  ACCOUNT_BG[account.colorIndex],
                )}
              />
              <span className="min-w-0 flex-1 truncate text-body text-foreground">
                {account.email}
              </span>
              {needsAuthorization.includes(account.email) && (
                <span className="shrink-0 text-micro text-danger">Needs authorization</span>
              )}
              <Button
                size="sm"
                variant="ghost"
                aria-label={`Remove ${account.email}`}
                onClick={() => onConfirm(confirming === account.id ? null : account.id)}
              >
                Remove
              </Button>
            </div>

            {confirming === account.id && (
              <div className="flex flex-col gap-2 rounded-[var(--radius)] border border-border bg-surface-raised p-2">
                <p className="text-micro leading-snug text-muted-foreground">
                  Removing {account.email} deletes the mail and calendar Mach has stored for it
                  on this Mac and forgets its authorization. Nothing in Gmail or Google Calendar
                  changes.
                </p>
                <div className="flex items-center gap-2">
                  <Button size="sm" variant="danger" onClick={() => remove(account)}>
                    Remove account
                  </Button>
                  <Button size="sm" variant="subtle" onClick={() => onConfirm(null)}>
                    Cancel
                  </Button>
                </div>
              </div>
            )}
          </div>
        ))}

        {failed && <FieldDescription className="text-danger">{failed}</FieldDescription>}

        <div>
          <Button variant="subtle" onClick={onAdd}>
            Add account
          </Button>
        </div>
      </FieldContent>
    </Field>
  );
}

/** The account picker's items, with "no opinion" first. */
function accountItems(accounts: readonly Account[]): { value: string; label: string }[] {
  return [
    { value: NO_DEFAULT_ACCOUNT, label: "First account" },
    ...accounts.map((account) => ({ value: String(account.id), label: account.email })),
  ];
}

/**
 * A `Select` over a list of values that are not necessarily strings.
 *
 * Base UI's value is a string, and every numeric preference here would
 * otherwise repeat the same `String(value)` / `Number(value)` pair at both ends
 * — which is exactly where an off-by-one type bug lives. The index is the wire
 * value, so the round trip cannot lose a type.
 */
function Choose<T>({
  id,
  label,
  choices,
  value,
  onChange,
}: {
  id: string;
  label: string;
  choices: Choice<T>[];
  value: T;
  onChange: (value: T) => void;
}) {
  const items = choices.map((choice, index) => ({
    value: String(index),
    label: choice.label,
  }));
  const selected = Math.max(
    choices.findIndex((choice) => choice.value === value),
    0,
  );

  return (
    <Select
      items={items}
      value={String(selected)}
      onValueChange={(next) => {
        if (next === null) return;
        const choice = choices[Number(next)];
        if (choice) onChange(choice.value);
      }}
    >
      <SelectTrigger id={id} aria-label={label}>
        <SelectValue />
      </SelectTrigger>
      <SelectContent>
        {items.map((item) => (
          <SelectItem key={item.value} value={item.value}>
            {item.label}
          </SelectItem>
        ))}
      </SelectContent>
    </Select>
  );
}

/**
 * A checkbox with its own label, for the settings that are genuinely binary.
 *
 * Everything else on this surface is a `Select`, deliberately — but "on or off"
 * through a two-item menu is a click and a read where a switch is neither, and
 * a checkbox is the control every other Mac settings window uses for exactly
 * this. The label is part of the hit area, which is the half people expect and
 * a bare `<Checkbox>` beside a `<span>` does not give you.
 */
function Toggle({
  id,
  label,
  checked,
  onChange,
}: {
  id: string;
  label: string;
  checked: boolean;
  onChange: (checked: boolean) => void;
}) {
  return (
    <label
      htmlFor={id}
      className="flex min-w-0 cursor-default items-center gap-2 py-[0.3125rem] text-body text-foreground"
    >
      <Checkbox id={id} checked={checked} onCheckedChange={onChange} />
      <span className="min-w-0 truncate">{label}</span>
    </label>
  );
}

/**
 * One line per account, each of which can be told to stay quiet.
 *
 * The same shape as {@link Signatures} and for the same reason: with at most
 * five mailboxes, showing them all is shorter to use than a picker and makes
 * the thing people get wrong — the work account being the muted one — visible
 * without a click.
 */
function NotifyingAccounts({
  accounts,
  prefs,
  onChange,
}: {
  accounts: readonly Account[];
  prefs: Preferences;
  onChange: (next: Record<string, boolean>) => void;
}) {
  const prefix = useId();
  return (
    <Field orientation="row">
      <FieldLabel>Accounts</FieldLabel>
      <FieldContent>
        {accounts.length === 0 ? (
          <FieldDescription>Add an account and it appears here.</FieldDescription>
        ) : (
          accounts.map((account) => (
            <label
              key={account.id}
              htmlFor={`${prefix}-${account.id}`}
              className="flex min-w-0 cursor-default items-center gap-2 text-body text-foreground"
            >
              <Checkbox
                id={`${prefix}-${account.id}`}
                disabled={!prefs.notificationsEnabled}
                checked={notifiesAccount(prefs, account.id)}
                onCheckedChange={(on) =>
                  onChange(withAccountNotifying(prefs, account.id, on))
                }
              />
              <span
                className={cn("h-2 w-2 shrink-0 rounded-[2px]", ACCOUNT_BG[account.colorIndex])}
              />
              <span className="min-w-0 truncate">{account.email}</span>
            </label>
          ))
        )}
      </FieldContent>
    </Field>
  );
}

/**
 * One signature per account, each in its own box.
 *
 * A single box with an account picker above it would be half the height and
 * would hide the thing people get wrong — that the work address is still
 * signing off as the personal one. With at most five accounts, showing all of
 * them is both shorter to use and impossible to misread.
 *
 * The signature is appended to a new message and to a reply, under the RFC 3676
 * "-- " line every other client uses to find it; the placeholder says where it
 * lands, which is as much as anyone needs on screen.
 */
function Signatures({
  accounts,
  signatures,
  onChange,
}: {
  accounts: readonly Account[];
  signatures: Record<string, string>;
  onChange: (next: Record<string, string>) => void;
}) {
  return (
    <Field orientation="row">
      <FieldLabel>Signature</FieldLabel>
      <FieldContent>
        {accounts.length === 0 ? (
          <FieldDescription>Add an account and its signature appears here.</FieldDescription>
        ) : (
          accounts.map((account) => (
            <div key={account.id} className="flex min-w-0 flex-col gap-1">
              <span className="flex items-center gap-1.5 text-micro text-faint-foreground">
                <span
                  className={cn("h-2 w-2 shrink-0 rounded-[2px]", ACCOUNT_BG[account.colorIndex])}
                />
                <span className="min-w-0 truncate">{account.email}</span>
              </span>
              <Textarea
                autoSize
                rows={2}
                maxRows={6}
                spellCheck={false}
                aria-label={`Signature for ${account.email}`}
                placeholder="Plain text, appended under a “-- ” line"
                value={signatures[String(account.id)] ?? ""}
                onChange={(event) => {
                  const next = { ...signatures };
                  const text = event.target.value;
                  if (text) next[String(account.id)] = text;
                  else delete next[String(account.id)];
                  onChange(next);
                }}
              />
            </div>
          ))
        )}
      </FieldContent>
    </Field>
  );
}

/**
 * Which brain answers ⌘K, and how to reach it.
 *
 * Self-contained on purpose: one section, one component here, nothing threaded
 * through the rest of the surface.
 *
 * The status line under the picker is the load-bearing part. "Not configured"
 * without an instruction was the original complaint, so on `Automatic` this
 * says what was *detected* — and when nothing was, it renders the sentence Rust
 * produced, which names both remedies. It is read from the same resolution
 * `agent_start` performs, so this cannot claim something the next ⌘K would
 * contradict.
 */
function AgentSettings({
  prefs,
  set,
  ids,
}: {
  prefs: Preferences;
  set: <K extends keyof Preferences>(key: K, value: Preferences[K]) => void;
  ids: ReturnType<typeof useIds>;
}) {
  const [status, setStatus] = useState<AgentBackendStatus>(UNKNOWN_BACKEND);

  // Re-read whenever the choice changes: switching to Claude Code on a machine
  // without one has to say so immediately, not at the next ⌘K.
  useEffect(() => {
    let live = true;
    void loadBackendStatus().then((next) => {
      if (live) setStatus(next);
    });
    return () => {
      live = false;
    };
  }, [prefs.agentBackend, prefs.agentCommand]);

  return (
    <>
      <Field orientation="row">
        <FieldLabel htmlFor={ids.agentBackend}>Runs on</FieldLabel>
        <Choose
          id={ids.agentBackend}
          label="Agent backend"
          choices={AGENT_BACKENDS}
          value={prefs.agentBackend}
          onChange={(value) => set("agentBackend", value)}
        />
        <FieldDescription>
          {status.message ? (
            <span className="text-danger">{status.message}</span>
          ) : (
            [status.label, status.claudePath].filter(Boolean).join(" · ")
          )}
        </FieldDescription>
      </Field>

      <Field orientation="row">
        <FieldLabel htmlFor={ids.agentModel}>Model</FieldLabel>
        <Input
          id={ids.agentModel}
          spellCheck={false}
          placeholder="Default"
          value={prefs.agentModel}
          onChange={(event) => set("agentModel", event.target.value)}
        />
      </Field>

      {prefs.agentBackend === "command" && (
        <Field orientation="row">
          <FieldLabel htmlFor={ids.agentCommand}>Command</FieldLabel>
          <Input
            id={ids.agentCommand}
            spellCheck={false}
            placeholder="/usr/local/bin/my-agent --flag"
            value={prefs.agentCommand}
            onChange={(event) => set("agentCommand", event.target.value)}
          />
          <FieldDescription>Contract: docs/agent-backends.md</FieldDescription>
        </Field>
      )}
    </>
  );
}

/** Stable ids so every label points at its own control. */
function useIds() {
  const prefix = useId();
  return {
    section: `${prefix}-section`,
    theme: `${prefix}-theme`,
    account: `${prefix}-account`,
    undo: `${prefix}-undo`,
    sendDelay: `${prefix}-send-delay`,
    weekStart: `${prefix}-week-start`,
    workStart: `${prefix}-work-start`,
    workEnd: `${prefix}-work-end`,
    sync: `${prefix}-sync`,
    notifications: `${prefix}-notifications`,
    badge: `${prefix}-badge`,
    agentBackend: `${prefix}-agent-backend`,
    agentModel: `${prefix}-agent-model`,
    agentCommand: `${prefix}-agent-command`,
  };
}
