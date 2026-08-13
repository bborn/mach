import { useCallback, useEffect, useRef, useState } from "react";
import { Archive, Bookmark, Clock, CornerUpLeft, MailX, PencilLine, ShieldAlert } from "lucide-react";
import { overlayOwnsKeyboard, useMach } from "@/hooks/useMach";
import { useKeyBindings } from "@/hooks/useKeymap";
import { ACCOUNT_BG } from "@/lib/colors";
import { COMPOSER_KEYS, keyboardInComposer, loadDraftForMessage } from "@/lib/compose";
import { MESSAGE_COLUMN_MAX } from "@/lib/message-body";
import { fullDate } from "@/lib/time";
import { cn } from "@/lib/utils";
import type { MessageId, ThreadId } from "@/types";
import { Button } from "@/components/ui/button";
import { ContextMenu, ContextMenuItem } from "@/components/ui/context-menu";
import { Hint } from "@/components/ui/kbd";
import { ScrollArea } from "@/components/ui/scroll-area";
import { ThreadMessage } from "./ThreadMessage";
import {
  MESSAGE_CURSOR,
  MESSAGE_ROW,
  focusedMessageId,
  moveMessageCursor,
  replyTarget,
} from "./thread-cursor";
import { unsubscribeAction } from "./unsubscribe-offer";
import { PluginViews } from "@/components/plugins/PluginView";

/**
 * The conversation.
 *
 * Messages are collapsed to their headers with the latest one open, which is
 * what a thread of forty replies needs and what a thread of one costs nothing.
 * Bodies render as real HTML inside a sandboxed frame — see
 * `MessageFrame.tsx` and `docs/message-rendering-invariants.md`.
 *
 * # The cursor inside a conversation
 *
 * `n` and `p` step between messages, which are Gmail's keys for exactly this
 * movement, and they move the *keyboard* rather than a piece of state — each
 * message header is already a button, so focus is the cursor and the app's own
 * focus ring draws it. What that buys is the thing this pane could not do
 * before: `r`, `a` and `f` answer the message the cursor is on rather than the
 * last one in the thread. `ComposerDock` reads it back through
 * `thread-cursor.ts`.
 *
 * ⏎ is a passthrough while the cursor is in a message: the control under it is
 * a real button, so the browser expands the message or opens the menu, and
 * nothing here has to reimplement what Enter on a button means.
 */
export function ReadingPane() {
  const { detail, detailLoading, ui, accountById, actions, live, isFavorite, threadFavorite } =
    useMach();
  const threadId = detail?.thread.id ?? null;

  // Which messages the reader has opened or closed by hand. Everything else
  // falls back to "the latest one is open".
  const [toggled, setToggled] = useState<Record<number, boolean>>({});
  useEffect(() => setToggled({}), [threadId]);

  /* ------------------------------------------------- answering one message */

  /** The per-message menu: what it is hanging off, and which message it is for. */
  const [menu, setMenu] = useState<{ anchor: Element; messageId: MessageId } | null>(null);
  /** Where the keyboard goes when the menu closes — the row it was opened on. */
  const returnTo = useRef<HTMLElement | null>(null);

  /**
   * Ask the composer for a reply, a reply-all or a forward of one message.
   *
   * Through the same `mach:compose` event `actions.replySelected` uses, with
   * the message named. The composer is another unit's file and owns the draft;
   * this pane knows only which message the reader meant.
   */
  const answer = useCallback((kind: "reply" | "replyAll" | "forward", messageId: MessageId) => {
    window.dispatchEvent(
      new CustomEvent("mach:compose", { detail: { kind, replyToId: messageId } }),
    );
  }, []);

  const openMenu = useCallback((messageId: MessageId, anchor: Element) => {
    returnTo.current =
      (anchor.closest(`[${MESSAGE_ROW}]`)?.querySelector(`[${MESSAGE_CURSOR}]`) as
        | HTMLElement
        | null) ?? null;
    setMenu({ anchor, messageId });
  }, []);

  /*
   * Live only while the conversation is on screen and the keyboard is not in a
   * composer — where `n` is a character, and where a message row is not what
   * the reader is looking at.
   */
  const inThread = () =>
    ui.mode === "mail" && !overlayOwnsKeyboard(ui) && !keyboardInComposer() && detail !== null;

  const openMenuAtCursor = () => {
    const id = replyTarget();
    if (id === null) return;
    const row = document.querySelector(`[${MESSAGE_ROW}="${id}"]`);
    const anchor = row?.querySelector(`[${MESSAGE_CURSOR}]`) ?? row;
    if (anchor) openMenu(id, anchor);
  };

  useKeyBindings([
    {
      keys: "n",
      group: "Mail",
      description: "Next message",
      when: inThread,
      handler: () => void moveMessageCursor(1),
    },
    {
      keys: "p",
      group: "Mail",
      description: "Previous message",
      when: inThread,
      handler: () => void moveMessageCursor(-1),
    },
    {
      /*
       * The key belongs to the control the cursor is on, not to the list
       * behind it. `passthrough` takes the lookup away from `enter → open
       * conversation` and then leaves the event alone, so the button expands
       * its message — or opens its menu — the way any button does.
       */
      keys: "enter",
      group: "Mail",
      description: "Expand this message",
      priority: 20,
      passthrough: true,
      when: () => inThread() && focusedMessageId() !== null,
      handler: () => {},
    },
    {
      // The same menu the ⋮ opens, on the row the keyboard is on.
      // `ThreadContextMenu` yields ⇧F10 while a message holds the cursor.
      keys: "shift+f10",
      group: "Mail",
      description: "Menu for this message",
      priority: 20,
      when: () => inThread() && replyTarget() !== null && menu === null,
      handler: () => openMenuAtCursor(),
    },
    {
      keys: "contextmenu",
      priority: 20,
      when: () => inThread() && replyTarget() !== null && menu === null,
      handler: () => openMenuAtCursor(),
    },
  ]);

  /**
   * Put a draft row back in the composer.
   *
   * The row holds a message id; the editable copy is keyed by draft id, and
   * Rust owns the mapping between them because the mirror row is renamed the
   * moment Gmail accepts the push. From there this is `openArtifact` — the same
   * route the agent's "Open draft" takes, navigating to the conversation and
   * asking the composer to resume by id. One way in, not two.
   */
  const openDraft = useCallback(
    (messageId: MessageId, fallbackThreadId: ThreadId) => {
      void (async () => {
        const draft = await loadDraftForMessage(messageId).catch(() => null);
        if (!draft) {
          /*
           * A draft written in another client is no longer refused: Rust adopts
           * it on this call, building the editable copy from the message body
           * already stored and carrying the Gmail draft id, so saving updates
           * the draft that exists rather than creating a second one.
           *
           * One case is left, and it is narrow. The draft id comes from
           * `users.drafts.list` and from nowhere else, so a draft that appeared
           * between two sweeps is a message Mach has and an id it does not.
           * The sweep runs at the end of every sync pass, after the replay that
           * stored the message, so this is a gap of one failed request rather
           * than of one pass — and it closes by itself.
           */
          actions.setStatus("Draft not synced yet", "error");
          return;
        }
        actions.openArtifact({
          kind: "draft",
          draftId: draft.id,
          threadId: draft.threadId ?? fallbackThreadId,
          accountId: draft.accountId,
          label: draft.subject || "Draft",
        });
      })();
    },
    [actions],
  );

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
    /*
     * The one place in the app that still spells a binding out, because it is
     * the one place with nothing else to read. A pane that says only "No
     * conversation selected" tells a first-run user what is missing and not how
     * to fix it, and there is no button here to infer it from.
     *
     * Three rows, not seven: how to move, how to open, and where the rest of
     * them are. `e archive` and `⌘K search` used to be here too — the first is
     * not what anyone needs before they have opened anything, and the second is
     * printed on the search field at the top of the window, so it was a second
     * copy of something on screen at the same time.
     */
    return (
      <div className="flex min-h-0 flex-1 flex-col items-center justify-center gap-3">
        <div className="text-list text-faint-foreground">No conversation selected</div>
        <div className="flex flex-wrap items-center justify-center gap-x-4 gap-y-2">
          <Hint keys={["j", "k"]} label="move" />
          <Hint keys={["enter"]} label="open" />
          <Hint keys={["?"]} label="shortcuts" />
        </div>
      </div>
    );
  }

  const { thread, messages } = detail;
  const account = accountById(thread.accountId);
  const latestId = messages.length > 0 ? messages[messages.length - 1]!.id : null;
  const favorited = isFavorite(threadFavorite);
  // What `r` will actually do. Every composer route resumes an existing draft
  // on this thread rather than preparing a fresh one (`ComposerDock.open`), so
  // while one exists "Reply" is the wrong word for the button that answers.
  const pendingDraft = messages.find((message) => message.isDraft) ?? null;
  // Which message carries the offer, and what the button says about it. The
  // same function `actions.unsubscribe` resolves through, so the label and the
  // gesture cannot disagree about which sender they mean.
  const unsubscribe = unsubscribeAction(messages);

  return (
    <>
      <header className="shrink-0 border-b border-border px-5 py-3">
        <div className="flex items-start gap-3">
          <div className="min-w-0 flex-1">
            {/* The one `text-title` in mail: what this pane is about. It was
                `text-reading`, the same size as the message bodies below it. */}
            <h1 className="truncate text-title font-semibold text-foreground">{thread.subject}</h1>
            <div className="mt-1 flex min-w-0 items-center gap-2 text-micro text-faint-foreground">
              {account && (
                <span className="flex min-w-0 items-center gap-1">
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
              Only for a conversation that has an offer, which is a small
              minority of them. A permanently present, permanently disabled
              button would be four more pixels of chrome on every thread of two
              people talking, explaining a feature neither of them needs.

              The two faces are not two shades of the same thing. `Unsubscribe`
              writes to the sender; `Report spam` tells Google. Which one is
              offered is Rust's answer, not this pane's — see `unsubscribeAction`.
            */}
            {unsubscribe && (
              <Button
                size="icon"
                data-testid="unsubscribe-action"
                title={unsubscribe.label}
                aria-label={unsubscribe.label}
                onClick={actions.unsubscribe}
              >
                {unsubscribe.offer.offer === "unsubscribe" ? (
                  <MailX size={14} strokeWidth={1.75} />
                ) : (
                  <ShieldAlert size={14} strokeWidth={1.75} />
                )}
              </Button>
            )}
            {/*
              `b`, not `h`. Both keys snooze — `h` is the Superhuman habit this
              app shipped with and still answers to — but `b` is Gmail's, it is
              the one `MailMode` publishes to the shortcut sheet and the status
              bar, and it is the one a Gmail hand will reach for. A tooltip that
              names the undocumented alias teaches the wrong key.
            */}
            <Button size="icon" title="Snooze (b)" onClick={() => actions.setSnooze(true)}>
              <Clock size={14} strokeWidth={1.75} />
            </Button>
            <Button
              size="icon"
              title={pendingDraft ? "Edit draft (r)" : "Reply (r)"}
              onClick={
                pendingDraft
                  ? () => openDraft(pendingDraft.id, thread.id)
                  : actions.replySelected
              }
            >
              {pendingDraft ? (
                <PencilLine size={14} strokeWidth={1.75} />
              ) : (
                <CornerUpLeft size={14} strokeWidth={1.75} />
              )}
            </Button>
          </div>
        </div>
      </header>

      <ScrollArea>
        {/*
          The column is the message body's own measure plus this element's
          padding, not a number chosen here.

          It used to be a Tailwind cap of 72ch, which reads as "72 characters"
          and is not: `ch` is the width of a zero in the *app's* font, so the
          cap came out at 645px, and the body inside it — a different font, at
          a different size — got 605px of that. A plain-text digest wrapped at
          ninety columns needs about 625px, so every line in it spilled its last
          word onto a line of its own. See [TEXT_MEASURE] for why a plain-text
          body cannot be given less room than the sender used.
        */}
        <div className="mx-auto w-full px-5 pb-16 pt-2" style={{ maxWidth: MESSAGE_COLUMN_MAX }}>
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
              onOpenDraft={() => openDraft(message.id, thread.id)}
              onMenu={(anchor) => openMenu(message.id, anchor)}
            />
          ))}

          {/*
            One menu for the conversation, moved to whichever message asked for
            it. Eleven mounted menus would be eleven popups' worth of Base UI
            behind a pane that is mostly text.

            The items name their keys rather than spelling them out, and the
            keys are the same three the cursor answers — this is the pointer's
            copy of `r` / `a` / `f`, not a second way to reply.
          */}
          <ContextMenu
            open={menu !== null}
            onOpenChange={(next) => !next && setMenu(null)}
            anchor={menu?.anchor ?? null}
            finalFocus={returnTo}
            label="Message"
          >
            <ContextMenuItem
              shortcut={COMPOSER_KEYS.reply}
              onClick={() => menu && answer("reply", menu.messageId)}
            >
              Reply
            </ContextMenuItem>
            <ContextMenuItem
              shortcut={COMPOSER_KEYS.replyAll}
              onClick={() => menu && answer("replyAll", menu.messageId)}
            >
              Reply all
            </ContextMenuItem>
            <ContextMenuItem
              shortcut={COMPOSER_KEYS.forward}
              onClick={() => menu && answer("forward", menu.messageId)}
            >
              Forward
            </ContextMenuItem>
          </ContextMenu>

          {/*
            There is no hint strip here.

            One used to sit under the last message — `r reply · a reply all ·
            f forward` — a few pixels above the composer dock's own strip,
            which says the same four things and is made of buttons that do
            them. Two rows of one control, drawn twice. The dock's survives:
            it is persistent rather than at the end of a scroll, it is
            operable by mouse as well as by key, and it is the unit that owns
            the composer, so it is the one that can say "edit draft" when
            that is what the key will do.
          */}
        </div>
      </ScrollArea>
    </>
  );
}
