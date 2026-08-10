import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useMach } from "@/hooks/useMach";
import { useContacts } from "@/hooks/useContacts";
import { useKeyBindings } from "@/hooks/useKeymap";
import { useViewportHeight } from "@/hooks/useViewportHeight";
import { usePreferences, useUiSession } from "@/components/prefs/PreferencesProvider";
import {
  COMPOSER_HEIGHT_BOUNDS,
  composeAccountId,
  sendDelayMs,
  signatureFor,
} from "@/lib/prefs";
import { withHtmlSignature } from "@/lib/email-html";
import { Kbd } from "@/components/ui/kbd";
import { Overlay } from "@/components/ui/dialog";
import { RESIZE_STEP, Resizer } from "@/components/ui/split";
import { Composer, type ComposerPresentation } from "./Composer";
import type { RichTextEditorHandle } from "./RichTextEditor";
import {
  DEFAULT_COMPOSER_HEIGHT,
  canPopOut,
  clampComposerHeight,
  composerPlacement,
  forgetPopOut,
  isOverDropTarget,
  isPoppedOut,
  popOutComposerHeight,
  togglePopOut,
} from "./composer-layout";
import { noteSent } from "@/lib/contacts";
import { clockTime } from "@/lib/time";
import { cn } from "@/lib/utils";
import {
  attachPaths,
  bodyAsHtml,
  carryForward,
  chooseAttachments,
  COMPOSER_KEYS,
  createAutosave,
  discardDraft,
  flushOutbox,
  hasWrittenBody,
  inlineImageDataUrl,
  inlineImageMarkup,
  inlineImages as loadInlineImages,
  isDraftEmpty,
  loadDraft,
  loadDraftForThread,
  newDraft,
  prepareDraft,
  removeAttachment,
  saveDraft,
  sendDraft,
  setAttachmentInline,
  undoSend,
  withCidReferences,
  type ArrivingFile,
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
 * # How big it is, and where
 *
 * The docked composer had four lines to write in and no way to ask for more,
 * under a conversation that kept the rest of the pane whether it needed it or
 * not. Two answers, and they are different sizes of answer.
 *
 * The handle on its top edge is the same `Resizer` the list divider and the
 * agent drawer use, so it is a focus stop with arrow keys as well as something
 * to drag, and ⌘⌥↑ ⌘⌥↓ reach it from inside the editor where the hands
 * already are. What it moves is the *body* height — the fields and the legend
 * are fixed furniture — which is kept in the UI session next to the list
 * width, because it is a divider somebody dragged rather than a decision they
 * would go looking for in ⌘,.
 *
 * ⇧⌘O gives the draft the whole window instead, through the same `Overlay` a
 * new message already uses. Nothing about the draft changes: it is the same
 * row, the same entry in the strip and the same text, with only the
 * presentation prop and a view flag keyed by draft id different — which is the
 * only way to add this without adding a second door in beside [[openDraft]].
 * React unmounts the editor to move it, so the caret is carried across by
 * number; see `caret-offset.ts`.
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
  const contacts = useContacts();
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
  const [confirming, setConfirming] = useState<string | null>(null);
  /** A file is over the composer, and letting go would attach it. */
  const [dropping, setDropping] = useState(false);
  /** A file is over the window at all, wherever the pointer is. */
  const [dragging, setDragging] = useState(false);
  /**
   * `cid:` → `data:` URL for the inline images of every open draft.
   *
   * One map for all of them rather than one per draft: content ids are unique
   * across drafts (they are the attachment row's own id), and a single map
   * survives switching between composers without a refetch.
   */
  const [inlineUrls, setInlineUrls] = useState<ReadonlyMap<string, string>>(
    () => new Map(),
  );
  /** Draft ids currently over the window rather than under a conversation. */
  const [popped, setPopped] = useState<string[]>([]);
  /**
   * Files a forward is fetching, by draft id.
   *
   * They are chips before they are rows. A forward of "here's the invoice" has
   * to show the invoice from the moment it opens, or the only difference
   * between "the file is on its way" and "the file was dropped" is how long you
   * wait — and dropping it silently is the bug this fixes.
   */
  const [arriving, setArriving] = useState<ReadonlyMap<string, ArrivingFile[]>>(
    () => new Map(),
  );

  /**
   * `drafts`, readable from a keystroke without depending on it.
   *
   * `open` has to know whether a composer is already up for this conversation
   * *before* it prepares one, and a callback that closed over `drafts` would be
   * rebuilt on every keystroke in every composer. The keystroke arrives long
   * after the render that set this, so it is current when it is read.
   */
  const held = useRef<Draft[]>([]);
  useEffect(() => {
    held.current = drafts;
  }, [drafts]);

  // The draft that was queued, kept so undo restores the text rather than an
  // empty composer.
  const recalled = useRef<Draft | null>(null);
  // The in-flight queue write, so undo can wait for the row's real id rather
  // than recall the optimistic one. Resolves to the entry Rust wrote.
  const queued = useRef<Promise<OutboxEntry> | null>(null);
  // Where focus starts in the new-message overlay. See `Composer`'s `toRef`.
  const toField = useRef<HTMLInputElement>(null);
  // And where it starts in the popped-out one: the message, not its address.
  const bodyField = useRef<HTMLElement>(null);
  /*
   * The editor of whichever composer is on screen.
   *
   * Held here because placing an image in the body is an edit *at the caret*,
   * and the caret lives in a document only the editor owns — it cannot be
   * expressed as a new `body` string handed down as a prop. Shared with the
   * composer the same way `toField` is.
   */
  const bodyEditor = useRef<RichTextEditorHandle>(null);
  /*
   * Where the caret was in the composer that is being replaced, and whose.
   *
   * Kept by draft id rather than as a bare number: the strip can switch to
   * another draft between the two composers, and putting draft A's caret into
   * draft B would be worse than not restoring one at all.
   */
  const caret = useRef<{ id: string; offset: number } | null>(null);

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

  /**
   * The editor's own channel: HTML in, straight onto the active draft.
   *
   * With one substitution on the way through. The editor is showing inline
   * images as `data:` URLs, because a `cid:` resolves to nothing in a webview;
   * the draft has to hold the `cid:` form, which is what a recipient can
   * resolve and what keeps a four-megabyte photograph out of a text column that
   * is rewritten on every autosave. See `withCidReferences`.
   */
  const changeBody = useCallback(
    (html: string) => {
      const current = drafts.find((entry) => entry.id === activeId);
      if (!current) return;
      change({ ...current, body: withCidReferences(html), bodyFormat: "html" });
    },
    [drafts, activeId, change],
  );

  /* ---------------------------------------------------------------- sizing */

  const { session: where, remember } = useUiSession();
  const [chosenHeight, setChosenHeight] = useState(DEFAULT_COMPOSER_HEIGHT);
  const restoredHeight = useRef(false);
  const viewport = useViewportHeight();

  // The stored height lands a tick after mount, like the rest of the session,
  // and only the first one counts: `remember` writes back into the same object,
  // so a second restore would fight every drag.
  const storedHeight = where.composerHeight;
  useEffect(() => {
    if (restoredHeight.current || storedHeight === undefined) return;
    restoredHeight.current = true;
    setChosenHeight(storedHeight);
  }, [storedHeight]);

  /*
   * What he chose and what fits are two different numbers, as they are for the
   * agent drawer. Keeping the choice unclamped is what makes a short window a
   * temporary condition rather than a decision.
   */
  const dockedHeight = clampComposerHeight(chosenHeight, viewport);
  const maxHeight = clampComposerHeight(COMPOSER_HEIGHT_BOUNDS.max, viewport);

  const resize = useCallback(
    (next: number) => {
      const clamped = clampComposerHeight(next, viewport);
      setChosenHeight(clamped);
      remember({ composerHeight: clamped });
    },
    [remember, viewport],
  );

  /** Close one composer, keeping its draft. */
  const close = useCallback(
    (id: string) => {
      autosaves.current.get(id)?.flush();
      autosaves.current.delete(id);
      // A composer that has gone is not popped out; leaving the id behind
      // would reopen the next one over the window without being asked.
      setPopped((current) => forgetPopOut(current, id));
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

  /* ----------------------------------------------------------- attachments */

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

  /**
   * Put the original's files on a forward, without the composer waiting for
   * them.
   *
   * The draft is already open when this runs. Anything the local cache holds
   * lands within a moment; the rest is a download and an upload away, and the
   * chips for those are drawn from `arriving` until the real rows arrive — a
   * name and a size the store already knew, so the composer never shows a file
   * count that is quietly short.
   *
   * Saved first, in the order [[attach]] uses and for the same reason: the
   * files are keyed by draft id, and a draft that is never written cannot own
   * them. It also means closing the composer offers to throw them away rather
   * than leaving rows nothing points at.
   */
  const carry = useCallback(
    async (target: Draft, expected: readonly ArrivingFile[]) => {
      const parent = target.replyToId;
      if (parent == null) return;
      setArriving((current) => new Map(current).set(target.id, [...expected]));
      try {
        const saved = await ensureSaved(target);
        const result = await carryForward(saved.id, parent);
        applyAttachments(saved.id, result.attachments);
        if (result.refused.length > 0) {
          actions.setStatus(result.refused.join(" · "), "error");
        }
      } catch (error: unknown) {
        actions.setStatus(errorMessage(error), "error");
      } finally {
        setArriving((current) => {
          const next = new Map(current);
          next.delete(target.id);
          return next;
        });
      }
    },
    [ensureSaved, applyAttachments, actions],
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
      // A composer already up for this conversation is focused, never doubled —
      // `openDraft` would decline a second one anyway, and preparing one to be
      // declined would leave a forward fetching the original's files onto a
      // draft id nothing on screen points at.
      const already = held.current.find((entry) => entry.threadId === threadId);
      if (already) {
        setActiveId(already.id);
        return;
      }
      void (async () => {
        // A half-written reply wins over a freshly prepared one: reopening a
        // conversation must not throw away what you already typed.
        //
        // Files count as having started it. A forward carries the original's
        // attachments and may not have a word typed in it yet; preparing a
        // second one would download them all again and leave the first set of
        // rows behind with nothing pointing at them.
        const existing = await loadDraftForThread(threadId).catch(() => null);
        if (existing && (hasWrittenBody(existing) || (existing.attachments?.length ?? 0) > 0)) {
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
        if (!prepared) return;
        const opened = signed(prepared.draft);
        openDraft(opened);
        // What will not fit is said now, while the draft is still being
        // written, rather than after ⌘⏎ when nothing can be done about it.
        if (prepared.refused.length > 0) {
          actions.setStatus(prepared.refused.join(" · "), "error");
        }
        // And the composer is already on screen before this starts.
        if (prepared.carries) void carry(opened, prepared.carrying);
      })();
    },
    [threadId, actions, signed, openDraft, carry],
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

  /**
   * Between the dock and the window, and back, without touching the draft.
   *
   * The only thing that changes is a view flag and where the composer is
   * rendered — no new row, no second entry in the strip, nothing that goes
   * near [[openDraft]]. The caret arrives from the composer that is about to
   * be unmounted, because it is the only thing that still knows it.
   */
  const popOut = useCallback((id: string, at: number | null) => {
    caret.current = at === null ? null : { id, offset: at };
    setPopped((current) => togglePopOut(current, id));
  }, []);

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

  /**
   * Fetch the bytes of a draft's inline images, so the composer can draw them.
   *
   * Merged into the existing map rather than replacing it: another composer's
   * images are still on screen behind this one, and a draft that has none must
   * not take them away.
   */
  const loadImages = useCallback(async (draftId: string) => {
    const images = await loadInlineImages(draftId).catch(() => []);
    if (images.length === 0) return;
    setInlineUrls((current) => {
      const next = new Map(current);
      for (const image of images) {
        const url = inlineImageDataUrl(image);
        if (url) next.set(image.contentId, url);
      }
      return next;
    });
  }, []);

  const attach = useCallback(
    (id: string, inline = false) => {
      const target = drafts.find((entry) => entry.id === id);
      if (!target) return;
      void (async () => {
        await ensureSaved(target);
        const result = await chooseAttachments(id, inline).catch((error: unknown) => {
          actions.setStatus(errorMessage(error), "error");
          return null;
        });
        if (!result) return;
        applyAttachments(id, result.attachments);
        const placed = result.added.filter((file) => file.inline && file.contentId);
        if (placed.length > 0) {
          await loadImages(id);
          for (const file of placed) {
            bodyEditor.current?.insert(inlineImageMarkup(file.contentId!, file.filename));
          }
        }
        // A file that was refused must say so by name. Silently attaching three
        // of four is the failure this project has paid most for.
        if (result.refused.length > 0) actions.setStatus(result.refused[0], "error");
      })();
    },
    [drafts, ensureSaved, applyAttachments, actions, loadImages],
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
   * Move one image between the body and the list under the message.
   *
   * Both halves are here, because the flag and the `<img>` are one act: an
   * attachment marked inline that nothing in the body points at is a part
   * Gmail sends and no client ever draws, and an `<img src="cid:…">` with no
   * inline part behind it is a broken image in the recipient's message.
   */
  const setInline = useCallback(
    (draftId: string, attachmentId: string, inline: boolean) => {
      void (async () => {
        const attachments = await setAttachmentInline(attachmentId, inline).catch(
          (error: unknown) => {
            actions.setStatus(errorMessage(error), "error");
            return null;
          },
        );
        if (!attachments) return;
        applyAttachments(draftId, attachments);
        const changed = attachments.find((file) => file.id === attachmentId);
        if (!changed?.contentId) return;
        if (inline) {
          await loadImages(draftId);
          bodyEditor.current?.insert(inlineImageMarkup(changed.contentId, changed.filename));
        } else {
          bodyEditor.current?.removeInline(changed.contentId);
        }
      })();
    },
    [applyAttachments, actions, loadImages],
  );

  /**
   * Fetch the bytes of anything inline that is not drawn yet.
   *
   * Keyed on the ids rather than on the drafts, so this runs when a draft is
   * opened or an image is placed and not on every keystroke. An image already
   * in the map is not fetched again: `loadImages` merges, and the bytes of a
   * file in SQLite do not change.
   */
  const unresolved = drafts
    .flatMap((entry) => entry.attachments ?? [])
    .filter((file) => file.inline && file.contentId && !inlineUrls.has(file.contentId))
    .map((file) => file.draftId);
  const pendingDrafts = [...new Set(unresolved)].join(",");
  useEffect(() => {
    if (!pendingDrafts) return;
    for (const id of pendingDrafts.split(",")) void loadImages(id);
  }, [pendingDrafts, loadImages]);

  /**
   * A file let go anywhere in the window must not navigate away from the app.
   *
   * The classic webview failure: the browser's default action for a dropped
   * file is to *open* it, which replaces the page — and the page is the
   * application, so the half-written draft in it goes too. In the real window
   * this cannot happen, because `dragDropEnabled` defaults to true and Tauri
   * takes the drop at the OS level before the DOM sees it. This guard is here
   * for the two cases where that is not true: the app running in a browser tab
   * against Vite, which is how most of the composer is developed, and the day
   * somebody sets `dragDropEnabled: false` to get `File` objects.
   *
   * Registered unconditionally and never removed while the dock is mounted. A
   * guard that only exists while a composer is open is a guard that is absent
   * at the moment the user drags a file at the app to *start* one.
   */
  useEffect(() => {
    const swallow = (event: DragEvent) => {
      // `dragover` has to be cancelled too, or the drop never fires as a drop —
      // cancelling only `drop` leaves the default action in place.
      if (event.dataTransfer?.types?.includes("Files")) event.preventDefault();
    };
    window.addEventListener("dragover", swallow);
    window.addEventListener("drop", swallow);
    return () => {
      window.removeEventListener("dragover", swallow);
      window.removeEventListener("drop", swallow);
    };
  }, []);

  /**
   * Files dropped on the composer.
   *
   * Tauri reports the drop with the paths, not with a `File`: the webview never
   * sees the bytes, and Rust reads them from disk.
   *
   * The position decides whether it lands. A drop over the thread list is not a
   * drop on the message being written, and attaching from there would be the
   * app guessing — so the composer draws a target while anything is being
   * dragged (`dragging`), lights it while the pointer is on it (`dropping`),
   * and a release anywhere else does nothing.
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
          const payload = event.payload;
          if (payload.type === "leave") {
            setDragging(false);
            setDropping(false);
            return;
          }
          if (payload.type === "enter" || payload.type === "over") {
            setDragging(true);
            setDropping(isOverDropTarget(payload.position));
            return;
          }
          const onTarget = isOverDropTarget(payload.position);
          setDragging(false);
          setDropping(false);
          if (onTarget) drop(payload.paths);
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

  /**
   * Which composer is on screen.
   *
   * A new message floats over the window and is shown wherever you are. A reply
   * is rendered only inside its own conversation — its tab is still in the
   * strip, and switching to it navigates there. A reply drawn under somebody
   * else's thread would read as a reply to *that*, which is the exact mistake
   * the overlay exists to prevent, seen from the other side.
   *
   * Derived here rather than at the render because the resize keys are gated on
   * it: they belong to a composer that is actually in the dock.
   */
  const visible =
    draft && (draft.kind === "new" || draft.threadId == null || draft.threadId === threadId)
      ? draft
      : null;
  const poppedOut = visible !== null && isPoppedOut(popped, visible.id);
  const docked = visible !== null && composerPlacement(visible.kind, poppedOut) === "dock";

  useKeyBindings([
    /*
     * The keyboard's own route to the handle on the top edge.
     *
     * The handle is a focus stop and answers ↑ ↓ once it has focus, but focus
     * is inside the editor while a reply is being written and ⇥ there is a
     * character, not a way out. These are the same two keystrokes the agent
     * drawer uses for its own divider — one gesture for "make this taller",
     * learned once — and they are live only while a composer is docked, which
     * is when the drawer's are not the ones the hands are aimed at. The
     * priority is what makes that an ordering rather than a tie.
     */
    {
      keys: "mod+alt+up",
      group: "Composer",
      description: "Taller",
      allowInInput: true,
      priority: 100,
      when: () => active && docked,
      handler: () => resize(dockedHeight + RESIZE_STEP),
    },
    {
      keys: "mod+alt+down",
      group: "Composer",
      description: "Shorter",
      allowInInput: true,
      priority: 100,
      when: () => active && docked,
      handler: () => resize(dockedHeight - RESIZE_STEP),
    },
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
      // Cancelled *and* forgotten. `close` flushes whatever the debounce is
      // holding, so leaving the entry in the map means one more `saveDraft`
      // after the message has been queued — a draft of a reply that has just
      // been sent. Rust refuses that write now as well; both ends, because it
      // is the specific accident that put a duplicate in his mailbox.
      autosaves.current.get(id)?.cancel();
      autosaves.current.delete(id);

      // ⌃S already named an instant; an ordinary send names one too, out of
      // the preference. Rust falls back to its own ten seconds when no instant
      // arrives, so passing it explicitly is what makes the setting the
      // authority rather than a suggestion.
      const sendAfter = scheduleAt ?? Date.now() + delay;

      /*
       * The composer closes now, not when Rust answers.
       *
       * Queuing is entirely local — build the bytes, write one row, drop the
       * draft — and it still is not instant: SQLite takes one writer at a time
       * and the sync loop is usually holding it against a store measured in
       * gigabytes. Waiting on that lock left `⌘⏎` sitting there for seconds
       * with the message apparently unsent, which is the one moment a mail
       * client cannot afford to feel uncertain.
       *
       * So the window closes, the undo pill appears, and the write happens
       * behind them. Nothing is lost if it fails: the draft is still in hand,
       * and the catch below puts it back on screen with the reason.
       */
      const optimistic: OutboxEntry = {
        id: `pending-${id}`,
        accountId: draft.accountId,
        threadId: draft.threadId ?? null,
        gmailThreadId: null,
        subject: draft.subject,
        state: "holding",
        sendAfter,
        createdAt: Date.now(),
        attempts: 0,
      };
      recalled.current = draft;
      setPending(optimistic);
      close(id);

      /*
       * Undo has to work during the gap. The pill is on screen before the row
       * exists, so ⌘Z can be pressed against an id Rust has never heard of;
       * `recall` waits on this instead of reading `pending.id`.
       */
      queued.current = sendDraft(draft, sendAfter)
        .then((result) => {
          // Only if this send is still the one on screen — a second ⌘⏎ while
          // the first was in flight would otherwise be overwritten by it.
          setPending((current) => (current?.id === optimistic.id ? result.entry : current));
          // Who you write to is the strongest signal the address book has, and
          // the queue write landing is the moment it is true rather than
          // half-typed. A send that never queued teaches it nothing.
          noteSent([...draft.to, ...draft.cc, ...draft.bcc]);
          // The reply is already in SQLite; this is what puts it on screen.
          actions.reload();
          return result.entry;
        })
        .catch((error) => {
          setPending((current) => (current?.id === optimistic.id ? null : current));
          actions.setStatus(errorMessage(error), "error");
          const restored = recalled.current;
          recalled.current = null;
          if (restored) openDraft(restored);
          throw error;
        });
    },
    [draft, actions, delay, close, openDraft],
  );

  const recall = useCallback(async () => {
    if (!pending) return;
    setPending(null);
    /*
     * Not `pending.id`. Between `⌘⏎` and Rust answering, that is the
     * optimistic id and no row carries it; undo pressed in the gap — which is
     * exactly when it is pressed — would recall nothing and report "Already
     * sent" about a message that had not been queued yet.
     *
     * Waiting here costs nothing the user sees: the pill is already gone.
     */
    const id = await (queued.current ?? Promise.resolve(null))
      .then((entry) => entry?.id ?? pending.id)
      .catch(() => null);
    if (id == null) return; // the queue failed; its catch has the screen
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

  // When the window closes, ask Rust to send.
  useEffect(() => {
    if (!pending || pending.state !== "holding") return;
    const delay = Math.max(pending.sendAfter - Date.now() + 150, 0);
    const timer = window.setTimeout(() => {
      /*
       * The strip goes now, not when Rust answers.
       *
       * It used to be cleared after `flushOutbox()` resolved, and that call
       * waits on the same single SQLite writer the sync loop holds — so on a
       * large store the strip sat there reading "Sending in 0s" with an Undo
       * that no longer meant anything, for as long as the lock was held. The
       * window is over at this instant by definition; nothing about the flush
       * changes that.
       */
      recalled.current = null;
      setPending(null);
      void (async () => {
        const { outcomes } = await flushOutbox().catch(() => ({ outcomes: [] }));
        const mine = outcomes.find((o) => o.id === pending.id);
        if (mine && !mine.sent) {
          actions.setStatus(mine.error ?? "That message could not be sent", "error");
        }
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

  /**
   * What the ghost completions are told about the conversation.
   *
   * The last message is the thing a reply is actually answering, so it is the
   * only one sent, truncated — a completion is a clause, and a thread's worth
   * of quoted history would cost more latency than the suggestion is worth.
   * Empty when there is no thread, which is exactly right for `c`.
   */
  const ghostContext = useMemo(() => {
    if (!detail) return undefined;
    const last = detail.messages[detail.messages.length - 1];
    const lines = [`This is a reply in the conversation "${detail.thread.subject}".`];
    if (last) {
      lines.push(
        `The message being answered, from ${last.from.name || last.from.email}:`,
        last.bodyText.slice(0, 1200),
      );
    }
    return lines;
  }, [detail]);

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
    /*
     * Scheduled, or just waiting out its window?
     *
     * A schedule is worth a clock time: it is minutes or hours away and the
     * instant is the information. An ordinary send is not. Counting ten down
     * to zero puts a timer on screen for something the person has already
     * finished thinking about, and the number is never the reason they look —
     * Undo is. So the strip says what happened and offers the one action.
     *
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
              <span className="text-foreground">Sent</span>
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
     * collapses to the one thing that remains true: the draft.
     *
     * A new message used to sit here too, ahead of everything else. It is the
     * one action in this strip that does not answer the conversation above
     * it — `c` still opens it from anywhere, but a chip among reply/forward
     * controls implied it belonged to this thread, and it does not.
     */
    return (
      <>
        {strip}
        <div className="shrink-0 border-t border-border">
          <div className="mx-auto flex max-w-[72ch] items-center gap-3 px-5 py-2 text-micro text-faint-foreground">
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
      presentation={presentation}
      active
      contacts={contacts}
      // A brand-new message has no conversation to continue, so there is
      // nothing for the model to finish from.
      ghostContext={visible.kind === "new" ? undefined : ghostContext}
      dropping={dropping}
      dragging={dragging}
      arriving={arriving.get(visible.id)}
      inlineImages={inlineUrls}
      toRef={toField}
      bodyRef={bodyField}
      editorRef={bodyEditor}
      bodyHeight={presentation === "dock" ? dockedHeight : popOutComposerHeight(viewport)}
      // Only the composer this caret was taken from may have it back.
      initialCaret={caret.current?.id === visible.id ? caret.current.offset : null}
      poppedOut={poppedOut}
      onPopOut={canPopOut(visible.kind) ? (at) => popOut(visible.id, at) : undefined}
      onChange={change}
      onBodyChange={changeBody}
      onSend={send}
      onClose={dismiss}
      onDiscard={() => discard(visible.id)}
      onAttach={(inline) => attach(visible.id, inline)}
      onRemoveAttachment={(attachmentId) => unattach(visible.id, attachmentId)}
      onSetInline={(attachmentId, inline) => setInline(visible.id, attachmentId, inline)}
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
   *
   * A reply that has been popped out asks for the same surface, for as long as
   * it is being written. The two differ only in the way out: a new message has
   * nowhere to go, so the backdrop closes it; a popped-out reply has a dock
   * waiting, so the backdrop puts it back rather than taking the composer away
   * for a click that landed beside it.
   */
  if (composerPlacement(visible.kind, poppedOut) === "overlay") {
    const isNew = visible.kind === "new";
    return (
      <>
        {strip}
        {question}
        <Overlay
          open
          onClose={isNew ? dismiss : () => popOut(visible.id, null)}
          align="center"
          /*
           * A new message starts where the message does not yet have anywhere
           * to go. A reply that has just been popped out starts where the
           * caret already was — the overlay places focus after its children
           * have, so without naming the body here the trap would land in the
           * To field and the drag of the last sentence would be over.
           */
          initialFocus={isNew ? toField : bodyField}
          labelledBy="composer-subject"
          // Popped out means "this draft is the thing I am doing": as tall as
          // the window will allow, where the new-message panel is a panel.
          className={isNew ? "max-w-[44rem]" : "max-w-[52rem] max-h-[90vh]"}
        >
          {editor("overlay")}
        </Overlay>
      </>
    );
  }

  return (
    <>
      {/* The handle sits on the composer's own top edge, above the strip, so
          the thing being sized is everything below it. `-my-1` costs the
          layout nothing and straddles the rule the composer already draws:
          the line it lights while it moves is that same edge, not a second
          one four pixels above it. */}
      <Resizer
        axis="y"
        size={dockedHeight}
        onResize={resize}
        min={COMPOSER_HEIGHT_BOUNDS.min}
        max={maxHeight}
        label="Reply height"
        className="-my-1 w-full"
      />
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
