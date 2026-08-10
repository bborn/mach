/**
 * Search, as a view rather than as a popup.
 *
 * ⌘K already answered "find me that one thing" — six fuzzy rows, gone as soon
 * as you pick one. This is the other half: a query you can keep, walk with
 * `j`/`k`, page through, and read the interpretation of. It replaces the thread
 * list in the same pane rather than covering the screen, so the reading pane
 * beside it keeps working and opening a result is the same gesture it is in the
 * mailbox — which is exactly what Gmail does, and why `e`, `s` and `#` still
 * archive, star and trash the row under the cursor while a search is up.
 *
 * # Why this component owns the pane
 *
 * It renders `children` — the mailbox list — whenever no search is live. That
 * inversion is what keeps `MailMode` to a two-line change and keeps every piece
 * of search state (the query, the parse, the page, the cursor) in one file
 * instead of threaded through the app's reducer. Search is a *mode of the list
 * pane*, and this is the file that says so.
 */

import { Search, X } from "lucide-react";
import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import type { Label, Thread, ThreadCursor } from "@/types";
import { getDataSource } from "@/lib/data";
import { useKeyBindings } from "@/hooks/useKeymap";
import { overlayOwnsKeyboard, useMach } from "@/hooks/useMach";
import { mailboxName } from "@/lib/mailboxes";
import {
  SEARCH_OPERATORS,
  parseSearchQuery,
  type ParsedSearch,
} from "@/lib/search-query";
import { cn } from "@/lib/utils";
import { BareInput } from "@/components/ui/input";
import { ScrollArea } from "@/components/ui/scroll-area";
import { SEARCH_EVENT, type SearchEventDetail } from "@/components/search/palette";
import { ThreadRow } from "./ThreadRow";

/** A page of results. Big enough that ⌘↓ has somewhere to go, small enough to be instant. */
const PAGE_SIZE = 60;

/**
 * How long the box waits before asking SQLite.
 *
 * Short, because the query is local and typically answers in tens of
 * milliseconds — but not zero, because a full-text term is only meaningful once
 * a couple of letters are in and `i`, `in`, `inv` would each cost a real query.
 */
const DEBOUNCE_MS = 120;

export function SearchView({ children }: { children: ReactNode }) {
  const { ui, dispatch, actions, accounts, labels, accountById, isUnread } = useMach();

  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<Thread[]>([]);
  const [cursor, setCursor] = useState<ThreadCursor | null>(null);
  const [loading, setLoading] = useState(false);
  const [loadingMore, setLoadingMore] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const input = useRef<HTMLInputElement>(null);
  const scroller = useRef<HTMLDivElement>(null);

  const live = open && ui.mode === "mail" && !overlayOwnsKeyboard(ui);
  // Bindings that move a cursor must not fire while the caret is in the box —
  // `j` there is a letter. The registry already refuses non-`allowInInput`
  // bindings inside an input; this is the same question asked of *our* input,
  // for the ones that do allow it.
  const typing = () => document.activeElement === input.current;

  const parsed = useMemo<ParsedSearch>(
    () => parseSearchQuery(query, { prefixLastTerm: true }),
    [query],
  );

  const focusInput = useCallback(() => {
    // Two frames: the input may not exist yet on the first, because opening the
    // view is what mounts it.
    requestAnimationFrame(() => {
      input.current?.focus();
      input.current?.select();
    });
  }, []);

  const start = useCallback(
    (text: string) => {
      setOpen(true);
      setQuery(text);
      actions.setPalette(false);
      focusInput();
    },
    [actions, focusInput],
  );

  const close = useCallback(() => {
    setOpen(false);
    setResults([]);
    setError(null);
    input.current?.blur();
  }, []);

  /* ---------------------------------------------------------------- ⌘K ---- */

  // The palette's search row hands the sentence over through a window event,
  // for the same reason the composer and the plugins panel do: those surfaces
  // live in other subtrees and must not import this one.
  useEffect(() => {
    const onSearch = (event: Event) => {
      const detail = (event as CustomEvent<SearchEventDetail>).detail;
      actions.setMode("mail");
      start(detail?.query ?? "");
    };
    window.addEventListener(SEARCH_EVENT, onSearch);
    return () => window.removeEventListener(SEARCH_EVENT, onSearch);
  }, [actions, start]);

  /* ------------------------------------------------------------ running --- */

  const source = getDataSource();
  const node = parsed.node;
  const accountId = ui.accountId;

  useEffect(() => {
    if (!open) return;
    if (!node) {
      setResults([]);
      setCursor(null);
      setLoading(false);
      return;
    }
    let alive = true;
    setLoading(true);
    const timer = window.setTimeout(() => {
      void source
        .searchThreads(query, PAGE_SIZE, { filter: node, accountId })
        .then((page) => {
          if (!alive) return;
          setResults(page.threads);
          setCursor(page.nextCursor);
          setError(null);
          setLoading(false);
        })
        .catch((caught: unknown) => {
          if (!alive) return;
          setResults([]);
          setCursor(null);
          setError(caught instanceof Error ? caught.message : "Search failed");
          setLoading(false);
        });
    }, DEBOUNCE_MS);
    return () => {
      alive = false;
      window.clearTimeout(timer);
    };
    // `query` is deliberately not a dependency: `node` is what the query *means*,
    // and typing a space or reordering words that parse the same must not cost
    // a second query. It is passed along only so the backend can rank the raw
    // text if it ever wants to.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, node, accountId, source]);

  const loadMore = useCallback(() => {
    if (!cursor || loadingMore || !node) return;
    setLoadingMore(true);
    void source
      .searchThreads(query, PAGE_SIZE, { filter: node, accountId, cursor })
      .then((page) => {
        setResults((rows) => [...rows, ...page.threads]);
        setCursor(page.nextCursor);
      })
      .catch(() => {
        /* the page that is already on screen is still true */
      })
      .finally(() => setLoadingMore(false));
  }, [cursor, loadingMore, node, source, query, accountId]);

  /* ------------------------------------------------------------- cursor --- */

  const index = useMemo(
    () => results.findIndex((t) => t.id === ui.threadId),
    [results, ui.threadId],
  );

  const openAt = useCallback(
    (next: number) => {
      const target = results[Math.max(0, Math.min(next, results.length - 1))];
      if (!target) return;
      // Same gesture as the mailbox: moving the cursor opens the conversation,
      // which is what keeps `e`/`s`/`#` acting on the row you are looking at.
      dispatch({ type: "thread", threadId: target.id });
      if (next >= results.length - 5) loadMore();
      requestAnimationFrame(() => {
        scroller.current
          ?.querySelector<HTMLElement>(`[data-thread-id="${target.id}"]`)
          ?.scrollIntoView({ block: "nearest" });
      });
    },
    [results, dispatch, loadMore],
  );

  const move = useCallback(
    (delta: number) => {
      if (results.length === 0) return;
      openAt(index === -1 ? 0 : index + delta);
    },
    [index, openAt, results.length],
  );

  /* ---------------------------------------------------------- keyboard --- */

  /*
   * `/` and ⌘F are registered above the palette at 210, which also puts them
   * above the floor a dialog's claim on the keyboard imposes — so unlike the
   * rest of mail they are not silenced by it and have to say so themselves.
   * Searching the list behind preferences is not destructive; it is still the
   * list answering a key that was not aimed at it.
   */
  const mail = ui.mode === "mail" && !overlayOwnsKeyboard(ui);

  useKeyBindings([
    {
      /*
       * Gmail's search key, finally landing where Gmail puts it.
       *
       * The palette also claims `/` — with the comment that Mach "has no search
       * box to focus", which was true until this file existed. Rather than edit
       * that binding out from under another unit, this one sits *above* it in
       * priority, which the registry treats as an explicit statement about who
       * wins rather than as a conflict (see `Keymap.conflicts`). The palette
       * keeps `/` everywhere this view is not: the calendar, and while ⌘K is
       * already up.
       *
       * Undocumented here for the same reason: the palette already prints a
       * `/ — Search mail` row in the help sheet, and that row is still true.
       * Two of them would only ask the reader to work out the difference.
       */
      keys: "/",
      priority: 210,
      when: () => mail && !open,
      handler: () => start(""),
    },
    {
      // The other half of the same key, for hands that learned it from every
      // other Mac app. Unclaimed here: there is no in-page find to collide
      // with, because the reading pane is a sandboxed iframe of its own.
      keys: "mod+f",
      group: "Global",
      description: "Search mail",
      priority: 210,
      allowInInput: true,
      when: () => mail,
      handler: () => (open ? focusInput() : start("")),
    },
    {
      keys: "escape",
      group: "Search",
      description: "Back to the mailbox",
      priority: 30,
      allowInInput: true,
      when: () => live,
      handler: () => close(),
    },
    {
      keys: "j",
      group: "Search",
      description: "Next result",
      priority: 30,
      when: () => live,
      handler: () => move(1),
    },
    {
      keys: "k",
      group: "Search",
      description: "Previous result",
      priority: 30,
      when: () => live,
      handler: () => move(-1),
    },
    // Arrows work from inside the box too, so a query and its results are one
    // surface rather than two you have to tab between.
    {
      keys: "down",
      priority: 30,
      allowInInput: true,
      when: () => live,
      handler: () => move(1),
    },
    {
      keys: "up",
      priority: 30,
      allowInInput: true,
      when: () => live,
      handler: () => move(-1),
    },
    {
      keys: "enter",
      group: "Search",
      description: "Open the result",
      priority: 30,
      allowInInput: true,
      when: () => live,
      handler: () => {
        if (typing()) {
          // The results are already there — search runs as you type — so Enter
          // is "I am done typing", not "go".
          input.current?.blur();
          if (index === -1) openAt(0);
          return;
        }
        openAt(index === -1 ? 0 : index);
      },
    },
    {
      keys: "o",
      priority: 30,
      when: () => live,
      handler: () => openAt(index === -1 ? 0 : index),
    },
  ]);

  /* --------------------------------------------------------------- view --- */

  if (!open) return <>{children}</>;

  const scope = accounts.find((a) => a.id === ui.accountId)?.name ?? "All accounts";
  const count = results.length === 0 ? "" : `${results.length}${cursor ? "+" : ""}`;

  return (
    <>
      <header className="flex shrink-0 flex-col border-b border-border">
        <div className="flex h-8 items-center gap-2 px-3">
          <Search size={13} strokeWidth={1.75} className="shrink-0 text-faint-foreground" />
          <BareInput
            ref={input}
            value={query}
            aria-label="Search mail"
            placeholder="from:tawny has:attachment newer_than:7d"
            className="text-list"
            onChange={(event) => setQuery(event.target.value)}
          />
          {count && (
            <span className="shrink-0 font-mono text-micro tabular-nums text-faint-foreground">
              {count}
            </span>
          )}
          <button
            type="button"
            onClick={close}
            aria-label="Close search"
            title="Close search (Esc)"
            className="flex h-5 w-5 shrink-0 items-center justify-center rounded-[var(--radius)] text-faint-foreground hover:text-foreground"
          >
            <X size={12} strokeWidth={1.75} />
          </button>
        </div>

        {/* What the query was understood to mean. Without this, an operator that
            went unrecognised looks exactly like one that found nothing. */}
        <div className="flex min-h-6 flex-wrap items-center gap-1 px-3 pb-1.5">
          {parsed.chips.map((chip, i) => (
            <span
              key={`${chip}-${i}`}
              className="rounded-[3px] border border-border bg-surface-raised px-1 py-px text-micro text-muted-foreground"
            >
              {chip}
            </span>
          ))}
          {parsed.unknown.map((raw) => (
            <span
              key={raw}
              // The chip is already struck through, which is what "ignored"
              // looks like; saying it again in words is the tooltip explaining
              // the picture next to it.
              title="Not an operator"
              className="rounded-[3px] border border-border px-1 py-px text-micro text-faint-foreground line-through"
            >
              {raw}
            </span>
          ))}
          <span className="ml-auto shrink-0 truncate text-micro text-faint-foreground">
            {scope}
          </span>
        </div>
      </header>

      <ScrollArea ref={scroller} role="listbox" aria-label="Search results">
        {error ? (
          <div className="px-3 py-6 text-center text-list text-danger">{error}</div>
        ) : !parsed.node ? (
          <Legend />
        ) : results.length === 0 ? (
          <div className="px-3 py-6 text-center text-list text-faint-foreground">
            {loading ? "Searching…" : "Nothing matches."}
          </div>
        ) : (
          <>
            {results.map((thread) => (
              <ThreadRow
                key={thread.id}
                thread={thread}
                account={accountById(thread.accountId)}
                unread={isUnread(thread)}
                cursor={thread.id === ui.threadId}
                checked={false}
                // Search results are their own list; the mailbox's selection is
                // pruned against the mailbox, so a tick here would vanish under
                // the next sync pass.
                selecting={false}
                context={mailboxFor(thread, labels)}
                onSelect={() => openAt(results.indexOf(thread))}
                onToggle={() => {}}
              />
            ))}
            {cursor && (
              <button
                type="button"
                onClick={loadMore}
                className="w-full px-3 py-2 text-left text-micro text-faint-foreground hover:text-foreground"
              >
                {loadingMore ? "Loading more…" : "More results"}
              </button>
            )}
          </>
        )}
      </ScrollArea>
      {/*
        No legend under the results.

        It read `j k move · ⏎ open · Esc mailbox` — the first two are the same
        rows, moved the same way, as the mailbox this list replaced, and Escape
        leaving a search is the one thing every keyboard user tries first. It
        also sat one strip above the status bar's copy of the same three.
      */}
    </>
  );
}

/**
 * Which mailbox a result lives in — the one thing a search row needs that a
 * mailbox row does not, because a search spans all of them. Gmail shows the
 * same chip for the same reason.
 */
function mailboxFor(thread: Thread, labels: readonly Label[]): string | undefined {
  const ids = new Set(thread.labelIds);
  for (const [id, name] of SYSTEM_MAILBOXES) {
    if (ids.has(id)) return name;
  }
  const user = labels.find((l) => l.kind === "user" && ids.has(l.id));
  if (user) return mailboxName(user);
  // Everything in Gmail is somewhere; nowhere in particular means archived.
  return "Archive";
}

/** Checked in order, so Trash beats Inbox on a thread that is in both. */
const SYSTEM_MAILBOXES: readonly [string, string][] = [
  ["TRASH", "Trash"],
  ["SPAM", "Spam"],
  ["DRAFT", "Draft"],
  ["SENT", "Sent"],
  ["INBOX", "Inbox"],
];

/** The operator list, shown while the box is still empty. */
function Legend() {
  return (
    <div className="px-3 py-4">
      <div className="pb-2 text-micro uppercase tracking-[0.06em] text-faint-foreground">
        Operators
      </div>
      <div className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1">
        {SEARCH_OPERATORS.map(({ syntax, hint }) => (
          <div key={syntax} className="contents">
            <code className="font-mono text-micro text-muted-foreground">{syntax}</code>
            <span className={cn("truncate text-micro text-faint-foreground")}>{hint}</span>
          </div>
        ))}
      </div>
    </div>
  );
}
