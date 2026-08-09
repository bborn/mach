import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useMach } from "@/hooks/useMach";
import { useKeyBindings } from "@/hooks/useKeymap";
import { usePreferences } from "@/components/prefs/PreferencesProvider";
import { composeAccountId, sendDelayMs, signatureFor } from "@/lib/prefs";
import { withHtmlSignature } from "@/lib/email-html";
import { Kbd } from "@/components/ui/kbd";
import { Overlay } from "@/components/ui/dialog";
import { Composer, type ComposerPresentation } from "./Composer";
import { clockTime } from "@/lib/time";
import { cn } from "@/lib/utils";
import {
  attachPaths,
  bodyAsHtml,
  chooseAttachments,
  COMPOSER_KEYS,
  createAutosave,
  discardDraft,
  flushOutbox,
  hasWrittenBody,
  isDraftEmpty,
  loadDraft,
  loadDraftForThread,
  newDraft,
  prepareDraft,
  removeAttachment,
  saveDraft,
  sendDraft,
  undoSend,
  type Autosave,
  type Draft,
  type DraftKind,
  type OutboxEntry,
} from "@/lib/compose";

/**
 * Everything around the composers: when they open, what they do with their
 * drafts, and the ten seconds after `⌘⏎`.
 *
 * # More than one at a time
 *
 * A reply sits under the conversation it answers and a new message floats over
 * the window; that distinction is the whole reason the presentation is a prop.
 * Several drafts at once must not cost either of them, so: **only ever one
 * composer is on screen, and the rest are a strip of tabs above it.** Not a
 * stack of panels — three overlapping composers is a window manager, and the
 * one thing the inline composer exists to avoid is windows. Not a split — the
 * message being answered is the other half of the screen and it does not get to
 * shrink because a second draft exists.
 *
 * The strip appears at two and disappears at one, so a person who never writes
 * two messages at once never sees it. `⌥1`…`⌥9` switch. Switching to a reply
 * navigates to its conversation, which is what puts it back in the dock where a
 * reply belongs; a draft whose conversation is not the one on screen keeps its
 * tab and its text and simply is not rendered.
 *
 * # The undo window
 *
 * `⌘⏎` does not send. It queues: Rust builds the RFC822 bytes, commits them to
 * SQLite with a `sendAfter` the send-delay preference away — ten seconds unless
 * ⌘, says otherwise, and zero is a legal answer — and writes the reply into the
 * thread so the conversation repaints immediately. This component then shows a
 * strip with a countdown, and when the countdown reaches zero asks Rust to
 * flush. Undo deletes the row before anything has left, and puts the text back
 * in a composer rather than leaving the user to retype it.
 *
 * The timer here is a *prompt*, not the mechanism. If this window closes at
 * second four the row is still in the database and the next flush — on the next
 * launch — sends it. That is why `flushOutbox()` runs on mount.
 *
 * # One draft, one row
 *
 * Three duplicate-draft bugs have been fixed in this path, and every one of them
 * came from two things claiming to be the same draft. Several composers make
 * that easier to do, so there is one rule here and it is enforced in one place:
 * **[[openDraft]] is the only way a composer comes into existence**, and it
 * refuses to add a second entry for a draft id or a second entry for a thread
 * that already has one. Nothing else calls `setDrafts` with a new row.
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
   * again would either duplicate the block or undo a decision.
   * `withHtmlSignature` is idempotent as a second line of defence, but the
   * emptiness check is the one that respects "I deleted my signature on this
   * one".
   */
  const signed = useCallback(
    (draft: Draft): Draft => {
      const signature = signatureFor(prefs, draft.accountId);
      const html = bodyAsHtml(draft);
      if (!signature || html.trim() !== "") return { ...draft, body: html, bodyFormat: "html" };
      return { ...draft, body: withHtmlSignature(html, signature), bodyFormat: "html" };
    },
    [prefs],
  );

  /** Every open composer, oldest first. The strip is this list. */
  const [drafts, setDrafts] = useState<Draft[]>([]);
  const [activeId, setActiveId] = useState<string | null>(null);
  const [pending, setPending] = useState<OutboxEntry | null>(null);
  const [busy, setBusy] = useState(false);
  const [now, setNow] = useState(() => Date.now());
  const [confirming, setConfirming] = useState<string | null>(null);
  const [dropping, setDropping] = useState(false);

  // The draft that was queued, kept so undo restores the text rather than an
  // empty composer.
  const recalled = useRef<Draft | null>(null);
  // Where focus starts in the new-message overlay. See `Composer`'s `toRef`.
  const toField = useRef<HTMLInputElement>(null);

  const draft = useMemo(
    () => drafts.find((entry) => entry.id === activeId) ?? null,
    [drafts, activeId],
  );

  /* ---------------------------------------------------------------- saving */

  /**
   * One debounce per draft.
   *
   * A single shared one would flush the draft you just switched *away* from
   * into the draft you switched *to*, because the pending value is whatever was
   * queued last and the save is keyed by the draft's own id.
   */
  const autosaves = useRef(new Map<string, Autosave>());
  const autosaveFor = useCallback((id: string): Autosave => {
    const existing = autosaves.current.get(id);
    if (existing) return existing;
    const created = createAutosave((next) => {
      // A draft with nothing in it is not worth a row; it would also
      // resurrect an empty composer every time the thread is reopened.
      if (isDraftEmpty(next)) return;
      void saveDraft(next).catch(() => {
        /* the editor must not lose focus because a write failed */
      });
    });
    autosaves.current.set(id, created);
    return created;
  }, []);

  const flushAll = useCallback(() => {
    for (const autosave of autosaves.current.values()) autosave.flush();
  }, []);

  const change = useCallback(
    (next: Draft) => {
      setDrafts((current) =>
        current.map((entry) => (entry.id === next.id ? next : entry)),
      );
      autosaveFor(next.id).queue(next);
    },
    [autosaveFor],
  );

  /** The editor's own channel: HTML in, straight onto the active draft. */
  const changeBody = useCallback(
    (html: string) => {
      const current = drafts.find((entry) => entry.id === activeId);
      if (!current) return;
      change({ ...current, body: html, bodyFormat: "html" });
    },
    [drafts, activeId, change],
  );

  /** Close one composer, keeping its draft. */
  const close = useCallback(
    (id: string) => {
      autosaves.current.get(id)?.flush();
      autosaves.current.delete(id);
      setDrafts((current) => {
        const remaining = current.filter((entry) => entry.id !== id);
        // Closing one must not disturb another: whichever was open next takes
        // over, and the rest keep their text exactly as it was.
        setActiveId((was) =>
          was === id ? (remaining[remaining.length - 1]?.id ?? null) : was,
        );
        return remaining;
      });
      setConfirming((was) => (was === id ? null : was));
    },
    [],
  );

  /* --------------------------------------------------------------- opening */

  /**
   * The one door in.
   *
   * Every route to a composer — `r`, `c`, ⌘K's "write to this person", the
   * agent's "open draft", undo-send putting the text back — arrives here, and
   * this is where the duplicate rule lives. A draft that is already open is
   * *focused*, not opened again; a thread that already has a composer keeps the
   * one it has. Without that, `r` on a conversation with a composer already up
   * would put two editors on one draft row and the second keystroke in either
   * would overwrite the other.
   */
  const openDraft = useCallback((incoming: Draft) => {
    setDrafts((current) => {
      const byId = current.find((entry) => entry.id === incoming.id);
      if (byId) {
        setActiveId(byId.id);
        return current;
      }
      const byThread =
        incoming.threadId != null &&
        current.find((entry) => entry.threadId === incoming.threadId);
      if (byThread) {
        setActiveId(byThread.id);
        return current;
      }
      setActiveId(incoming.id);
      return [...current, incoming];
    });
  }, []);

  const open = useCallback(
    (kind: DraftKind) => {
      if (threadId === null) return;
      void (async () => {
        // A half-written reply wins over a freshly prepared one: reopening a
        // conversation must not throw away what you already typed.
        const existing = await loadDraftForThread(threadId).catch(() => null);
        if (existing && hasWrittenBody(existing)) {
          openDraft(signed(existing));
          return;
        }
        const prepared = await prepareDraft(threadId, kind).catch((error: unknown) => {
          actions.setStatus(errorMessage(error), "error");
          return null;
        });
        // `prepare` reads the thread — its account, its recipients, its
        // References header — and comes back with an empty body. The signature
        // is the one thing it cannot know, because it is not in the thread.
        if (prepared) openDraft(signed(prepared));
      })();
    },
    [threadId, actions, signed, openDraft],
  );

  /**
   * `c` — a message to somebody, from nothing.
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
      openDraft(to ? { ...blank, to: [{ email: to }] } : blank);
    },
    [ui.accountId, accounts, actions, prefs, signed, openDraft],
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
        if (found) openDraft(signed(found));
        else actions.setStatus("That draft is gone", "error");
      })();
    },
    [actions, openDraft, signed],
  );

  /** Bring a composer forward, and its conversation with it. */
  const switchTo = useCallback(
    (id: string) => {
      const target = drafts.find((entry) => entry.id === id);
      if (!target) return;
      setActiveId(id);
      setConfirming(null);
      // A reply belongs under the message it answers. Switching to one whose
      // conversation is not on screen opens that conversation, which is what
      // puts the composer back in the dock rather than leaving it invisible.
      if (target.threadId != null && target.threadId !== threadId) {
        actions.selectThread(target.threadId);
      }
    },
    [drafts, threadId, actions],
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

  /* ------------------------------------------------------------- discarding */

  /**
   * Throw a draft away: the row, the copy in the conversation, and the one on
   * Gmail.
   *
   * **Confirmed rather than undoable, and the difference is not a preference.**
   * Everything else destructive in Mach has an exact inverse the command layer
   * can replay — an archive puts a label back. A discard does not: it ends with
   * `drafts.delete`, and Gmail does not hand the id back, so "undo" could only
   * mean creating a *different* draft that happens to contain the same words.
   * The sweep in `sync::mail` would then have two things to reconcile, which is
   * precisely the shape of the three duplicate bugs already fixed here. A draft
   * is also the one thing in this app with no copy anywhere else once it is
   * gone. So: one keystroke to ask, a second to mean it.
   *
   * A draft with nothing in it skips the question entirely, because there is
   * nothing to lose and being asked would be the software admiring its own
   * caution.
   */
  const discard = useCallback(
    (id: string) => {
      const target = drafts.find((entry) => entry.id === id);
      if (!target) return;
      if (!isDraftEmpty(target) && confirming !== id) {
        setConfirming(id);
        return;
      }
      autosaves.current.get(id)?.cancel();
      autosaves.current.delete(id);
      close(id);
      void discardDraft(id)
        .then((result) => {
          if (result.remote === "failed") {
            // The rows here are gone and Gmail's copy is not. Saying so is the
            // whole point: the next sync will find that draft and put it back.
            actions.setStatus(
              result.error ?? "Gmail still has that draft — it may come back",
              "error",
            );
          }
          actions.reload();
        })
        .catch((error: unknown) => actions.setStatus(errorMessage(error), "error"));
    },
    [drafts, confirming, close, actions],
  );

  /* ------------------------------------------------------------ attachments */

  const applyAttachments = useCallback(
    (id: string, attachments: Draft["attachments"]) => {
      setDrafts((current) =>
        current.map((entry) => (entry.id === id ? { ...entry, attachments } : entry)),
      );
    },
    [],
  );

  /**
   * Attaching writes to the store, and the store keys the files by draft id —
   * so the draft has to *have* a row before a file can point at it. A composer
   * opened and never typed in has no row yet, which is why this saves first.
   */
  const ensureSaved = useCallback(
    async (target: Draft): Promise<Draft> => {
      autosaves.current.get(target.id)?.cancel();
      const saved = await saveDraft(target).catch(() => null);
      if (!saved) return target;
      setDrafts((current) =>
        current.map((entry) =>
          entry.id === target.id
            ? { ...entry, remote: saved.remote, threadId: saved.threadId }
            : entry,
        ),
      );
      return saved;
    },
    [],
  );

  const attach = useCallback(
    (id: string) => {
      const target = drafts.find((entry) => entry.id === id);
      if (!target) return;
      void (async () => {
        await ensureSaved(target);
        const result = await chooseAttachments(id).catch((error: unknown) => {
          actions.setStatus(errorMessage(error), "error");
          return null;
        });
        if (!result) return;
        applyAttachments(id, result.attachments);
        // A file that was refused must say so by name. Silently attaching three
        // of four is the failure this project has paid most for.
        if (result.refused.length > 0) actions.setStatus(result.refused[0], "error");
      })();
    },
    [drafts, ensureSaved, applyAttachments, actions],
  );

  const drop = useCallback(
    (paths: string[]) => {
      const target = drafts.find((entry) => entry.id === activeId);
      if (!target || paths.length === 0) return;
      void (async () => {
        await ensureSaved(target);
        const result = await attachPaths(target.id, paths).catch((error: unknown) => {
          actions.setStatus(errorMessage(error), "error");
          return null;
        });
        if (!result) return;
        applyAttachments(target.id, result.attachments);
        if (result.refused.length > 0) actions.setStatus(result.refused[0], "error");
      })();
    },
    [drafts, activeId, ensureSaved, applyAttachments, actions],
  );

  const unattach = useCallback(
    (id: string, attachmentId: string) => {
      void removeAttachment(attachmentId)
        .then((attachments) => applyAttachments(id, attachments))
        .catch((error: unknown) => actions.setStatus(errorMessage(error), "error"));
    },
    [applyAttachments, actions],
  );

  /**
   * Files dropped on the window.
   *
   * Tauri reports the drop with the paths, not with a `File`: the webview never
   * sees the bytes, and Rust reads them from disk. That is also why there is no
   * `preventDefault` dance here — the browser's own drop handling is off.
   */
  useEffect(() => {
    if (!drafts.length) return;
    let cancelled = false;
    const stop: Array<() => void> = [];
    void (async () => {
      try {
        const { getCurrentWebview } = await import("@tauri-apps/api/webview");
        const unlisten = await getCurrentWebview().onDragDropEvent((event) => {
          if (cancelled) return;
          if (event.payload.type === "over") setDropping(true);
          else if (event.payload.type === "leave") setDropping(false);
          else if (event.payload.type === "drop") {
            setDropping(false);
            drop(event.payload.paths);
          }
        });
        stop.push(unlisten);
      } catch {
        /* a browser tab has no webview to listen to, and no paths either */
      }
    })();
    return () => {
      cancelled = true;
      for (const unlisten of stop) unlisten();
    };
  }, [drafts.length, drop]);

  /* ------------------------------------------------------------------ keys */

  useKeyBindings([
    {
      keys: COMPOSER_KEYS.compose,
      group: "Write",
      description: "New message",
      priority: 5,
      when: () => active,
      handler: () => openNew(),
    },
    {
      keys: COMPOSER_KEYS.composeAnother,
      group: "Write",
      description: "New message, without closing this one",
      allowInInput: true,
      // Above the floor an overlay claims, or it would be dead in the one place
      // it is most needed: inside a composer.
      priority: 110,
      when: () => active,
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
    /*
     * The draft at the cursor, thrown away without opening it.
     *
     * An abandoned draft is found in the Drafts list far more often than in an
     * open composer, and the list is another unit's file. So the key is
     * registered here, where the compose engine already is, and answers for
     * whichever draft the mail view is pointing at: the open conversation's, if
     * it has one.
     */
    {
      keys: COMPOSER_KEYS.discard,
      group: "Write",
      description: "Discard the draft in this conversation",
      priority: 5,
      when: () => active && draft === null && threadDraft,
      handler: () => void discardThreadDraft(),
    },
  ]);

  // ⌥1…⌥9 — the tab strip's keys. Registered from data so the strip and the
  // keys cannot disagree about which composer is which.
  useKeyBindings(
    drafts.slice(0, 9).map((entry, index) => ({
      keys: `alt+${index + 1}`,
      group: "Write",
      description: `Composer ${index + 1}`,
      allowInInput: true,
      priority: 110,
      when: () => active && drafts.length > 1,
      handler: () => switchTo(entry.id),
    })),
  );

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
      const id = draft.id;
      autosaves.current.get(id)?.cancel();
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
          close(id);
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
    [draft, actions, delay, close],
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
      openDraft(restored);
      void saveDraft(restored).catch(() => {});
    }
    actions.reload();
  }, [pending, actions, openDraft]);

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

  // Save what is pending when the window goes away. Switching conversations no
  // longer closes anything — a draft you are still writing survives being
  // navigated past — so this is what used to ride along with that effect.
  useEffect(() => flushAll, [flushAll]);

  /* ---------------------------------------------------------------- render */

  const hasThread = threadId !== null && detail !== null;
  const holding = pending !== null && pending.state === "holding";
  // An unsent draft mirrored into this conversation. Read off the messages
  // rather than asked of the store, so the strip and the row the reader is
  // looking at cannot disagree.
  const threadDraft = detail?.messages.some((message) => message.isDraft) ?? false;

  /**
   * Discard the conversation's draft without opening it.
   *
   * It goes through the same `discard` as the composer's own key, by opening
   * the draft first: one path to the three deletions, which is the rule the
   * duplicate bugs were fixed by.
   */
  const discardThreadDraft = useCallback(async () => {
    if (threadId === null) return;
    const found = await loadDraftForThread(threadId).catch(() => null);
    if (!found) {
      actions.setStatus("There is no draft here to throw away", "error");
      return;
    }
    openDraft(signed(found));
    setConfirming(found.id);
  }, [threadId, actions, openDraft, signed]);

  // A conversation is no longer the only reason this exists: `c` opens a draft
  // with no thread behind it, and the undo strip has to survive whatever the
  // list does next.
  if (!hasThread && drafts.length === 0 && !holding) return null;

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

  /**
   * Which composer is on screen.
   *
   * A new message floats over the window and is shown wherever you are. A reply
   * is rendered only inside its own conversation — its tab is still in the
   * strip, and switching to it navigates there. A reply drawn under somebody
   * else's thread would read as a reply to *that*, which is the exact mistake
   * the overlay exists to prevent, seen from the other side.
   */
  const visible =
    draft && (draft.kind === "new" || draft.threadId == null || draft.threadId === threadId)
      ? draft
      : null;

  const strip = drafts.length > 1 && (
    <div className="shrink-0 border-t border-border bg-surface">
      <div className="mx-auto flex max-w-[72ch] items-center gap-1 overflow-x-auto px-5 py-1.5">
        {drafts.map((entry, index) => (
          <button
            key={entry.id}
            type="button"
            onClick={() => switchTo(entry.id)}
            className={cn(
              "inline-flex max-w-[24ch] shrink-0 items-center gap-1.5 rounded-[var(--radius)]",
              "px-2 py-0.5 text-micro",
              entry.id === activeId
                ? "bg-hover text-foreground"
                : "text-faint-foreground hover:text-foreground",
            )}
          >
            <Kbd keys={`alt+${index + 1}`} className="border-none bg-transparent px-0" />
            <span className="min-w-0 truncate">{title(entry)}</span>
          </button>
        ))}
      </div>
    </div>
  );

  if (!visible) {
    if (!hasThread) return strip || null;
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
      <>
        {strip}
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
              <>
                <button
                  type="button"
                  onClick={() => open("reply")}
                  className="inline-flex items-center gap-1.5 text-danger hover:brightness-110"
                >
                  <Kbd keys={COMPOSER_KEYS.reply} /> edit draft
                </button>
                <button
                  type="button"
                  onClick={() => void discardThreadDraft()}
                  className="inline-flex items-center gap-1.5 hover:text-danger"
                >
                  <Kbd keys={COMPOSER_KEYS.discard} /> discard draft
                </button>
              </>
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
      </>
    );
  }

  const dismiss = () => {
    close(visible.id);
    // Closing an untouched composer should not leave a row behind, or
    // reopening the thread offers an empty reply for ever. It may also have
    // written one already — autosave declines to, but `attach` saves on
    // purpose — so this is a discard rather than a local forget.
    if (isDraftEmpty(visible)) void discardDraft(visible.id).catch(() => {});
  };

  const editor = (presentation: ComposerPresentation) => (
    <Composer
      draft={visible}
      html={visible.body}
      busy={busy}
      presentation={presentation}
      active
      dropping={dropping}
      toRef={toField}
      onChange={change}
      onBodyChange={changeBody}
      onSend={send}
      onClose={dismiss}
      onDiscard={() => discard(visible.id)}
      onAttach={() => attach(visible.id)}
      onRemoveAttachment={(attachmentId) => unattach(visible.id, attachmentId)}
    />
  );

  const question = confirming === visible.id && (
    <div className="shrink-0 border-t border-danger/40 bg-surface">
      <div className="mx-auto flex max-w-[72ch] items-center gap-3 px-5 py-2 text-list text-muted-foreground">
        <span className="min-w-0 flex-1 truncate">
          Throw this draft away? It is not kept anywhere else.
        </span>
        <button
          type="button"
          onClick={() => discard(visible.id)}
          className="shrink-0 text-list text-danger hover:brightness-110"
        >
          Discard
        </button>
        <button
          type="button"
          onClick={() => setConfirming(null)}
          className="shrink-0 text-list hover:text-foreground"
        >
          Keep
        </button>
      </div>
    </div>
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
  if (visible.kind === "new") {
    return (
      <>
        {strip}
        {question}
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
      </>
    );
  }

  return (
    <>
      {strip}
      {question}
      {editor("dock")}
    </>
  );
}

/**
 * What a composer's tab says.
 *
 * The subject, because that is what the message is; then the first recipient,
 * because a message with no subject still has somebody it is going to; then the
 * kind, so a reply whose subject has not loaded yet does not sit in the strip
 * calling itself a new message.
 */
function title(draft: Draft): string {
  if (draft.subject.trim()) return draft.subject;
  const first = draft.to[0];
  if (first) return first.name?.trim() || first.email;
  switch (draft.kind) {
    case "reply":
    case "replyAll":
    case "adopted":
      return "Reply";
    case "forward":
      return "Forward";
    default:
      return "New message";
  }
}

function errorMessage(error: unknown): string {
  if (error && typeof error === "object" && "message" in error) {
    return String((error as { message: unknown }).message);
  }
  return String(error);
}
