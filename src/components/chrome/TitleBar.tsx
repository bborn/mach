import { Calendar, Mail, Search } from "lucide-react";
import { useMach, type Mode } from "@/hooks/useMach";
import { ACCOUNT_BG } from "@/lib/colors";
import { cn } from "@/lib/utils";
import { Kbd } from "@/components/ui/kbd";

const MODES: { id: Mode; label: string; icon: typeof Mail; keys: string }[] = [
  { id: "mail", label: "Mail", icon: Mail, keys: "g i" },
  { id: "calendar", label: "Calendar", icon: Calendar, keys: "g c" },
];

export function TitleBar() {
  const { ui, actions, accounts } = useMach();

  return (
    <header
      data-tauri-drag-region
      className="flex h-10 shrink-0 items-center gap-3 border-b border-border bg-surface pl-[5.5rem] pr-2"
    >
      <span className="select-none font-mono text-micro tracking-[0.12em] text-faint-foreground">
        MACH
      </span>

      <nav className="flex items-center gap-px rounded-[var(--radius)] border border-border p-px">
        {MODES.map((mode) => {
          const Icon = mode.icon;
          const active = ui.mode === mode.id;
          return (
            <button
              key={mode.id}
              type="button"
              onClick={() => actions.setMode(mode.id)}
              className={cn(
                "flex h-5.5 items-center gap-1.5 rounded-[3px] px-2 py-0.5 text-micro transition-colors",
                active
                  ? "bg-surface-raised text-foreground"
                  : "text-muted-foreground hover:text-foreground",
              )}
              title={`${mode.label} — ${mode.keys}`}
            >
              <Icon size={12} strokeWidth={1.75} />
              {mode.label}
            </button>
          );
        })}
      </nav>

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

      <div className="ml-auto flex shrink-0 items-center gap-1.5" title="Accounts">
        {accounts.map((account) => (
          <span
            key={account.id}
            title={account.email}
            className={cn("h-1.5 w-1.5 rounded-full", ACCOUNT_BG[account.colorIndex])}
          />
        ))}
      </div>
    </header>
  );
}
