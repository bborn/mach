import { Calendar, CornerDownLeft, Mail, Search, Sparkles, Tag, Terminal, User } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import type { Participant } from "@/types";
import { useKeyBindings } from "@/hooks/useKeymap";
import { useMach } from "@/hooks/useMach";
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
  { id: "go-mail", title: "Go to mail", hint: "G then I", keywords: "inbox mail" },
  { id: "go-calendar", title: "Go to calendar", hint: "G then C", keywords: "calendar week" },
  { id: "view-day", title: "Calendar: day view", hint: "1", keywords: "day" },
  { id: "view-week", title: "Calendar: week view", hint: "2", keywords: "week" },
  { id: "view-month", title: "Calendar: month view", hint: "3", keywords: "month" },
  { id: "today", title: "Jump to today", hint: "T", keywords: "now today" },
  { id: "archive", title: "Archive conversation", hint: "E", keywords: "archive done" },
  { id: "snooze", title: "Snooze conversation", hint: "H", keywords: "snooze later" },
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
  { id: "sync-now", title: "Sync now", keywords: "sync refresh fetch" },
  { id: "theme", title: "Cycle theme (system / light / dark)", keywords: "dark light theme" },
];

export function CommandPalette() {
  const { ui, actions, dispatch, allThreads, events, labels, accountById } = useMach();

  // Every label is reachable here, which is why the rail does not list them.
  const mailboxes = useMemo(
    () => mailboxTargets(labels, (id) => accountById(id)?.name),
    [labels, accountById],
  );

  // People come from what is loaded, not from a directory: the addresses you
  // have actually corresponded with are the ones worth ranking.
  const people = useMemo<Participant[]>(() => {
    const byEmail = new Map<string, Participant>();
    for (const thread of allThreads) {
      for (const participant of thread.participants) {
        if (participant.email && !byEmail.has(participant.email)) {
          byEmail.set(participant.email, participant);
        }
      }
    }
    for (const event of events) {
      for (const attendee of event.attendees) {
        if (attendee.email && !byEmail.has(attendee.email)) {
          byEmail.set(attendee.email, attendee);
        }
      }
    }
    return [...byEmail.values()];
  }, [allThreads, events]);
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
      commands: COMMANDS,
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
          actions.setStatus(`Compose to ${email} — the composer arrives in a later unit`);
          actions.setPalette(false);
        },
      },
    });

    function runShellCommand(id: string) {
      switch (id) {
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
        case "snooze":
          return actions.snoozeSelected();
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
      }
    }
  }, [open, query, allThreads, events, people, mailboxes, actions, dispatch]);

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
      description: "Open search and commands",
      allowInInput: true,
      priority: 200,
      handler: () => actions.setPalette(!open),
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
        actions.setStatus(
          query.trim()
            ? `Agent handoff is a later unit — would ask: “${query.trim()}”`
            : "Type a question first, then ⇥ hands it to the agent",
        );
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
            {query.trim() ? "No local matches." : "Type to search. Everything here is local."}
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

      <footer className="flex h-7 shrink-0 items-center gap-3 border-t border-border px-3">
        <span className="flex items-center gap-1">
          <Kbd keys="up" />
          <Kbd keys="down" />
          <span className="text-micro text-faint-foreground">navigate</span>
        </span>
        <span className="flex items-center gap-1">
          <CornerDownLeft size={11} strokeWidth={1.75} className="text-faint-foreground" />
          <span className="text-micro text-faint-foreground">open</span>
        </span>
        <span className="ml-auto text-micro text-faint-foreground">
          operators and agent handoff land in a later unit
        </span>
      </footer>
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
