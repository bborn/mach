import { useEffect, useId, useState } from "react";
import type { Account } from "@/types";
import { useKeyBindings } from "@/hooks/useKeymap";
import { useMach } from "@/hooks/useMach";
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
import { Kbd } from "@/components/ui/kbd";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Textarea } from "@/components/ui/textarea";
import { PREFERENCES_EVENT, preferencesResolver } from "./palette";
import { usePreferencesStore } from "./PreferencesProvider";

/**
 * The preferences dialog: ⌘, and nothing else on screen.
 *
 * # It is a dialog, not a pane
 *
 * Same call the event editor made, for the same reason. A settings *pane* has to
 * live somewhere — a route, a rail item, a third column — and every one of those
 * costs a permanent piece of the window to serve a surface opened once a month.
 * A modal costs nothing when it is closed.
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
 * There is no Save button. Every control writes on change, which is what a
 * preferences surface should do and what every native one does — a form that
 * can be abandoned half-applied is a form that needs a confirmation dialog, and
 * this is a list of independent switches, not a transaction. "Done" closes; it
 * does not commit.
 *
 * # Keyboard
 *
 * Tab walks the controls in reading order because they are in the DOM in
 * reading order; `Overlay` traps focus and restores it. Escape closes, unless a
 * select menu is open — then the key belongs to that menu, which is what
 * `anyPopupOpen()` is checking (see `lib/popups.ts`). ⌘, toggles, at overlay
 * priority so it works from inside a text field, which is where the caret will
 * be if you are in the middle of typing a signature.
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

const DENSITIES: Choice<Preferences["density"]>[] = [
  { value: "comfortable", label: "Comfortable" },
  { value: "compact", label: "Compact" },
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
/* The dialog                                                                  */
/* -------------------------------------------------------------------------- */

export function PreferencesDialog() {
  const { prefs, set, loaded } = usePreferencesStore();
  const { accounts } = useMach();
  const [open, setOpen] = useState(false);
  const ids = useIds();
  const [notifications, setNotifications] = useState<NotificationStatus>(NO_NOTIFICATIONS);

  // Read when the dialog opens rather than once at mount: the answer can change
  // while the app is running — System Settings is a few clicks away — and this
  // is the only surface that renders it.
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
      // Escape out of a menu would also close the dialog.
      when: () => open && !anyPopupOpen(),
      handler: () => setOpen(false),
    },
  ]);

  if (!open) return null;

  const accountItems = [
    { value: NO_DEFAULT_ACCOUNT, label: "First account" },
    ...accounts.map((account) => ({ value: String(account.id), label: account.email })),
  ];

  return (
    <Overlay
      open
      onClose={() => setOpen(false)}
      align="center"
      labelledBy="preferences-title"
      className="max-w-[34rem]"
    >
      <header className="flex shrink-0 items-baseline justify-between border-b border-border px-4 py-3">
        <h2 id="preferences-title" className="text-body font-medium text-foreground">
          Preferences
        </h2>
        <span className="text-micro text-faint-foreground">
          {loaded ? "Saved as you change them" : "Loading…"}
        </span>
      </header>

      <div className="min-h-0 flex-1 overflow-y-auto px-4 py-3">
        <FieldGroup>
          <Section title="Appearance" />

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

          <Field orientation="row">
            <FieldLabel htmlFor={ids.density}>Density</FieldLabel>
            <Choose
              id={ids.density}
              label="List density"
              choices={DENSITIES}
              value={prefs.density}
              onChange={(value) => set("density", value)}
            />
            <FieldDescription>
              Compact tightens the thread row and the type scale with it — a few more
              conversations on screen, at the cost of some air.
            </FieldDescription>
          </Field>

          <Section title="Mail" />

          <Field orientation="row">
            <FieldLabel htmlFor={ids.account}>Write from</FieldLabel>
            <Select
              items={accountItems}
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
              <SelectTrigger id={ids.account} aria-label="Default account for new messages">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {accountItems.map((item) => {
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
            <FieldDescription>
              Only used when there is nothing to infer from — a reply already knows its account,
              and so does a list filtered to one.
            </FieldDescription>
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
            <FieldDescription>
              How long ⌘Z stays offered after a command. One keystroke can archive fifty
              conversations; this is how long you have to notice.
            </FieldDescription>
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
            <FieldDescription>
              A sent message waits this long in the outbox, recallable with ⌘Z, before it leaves.
            </FieldDescription>
          </Field>

          <Signatures
            accounts={accounts}
            signatures={prefs.signatures}
            onChange={(next) => set("signatures", next)}
          />

          <Section title="Calendar" />

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
                    // The band has to stay a band: dragging the start past the
                    // end pushes the end rather than inverting the pair.
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
            <FieldDescription>
              The day grid shades the hours outside these and opens on them.
            </FieldDescription>
          </Field>

          <Section title="Notifications" />

          <Field orientation="row">
            <FieldLabel htmlFor={ids.notifications}>New mail</FieldLabel>
            <Toggle
              id={ids.notifications}
              label="Show a notification when mail arrives"
              checked={prefs.notificationsEnabled}
              onChange={(on) => {
                set("notificationsEnabled", on);
                // The permission prompt belongs to this moment and no other —
                // the user has just said they want this, so macOS asking why is
                // a question with an obvious answer.
                if (on) void requestNotificationPermission().then(setNotifications);
              }}
            />
            <FieldDescription>
              Unread mail that reaches the inbox, from somebody other than you. Promotions,
              Social, Updates and Forums stay quiet unless the message continues a conversation
              you have written to. Several arriving together are one notification.
              {prefs.notificationsEnabled && notifications.permission === "denied" && (
                <span className="mt-1 block text-danger">
                  macOS is not delivering Mach's notifications. Turn them on for Mach in System
                  Settings → Notifications.
                </span>
              )}
            </FieldDescription>
          </Field>

          <NotifyingAccounts
            accounts={accounts}
            prefs={prefs}
            onChange={(next) => set("notificationAccounts", next)}
          />

          <Field orientation="row">
            <FieldLabel htmlFor={ids.badge}>Dock</FieldLabel>
            <Toggle
              id={ids.badge}
              label="Show the unread count on the Dock icon"
              checked={prefs.badgeEnabled}
              onChange={(on) => set("badgeEnabled", on)}
            />
            <FieldDescription>
              Counts unread conversations still in an inbox, across every account — so archiving
              everything takes it to nothing, which is the point of the gesture.
            </FieldDescription>
          </Field>

          <Section title="Sync" />

          <Field orientation="row">
            <FieldLabel htmlFor={ids.sync}>Check mail</FieldLabel>
            <Choose
              id={ids.sync}
              label="Sync interval"
              choices={SYNC_INTERVALS}
              value={prefs.syncIntervalSeconds}
              onChange={(value) => set("syncIntervalSeconds", value)}
            />
            <FieldDescription>
              Incoming mail also arrives on Gmail's own push; this is the floor under it. Takes
              effect on the next pass.
            </FieldDescription>
          </Field>

          <AgentSettings prefs={prefs} set={set} ids={ids} />
        </FieldGroup>
      </div>

      <footer className="flex h-9 shrink-0 items-center gap-2 border-t border-border px-4">
        <span className="flex items-center gap-1.5">
          <Kbd keys="escape" />
          <span className="text-micro text-faint-foreground">close</span>
        </span>
        <Button variant="subtle" className="ml-auto" onClick={() => setOpen(false)}>
          Done
        </Button>
      </footer>
    </Overlay>
  );
}

/* -------------------------------------------------------------------------- */
/* Parts                                                                       */
/* -------------------------------------------------------------------------- */

function Section({ title }: { title: string }) {
  return (
    <h3 className="mt-2 border-b border-border pb-1 text-micro font-medium uppercase tracking-[0.06em] text-faint-foreground first:mt-0">
      {title}
    </h3>
  );
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
      <FieldDescription>
        Appended to a message you start and to a reply, under the standard “-- ” line every
        other client uses to find it.
      </FieldDescription>
    </Field>
  );
}

/**
 * Which brain answers ⌘K, and how to reach it.
 *
 * Self-contained on purpose: one block in the field group, one component here,
 * nothing threaded through the rest of the dialog.
 *
 * The status line under the picker is the load-bearing part. "Not configured"
 * without an instruction was the original complaint, so on `Automatic` this
 * says what was *detected* — and when nothing was, it renders the sentence Rust
 * produced, which names both remedies. It is read from the same resolution
 * `agent_start` performs, so the dialog cannot claim something the next ⌘K
 * would contradict.
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
      <Section title="Agent" />

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
    theme: `${prefix}-theme`,
    density: `${prefix}-density`,
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
