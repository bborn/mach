import { Search } from "lucide-react";
import { useMach } from "@/hooks/useMach";
import { cn } from "@/lib/utils";
import { Kbd } from "@/components/ui/kbd";

/*
 * The Mail/Calendar segmented control used to live here.
 *
 * It has moved into the rail, where Inbox and Calendar sit as peers — which is
 * where an app states what its surfaces are. The title bar is now what it
 * should have been: the window's identity and its search.
 * ⌘1/⌘2 and `g m`/`g c` are unchanged, and the calendar's own header carries a
 * Mail button so the trip back is not keyboard-only.
 */
export function TitleBar() {
  const { actions } = useMach();

  return (
    <header
      data-tauri-drag-region
      className="flex h-10 shrink-0 items-center gap-3 border-b border-border bg-surface pl-[5.5rem] pr-2"
    >
      <span className="select-none font-mono text-micro tracking-[0.12em] text-faint-foreground">
        MACH
      </span>

      <button
        type="button"
        onClick={() => actions.setPalette(true)}
        className={cn(
          "flex h-6 w-full max-w-sm items-center gap-2 rounded-[var(--radius)]",
          "border border-border bg-background px-2 text-list text-faint-foreground",
          "hover:border-border-strong",
        )}
      >
        <Search size={12} strokeWidth={1.75} className="shrink-0" />
        <span className="min-w-0 flex-1 truncate text-left">Search</span>
        <Kbd keys="mod+k" className="border-none bg-transparent px-0" />
      </button>
    </header>
  );
}
