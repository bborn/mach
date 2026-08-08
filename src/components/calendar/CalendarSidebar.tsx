import { ChevronDown, ChevronRight } from "lucide-react";
import { useState } from "react";
import type { Account, Calendar, CalendarId, AccountId } from "@/types";
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
  const [collapsed, setCollapsed] = useState<AccountId[]>([]);

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
                  setCollapsed((prev) =>
                    prev.includes(account.id)
                      ? prev.filter((id) => id !== account.id)
                      : [...prev, account.id],
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
                return (
                  <button
                    key={calendar.id}
                    type="button"
                    onClick={() => onToggle(calendar.id)}
                    title={`${off ? "Show" : "Hide"} ${calendarLabel(calendar)}${
                      slot < 9 ? ` (${formatBinding(`v ${slot + 1}`)})` : ""
                    }`}
                    className="flex items-center gap-1.5 rounded-[3px] py-[3px] pl-4 pr-1 text-left hover:bg-row-hover"
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
                    <span
                      className={cn(
                        "min-w-0 flex-1 truncate text-micro",
                        off ? "text-faint-foreground" : "text-foreground",
                      )}
                    >
                      {calendarLabel(calendar)}
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
 * Google hands back calendar ids as names for the ones it has no summary for —
 * a 64-character group address is not a label. Shorten it to something a human
 * can scan without losing which calendar it is.
 */
export function calendarLabel(calendar: Calendar): string {
  const name = calendar.name.trim();
  if (!name.includes("@")) return name;
  const [local, domain = ""] = name.split("@");
  if (domain.includes("holiday")) return `Holidays · ${local.replace(/^en\./, "").replace("#", " ")}`;
  if (domain.includes("group.calendar.google.com") || domain.includes("import.calendar.google.com")) {
    return `Shared · ${local.slice(0, 6)}`;
  }
  return name;
}
