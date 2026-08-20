import { Bookmark } from "lucide-react";
import { useCallback, useEffect, useRef } from "react";
import type { ThreadId } from "@/types";
import { useMach } from "@/hooks/useMach";
import { mailboxName } from "@/lib/mailboxes";
import { cn } from "@/lib/utils";
import { ScrollArea } from "@/components/ui/scroll-area";
import { ShortcutTooltip } from "@/components/ui/tooltip";
import { MailboxNotice } from "./MailboxNotice";
import { ThreadContextMenu } from "./ThreadContextMenu";
import { GMAIL_DRAFT, ThreadRow } from "./ThreadRow";

export function ThreadList() {
  const {
    visibleThreads,
    accounts,
    labels,
    ui,
    state,
    hasMore,
    loadingMore,
    isUnread,
    isFavorite,
    isRowSelected,
    viewFavorite,
    actions,
  } = useMach();
  const scroller = useRef<HTMLDivElement>(null);
  const sentinel = useRef<HTMLDivElement>(null);

  // One click handler for every row, for the life of the list.
  //
  // `actions` is rebuilt whenever the cursor moves — `ui.threadId` is in its
  // dependency list, and has to be — so anything derived from it directly would
  // change identity on every `j`, and a changed prop is a re-render of all three
  // hundred rows. Reading it through a ref is the same trick `useKeyBindings`
  // uses for the same reason: the handler always sees the current actions, and
  // the rows never see a new handler.
  const latest = useRef(actions);
  latest.current = actions;
  const clickThread = useCallback(
    (id: ThreadId, modifiers: { extend: boolean; toggle: boolean }) =>
      latest.current.clickThread(id, modifiers),
    [],
  );

  // Keyboard selection has to drag the viewport with it, but only as far as
  // strictly necessary — `nearest` keeps the list from jumping under the eye.
  useEffect(() => {
    if (ui.threadId === null) return;
    const row = scroller.current?.querySelector<HTMLElement>(
      `[data-thread-id="${ui.threadId}"]`,
    );
    row?.scrollIntoView({ block: "nearest" });
  }, [ui.threadId]);

  // Infinite scroll: one more page when the tail comes into view, and never
  // more than one request in flight (`loadMore` is a no-op while loading).
  //
  // `actions` is read through the ref here too, and for a sharper reason than
  // render cost: with it in the dependency list this effect tore down and
  // rebuilt an `IntersectionObserver` on every cursor move, and an observer
  // reports nothing until the next layout — so the page that should have been
  // fetched while the sentinel sat in the 600px margin was fetched a keystroke
  // later than it should have been.
  useEffect(() => {
    const target = sentinel.current;
    const root = scroller.current;
    if (!target || !root || !hasMore) return;
    const observer = new IntersectionObserver(
      (entries) => {
        if (entries.some((entry) => entry.isIntersecting)) latest.current.loadMore();
      },
      { root, rootMargin: "600px 0px" },
    );
    observer.observe(target);
    return () => observer.disconnect();
  }, [hasMore, visibleThreads.length]);

  const scope = accounts.find((a) => a.id === ui.accountId)?.name;
  const label = labels.find((l) => l.id === ui.labelId);
  const mailbox = label ? mailboxName(label) : ui.labelId;
  const unreadCount = visibleThreads.filter(isUnread).length;
  const selectedCount = ui.selection.ids.length;
  // Any view can be pinned, which is what makes labels living in ⌘K survivable:
  // find one once, keep it in the rail for as long as it is worth a row.
  const favorited = isFavorite(viewFavorite);

  return (
    <>
      <header className="group flex h-8 shrink-0 items-center gap-2 border-b border-border px-3">
        <span className="truncate text-list font-medium text-foreground">{mailbox}</span>
        {scope && <span className="truncate text-micro text-faint-foreground">{scope}</span>}
        <ShortcutTooltip
          label={favorited ? "Remove from favorites" : "Add to favorites"}
          keys="shift+f"
        >
        <button
          type="button"
          onClick={actions.toggleFavoriteView}
          aria-pressed={favorited}
          aria-label={favorited ? "Remove from favorites" : "Add to favorites"}
          className={cn(
            "ml-auto flex h-5 w-5 shrink-0 items-center justify-center rounded-[var(--radius)]",
            favorited
              ? "text-accent"
              : "text-faint-foreground opacity-0 hover:text-foreground focus-visible:opacity-100 group-hover:opacity-100",
          )}
        >
          <Bookmark
            size={12}
            strokeWidth={1.75}
            fill={favorited ? "currentColor" : "none"}
          />
        </button>
        </ShortcutTooltip>
        {/* While a selection is live the header counts that instead: it is the
            number the next keystroke is about to act on. */}
        {selectedCount > 0 ? (
          <span className="shrink-0 font-mono text-micro tabular-nums text-accent">
            {selectedCount} selected
          </span>
        ) : (
          <span className="shrink-0 font-mono text-micro tabular-nums text-faint-foreground">
            {unreadCount > 0 ? `${unreadCount} / ` : ""}
            {visibleThreads.length}
            {hasMore ? "+" : ""}
          </span>
        )}
      </header>

      <ScrollArea ref={scroller} role="listbox" aria-label="Conversations">
        {state.kind !== "ready" ? (
          <MailboxNotice />
        ) : (
          <>
            <ThreadContextMenu>
            {visibleThreads.map((thread) => (
              <ThreadRow
                key={thread.id}
                thread={thread}
                unread={isUnread(thread)}
                cursor={thread.id === ui.threadId}
                checked={isRowSelected(thread.id)}
                selecting={selectedCount > 0}
                // Everything in Drafts is a draft; the mark belongs where the
                // fact is news, which is Inbox, a label, or the unified list.
                draft={ui.labelId !== GMAIL_DRAFT && thread.labelIds.includes(GMAIL_DRAFT)}
                // Which gesture this is depends on the modifiers; the state
                // layer owns the rules, the row just reports what was held.
                onSelect={clickThread}
              />
            ))}
            </ThreadContextMenu>
            <div ref={sentinel} aria-hidden className="h-px" />
            {hasMore && (
              <div className="px-3 py-2 text-micro text-faint-foreground">
                {loadingMore ? "Loading more…" : "Scroll for more"}
              </div>
            )}
          </>
        )}
      </ScrollArea>
    </>
  );
}
