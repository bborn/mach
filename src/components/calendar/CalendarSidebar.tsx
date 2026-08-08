import { useEffect, useRef } from "react";
import { ChevronDown, ChevronRight } from "lucide-react";
import type { Account, Calendar, CalendarId, AccountId } from "@/types";
import { useUiSession } from "@/components/prefs/PreferencesProvider";
import { Checkbox } from "@/components/ui/checkbox";
import { Label } from "@/components/ui/label";
import { calendarFill, type HueIndex } from "@/lib/calendar-palette";
import { cn } from "@/lib/utils";
import { formatBinding } from "@/lib/keymap";

export interface CalendarSettings {
  mergeDuplicates: boolean;
  showDeclined: boolean;
  showWeekends: boolean;
}

interface SidebarProps {
  accounts: Account[];
  calendars: Calendar[];
  hidden: CalendarId[];
  hueFor: (id: CalendarId) => HueIndex;
  dark: boolean;
  soloAccount: AccountId | null;
  onToggle: (id: CalendarId) => void;
  onSolo: (id: AccountId | null) => void;
  settings: CalendarSettings;
  onSettings: (next: CalendarSettings) => void;
}

/**
 * Calendars, grouped by account (§7).
 *
 * Colour says *which calendar*; the sidebar says *which account*. That split is
 * the only way five accounts stay legible — within one account you still need
 * work and personal to look different, so account cannot own the hue.
 */
export function CalendarSidebar({
  accounts,
  calendars,
  hidden,
  hueFor,
  dark,
  soloAccount,
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
  const { session, remember } = useUiSession();
  const collapsed = session.collapsedCalendarAccounts ?? [];
  const setCollapsed = (next: AccountId[]) =>
    remember({ collapsedCalendarAccounts: next });

  /*
   * Google's own "is this calendar shown", adopted exactly once per calendar.
   *
   * `selected` is a real preference the user set in Google, and ignoring it is
   * why an account with a dozen subscribed calendars opened as a wall of blocks
   * the user had already turned off somewhere else. Adopting it happens through
   * the same `onToggle` every click uses rather than through a second source of
   * truth, so there is only ever one answer to "is this calendar hidden".
   *
   * The ref is what makes it *initial* state rather than a policy: once a
   * calendar has been seeded it is never seeded again, so turning a
   * Google-hidden calendar back on here sticks, and nothing fights the user.
   * A ref rather than persisted session state because `hidden` itself is not
   * persisted — both should live exactly as long as the window does, and today
   * they do because `App` keeps both mode layers mounted and merely hides the
   * inactive one. A refactor that unmounts `CalendarMode` on a mode switch would
   * quietly turn this into a policy that re-hides a calendar every time you come
   * back from mail, and this sentence is the only warning it would get.
   */
  const seeded = useRef(new Set<CalendarId>());
  useEffect(() => {
    for (const calendar of calendars) {
      if (seeded.current.has(calendar.id)) continue;
      seeded.current.add(calendar.id);
      if (calendar.selected === false && !hidden.includes(calendar.id)) {
        onToggle(calendar.id);
      }
    }
  }, [calendars, hidden, onToggle]);

  const groups = accounts.map((account) => ({
    account,
    calendars: calendars.filter((calendar) => calendar.accountId === account.id),
  }));
  // Calendars whose account is gone should still be reachable.
  const orphans = calendars.filter((c) => !accounts.some((a) => a.id === c.accountId));
  if (orphans.length > 0) {
    groups.push({
      account: { id: -1, email: "Other calendars", name: "Other", colorIndex: 1, kind: "personal" },
      calendars: orphans,
    });
  }

  let index = 0;

  return (
    <aside className="flex w-52 shrink-0 flex-col gap-2 overflow-y-auto border-r border-border bg-surface px-2 py-2">
      {groups.map(({ account, calendars: owned }, accountIndex) => {
        const isCollapsed = collapsed.includes(account.id);
        const soloed = soloAccount === account.id;
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
                className="flex min-w-0 flex-1 items-center gap-1 text-left text-micro text-muted-foreground hover:text-foreground"
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
                  onClick={() => onSolo(soloed ? null : account.id)}
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
              owned.map((calendar) => {
                const off = hidden.includes(calendar.id);
                const slot = index++;
                const label = calendarLabel(calendar);
                return (
                  <button
                    key={calendar.id}
                    type="button"
                    onClick={() => onToggle(calendar.id)}
                    // A row is a toggle, so it says so. The swatch is the state
                    // indicator and a swatch cannot be read aloud.
                    aria-pressed={!off}
                    title={calendarTooltip(calendar, off, slot)}
                    className="flex items-center gap-1.5 rounded-[3px] py-[3px] pl-4 pr-1 text-left hover:bg-row-hover focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring"
                  >
                    <span
                      className="h-2.5 w-2.5 shrink-0 rounded-[3px]"
                      style={{
                        background: off ? "transparent" : calendarFill(hueFor(calendar.id), dark),
                        boxShadow: off
                          ? `inset 0 0 0 1px ${calendarFill(hueFor(calendar.id), dark)}`
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
                    <span
                      className={cn(
                        "min-w-0 flex-1 truncate text-micro",
                        off ? "text-faint-foreground" : "text-foreground",
                      )}
                    >
                      {label}
                    </span>
                  </button>
                );
              })}
          </div>
        );
      })}

      <div className="mt-auto flex flex-col gap-1 border-t border-border pt-2">
        <Toggle
          label="Merge duplicates"
          hint="One block when the same meeting is on several accounts"
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
    <Label title={hint} className="cursor-pointer gap-1.5 rounded-[3px] px-1 py-[3px] hover:bg-row-hover">
      <Checkbox checked={on} onCheckedChange={onChange} />
      <span className="min-w-0 flex-1 truncate text-micro text-muted-foreground">{label}</span>
    </Label>
  );
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
 */
function calendarTooltip(calendar: Calendar, off: boolean, slot: number): string {
  const shortcut = slot < 9 ? ` (${formatBinding(`v ${slot + 1}`)})` : "";
  const description = calendar.description?.trim();
  const readOnly =
    calendar.accessRole === "reader" || calendar.accessRole === "freeBusyReader";
  return [
    calendarLabel(calendar),
    description || null,
    [
      readOnly ? "Read-only" : null,
      `${off ? "Show" : "Hide"}${shortcut}`,
    ]
      .filter(Boolean)
      .join(" · "),
  ]
    .filter(Boolean)
    .join("\n");
}
