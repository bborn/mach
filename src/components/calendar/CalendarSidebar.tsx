import { useEffect, useMemo, useRef, useState } from "react";
import { ChevronDown, ChevronRight, X } from "lucide-react";
import type { Account, Calendar, CalendarId, AccountId } from "@/types";
import { useUiSession } from "@/components/prefs/PreferencesProvider";
import { Checkbox } from "@/components/ui/checkbox";
import { Label } from "@/components/ui/label";
import { calendarFill, calendarInk, type CalendarColor } from "@/lib/calendar-palette";
import { sameSolo, type Solo } from "@/lib/calendar-solo";
import { cn } from "@/lib/utils";
import { formatBinding } from "@/lib/keymap";
import type { CalendarVisibility } from "@/lib/prefs";

export interface CalendarSettings {
  mergeDuplicates: boolean;
  showDeclined: boolean;
  showWeekends: boolean;
}

interface SidebarProps {
  accounts: Account[];
  calendars: Calendar[];
  hidden: CalendarId[];
  colorFor: (id: CalendarId) => CalendarColor;
  dark: boolean;
  /** What is soloed right now, account or calendar. See `lib/calendar-solo`. */
  solo: Solo | null;
  onToggle: (id: CalendarId) => void;
  /**
   * "Solo this." Whether that starts, moves or clears the solo is `nextSolo`'s
   * decision, in the shell, so the rail and the keyboard cannot disagree.
   */
  onSolo: (target: Solo) => void;
  settings: CalendarSettings;
  onSettings: (next: CalendarSettings) => void;
}

const NO_DECISIONS: Record<string, CalendarVisibility> = {};

/**
 * Calendars, grouped by account (§7).
 *
 * Colour says *which calendar*; the sidebar says *which account*. That split is
 * the only way five accounts stay legible — within one account you still need
 * work and personal to look different, so account cannot own the hue.
 *
 * # Two ideas, because a rail has two jobs
 *
 * A row can be off (`hidden`) or gone (`unlisted`), and collapsing those into
 * one control gets one of the two jobs wrong whichever way you collapse it.
 *
 * Taking the tick off Family to plan a work week is a thing people do several
 * times a day and undo a minute later; the row has to stay exactly where it is,
 * because the next thing you do is put the tick back. If unticking also removed
 * the row, every momentary hide would reflow the rail and leave you hunting for
 * the calendar you turned off ten seconds ago.
 *
 * Being finished with a calendar is the other job, and it is done once. Eleven
 * rows under one account, of which "Earnest Capital Calendar (Subscription
 * Expired)" is dead and three are second copies of the same holidays feed, is
 * not a list you want to keep reading past for the rest of the year. That one
 * wants the row gone.
 *
 * Reversible without leaving the app, both ways: an unlisted calendar sits in
 * the "Hidden from list" disclosure at the bottom of the rail, one press from
 * coming back, and `v <digit>` still addresses it — asking for its events is
 * taken as asking for it back, so the row returns with them.
 *
 * # A local decision outranks Google's `selected`
 *
 * `selected` is a real preference the user set in Google, and ignoring it is why
 * an account with a dozen subscribed calendars used to open as a wall of blocks
 * the user had already turned off somewhere else. So it is still adopted — but
 * exactly once per calendar, ever, and the record of having adopted it is the
 * *presence* of an entry in the persisted map. See `UiSession.calendarVisibility`.
 *
 * That "ever" is the whole of the fix. The adoption used to be a `useRef`, which
 * is once per window, and once per window is indistinguishable from once per
 * calendar only while nothing is written down. The moment hides persisted, a ref
 * would have re-hidden — on every launch, silently — any calendar Google still
 * thinks is deselected and the user had turned back on here. A sync that reports
 * `selected: true` cannot undo a local hide either, for the same reason: the
 * entry exists, so nothing reads `selected` again.
 */
export function CalendarSidebar({
  accounts,
  calendars,
  hidden,
  colorFor,
  dark,
  solo,
  onToggle,
  onSolo,
  settings,
  onSettings,
}: SidebarProps) {
  /*
   * Which account groups are folded up — remembered across launches.
   *
   * This was `useState([])`, so a five-account sidebar that had been tidied
   * down to one open group came back fully expanded every morning. It is
   * session state, not a setting: nobody would look for it in ⌘,, and the app
   * simply ought to be where you left it.
   */
  const { session, remember, loaded } = useUiSession();
  const collapsed = session.collapsedCalendarAccounts ?? [];
  const setCollapsed = (next: AccountId[]) =>
    remember({ collapsedCalendarAccounts: next });

  const decisions = session.calendarVisibility ?? NO_DECISIONS;
  const decide = (patch: Record<CalendarId, CalendarVisibility>) =>
    remember({ calendarVisibility: { ...decisions, ...patch } });

  /*
   * The calendars this window has already reconciled against the stored map.
   *
   * A ref, and it has to be, because it is a statement about *this render tree*
   * rather than about the user: below, it decides which direction the two
   * disagree in. Before a calendar is in this set the stored map is the truth
   * and `hidden` is a default that has not caught up; after, `hidden` is the
   * truth and the map is a record of it. Without that flip, restoring a hide at
   * launch and recording a hide from a keystroke are the same two values in
   * disagreement and there is no way to tell which one to move.
   */
  const reconciled = useRef(new Set<CalendarId>());

  useEffect(() => {
    // Nothing may be reconciled against a session that has not arrived: the
    // stored map is the truth for a calendar's first pass, and `{}` a tick
    // before the read lands would make every calendar look undecided and adopt
    // Google's answer over the user's.
    if (!loaded) return;
    const { toggles, patch, seen } = reconcileVisibility(
      calendars,
      decisions,
      hidden,
      reconciled.current,
    );
    for (const id of seen) reconciled.current.add(id);
    for (const id of toggles) onToggle(id);
    if (Object.keys(patch).length > 0) {
      remember({ calendarVisibility: { ...decisions, ...patch } });
    }
  }, [loaded, calendars, decisions, hidden, onToggle, remember]);

  const { groups, unlisted } = useMemo(
    () => calendarRows(accounts, calendars, decisions),
    [accounts, calendars, decisions],
  );

  /*
   * Both writes, and the order they happen in.
   *
   * `onToggle` dispatches into the shell's reducer, so the grid has repainted
   * by the end of this frame; `remember` is debounced and lands half a second
   * later. Nothing on screen waits for the write.
   */
  const hide = (row: CalendarRow) => {
    decide({ [row.calendar.id]: "unlisted" });
    if (!hidden.includes(row.calendar.id)) onToggle(row.calendar.id);
  };
  const restore = (row: CalendarRow) => {
    decide({ [row.calendar.id]: "shown" });
    if (hidden.includes(row.calendar.id)) onToggle(row.calendar.id);
  };

  return (
    <aside className="flex w-52 shrink-0 flex-col gap-2 overflow-y-auto border-r border-border bg-surface px-2 py-2">
      {groups.map(({ account, rows }, accountIndex) => {
        const isCollapsed = collapsed.includes(account.id);
        const soloed = sameSolo(solo, { kind: "account", id: account.id });
        return (
          <div key={account.id} className="flex flex-col">
            <div className="flex items-center gap-1">
              <button
                type="button"
                onClick={() =>
                  setCollapsed(
                    collapsed.includes(account.id)
                      ? collapsed.filter((id) => id !== account.id)
                      : [...collapsed, account.id],
                  )
                }
                // Smaller and fainter than the calendars under it. An address
                // answers "whose", once per group, so it takes the metadata step
                // of the ramp while the names take the reading one. It is left
                // in its own case, because an address set in caps is harder to
                // read as an address.
                className="flex min-w-0 flex-1 items-center gap-1 text-left text-micro font-medium text-faint-foreground hover:text-foreground"
                title={account.email}
              >
                {isCollapsed ? (
                  <ChevronRight size={11} strokeWidth={2} className="shrink-0" />
                ) : (
                  <ChevronDown size={11} strokeWidth={2} className="shrink-0" />
                )}
                <span className="truncate">{account.email}</span>
              </button>
              {accountIndex < 5 && account.id >= 0 && (
                <button
                  type="button"
                  onClick={() => onSolo({ kind: "account", id: account.id })}
                  aria-pressed={soloed}
                  title={`${soloed ? "Show every account" : "Show only this account"} (${formatBinding(`s ${accountIndex + 1}`)})`}
                  className={cn(
                    "shrink-0 rounded-[3px] px-1 text-micro",
                    soloed
                      ? "bg-accent text-accent-foreground"
                      : "text-faint-foreground hover:text-foreground",
                  )}
                >
                  solo
                </button>
              )}
            </div>

            {!isCollapsed &&
              rows.map((row) => (
                <CalendarRowView
                  key={row.calendar.id}
                  row={row}
                  accounts={accounts}
                  off={hidden.includes(row.calendar.id)}
                  soloed={sameSolo(solo, { kind: "calendar", id: row.calendar.id })}
                  colorFor={colorFor}
                  dark={dark}
                  onToggle={onToggle}
                  onSolo={onSolo}
                  onHide={hide}
                />
              ))}
          </div>
        );
      })}

      <div className="mt-auto flex flex-col gap-1 border-t border-border pt-2">
        {unlisted.length > 0 && (
          <HiddenFromList
            rows={unlisted}
            accounts={accounts}
            colorFor={colorFor}
            dark={dark}
            onRestore={restore}
          />
        )}
        <Toggle
          label="Merge duplicates"
          hint="One block per meeting, across accounts"
          on={settings.mergeDuplicates}
          onChange={(on) => onSettings({ ...settings, mergeDuplicates: on })}
        />
        <Toggle
          label="Show declined"
          hint="Outlined and struck through"
          on={settings.showDeclined}
          onChange={(on) => onSettings({ ...settings, showDeclined: on })}
        />
        <Toggle
          label="Weekends"
          hint="Sat and Sun columns"
          on={settings.showWeekends}
          onChange={(on) => onSettings({ ...settings, showWeekends: on })}
        />
      </div>
    </aside>
  );
}

/**
 * One calendar's row: the toggle, the `solo` chip, and the × that takes it out
 * of the list.
 *
 * # Three buttons, and one of them also answers a modifier
 *
 * This used to read "two buttons rather than one button with a second gesture
 * on it", and the reasoning still holds where it was aimed: a control whose
 * second half exists only as ⌥-click is a control with an invisible half, and
 * the modifier is the half nobody finds. So solo is a button — its own tab
 * stop, its own accessible name, Space works on it — and it carries the same
 * word the account heading above it already uses.
 *
 * ⌥-click on the row is then an accelerator for a thing that is already on
 * screen, which is a different proposition from being the only way in. It is
 * the gesture audio software has used for solo for thirty years and the one
 * Google Calendar offers as "Display this only", and its keyboard twin is ⌥
 * with the calendar's own digit — the same modifier, so the two are one thing
 * to remember rather than two.
 *
 * # Faded, not absent
 *
 * `solo` and the × are drawn at zero opacity until the row is hovered or the
 * button itself is focused. Both are in the accessibility tree and in the tab
 * order throughout — `opacity`, not `display` — so this keeps thirty small
 * controls off a rail that has to stay readable without hiding anything from
 * someone arriving by keyboard.
 *
 * The soloed row is the exception: its chip stays lit and accent-filled with
 * the pointer nowhere near it, because while one calendar is soloed the way
 * back has to be visible without hunting for it.
 */
function CalendarRowView({
  row,
  accounts,
  off,
  soloed,
  colorFor,
  dark,
  onToggle,
  onSolo,
  onHide,
}: {
  row: CalendarRow;
  accounts: Account[];
  off: boolean;
  soloed: boolean;
  colorFor: (id: CalendarId) => CalendarColor;
  dark: boolean;
  onToggle: (id: CalendarId) => void;
  onSolo: (target: Solo) => void;
  onHide: (row: CalendarRow) => void;
}) {
  const calendar = row.calendar;
  const label = calendarLabel(calendar);
  const solo = () => onSolo({ kind: "calendar", id: calendar.id });
  // ⌥ plus the calendar's own digit, which is what `CalendarMode` binds. Out
  // past the ninth calendar there is no digit left, so the chip's tooltip drops
  // the parenthetical rather than naming a key that does nothing.
  const soloKeys = row.slot >= 0 && row.slot < 9 ? ` (${formatBinding(`alt+${row.slot + 1}`)})` : "";
  return (
    <div className="group flex items-center rounded-[3px] pl-4 pr-1 hover:bg-row-hover">
      <button
        type="button"
        onClick={(event) => (event.altKey ? solo() : onToggle(calendar.id))}
        // A row is a toggle, so it says so. The swatch is the state
        // indicator and a swatch cannot be read aloud.
        aria-pressed={!off}
        title={calendarTooltip(row, accounts, off)}
        className="flex min-w-0 flex-1 items-center gap-2 py-1 text-left focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring"
      >
        <span
          className="h-2.5 w-2.5 shrink-0 rounded-[3px]"
          style={{
            // Filled, the swatch is the calendar's colour exactly —
            // it has to match the blocks in the grid. Emptied, it is
            // a 1px hairline, and a hairline of `#fbd75b` on a white
            // rail is not visible; the outlined form therefore uses
            // the same colour fitted for contrast against the page,
            // which is what an unanswered invitation's border does
            // for the same reason.
            background: off ? "transparent" : calendarFill(colorFor(calendar.id), dark),
            boxShadow: off
              ? `inset 0 0 0 1px ${calendarInk(colorFor(calendar.id), dark)}`
              : undefined,
          }}
        />
        {/*
         * The name truncates because the rail is 13rem wide and
         * "Holidays in United States" is not. The full string lives
         * on the button's `title`, so the tooltip is the overflow —
         * which is why the tooltip leads with the name rather than
         * with the verb.
         */}
        {/*
          `text-list`, where the account address above it stays at
          `text-micro`. The rail used to be one size top to bottom,
          so a group header and its members read as one list of
          eleven-pixel strings. A step between them says which is the
          heading, and the calendar's name is what anyone comes to
          this rail to find.
        */}
        <span
          className={cn(
            "min-w-0 flex-1 truncate text-list",
            off ? "text-faint-foreground" : "text-foreground",
          )}
        >
          {label}
        </span>
      </button>
      <button
        type="button"
        onClick={solo}
        aria-pressed={soloed}
        aria-label={soloed ? "Show every calendar" : `Show only ${label}`}
        title={`${soloed ? "Show every calendar" : "Show only this calendar"}${soloKeys}`}
        className={cn(
          "shrink-0 rounded-[3px] px-1 text-micro focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring",
          soloed
            ? "bg-accent text-accent-foreground"
            : "text-faint-foreground opacity-0 hover:text-foreground focus-visible:opacity-100 group-hover:opacity-100",
        )}
      >
        solo
      </button>
      <button
        type="button"
        onClick={() => onHide(row)}
        aria-label={`Hide ${label} from list`}
        title="Hide from list"
        className="shrink-0 rounded-[3px] p-0.5 text-faint-foreground opacity-0 hover:text-foreground focus-visible:opacity-100 focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring group-hover:opacity-100"
      >
        <X size={11} strokeWidth={2} />
      </button>
    </div>
  );
}

/**
 * The way back for a calendar taken out of the list.
 *
 * At the bottom, under the same fold as the three switches, because it is a
 * place you go once when you want something back rather than part of reading
 * the rail. It renders nothing at all when nothing has been hidden, so the
 * ordinary rail is unchanged by the feature's existence.
 */
function HiddenFromList({
  rows,
  accounts,
  colorFor,
  dark,
  onRestore,
}: {
  rows: CalendarRow[];
  accounts: Account[];
  colorFor: (id: CalendarId) => CalendarColor;
  dark: boolean;
  onRestore: (row: CalendarRow) => void;
}) {
  // Local, and closed on every launch, unlike the account groups above. Those
  // are how the rail is arranged and the arrangement should survive a quit;
  // this is a drawer you open to fetch one thing back out of.
  const [open, setOpen] = useState(false);
  return (
    <div className="flex flex-col pb-1">
      <button
        type="button"
        onClick={() => setOpen(!open)}
        aria-expanded={open}
        className="flex min-w-0 items-center gap-1 rounded-[3px] px-1 py-1 text-left text-micro text-faint-foreground hover:text-foreground"
      >
        {open ? (
          <ChevronDown size={11} strokeWidth={2} className="shrink-0" />
        ) : (
          <ChevronRight size={11} strokeWidth={2} className="shrink-0" />
        )}
        <span className="truncate">Hidden from list ({rows.length})</span>
      </button>
      {open &&
        rows.map((row) => {
          const label = calendarLabel(row.calendar);
          return (
            <button
              key={row.calendar.id}
              type="button"
              onClick={() => onRestore(row)}
              title={calendarTooltip(row, accounts, true)}
              aria-label={`Show ${label} in list`}
              className="flex items-center gap-2 rounded-[3px] py-1 pl-4 pr-1 text-left hover:bg-row-hover focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring"
            >
              <span
                className="h-2.5 w-2.5 shrink-0 rounded-[3px]"
                style={{
                  boxShadow: `inset 0 0 0 1px ${calendarInk(colorFor(row.calendar.id), dark)}`,
                }}
              />
              <span className="min-w-0 flex-1 truncate text-list text-faint-foreground">
                {label}
              </span>
            </button>
          );
        })}
    </div>
  );
}

function Toggle({
  label,
  hint,
  on,
  onChange,
}: {
  label: string;
  hint: string;
  on: boolean;
  onChange: (on: boolean) => void;
}) {
  // The box used to be a `<button>` wrapping a hand-drawn square: it looked
  // right and announced itself as a button with no state. Same pixels, real
  // `role="checkbox"` and `aria-checked`.
  return (
    <Label title={hint} className="cursor-pointer gap-2 rounded-[3px] px-1 py-1 hover:bg-row-hover">
      <Checkbox checked={on} onCheckedChange={onChange} />
      <span className="min-w-0 flex-1 truncate text-micro text-muted-foreground">{label}</span>
    </Label>
  );
}

/* -------------------------------------------------------------------------- */
/* Rows — one per calendar, not one per subscription                           */
/* -------------------------------------------------------------------------- */

/**
 * A calendar as the rail draws it: one row, however many accounts carry it.
 */
export interface CalendarRow {
  /** The copy whose account hosts the row — see {@link hostCopy}. */
  calendar: Calendar;
  /** Every account subscribing to this calendar, the host's first. */
  accountIds: AccountId[];
  /**
   * The calendar's index in the `calendars` array, which is what `v <digit>`
   * counts. Taken from the array rather than from the render, because those two
   * had drifted: the rail numbered the rows it painted, so a collapsed group
   * shifted every number below it and the tooltip advertised a shortcut that
   * toggled a different calendar.
   */
  slot: number;
  state: CalendarVisibility;
}

/**
 * The rail's rows, grouped by account, with duplicate subscriptions collapsed.
 *
 * # What the duplicates in this rail actually were
 *
 * Both of the things that looked like duplication are the same thing, and it is
 * not two calendars with one name. Read out of the store:
 *
 *   `en.usa#holiday@group.v.calendar.google.com`   4 rows, accounts 1,2,3,4
 *   `bruno.bornsztein@clickfunnels.com`            2 rows, accounts 1 and 4
 *
 * One calendar id each. Four accounts subscribe to Google's US holidays feed,
 * and the clickfunnels calendar is owned by the clickfunnels account and shared
 * into the gmail one. `calendars` is a list of *subscriptions* — one row per
 * (account, calendar) pair, which is what `calendarList.list` returns — and the
 * rail was drawing one row per subscription.
 *
 * Which made the four holidays rows worse than redundant: visibility is keyed
 * by calendar id (`ui.hiddenCalendars`, and `visibleEvents` filtering on
 * `event.calendarId`), so those four rows were four copies of *one switch*.
 * Unticking any of them turned all four pale at once. The rail was showing a
 * duplicated control, not a duplicated calendar.
 *
 * So a row is a calendar id. Collapsing them is not merging anything: it is
 * drawing the switch that already existed, once.
 *
 * # Which account a collapsed row belongs under
 *
 * The one with the strongest claim: primary, then writable, then first seen.
 * `bruno.bornsztein@clickfunnels.com` is `owner` and primary on the clickfunnels
 * account and `reader` on the gmail one, so it belongs under clickfunnels — and
 * taking the access role from that copy is what stops the row calling a calendar
 * he owns read-only. Nobody owns the holidays feed, so it stays where it was
 * first seen.
 *
 * The other accounts are not dropped on the floor; they are named in the row's
 * tooltip. Two *different* calendars that happen to share a name keep their own
 * rows under their own accounts, which is the case the account heading and that
 * tooltip line exist to tell apart.
 *
 * This has nothing to do with the "Merge duplicates" switch at the foot of the
 * rail, which is about events: one meeting that reached two accounts drawing as
 * one block instead of two. See `lib/calendar-merge.ts`.
 */
export function calendarRows(
  accounts: Account[],
  calendars: Calendar[],
  decisions: Record<string, CalendarVisibility>,
): { groups: { account: Account; rows: CalendarRow[] }[]; unlisted: CalendarRow[] } {
  const copies = new Map<CalendarId, Calendar[]>();
  for (const calendar of calendars) {
    const found = copies.get(calendar.id);
    if (found) found.push(calendar);
    else copies.set(calendar.id, [calendar]);
  }

  const rows: CalendarRow[] = [];
  for (const [id, group] of copies) {
    const host = hostCopy(group);
    rows.push({
      calendar: host,
      accountIds: [host.accountId, ...group.filter((c) => c !== host).map((c) => c.accountId)],
      slot: calendars.findIndex((c) => c.id === id),
      state: decisions[id] ?? initialVisibility(group),
    });
  }

  const unlisted = rows.filter((row) => row.state === "unlisted");
  const listed = rows.filter((row) => row.state !== "unlisted");

  /*
   * An account with nothing under it is not a heading, it is a stub.
   *
   * The heading exists to say whose the calendars below it are, and it carries
   * a fold triangle and a "solo" button — so an account whose only calendar was
   * unlisted kept a row that named nothing, folded nothing, and offered to solo
   * a set with no members. The calendar is still one press away under "Hidden
   * from list", and restoring it brings the heading back with it.
   *
   * Empty because Google returned no calendars for the account at all is the
   * same picture and gets the same answer.
   */
  const groups = accounts
    .map((account) => ({
      account,
      rows: listed.filter((row) => row.calendar.accountId === account.id),
    }))
    .filter((group) => group.rows.length > 0);
  // Calendars whose account is gone should still be reachable.
  const orphans = listed.filter((row) => !accounts.some((a) => a.id === row.calendar.accountId));
  if (orphans.length > 0) {
    groups.push({
      account: { id: -1, email: "Other calendars", name: "Other", colorIndex: 1, kind: "personal" },
      rows: orphans,
    });
  }

  return { groups, unlisted };
}

/**
 * The subscription that speaks for a calendar: primary, then writable, then the
 * first one seen. See the note on {@link calendarRows}.
 */
function hostCopy(group: Calendar[]): Calendar {
  return group.find((c) => c.primary) ?? group.find((c) => writable(c)) ?? group[0];
}

/** `undefined` is "never fetched", which every reader in the app takes as yes. */
function writable(calendar: Calendar): boolean {
  return calendar.accessRole !== "reader" && calendar.accessRole !== "freeBusyReader";
}

/**
 * Where a calendar starts, the one time it is asked.
 *
 * Structural, both of them. `deleted` is Google saying the subscription is gone
 * — the row survives only because its events are still in the store and want a
 * name — so it starts out of the list rather than merely unticked. `selected` is
 * Google's own "is this calendar shown".
 *
 * Neither reads the calendar's *title*. "Earnest Capital Calendar (Subscription
 * Expired)" is dead and says so in its name, and matching on that string would
 * work exactly until the next dead calendar was named something else.
 *
 * A calendar several accounts subscribe to gets one answer for all of them, and
 * the permissive one: the US holidays feed is deselected on two of his accounts
 * and selected on the other two, and a single row that starts off would be
 * hiding events he has switched on in Google.
 */
function initialVisibility(group: Calendar[]): CalendarVisibility {
  if (group.every((c) => c.deleted)) return "unlisted";
  if (group.every((c) => c.selected === false)) return "hidden";
  return "shown";
}

/* -------------------------------------------------------------------------- */
/* Reconciliation — the stored map against the window's live `hidden`          */
/* -------------------------------------------------------------------------- */

export interface Reconciliation {
  /** Calendars whose live visibility disagrees with the stored decision. */
  toggles: CalendarId[];
  /** Decisions to write: adoptions from Google, and hides made this session. */
  patch: Record<string, CalendarVisibility>;
  /** Calendars now reconciled, for the caller's set. */
  seen: CalendarId[];
}

/**
 * One pass over the calendars, in whichever direction that calendar is due.
 *
 * A calendar the window has not reconciled yet is *restored*: the stored
 * decision — or Google's `selected`, adopted once, if there is none — is pushed
 * into `hidden`. A calendar it has is *recorded*: `hidden` is the truth, because
 * something moved it since, and the map is brought up to date. `v <digit>` and a
 * click on the row both arrive here as the second case, which is why neither
 * needs to write the map itself.
 *
 * `unlisted` survives being recorded in one direction only. Left hidden it stays
 * unlisted; turned back on — which `v <digit>` can still do for a calendar with
 * no row — it becomes `shown`, and the row comes back with its events. Asking
 * for a calendar's events is the same as asking to see the calendar, and the
 * alternative is events on the grid belonging to a row you cannot find.
 *
 * Pure, and taking the reconciled set as an argument, because the direction it
 * chooses is the whole of the bug it fixes and a bug you can only reproduce by
 * relaunching an app is one nobody writes a second test for.
 */
export function reconcileVisibility(
  calendars: Calendar[],
  decisions: Record<string, CalendarVisibility>,
  hidden: CalendarId[],
  reconciled: ReadonlySet<CalendarId>,
): Reconciliation {
  const toggles: CalendarId[] = [];
  const patch: Record<string, CalendarVisibility> = {};
  const seen: CalendarId[] = [];

  const copies = new Map<CalendarId, Calendar[]>();
  for (const calendar of calendars) {
    const found = copies.get(calendar.id);
    if (found) found.push(calendar);
    else copies.set(calendar.id, [calendar]);
  }

  for (const [id, group] of copies) {
    const off = hidden.includes(id);
    const stored = decisions[id];

    if (!reconciled.has(id)) {
      seen.push(id);
      const decision = stored ?? initialVisibility(group);
      if (stored === undefined) patch[id] = decision;
      if ((decision !== "shown") !== off) toggles.push(id);
      continue;
    }

    const now: CalendarVisibility = off ? (stored === "unlisted" ? "unlisted" : "hidden") : "shown";
    if (stored !== now) patch[id] = now;
  }

  return { toggles, patch, seen };
}

/**
 * What to call a calendar.
 *
 * This function used to make names up. It had to: nothing in Mach had ever
 * called `calendarList.list`, so the only string available was the calendar id,
 * and `c_d814cb1f…@group.calendar.google.com` is not a label. So it produced
 * `Shared · d814cb` — a string that appears nowhere in Google, tells the user
 * nothing except that six hex digits exist, and is indistinguishable from the
 * next four calendars that also start with `Shared · `.
 *
 * Since migration 6 the name arrives from Google (`summaryOverride ?? summary`,
 * with the account holder's name substituted on the primary), so the invention
 * is gone and this is nearly always a passthrough.
 *
 * What remains is the gap: a calendar whose events have synced but whose
 * metadata sweep has not, in which case `name` is still the id. That case
 * *shortens the id* instead of inventing a label — `en.usa#holiday` rather than
 * `Holidays · usa holiday` — because a truncated true thing degrades honestly
 * and a plausible false thing does not. Ordinary addresses are left whole: a
 * calendar id that is somebody's actual email is already a name.
 */
export function calendarLabel(calendar: Calendar): string {
  const name = calendar.name.trim();
  if (!name) return calendar.id;
  const at = name.lastIndexOf("@");
  if (at <= 0) return name;
  const local = name.slice(0, at);
  const domain = name.slice(at + 1);
  return domain.endsWith("calendar.google.com") ? local : name;
}

/**
 * The row's tooltip, and the only place a long name is readable in full.
 *
 * Name first, because the tooltip doubles as the overflow for a truncated
 * label — a tooltip that opens with "Hide" buries the one word the user hovered
 * to find.
 *
 * Then the account, always, even though the group heading a few rows up says the
 * same thing. Four accounts and a rail that scrolls means the heading is often
 * off screen, and "which of my accounts is this Holidays in United States" is
 * the exact question two identically named calendars raise. The second line is
 * the answer, and for a calendar several accounts subscribe to it is the whole
 * list of them.
 */
export function calendarTooltip(row: CalendarRow, accounts: Account[], off: boolean): string {
  const shortcut = row.slot >= 0 && row.slot < 9 ? ` (${formatBinding(`v ${row.slot + 1}`)})` : "";
  const description = row.calendar.description?.trim();
  const emails = row.accountIds
    .map((id) => accounts.find((a) => a.id === id)?.email)
    .filter((email): email is string => Boolean(email));
  const readOnly = !writable(row.calendar);
  return [
    calendarLabel(row.calendar),
    emails.length > 0 ? emails.join(", ") : null,
    description || null,
    [readOnly ? "Read-only" : null, `${off ? "Show" : "Hide"}${shortcut}`]
      .filter(Boolean)
      .join(" · "),
  ]
    .filter(Boolean)
    .join("\n");
}
