import { useState } from "react";
import { KeymapProvider, useKeyBindings, useKeymap } from "@/hooks/useKeymap";
import type { KeyBinding } from "@/lib/keymap";
import type { LabelId } from "@/types";
import { MachProvider, useMach } from "@/hooks/useMach";
import { cn } from "@/lib/utils";
import { StatusBar } from "@/components/chrome/StatusBar";
import { TitleBar } from "@/components/chrome/TitleBar";
import { CalendarMode } from "@/components/calendar/CalendarMode";
import { MailMode } from "@/components/mail/MailMode";
import { CommandPalette } from "@/components/palette/CommandPalette";
import { ShortcutSheet } from "@/components/chrome/ShortcutSheet";
import { AddAccountDialog } from "@/components/accounts/AddAccountDialog";
import { FeedbackDialog } from "@/components/feedback/FeedbackDialog";
import { AgentDock } from "@/components/agent/AgentDock";
import { PluginProvider } from "@/hooks/usePlugins";
import { PluginAskDialog } from "@/components/plugins/PluginAskDialog";
import { PluginsPanel } from "@/components/plugins/PluginsPanel";
import { PreferencesProvider } from "@/components/prefs/PreferencesProvider";
import { PreferencesDialog } from "@/components/prefs/PreferencesDialog";
import { SessionMemory } from "@/components/prefs/SessionMemory";

export default function App() {
  return (
    <KeymapProvider>
      {/*
        Outside `MachProvider` because `useMach` reads it: the theme is a
        preference now, and `ui.theme` mirrors it rather than owning it.
      */}
      <PreferencesProvider>
        <MachProvider>
          {/*
            Inside `MachProvider` because a plugin acts through the same actions
            the keyboard does, and outside `Shell` because its ⌘K entries and
            keybindings have to be registered before anything can reach for them.
          */}
          <PluginProvider>
            <Shell />
          </PluginProvider>
        </MachProvider>
      </PreferencesProvider>
    </KeymapProvider>
  );
}

function Shell() {
  const { ui, actions, dispatch } = useMach();
  const keymap = useKeymap();

  /**
   * `g <letter>` — Gmail's jump set, and the one place Mach's keymap and
   * Gmail's genuinely collided.
   *
   * Gmail spends `g i` on "go to Inbox". Mach spent it on "go to mail", which
   * is the same gesture aimed one level higher, so the two could not both be
   * right. Gmail wins, and loses nothing: `g i` still leaves the calendar, it
   * just also says which mailbox you land in. Mode switching keeps `g c` for
   * the calendar and takes `g m` for mail — a letter Gmail does not use — so
   * "get me back to mail without moving my mailbox" is still one gesture.
   *
   * These are global rather than mail-scoped on purpose: `g s` from the
   * calendar means "show me my starred mail", which is exactly what somebody
   * with Gmail in their hands expects, and gating it on mail mode would make
   * the keys work only where you already are.
   *
   * The exception is `g d`. The calendar has meant "go to date" by it since
   * before this, that is the Google Calendar idiom, and a date picker is worth
   * more than a second route to Drafts on a surface with no drafts on it. So
   * `g d` is Drafts in mail and Go to date in the calendar — mutually
   * exclusive `when`s, no tie for `conflicts()` to report.
   */
  const goToMailbox = (labelId: LabelId) => {
    actions.setMode("mail");
    dispatch({ type: "label", labelId });
  };

  /*
   * `?` is global, not per-mode.
   *
   * It lived inside CalendarMode for a while, so pressing it in mail did
   * nothing — which is the opposite of what a discovery surface is for. The
   * bindings are snapshotted at keypress rather than read while the sheet is
   * open, because opening it is itself a mode change: most bindings gate
   * themselves off, so asking the registry then answers "almost nothing".
   */
  const [shortcuts, setShortcuts] = useState<readonly KeyBinding[] | null>(null);

  useKeyBindings([
    {
      keys: "?",
      group: "Global",
      description: "Keyboard shortcuts",
      priority: 90,
      when: () => !ui.paletteOpen,
      handler: () => setShortcuts((open) => (open ? null : keymap.active())),
    },
    {
      keys: "escape",
      priority: 120,
      allowInInput: true,
      when: () => shortcuts !== null,
      handler: () => setShortcuts(null),
    },
    /*
     * While the sheet is up, nothing else answers a key.
     *
     * It is a reference card, not a mode you act from — and without this,
     * reading it with `j` would walk the list underneath it. Escape sits
     * above this at 120 and closes.
     */
    {
      keys: "*",
      priority: 119,
      allowInInput: true,
      when: () => shortcuts !== null,
      handler: () => {},
    },
    /*
     * ⌘Z and ⇧⌘Z — global, because undo is.
     *
     * Three things had to be true at once here, and all three are properties
     * of the registry rather than of this handler:
     *
     *  * **Text editing keeps ⌘Z.** No `allowInInput`, so the dispatcher drops
     *    these bindings entirely while focus is in the composer, an address
     *    field or the palette — and because nothing matched, nothing calls
     *    `preventDefault`, so the WebView's own undo handles the keystroke and
     *    ⌘Z means "un-type that" exactly where it should.
     *  * **The feedback dialog keeps winning.** Its ⌘Z is registered at
     *    priority 300 to undo an annotation stroke; the composer's undo-send
     *    sits at 120 inside its ten seconds. Both outrank this, so this is the
     *    one that gives way — and because they differ in priority,
     *    `conflicts()` sees no tie and reports nothing new.
     *  * **A double fire is harmless.** ⌘Z can arrive from the keyboard and
     *    from the Edit menu, and `runUndo` pops the stack through a ref before
     *    it dispatches anything, so the second arrival takes the next entry
     *    rather than running this one's inverse twice.
     *
     * Mode-scoped `z` stays exactly where it was in mail and the calendar —
     * Gmail's key, the same action, the same stack.
     */
    {
      keys: "mod+z",
      group: "Global",
      description: "Undo",
      handler: () => actions.undo(),
    },
    {
      keys: "shift+mod+z",
      group: "Global",
      description: "Redo",
      handler: () => actions.redo(),
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

    /* The jump set. Rail order, not Gmail's documentation order — this is the
       list on screen to the left, and the keys should read down it. */
    {
      keys: "g i",
      group: "Go to",
      description: "Inbox",
      handler: () => goToMailbox("INBOX"),
    },
    {
      keys: "g s",
      group: "Go to",
      description: "Starred",
      handler: () => goToMailbox("STARRED"),
    },
    {
      keys: "g b",
      group: "Go to",
      description: "Snoozed",
      handler: () => goToMailbox("SNOOZED"),
    },
    {
      // Mail only. The calendar keeps `g d` for its date picker — see above.
      keys: "g d",
      group: "Go to",
      description: "Drafts",
      when: () => ui.mode === "mail",
      handler: () => goToMailbox("DRAFT"),
    },
    {
      keys: "g t",
      group: "Go to",
      description: "Sent",
      handler: () => goToMailbox("SENT"),
    },
    {
      // Gmail calls this "All mail". Mach has no all-mail view — the rail's
      // Archive is the nearest mailbox to it, so the key goes there and the
      // sheet says what it actually opens rather than what Gmail calls it.
      keys: "g a",
      group: "Go to",
      description: "Archive",
      handler: () => goToMailbox("ARCHIVE"),
    },
    {
      keys: "g m",
      group: "Go to",
      description: "Mail, same mailbox",
      handler: () => actions.setMode("mail"),
    },
    {
      keys: "g c",
      group: "Go to",
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
      <ShortcutSheet
        open={shortcuts !== null}
        bindings={shortcuts ?? []}
        onClose={() => setShortcuts(null)}
      />
      <AddAccountDialog />
      <FeedbackDialog />
      <PluginAskDialog />
      <PluginsPanel />
      <PreferencesDialog />
      {/* Renders nothing; it is the one place `ui` and the store are both in
          scope, which is what remembering where the window was needs. */}
      <SessionMemory />
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
