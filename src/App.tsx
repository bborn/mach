import { KeymapProvider, useKeyBindings } from "@/hooks/useKeymap";
import { MachProvider, useMach } from "@/hooks/useMach";
import { cn } from "@/lib/utils";
import { StatusBar } from "@/components/chrome/StatusBar";
import { TitleBar } from "@/components/chrome/TitleBar";
import { CalendarMode } from "@/components/calendar/CalendarMode";
import { MailMode } from "@/components/mail/MailMode";
import { CommandPalette } from "@/components/palette/CommandPalette";
import { AddAccountDialog } from "@/components/accounts/AddAccountDialog";
import { FeedbackDialog } from "@/components/feedback/FeedbackDialog";
import { AgentDock } from "@/components/agent/AgentDock";

export default function App() {
  return (
    <KeymapProvider>
      <MachProvider>
        <Shell />
      </MachProvider>
    </KeymapProvider>
  );
}

function Shell() {
  const { ui, actions } = useMach();

  useKeyBindings([
    {
      keys: "g i",
      group: "Global",
      description: "Go to mail",
      handler: () => actions.setMode("mail"),
    },
    {
      keys: "g c",
      group: "Global",
      description: "Go to calendar",
      handler: () => actions.setMode("calendar"),
    },
    {
      keys: "mod+1",
      group: "Global",
      description: "Mail",
      handler: () => actions.setMode("mail"),
    },
    {
      keys: "mod+2",
      group: "Global",
      description: "Calendar",
      handler: () => actions.setMode("calendar"),
    },
  ]);

  return (
    <div className="flex h-full w-full flex-col overflow-hidden bg-background text-foreground">
      <TitleBar />

      {/*
        Both modes stay mounted for the life of the window. Switching flips
        visibility, so it costs a repaint rather than a mount — and every
        scroll position, every selection, survives the round trip. `invisible`
        rather than `hidden` on purpose: `display: none` drops layout, and a
        pane with no layout forgets where it was scrolled to.
      */}
      <main className="relative min-h-0 flex-1">
        <ModeLayer active={ui.mode === "mail"} label="Mail">
          <MailMode />
        </ModeLayer>
        <ModeLayer active={ui.mode === "calendar"} label="Calendar">
          <CalendarMode />
        </ModeLayer>
      </main>

      {/*
        Above the status bar and below the modes: agent sessions are part of
        the window's furniture, not an overlay. Asking is never modal — the
        list keeps its focus and its scroll while a session works, which is why
        this is a strip of pills rather than a dialog. Renders nothing until
        there is a session.
      */}
      <AgentDock />

      <StatusBar />
      <CommandPalette />
      <AddAccountDialog />
      <FeedbackDialog />
    </div>
  );
}

function ModeLayer({
  active,
  label,
  children,
}: {
  active: boolean;
  label: string;
  children: React.ReactNode;
}) {
  return (
    <section
      aria-label={label}
      aria-hidden={!active}
      inert={!active}
      className={cn(
        "absolute inset-0",
        active ? "visible z-10" : "invisible z-0 pointer-events-none",
      )}
    >
      {children}
    </section>
  );
}
