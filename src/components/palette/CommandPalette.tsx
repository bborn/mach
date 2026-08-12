import { Calendar, Mail, Search, Sparkles, Tag, Terminal, User } from "lucide-react";
import { useEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";
import type { Participant } from "@/types";
import { useKeyBindings } from "@/hooks/useKeymap";
import { useMach } from "@/hooks/useMach";
import { useContacts } from "@/hooks/useContacts";
import { mailboxTargets } from "@/lib/mailboxes";
import {
  KIND_LABELS,
  KIND_ORDER,
  resolve,
  type PaletteCommand,
  type PaletteResult,
  type PaletteResultKind,
} from "@/lib/palette/resolver";
import {
  applyFrecency,
  load as loadFrecency,
  record as recordUse,
  save as saveFrecency,
  type FrecencyStore,
} from "@/lib/palette/frecency";
import { ghostEnabled, setGhostEnabled } from "@/lib/ghost";
import { forcedSyncInFlight, subscribeForcedSync } from "@/lib/force-sync";
import { listTime, monthShort, shortTime } from "@/lib/time";
import { cn } from "@/lib/utils";
import { Overlay } from "@/components/ui/dialog";
import { BareInput } from "@/components/ui/input";
import { Kbd } from "@/components/ui/kbd";

const ICONS: Record<PaletteResultKind, typeof Mail> = {
  thread: Mail,
  event: Calendar,
  person: User,
  mailbox: Tag,
  command: Terminal,
  agent: Sparkles,
};

const COMMANDS: PaletteCommand[] = [
  { id: "compose", title: "New message", hint: "C", keywords: "compose write new mail send" },
  { id: "go-mail", title: "Go to mail", hint: "G then M", keywords: "inbox mail" },
  { id: "go-calendar", title: "Go to calendar", hint: "G then C", keywords: "calendar week" },
  { id: "view-day", title: "Calendar: day view", hint: "1", keywords: "day" },
  { id: "view-week", title: "Calendar: week view", hint: "2", keywords: "week" },
  { id: "view-month", title: "Calendar: month view", hint: "3", keywords: "month" },
  { id: "today", title: "Jump to today", hint: "T", keywords: "now today" },
  { id: "archive", title: "Archive conversation", hint: "E", keywords: "archive done" },
  { id: "snooze", title: "Snooze conversation", hint: "B", keywords: "snooze later" },
  {
    id: "report-spam",
    title: "Report spam",
    hint: "!",
    keywords: "spam junk phishing report block",
  },
  {
    id: "favorite-view",
    title: "Favorite this mailbox",
    hint: "⇧F",
    keywords: "favorite pin sidebar bookmark label mailbox view",
  },
  {
    id: "favorite-thread",
    title: "Favorite this conversation",
    keywords: "favorite pin sidebar bookmark conversation thread",
  },
  { id: "all-accounts", title: "Show all accounts", keywords: "unified accounts" },
  { id: "add-account", title: "Add a Google account", keywords: "account oauth connect google" },
  {
    id: "sync-now",
    title: "Sync now",
    hint: "⇧⌘R",
    keywords: "sync refresh fetch reload update check mail calendar google now force",
  },
  { id: "theme", title: "Cycle theme (system / light / dark)", keywords: "dark light theme" },
  {
    id: "plugins",
    title: "Plugins…",
    keywords: "plugin plugins extensions install add-on sandbox",
  },
  {
    id: "ghost",
    title: "Ghost completions: on / off",
    keywords: "ghost autocomplete completion suggestion ai copilot inline",
  },
];

/**
 * The commands, with the one that has a live state folded in.
 *
 * "Sync now" is the only entry here that talks to Google, so it is the only one
 * that can be *in progress* when you open the palette. Its shortcut column
 * carries that instead: ⇧⌘R when there is something to press, "Syncing" while
 * a pass is running. Pressing it again is already harmless — the action refuses
 * and the engine refuses behind it — and this is what stops it looking like
 * nothing happened.
 */
export function commandsWith(syncing: boolean): PaletteCommand[] {
  if (!syncing) return COMMANDS;
  return COMMANDS.map((command) =>
    command.id === "sync-now" ? { ...command, hint: "Syncing" } : command,
  );
}

export function CommandPalette() {
  const { ui, actions, dispatch, allThreads, events, labels, accountById } = useMach();

  const syncing = useSyncExternalStore(
    subscribeForcedSync,
    () => forcedSyncInFlight("all"),
    () => false,
  );
  const commands = useMemo(() => commandsWith(syncing), [syncing]);

  // Every label is reachable here, which is why the rail does not list them.
  const mailboxes = useMemo(
    () => mailboxTargets(labels, (id) => accountById(id)?.name),
    [labels, accountById],
  );

  // People come from what is loaded, not from a directory: the addresses you
  // have actually corresponded with are the ones worth ranking. The same index
  // the composer's To field completes from — one address book, not two.
  const contacts = useContacts();
  const people = useMemo<Participant[]>(
    () => contacts.map((contact) => ({ name: contact.name ?? contact.email, email: contact.email })),
    [contacts],
  );
  const [query, setQuery] = useState("");
  const [cursor, setCursor] = useState(0);
  const listRef = useRef<HTMLDivElement>(null);

  const open = ui.paletteOpen;

  useEffect(() => {
    if (!open) {
      setQuery("");
      setCursor(0);
    }
  }, [open]);

  const results = useMemo<PaletteResult[]>(() => {
    if (!open) return [];
    return resolve({
      query,
      threads: allThreads,
      events,
      people,
      mailboxes,
      commands,
      actions: {
        openThread: (id) => {
          actions.setMode("mail");
          dispatch({ type: "thread", threadId: id });
          actions.setPalette(false);
        },
        openMailbox: (id) => {
          actions.setMode("mail");
          dispatch({ type: "label", labelId: id });
          actions.setPalette(false);
        },
        openEvent: (id) => {
          const event = events.find((e) => e.id === id);
          actions.setMode("calendar");
          if (event) dispatch({ type: "anchor", anchor: event.start });
          dispatch({ type: "event", eventId: id });
          actions.setPalette(false);
        },
        runCommand: (id) => {
          runShellCommand(id);
          actions.setPalette(false);
        },
        composeTo: (email) => {
          actions.setMode("mail");
          window.dispatchEvent(
            new CustomEvent("mach:compose", { detail: { kind: "new", to: email } }),
          );
          actions.setPalette(false);
        },
      },
    });

    function runShellCommand(id: string) {
      switch (id) {
        // A window event rather than a call, for the same reason `plugins`
        // uses one: the composer lives in another unit's subtree.
        case "compose":
          actions.setMode("mail");
          return window.dispatchEvent(
            new CustomEvent("mach:compose", { detail: { kind: "new" } }),
          );
        case "go-mail":
          return actions.setMode("mail");
        case "go-calendar":
          return actions.setMode("calendar");
        case "view-day":
          actions.setMode("calendar");
          return actions.setCalendarView("day");
        case "view-week":
          actions.setMode("calendar");
          return actions.setCalendarView("week");
        case "view-month":
          actions.setMode("calendar");
          return actions.setCalendarView("month");
        case "today":
          return actions.goToday();
        case "archive":
          return actions.archiveSelected();
        case "report-spam":
          return actions.reportSpamSelected();
        case "snooze":
          // Hands over to the picker rather than committing a time of its own.
          // The reducer's `snooze` case shuts this palette on the way out, so
          // the two overlays never stack.
          return actions.setSnooze(true);
        case "favorite-view":
          return actions.toggleFavoriteView();
        case "favorite-thread":
          return actions.toggleFavoriteThread();
        case "all-accounts":
          return dispatch({ type: "account", accountId: null });
        case "add-account":
          return actions.setAddAccount(true);
        case "sync-now":
          return actions.syncNow();
        case "theme":
          return actions.cycleTheme();
        // A window event rather than a call, for the same reason the composer
        // uses one: the panel lives in another unit's subtree and the palette
        // must not import it.
        case "plugins":
          return window.dispatchEvent(new CustomEvent("mach:plugins"));
        // Ghost text sends what you are writing to a model. Turning that off
        // has to be one keystroke away and must not mean unsetting the key the
        // agent also uses — hence a switch of its own.
        case "ghost": {
          const next = !ghostEnabled();
          setGhostEnabled(next);
          return actions.setStatus(
            next ? "Ghost completions on" : "Ghost completions off",
            "info",
          );
        }
      }
    }
  }, [open, query, allThreads, events, people, mailboxes, commands, actions, dispatch]);

  /*
   * What he actually uses, boosting near-ties and filling the empty query.
   * Read once — the palette re-ranks on every keystroke and re-reading
   * localStorage there would put a synchronous disk hit in the type loop.
   */
  const [frecency, setFrecency] = useState<FrecencyStore>(loadFrecency);

  const ranked = useMemo(
    () => applyFrecency(results, frecency, Date.now()),
    [results, frecency],
  );

  /** Records the pick, then runs it. */
  const choose = (result: PaletteResult) => {
    setFrecency((prev) => {
      const next = recordUse(prev, result.id, Date.now());
      saveFrecency(next);
      return next;
    });
    result.run();
  };

  const grouped = useMemo(() => {
    const byKind = new Map<PaletteResultKind, PaletteResult[]>();
    for (const result of ranked) {
      const list = byKind.get(result.kind) ?? [];
      list.push(result);
      byKind.set(result.kind, list);
    }
    return KIND_ORDER.filter((kind) => byKind.get(kind)?.length).map((kind) => ({
      kind,
      // Frecency only reorders *within* a kind — commands do not get promoted
      // into the middle of mail results because they were used a lot.
      items: byKind.get(kind)!.slice().sort((a, b) => (b.score ?? 0) - (a.score ?? 0)),
    }));
  }, [ranked]);

  const flat = useMemo(() => grouped.flatMap((g) => g.items), [grouped]);
  const activeId = flat[Math.min(cursor, flat.length - 1)]?.id;

  useEffect(() => setCursor(0), [query]);

  useEffect(() => {
    listRef.current
      ?.querySelector<HTMLElement>('[data-active="true"]')
      ?.scrollIntoView({ block: "nearest" });
  }, [activeId]);

  const move = (delta: number) => {
    if (flat.length === 0) return;
    setCursor((c) => (c + delta + flat.length) % flat.length);
  };

  useKeyBindings([
    {
      keys: "mod+k",
      group: "Global",
      description: "Search and commands",
      allowInInput: true,
      priority: 200,
      handler: () => actions.setPalette(!open),
    },
    {
      /*
       * Gmail's `/` focuses its search box. Mach has no search box to focus —
       * search *is* the palette, and the palette is ⌘K — so `/` opens that
       * instead of nothing. It is the one Gmail key here that lands somewhere
       * other than where Gmail puts it, and it lands on the same intent.
       *
       * Not `allowInInput`, so a slash typed into the palette, the composer or
       * an address field is a slash.
       */
      keys: "/",
      group: "Global",
      description: "Search mail",
      priority: 200,
      when: () => !open,
      handler: () => actions.setPalette(true),
    },
    {
      keys: "escape",
      group: "Global",
      description: "Close",
      allowInInput: true,
      priority: 100,
      when: () => open,
      handler: () => actions.setPalette(false),
    },
    { keys: "down", allowInInput: true, priority: 100, when: () => open, handler: () => move(1) },
    { keys: "up", allowInInput: true, priority: 100, when: () => open, handler: () => move(-1) },
    { keys: "ctrl+n", allowInInput: true, priority: 100, when: () => open, handler: () => move(1) },
    { keys: "ctrl+p", allowInInput: true, priority: 100, when: () => open, handler: () => move(-1) },
    {
      keys: "enter",
      allowInInput: true,
      priority: 100,
      when: () => open,
      handler: () => { const r = flat[cursor]; if (r) choose(r); },
    },
    {
      // The agent handoff seam. Deliberately a keystroke, never a pause —
      // ordinary typing must stay local and instant.
      keys: "tab",
      group: "Global",
      description: "Ask the agent",
      allowInInput: true,
      priority: 100,
      when: () => open,
      handler: () => {
        // The agent unit registers its own ⇥ resolver at a higher priority and
        // takes this over. This remains only for the empty-query case, which
        // that resolver declines to claim.
        actions.setStatus("Type a question first");
      },
    },
  ]);

  return (
    <Overlay open={open} onClose={() => actions.setPalette(false)} labelledBy="palette-input">
      <div className="flex h-10 shrink-0 items-center gap-2 border-b border-border px-3">
        <Search size={14} strokeWidth={1.75} className="shrink-0 text-faint-foreground" />
        <BareInput
          id="palette-input"
          value={query}
          placeholder="Search mail, labels, calendar and people — or > for commands"
          onChange={(e) => setQuery(e.target.value)}
        />
        {query.trim() && (
          <span className="flex shrink-0 items-center gap-1 whitespace-nowrap">
            <Kbd keys="tab" />
            <span className="text-micro text-faint-foreground">ask</span>
          </span>
        )}
      </div>

      <div ref={listRef} className="min-h-0 flex-1 overflow-y-auto py-1">
        {flat.length === 0 ? (
          <div className="px-3 py-6 text-center text-list text-faint-foreground">
            {/*
              The empty prompt used to add "Everything here is local" — true,
              and an architecture note, which is a thing a person reaches for
              ⌘K to *avoid* reading. The footer already says what ⇥ does.
            */}
            {query.trim() ? "No matches" : "Type to search"}
          </div>
        ) : (
          grouped.map((group) => (
            <section key={group.kind}>
              <div className="px-3 pb-0.5 pt-2 text-micro font-medium uppercase tracking-[0.06em] text-faint-foreground">
                {KIND_LABELS[group.kind]}
              </div>
              {group.items.map((result) => {
                const Icon = ICONS[result.kind];
                const active = result.id === activeId;
                return (
                  <div
                    key={result.id}
                    data-active={active}
                    onMouseMove={() => setCursor(flat.indexOf(result))}
                    onClick={() => choose(result)}
                    className={cn(
                      "flex h-7 cursor-default items-center gap-2 px-3 text-list",
                      active ? "bg-row-selected" : "",
                    )}
                  >
                    <Icon size={13} strokeWidth={1.75} className="shrink-0 text-faint-foreground" />
                    <span className="shrink-0 truncate text-foreground" style={{ maxWidth: "22rem" }}>
                      {result.title}
                    </span>
                    {result.subtitle && (
                      <span className="min-w-0 flex-1 truncate text-micro text-faint-foreground">
                        {result.subtitle}
                      </span>
                    )}
                    <span className="ml-auto shrink-0 pl-2 font-mono text-micro text-faint-foreground">
                      {result.meta ?? metaFor(result)}
                    </span>
                  </div>
                );
              })}
            </section>
          ))
        )}
      </div>

      {/*
        No footer legend.

        It said `↑↓ navigate · ⏎ open · ⇥ asks the agent`. Arrows and Enter are
        how every palette anyone has used already works, and the third was a
        second copy of the `⇥ ask` chip in the field above — which appears the
        moment there is a query, which is the moment it means anything.
      */}
    </Overlay>
  );

  function metaFor(result: PaletteResult): string {
    if (result.kind === "thread") {
      const thread = allThreads.find((t) => `thread:${t.id}` === result.id);
      return thread ? listTime(thread.timestamp) : "";
    }
    if (result.kind === "event") {
      const event = events.find((e) => `event:${e.id}` === result.id);
      // Recurring instances share a title, so the date is what tells them apart.
      if (!event) return "";
      const day = new Date(event.start);
      return `${monthShort(day)} ${day.getDate()} · ${shortTime(event.start)}`;
    }
    return "";
  }
}
