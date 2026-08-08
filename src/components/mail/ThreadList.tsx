import { Bookmark } from "lucide-react";
import { useEffect, useRef } from "react";
import { useMach } from "@/hooks/useMach";
import { mailboxName } from "@/lib/mailboxes";
import { cn } from "@/lib/utils";
import { ScrollArea } from "@/components/ui/scroll-area";
import { MailboxNotice } from "./MailboxNotice";
import { ThreadRow } from "./ThreadRow";

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
    accountById,
    actions,
  } = useMach();
  const scroller = useRef<HTMLDivElement>(null);
  const sentinel = useRef<HTMLDivElement>(null);

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
  useEffect(() => {
    const target = sentinel.current;
    const root = scroller.current;
    if (!target || !root || !hasMore) return;
    const observer = new IntersectionObserver(
      (entries) => {
        if (entries.some((entry) => entry.isIntersecting)) actions.loadMore();
      },
      { root, rootMargin: "600px 0px" },
    );
    observer.observe(target);
    return () => observer.disconnect();
  }, [hasMore, actions, visibleThreads.length]);

  const scope = accounts.find((a) => a.id === ui.accountId)?.name ?? "All accounts";
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
        <span className="truncate text-micro text-faint-foreground">{scope}</span>
        <button
          type="button"
          onClick={actions.toggleFavoriteView}
          title={favorited ? "Remove from favorites (⇧F)" : "Add to favorites (⇧F)"}
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
            {visibleThreads.map((thread) => (
              <ThreadRow
                key={thread.id}
                thread={thread}
                account={accountById(thread.accountId)}
                unread={isUnread(thread)}
                cursor={thread.id === ui.threadId}
                checked={isRowSelected(thread.id)}
                selecting={selectedCount > 0}
                // Which gesture this is depends on the modifiers; the state
                // layer owns the rules, this just reports what was held.
                onSelect={(event) =>
                  actions.clickThread(thread.id, {
                    extend: event.shiftKey,
                    toggle: event.metaKey || event.ctrlKey,
                  })
                }
                onToggle={() => actions.clickThread(thread.id, { extend: false, toggle: true })}
              />
            ))}
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
