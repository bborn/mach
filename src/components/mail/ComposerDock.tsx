import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useMach } from "@/hooks/useMach";
import { useKeyBindings } from "@/hooks/useKeymap";
import { usePreferences } from "@/components/prefs/PreferencesProvider";
import { composeAccountId, sendDelayMs, signatureFor, withSignature } from "@/lib/prefs";
import { Kbd } from "@/components/ui/kbd";
import { Overlay } from "@/components/ui/dialog";
import { Composer, type ComposerPresentation } from "./Composer";
import { clockTime } from "@/lib/time";
import { cn } from "@/lib/utils";
import {
  COMPOSER_KEYS,
  createAutosave,
  discardDraft,
  flushOutbox,
  loadDraft,
  loadDraftForThread,
  newDraft,
  prepareDraft,
  saveDraft,
  sendDraft,
  undoSend,
  type Draft,
  type DraftKind,
  type OutboxEntry,
} from "@/lib/compose";

/**
 * Everything around the composer: when it opens, what it does with the draft,
 * and the ten seconds after `⌘⏎`.
 *
 * It sits at the bottom of the reading pane rather than in a modal, so the
 * message being answered stays on screen — which is the one thing every
 * separate-window composer gets wrong and the reason the spec chose inline.
 *
 * # The undo window
 *
 * `⌘⏎` does not send. It queues: Rust builds the RFC822 bytes, commits them to
 * SQLite with a `sendAfter` the send-delay preference away — ten seconds unless
 * ⌘, says otherwise, and zero is a legal answer — and writes the reply into the
 * thread so the conversation repaints immediately. This component then shows a
 * strip with a countdown, and when the countdown reaches zero asks Rust to
 * flush. Undo deletes the row before anything has left, and puts the text back
 * in the composer rather than leaving the user to retype it.
 *
 * The timer here is a *prompt*, not the mechanism. If this window closes at
 * second four the row is still in the database and the next flush — on the next
 * launch — sends it. That is why `flushOutbox()` runs on mount.
 */
export function ComposerDock() {
  const { ui, detail, accounts, actions } = useMach();
  const prefs = usePreferences();
  const threadId = ui.threadId;
  const active = ui.mode === "mail" && !ui.paletteOpen;

  /**
   * Put the account's signature under a draft that has not been written in yet.
   *
   * Only an empty body gets one. A draft that came back from SQLite with text
   * in it has already been signed — or deliberately unsigned — and appending
   * again would either duplicate the block or undo a decision. `withSignature`
   * is idempotent as a second line of defence, but the emptiness check is the
   * one that respects "I deleted my signature on this one".
   */
  const signed = useCallback(
    (draft: Draft): Draft => {
      const signature = signatureFor(prefs, draft.accountId);
      if (!signature || draft.body.trim() !== "") return draft;
      return { ...draft, body: withSignature(draft.body, signature) };
    },
    [prefs],
  );

  const [draft, setDraft] = useState<Draft | null>(null);
  const [pending, setPending] = useState<OutboxEntry | null>(null);
  const [busy, setBusy] = useState(false);
  const [now, setNow] = useState(() => Date.now());

  // The draft that was queued, kept so undo restores the text rather than an
  // empty composer.
  const recalled = useRef<Draft | null>(null);
  // Where focus starts in the new-message overlay. See `Composer`'s `toRef`.
  const toField = useRef<HTMLInputElement>(null);

  /* ---------------------------------------------------------------- saving */

  const autosave = useMemo(
    () =>
      createAutosave((next) => {
        // A draft with nothing in it is not worth a row; it would also
        // resurrect an empty composer every time the thread is reopened.
        if (next.body.trim() === "" && next.to.length === 0) return;
        void saveDraft(next).catch(() => {
          /* the editor must not lose focus because a write failed */
        });
      }),
    [],
  );

  const change = useCallback(
    (next: Draft) => {
      setDraft(next);
      autosave.queue(next);
    },
    [autosave],
  );

  const close = useCallback(() => {
    autosave.flush();
    setDraft(null);
  }, [autosave]);

  /*
   * Switching conversations closes the composer and saves what was typed —
   * unless the draft in it belongs to the conversation being switched *to*.
   *
   * That exception is what makes "open this draft" work from anywhere. Opening
   * an artifact navigates and then asks for the draft, and those two arrive in
   * whichever order React schedules them; without this the navigation would
   * close the composer the request had just filled. Keeping a draft that
   * already matches the thread is also simply correct on its own.
   */
  useEffect(() => {
    autosave.flush();
    setDraft((current) =>
      current && (current.threadId ?? null) === threadId ? current : null,
    );
  }, [threadId, autosave]);

  /* --------------------------------------------------------------- opening */

  const open = useCallback(
    (kind: DraftKind) => {
      if (threadId === null) return;
      void (async () => {
        // A half-written reply wins over a freshly prepared one: reopening a
        // conversation must not throw away what you already typed.
        const existing = await loadDraftForThread(threadId).catch(() => null);
        if (existing && existing.body.trim() !== "") {
          setDraft(existing);
          return;
        }
        const prepared = await prepareDraft(threadId, kind).catch((error: unknown) => {
          actions.setStatus(errorMessage(error), "error");
          return null;
        });
        // `prepare` reads the thread — its account, its recipients, its
        // References header — and comes back with an empty body. The signature
        // is the one thing it cannot know, because it is not in the thread.
        if (prepared) setDraft(signed(prepared));
      })();
    },
    [threadId, actions, signed],
  );

  /**
   * `c` — a message to somebody, from nothing.
   *
   * This was the one thing the keyboard could not do. Every composer route ran
   * through a thread: reply, reply-all and forward all need one, so writing to
   * a person you were not already in a conversation with had no key at all and
   * no menu item either. Gmail spends `c` on it, and so does this.
   *
   * It opens over the reading pane like every other composer here, whether or
   * not a conversation is open, because the alternative is a window and the
   * whole point of the inline composer is that there isn't one.
   */
  const openNew = useCallback(
    (to?: string) => {
      // The account the list is scoped to, then the preference, then the first
      // one — see `composeAccountId`. This is the *only* composer route with a
      // choice to make: every other one is answering a thread that already
      // belongs to an account.
      const accountId = composeAccountId(prefs, accounts, ui.accountId);
      if (accountId === undefined) {
        actions.setStatus("Add an account before writing a message", "error");
        return;
      }
      const blank = signed(newDraft(accountId));
      // ⌘K's "write to this person" arrives here with an address already in
      // hand; `c` arrives with none, and the composer puts the cursor in the
      // To field for exactly that case.
      setDraft(to ? { ...blank, to: [{ email: to }] } : blank);
    },
    [ui.accountId, accounts, actions, prefs, signed],
  );

  /**
   * Resume a specific draft — the other end of the agent's "Open draft".
   *
   * By id, not by thread: the agent can leave two drafts on one conversation,
   * and `loadDraftForThread` would hand back whichever was touched last rather
   * than the one whose button was pressed.
   */
  const resume = useCallback(
    (draftId: string) => {
      void (async () => {
        const found = await loadDraft(draftId).catch(() => null);
        if (found) setDraft(found);
        else actions.setStatus("That draft is gone", "error");
      })();
    },
    [actions],
  );

  // The reading pane's reply button lives in another unit's file, so it asks
  // for the composer through an event rather than a prop. So does the agent
  // drawer, through `actions.openArtifact`.
  useEffect(() => {
    const onRequest = (event: Event) => {
      const detail = (event as CustomEvent<{ kind?: string; to?: string; draftId?: string }>)
        .detail;
      const kind = detail?.kind ?? "reply";
      if (kind === "draft" && detail?.draftId) resume(detail.draftId);
      else if (kind === "new") openNew(detail?.to);
      else open(kind as DraftKind);
    };
    window.addEventListener("mach:compose", onRequest);
    return () => window.removeEventListener("mach:compose", onRequest);
  }, [open, openNew, resume]);

  useKeyBindings([
    {
      keys: COMPOSER_KEYS.compose,
      group: "Write",
      description: "New message",
      priority: 5,
      when: () => active && draft === null,
      handler: () => openNew(),
    },
    {
      keys: COMPOSER_KEYS.reply,
      group: "Write",
      description: "Reply",
      priority: 5,
      when: () => active && draft === null,
      handler: () => open("reply"),
    },
    {
      keys: COMPOSER_KEYS.replyAll,
      group: "Write",
      description: "Reply all",
      priority: 5,
      when: () => active && draft === null,
      handler: () => open("replyAll"),
    },
    {
      keys: COMPOSER_KEYS.forward,
      group: "Write",
      description: "Forward",
      priority: 5,
      when: () => active && draft === null,
      handler: () => open("forward"),
    },
    {
      keys: COMPOSER_KEYS.undoSend,
      group: "Write",
      description: "Undo send",
      allowInInput: true,
      priority: 120,
      when: () => pending !== null && pending.state === "holding",
      handler: () => void recall(),
    },
  ]);

  /* --------------------------------------------------------------- sending */

  /** How long a queued message waits before it actually leaves. */
  const delay = sendDelayMs(prefs);

  const send = useCallback(
    (scheduleAt?: number) => {
      if (!draft) return;
      if (draft.to.length === 0 && draft.cc.length === 0 && draft.bcc.length === 0) {
        actions.setStatus("This message has no recipients", "error");
        return;
      }
      autosave.cancel();
      setBusy(true);
      void (async () => {
        try {
          // ⌃S already named an instant; an ordinary send names one too, out of
          // the preference. Rust falls back to its own ten seconds when no
          // instant arrives, so passing it explicitly is what makes the setting
          // the authority rather than a suggestion.
          const result = await sendDraft(draft, scheduleAt ?? Date.now() + delay);
          recalled.current = draft;
          setPending(result.entry);
          setDraft(null);
          setNow(Date.now());
          // The reply is already in SQLite; this is what puts it on screen.
          actions.reload();
        } catch (error) {
          actions.setStatus(errorMessage(error), "error");
        } finally {
          setBusy(false);
        }
      })();
    },
    [draft, autosave, actions, delay],
  );

  const recall = useCallback(async () => {
    if (!pending) return;
    const id = pending.id;
    setPending(null);
    const cancelled = await undoSend(id).catch(() => false);
    if (!cancelled) {
      actions.setStatus("Already sent", "error");
      return;
    }
    const restored = recalled.current;
    recalled.current = null;
    if (restored) {
      setDraft(restored);
      void saveDraft(restored).catch(() => {});
    }
    actions.reload();
  }, [pending, actions]);

  /* -------------------------------------------------------------- flushing */

  // Drain anything a previous session left behind. This is the half of the
  // durability claim that no timer can provide.
  useEffect(() => {
    void flushOutbox()
      .then(({ pending: rows }) => {
        const holding = rows.find((row) => row.state === "holding");
        if (holding) setPending(holding);
      })
      .catch(() => {});
  }, []);

  // Tick only while there is something to count down.
  useEffect(() => {
    if (!pending || pending.state !== "holding") return;
    const timer = window.setInterval(() => setNow(Date.now()), 250);
    return () => window.clearInterval(timer);
  }, [pending]);

  // When the window closes, ask Rust to send.
  useEffect(() => {
    if (!pending || pending.state !== "holding") return;
    const delay = Math.max(pending.sendAfter - Date.now() + 150, 0);
    const timer = window.setTimeout(() => {
      void (async () => {
        const { outcomes } = await flushOutbox().catch(() => ({ outcomes: [] }));
        const mine = outcomes.find((o) => o.id === pending.id);
        if (mine && !mine.sent) {
          actions.setStatus(mine.error ?? "That message could not be sent", "error");
        }
        recalled.current = null;
        setPending(null);
        actions.reload();
      })();
    }, delay);
    return () => window.clearTimeout(timer);
  }, [pending, actions]);

  /* ---------------------------------------------------------------- render */

  const hasThread = threadId !== null && detail !== null;
  const holding = pending !== null && pending.state === "holding";
  // An unsent draft mirrored into this conversation. Read off the messages
  // rather than asked of the store, so the strip and the row the reader is
  // looking at cannot disagree.
  const threadDraft = detail?.messages.some((message) => message.isDraft) ?? false;

  // A conversation is no longer the only reason this exists: `c` opens a draft
  // with no thread behind it, and the undo strip has to survive whatever the
  // list does next.
  if (!hasThread && !draft && !holding) return null;

  if (pending && pending.state === "holding") {
    const remaining = Math.max(0, Math.ceil((pending.sendAfter - now) / 1000));
    /*
     * Scheduled, or just waiting out its window?
     *
     * The difference is what the strip says — a clock time versus a countdown —
     * and it is decided by comparing the wait against the send delay in force.
     * The second of slack matters: the delay is applied to a `Date.now()` read
     * a few milliseconds before Rust reads its own, so an exact comparison
     * would call every ordinary send a schedule. An entry recovered from a
     * previous session is measured against today's preference, which is the
     * best available answer and wrong only in the direction of showing a clock.
     */
    const scheduled = pending.sendAfter - pending.createdAt > delay + 1000;
    return (
      <div className="shrink-0 border-t border-border bg-surface">
        <div
          className={cn(
            "mx-auto flex max-w-[72ch] items-center gap-3 px-5 py-2.5",
            "text-list text-muted-foreground",
          )}
        >
          <span className="min-w-0 flex-1 truncate">
            {scheduled ? (
              <>
                Scheduled for{" "}
                <span className="font-mono tabular-nums text-foreground">
                  {clockTime(pending.sendAfter)}
                </span>
              </>
            ) : (
              <>
                Sending in{" "}
                <span className="font-mono tabular-nums text-foreground">{remaining}s</span>
              </>
            )}
            <span className="text-faint-foreground"> · {pending.subject}</span>
          </span>
          <button
            type="button"
            onClick={() => void recall()}
            className="inline-flex shrink-0 items-center gap-1.5 text-list text-accent hover:brightness-110"
          >
            Undo <Kbd keys={COMPOSER_KEYS.undoSend} />
          </button>
        </div>
      </div>
    );
  }

  if (!draft) {
    if (!hasThread) return null;
    /*
     * What the strip may honestly offer depends on whether this conversation
     * already holds an unsent draft.
     *
     * `open()` resumes an existing draft ahead of preparing a new one — for
     * every kind, because half-written text must not be thrown away by a
     * keystroke. The consequence is that while a draft exists, `r`, `a` and
     * `f` all do the same thing: they reopen it. Offering three labels for one
     * behaviour is the same fault as drawing one control twice, so the strip
     * collapses to the two things that remain true: a new message, and the
     * draft.
     */
    return (
      <div className="shrink-0 border-t border-border">
        <div className="mx-auto flex max-w-[72ch] items-center gap-3 px-5 py-2 text-micro text-faint-foreground">
          <button
            type="button"
            onClick={() => openNew()}
            className="inline-flex items-center gap-1.5 hover:text-foreground"
          >
            <Kbd keys={COMPOSER_KEYS.compose} /> new
          </button>
          {threadDraft ? (
            <button
              type="button"
              onClick={() => open("reply")}
              className="inline-flex items-center gap-1.5 text-danger hover:brightness-110"
            >
              <Kbd keys={COMPOSER_KEYS.reply} /> edit draft
            </button>
          ) : (
            <>
              <button
                type="button"
                onClick={() => open("reply")}
                className="inline-flex items-center gap-1.5 hover:text-foreground"
              >
                <Kbd keys={COMPOSER_KEYS.reply} /> reply
              </button>
              <button
                type="button"
                onClick={() => open("replyAll")}
                className="inline-flex items-center gap-1.5 hover:text-foreground"
              >
                <Kbd keys={COMPOSER_KEYS.replyAll} /> reply all
              </button>
              <button
                type="button"
                onClick={() => open("forward")}
                className="inline-flex items-center gap-1.5 hover:text-foreground"
              >
                <Kbd keys={COMPOSER_KEYS.forward} /> forward
              </button>
            </>
          )}
        </div>
      </div>
    );
  }

  const dismiss = () => {
    close();
    // Closing an untouched composer should not leave a row behind, or
    // reopening the thread offers an empty reply for ever.
    if (draft.body.trim() === "") void discardDraft(draft.id).catch(() => {});
  };

  const editor = (presentation: ComposerPresentation) => (
    <Composer
      draft={draft}
      busy={busy}
      presentation={presentation}
      toRef={toField}
      onChange={change}
      onSend={send}
      onClose={dismiss}
    />
  );

  /*
   * A new message is not part of the conversation on screen.
   *
   * Docking it under the open thread put the composer directly beneath
   * somebody else's message, with that message still visible above it and the
   * reading pane still scrolled to it — which reads as a reply, because that is
   * exactly what the same shape means one keystroke earlier. A reply *is* about
   * the thread above it and keeps the dock; a new message gets a surface of its
   * own, over the window, in the manner of ⌘K.
   *
   * Nothing about the draft changes with the presentation: `newDraft` carries
   * no thread, no recipients, no subject and no references, so the message
   * genuinely has nothing to do with what is behind it.
   */
  if (draft.kind === "new") {
    return (
      <Overlay
        open
        onClose={dismiss}
        align="center"
        // A new message starts where the message does not yet have anywhere to
        // go. The overlay's fallback is "the first field in the panel", which
        // is the subject.
        initialFocus={toField}
        labelledBy="composer-subject"
        className="max-w-[44rem]"
      >
        {editor("overlay")}
      </Overlay>
    );
  }

  return editor("dock");
}

function errorMessage(error: unknown): string {
  if (error && typeof error === "object" && "message" in error) {
    return String((error as { message: unknown }).message);
  }
  return String(error);
}
