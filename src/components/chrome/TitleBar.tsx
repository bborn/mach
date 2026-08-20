import { Search } from "lucide-react";
import { useMach } from "@/hooks/useMach";
import { cn } from "@/lib/utils";
import { Kbd } from "@/components/ui/kbd";
import { ShortcutTooltip } from "@/components/ui/tooltip";
import { openSearch } from "@/components/search/palette";

const MODES = [
  { id: "mail" as const, label: "Mail", keys: "mod+1" },
  { id: "calendar" as const, label: "Calendar", keys: "mod+2" },
];

export function TitleBar() {
  const { ui, actions } = useMach();

  return (
    <header
      data-tauri-drag-region
      className="flex h-10 shrink-0 items-center gap-3 border-b border-border bg-surface pl-[5.5rem] pr-2"
    >
      {/*
        Mail and Calendar are the two surfaces of the window, so they live in
        the chrome — not as a mailbox between Inbox and Folders, and not as a
        one-off Mail button on the calendar. ⌘1/⌘2 and `g m`/`g c` are unchanged.
      */}
      <div className="flex items-center gap-px rounded-[var(--radius)] border border-border p-px">
        {MODES.map((mode) => (
          <ShortcutTooltip key={mode.id} label={mode.label} keys={mode.keys}>
            <button
              type="button"
              onClick={() => actions.setMode(mode.id)}
              className={cn(
                "flex h-6 items-center rounded-[3px] px-2 text-list transition-colors",
                ui.mode === mode.id
                  ? "bg-background text-foreground"
                  : "text-muted-foreground hover:text-foreground",
              )}
            >
              {mode.label}
            </button>
          </ShortcutTooltip>
        ))}
      </div>
      <div className="ml-auto flex shrink-0 items-center gap-1">
        <ShortcutTooltip label="Search" keys={["/", "mod+f"]}>
          <button
            type="button"
            onClick={() => openSearch("")}
            className={cn(
              "flex h-6 items-center gap-1.5 rounded-[var(--radius)] px-2",
              "text-list text-faint-foreground hover:bg-row-hover hover:text-foreground",
            )}
          >
            <Search size={12} strokeWidth={1.75} className="shrink-0" />
            Search
          </button>
        </ShortcutTooltip>
        <ShortcutTooltip label="Command palette" keys="mod+k">
          <button
            type="button"
            onClick={() => actions.setPalette(true)}
            className={cn(
              "flex h-6 items-center rounded-[var(--radius)] px-1.5",
              "text-faint-foreground hover:bg-row-hover hover:text-foreground",
            )}
          >
            <Kbd keys="mod+k" />
          </button>
        </ShortcutTooltip>
      </div>
    </header>
  );
}
