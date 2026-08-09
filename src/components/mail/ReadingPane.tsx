import { useEffect, useState } from "react";
import { Archive, Bookmark, Clock, CornerUpLeft } from "lucide-react";
import { useMach } from "@/hooks/useMach";
import { ACCOUNT_BG } from "@/lib/colors";
import { fullDate } from "@/lib/time";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Hint, Kbd } from "@/components/ui/kbd";
import { ScrollArea } from "@/components/ui/scroll-area";
import { ThreadMessage } from "./ThreadMessage";
import { PluginViews } from "@/components/plugins/PluginView";

/**
 * The conversation.
 *
 * Messages are collapsed to their headers with the latest one open, which is
 * what a thread of forty replies needs and what a thread of one costs nothing.
 * Bodies render as real HTML inside a sandboxed frame — see
 * `MessageFrame.tsx` and `docs/message-rendering-invariants.md`.
 */
export function ReadingPane() {
  const { detail, detailLoading, ui, accountById, actions, live, isFavorite, threadFavorite } =
    useMach();
  const threadId = detail?.thread.id ?? null;

  // Which messages the reader has opened or closed by hand. Everything else
  // falls back to "the latest one is open".
  const [toggled, setToggled] = useState<Record<number, boolean>>({});
  useEffect(() => setToggled({}), [threadId]);

  if (!detail && detailLoading && ui.threadId !== null) {
    /*
     * A local read, so this is a frame or two — but "no conversation selected"
     * would be a lie for those frames, and a flash of the empty state under
     * every ⏎ reads as the app losing the selection.
     *
     * Nothing is said about it. Naming the wait is the software talking about
     * itself: the reader pressed a key a moment ago and knows what they asked
     * for, and by the time the sentence is read it is gone. An empty pane for
     * two frames is what a fast app looks like.
     */
    return <div className="flex min-h-0 flex-1" />;
  }

  if (!detail) {
    return (
      <div className="flex min-h-0 flex-1 flex-col items-center justify-center gap-3">
        <div className="text-list text-faint-foreground">No conversation selected</div>
        <div className="flex flex-wrap items-center justify-center gap-x-4 gap-y-2">
          <Hint keys={["j", "k"]} label="move" />
          <Hint keys={["enter"]} label="open" />
          <Hint keys={["e"]} label="archive" />
          <Hint keys={["mod+k"]} label="search" />
        </div>
      </div>
    );
  }

  const { thread, messages } = detail;
  const account = accountById(thread.accountId);
  const latestId = messages.length > 0 ? messages[messages.length - 1]!.id : null;
  const favorited = isFavorite(threadFavorite);

  return (
    <>
      <header className="shrink-0 border-b border-border px-5 py-3">
        <div className="flex items-start gap-3">
          <div className="min-w-0 flex-1">
            <h1 className="truncate text-reading font-medium text-foreground">{thread.subject}</h1>
            <div className="mt-1 flex min-w-0 items-center gap-2 text-micro text-faint-foreground">
              {account && (
                <span className="flex min-w-0 items-center gap-1.5">
                  <span
                    className={cn(
                      "h-1.5 w-1.5 shrink-0 rounded-full",
                      ACCOUNT_BG[account.colorIndex],
                    )}
                  />
                  <span className="truncate">{account.email}</span>
                </span>
              )}
              <span aria-hidden>·</span>
              <span className="shrink-0">
                {thread.messageCount} message{thread.messageCount === 1 ? "" : "s"}
              </span>
              <span aria-hidden>·</span>
              <span className="truncate">{fullDate(thread.timestamp)}</span>
            </div>
          </div>

          <div className="flex shrink-0 items-center gap-1">
            <Button
              size="icon"
              title={favorited ? "Remove from favorites (⇧F)" : "Add to favorites (⇧F)"}
              aria-pressed={favorited}
              className={favorited ? "text-accent" : undefined}
              onClick={actions.toggleFavoriteThread}
            >
              <Bookmark
                size={14}
                strokeWidth={1.75}
                fill={favorited ? "currentColor" : "none"}
              />
            </Button>
            <Button size="icon" title="Archive (e)" onClick={actions.archiveSelected}>
              <Archive size={14} strokeWidth={1.75} />
            </Button>
            {/*
              `b`, not `h`. Both keys snooze — `h` is the Superhuman habit this
              app shipped with and still answers to — but `b` is Gmail's, it is
              the one `MailMode` publishes to the shortcut sheet and the status
              bar, and it is the one a Gmail hand will reach for. A tooltip that
              names the undocumented alias teaches the wrong key.
            */}
            <Button size="icon" title="Snooze (b)" onClick={actions.snoozeSelected}>
              <Clock size={14} strokeWidth={1.75} />
            </Button>
            <Button size="icon" title="Reply (r)" onClick={actions.replySelected}>
              <CornerUpLeft size={14} strokeWidth={1.75} />
            </Button>
          </div>
        </div>
      </header>

      <ScrollArea>
        <div className="mx-auto max-w-[72ch] px-5 pb-16 pt-2">
          {/*
            The one slot a plugin may occupy in the reading pane: a line
            *above* the conversation, never a transform of the body. The
            sanitizer's output is tested against 52 attacks and is not a plugin
            API — see "message-body transforms: never" in docs/plugins.md.
          */}
          <PluginViews surface="reading-pane" threadId={thread?.id ?? null} />

          {messages.map((message) => (
            <ThreadMessage
              key={message.id}
              message={message}
              live={live}
              expanded={toggled[message.id] ?? message.id === latestId}
              onToggle={() =>
                setToggled((current) => ({
                  ...current,
                  [message.id]: !(current[message.id] ?? message.id === latestId),
                }))
              }
            />
          ))}

          <div className="mt-2 flex items-center gap-2 border-t border-border pt-4 text-micro text-faint-foreground">
            <Kbd keys="r" /> reply · <Kbd keys="a" /> reply all · <Kbd keys="f" /> forward
          </div>
        </div>
      </ScrollArea>
    </>
  );
}
