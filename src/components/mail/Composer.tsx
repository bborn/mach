import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
  type Ref,
  type RefObject,
} from "react";
import { Image as ImageIcon, LoaderCircle, Paperclip, X } from "lucide-react";
import { useKeyBindings, useKeymap } from "@/hooks/useKeymap";
import { OVERLAY_KEY_FLOOR } from "@/lib/keymap";
import { useGhostText } from "@/hooks/useGhostText";
import { Kbd } from "@/components/ui/kbd";
import { GhostHint, GhostText } from "@/components/ui/ghost";
import {
  AddressSuggestions,
  typeaheadFieldProps,
  useAddressTypeahead,
} from "@/components/ui/address-typeahead";
import { anyPopupOpen } from "@/lib/popups";
import { cn } from "@/lib/utils";
import { RichTextEditor, type RichTextEditorHandle } from "./RichTextEditor";
import type { Contact } from "@/lib/contacts";
import { COMPOSER_COLUMN, COMPOSER_FIXED_ROW, type DropRegion } from "./composer-layout";
import type { PendingAttachment } from "./composer-attach";
import {
  COMPOSER_KEYS,
  formatRecipients,
  hasSubject,
  humanSize,
  isInlinableImage,
  isLocalOnly,
  parseRecipients,
  scheduleOptions,
  type Draft,
  type DraftAttachment,
} from "@/lib/compose";

/**
 * Where this composer is being shown.
 *
 * Only the outer chrome differs — the fields, the keys, the draft and the send
 * path are identical, which is why this is a prop rather than a second
 * component. `dock` is the strip under the reading pane, correct for a reply
 * because a reply *is* about the conversation above it. `overlay` is a panel
 * floating over the window, for a new message, which is about nothing on
 * screen: docked under whatever happened to be open, it read as part of that
 * conversation.
 */
export type ComposerPresentation = "dock" | "overlay";

interface ComposerProps {
  draft: Draft;
  /** The body as HTML — converted once by the dock, never re-derived here. */
  html: string;
  onChange: (draft: Draft) => void;
  onBodyChange: (html: string) => void;
  onSend: (scheduleAt?: number) => void;
  onClose: () => void;
  onDiscard: () => void;
  /**
   * The question is being asked about this draft, so the footer becomes it.
   *
   * A discard cannot be undone, so it is asked about — and the asking used to
   * be a bar on the composer's *top* edge, roughly 460px above the control
   * that raised it. From the writer's side, pressing `discard` painted nothing
   * anywhere near his eye or his pointer, which reads as the click having
   * missed. The answer belongs where the question was asked, so it is the
   * footer's other state rather than a row of its own.
   */
  confirmingDiscard?: boolean;
  /** Keep the draft: the answer that leaves everything exactly as it was. */
  onKeepDraft?: () => void;
  /**
   * The footer's other question: this message has no subject.
   *
   * A sibling of `confirmingDiscard` and drawn by the same band, because it is
   * the same act — the footer asking about the thing the footer was just told
   * to do. Owned where that one is owned, and only one of the two can be up:
   * see `asking`.
   *
   * Unlike a discard this *is* undoable — a queued message waits out its window
   * with ⌘Z on offer. The question is still worth its keystroke, because the
   * undo only helps somebody who noticed, and the missing subject is precisely
   * the thing you do not notice until it is in somebody else's inbox. What that
   * buys it is one keystroke of cost and no more: it never traps focus, and
   * Escape is out.
   */
  confirmingSubject?: boolean;
  /**
   * Raise that question. The instant `later` named rides along, so the answer
   * sends the schedule rather than a message that leaves now.
   */
  onConfirmSubject?: (scheduleAt?: number) => void;
  /** Yes: send it, subject or not. Immediately — nothing asks twice. */
  onSendAnyway?: () => void;
  /** No: put the question away and go on writing. */
  onKeepWriting?: () => void;
  /** Open the file panel. `inline` asks for the images to go in the body. */
  onAttach: (inline?: boolean) => void;
  onRemoveAttachment: (attachmentId: string) => void;
  /** Move one image between the body and the list under the message. */
  onSetInline?: (attachmentId: string, inline: boolean) => void;
  /**
   * Move this draft between the dock and the window, carrying the caret.
   *
   * The offset is handed up rather than kept here because this component is
   * the thing being unmounted: the composer that comes back is a different
   * one, and only the dock is around to tell it where the caret was. Absent
   * for a composer with nowhere to go — see `canPopOut`.
   */
  onPopOut?: (caret: number | null) => void;
  /** Whether this composer is currently the popped-out one. Labels the key. */
  poppedOut?: boolean;
  /** How tall the writing area stands. The handle on the top edge moves it. */
  bodyHeight: number;
  /** Where the caret was in the composer this one is replacing, if it is. */
  initialCaret?: number | null;
  /** Set while the send is in flight — the fields go read-only, not blank. */
  busy?: boolean;
  presentation?: ComposerPresentation;
  /** False for a composer sitting behind another one: its keys are not live. */
  active?: boolean;
  /**
   * Where on this composer the pointer is, while files are over it.
   *
   * Two answers rather than one boolean, because letting go has two different
   * outcomes: over the writing area an image goes *in* the message, anywhere
   * else it goes beside it. The user is choosing between those before the
   * release, so the composer has to draw which one is currently selected.
   */
  dropTarget?: DropRegion | null;
  /**
   * Files are being dragged over the window, wherever the pointer is.
   *
   * Separate from `dropTarget` so the composer can say where the files have to
   * land *before* the pointer is there. Without it the only feedback is at the
   * moment of release, by which time the choice has been made.
   */
  dragging?: boolean;
  /**
   * Whether a body drop would place these files rather than attach them.
   *
   * A guess from the dragged filenames, and it is only ever used to label the
   * hint — the real decision is Rust's, from the bytes. A `.zip` dragged over
   * the writing area must not be promised a picture in the message.
   */
  draggingInlinable?: boolean;
  /**
   * Files let go on this draft that the store has not answered for yet.
   *
   * Drawn as chips beside the real ones, from the name in the dropped path.
   * See `composer-attach.ts` — the round trip is not what has to be fast.
   */
  pending?: readonly PendingAttachment[];
  /** `cid:` → `data:` URL for the images that are part of this message. */
  inlineImages?: ReadonlyMap<string, string>;
  /**
   * The editor itself, for the dock.
   *
   * Placing an image in the body is an edit at the caret, and the caret is
   * inside a document only this editor owns — so the act cannot be expressed as
   * a new `body` string handed down. Shared the same way `toRef` is: given, the
   * composer uses it in place of its own.
   */
  editorRef?: RefObject<RichTextEditorHandle | null>;
  /**
   * Somewhere for the surface around this to hold on to the To field.
   *
   * The overlay traps focus and places it, and its effect runs *after* this
   * component's — so the composer focusing the field itself is not enough, and
   * the overlay's own fallback ("the first input in the panel") lands on the
   * subject. This is how it is told which field is the one to start in.
   */
  toRef?: RefObject<HTMLInputElement | null>;
  /**
   * The same again for the writing area, which the popped-out overlay starts
   * focus in. It is a contenteditable rather than an input, so the overlay's
   * own "first field in the panel" fallback cannot find it.
   */
  bodyRef?: RefObject<HTMLElement | null>;
  /** Everyone the app has seen, for the address fields. */
  contacts?: readonly Contact[];
  /** Lines describing what is being answered, for the ghost completions. */
  ghostContext?: readonly string[];
}

/** Shared by the subject field and the ghost mirror behind it. Must not drift. */
const SUBJECT_TYPE = "text-list font-medium";


/**
 * The composer.
 *
 * Three address fields, a rich-text editor, the files going with it, and a
 * legend of keys.
 *
 * The decisions worth knowing:
 *
 *  * **The editor is HTML, and so is the message.** It was a `<textarea>` of
 *    markdown-ish source, which made the two MIME parts trivial to derive and
 *    made `**bold**` the thing you looked at while writing. `RichTextEditor`
 *    replaced it; the `text/plain` part is now read back out of the HTML. The
 *    toolbar is four formats and a link, all of which have keys — it is there so
 *    the formatting is *visible*, which is the half a keyboard cannot do.
 *  * **The header is the subject, and for a reply it is not editable.** A
 *    reply's subject is derived, not chosen; offering the field is how the
 *    "Re: Re: Re:" chains this codebase goes out of its way to prevent get
 *    typed back in by hand.
 *  * **Cc and Bcc stay hidden until they have content or are asked for.**
 *    Reply-all fills Cc, so the row appears exactly when it means something.
 *  * **The address fields hold text, not a parsed list.** Parsing on every
 *    keystroke and rendering the result back would eat the comma the moment you
 *    typed it. The draft carries the parsed value; the field carries what you
 *    are typing.
 *  * **The footer is a button bar that reads as a legend.** It was a legend
 *    with two buttons hidden in it: `send`, `later` and `close` were `<span>`s,
 *    `attach`, `discard` and `pop out` were buttons, and all six carried the
 *    same class. Half the row did nothing when it was clicked and looked
 *    exactly like the half that did — reported as "these should feel like
 *    buttons/clickable". Every item is a button now, each one the same box:
 *    key chip, label, a hit target with padding, hover fill, pressed state and
 *    the app's focus ring. What makes it still read as a legend rather than a
 *    toolbar is that the chip is on every one of them, so the row is answering
 *    "what are the keys" first and "what can I click" second.
 *
 * # Completion
 *
 * Two kinds, and they are not the same feature. The address fields complete
 * from people the app has already seen — local, instant, and always available.
 * The subject offers a grey continuation from the model, which exists only when
 * `ANTHROPIC_API_KEY` does; without it that path resolves to the empty string
 * and the field is exactly what it was. Both are taken with ⇥ and refused by
 * carrying on typing.
 *
 * The body has no ghost text. It offered one while the body was a `<textarea>`
 * and `GhostText` could mirror its value behind it; the body is a
 * contenteditable now, which that mirror cannot measure. The model half of the
 * feature is still here and still wired — see `useGhostText` — so restoring it
 * is a question of drawing the suggestion inside `RichTextEditor`.
 */
export function Composer({
  draft,
  html,
  onChange,
  onBodyChange,
  onSend,
  onClose,
  onDiscard,
  confirmingDiscard = false,
  onKeepDraft,
  confirmingSubject = false,
  onConfirmSubject,
  onSendAnyway,
  onKeepWriting,
  onAttach,
  onRemoveAttachment,
  onSetInline,
  onPopOut,
  poppedOut = false,
  bodyHeight,
  initialCaret = null,
  busy = false,
  presentation = "dock",
  active = true,
  dropTarget = null,
  dragging = false,
  draggingInlinable = false,
  pending = [],
  inlineImages,
  editorRef,
  toRef,
  bodyRef,
  contacts = [],
  ghostContext,
}: ComposerProps) {
  const ownEditor = useRef<RichTextEditorHandle>(null);
  const editor = editorRef ?? ownEditor;
  const subjectField = useRef<HTMLInputElement>(null);
  const ownToField = useRef<HTMLInputElement>(null);
  const toField = toRef ?? ownToField;
  const [showCc, setShowCc] = useState(draft.cc.length > 0 || draft.bcc.length > 0);
  const [scheduling, setScheduling] = useState(false);
  const [focus, setFocus] = useState<"subject" | null>(null);
  const [caretAtEnd, setCaretAtEnd] = useState(true);

  const isReply = draft.kind === "reply" || draft.kind === "replyAll";
  const attachments = draft.attachments ?? [];

  // What the address fields display. Seeded from the draft when the composer
  // opens on a different draft, and owned by the fields after that.
  const [raw, setRaw] = useState(() => ({
    to: formatRecipients(draft.to),
    cc: formatRecipients(draft.cc),
    bcc: formatRecipients(draft.bcc),
  }));
  useEffect(() => {
    setRaw({
      to: formatRecipients(draft.to),
      cc: formatRecipients(draft.cc),
      bcc: formatRecipients(draft.bcc),
    });
    setShowCc(draft.cc.length > 0 || draft.bcc.length > 0);
    setScheduling(false);
  }, [draft.id]); // eslint-disable-line -- only when a different draft is opened

  // Focus lands in the body for a reply, whose recipients are already right,
  // and in the To field for anything the user still has to address.
  useEffect(() => {
    if (!active) return;
    if (draft.to.length === 0 && toField.current) toField.current.focus();
    else editor.current?.focus();
  }, [draft.id, active]); // eslint-disable-line -- on open, not on every keystroke

  const options = useMemo(() => scheduleOptions(), [scheduling]);

  /**
   * Every way this panel sends: ⌘⏎, the footer's `send`, and each instant on
   * the `later` row.
   *
   * Which is why the subject is checked *here* rather than on the key. A
   * scheduled message with no subject arrives with no subject — the delay
   * changes nothing about what lands — so the one gate has to sit where all
   * three meet, and this is it.
   *
   * `onConfirmSubject` gates the gate: a composer rendered without a surface to
   * hold the question sends the way it always did rather than swallowing the
   * keystroke into a question nothing can draw.
   */
  const send = useCallback(
    (at?: number) => {
      setScheduling(false);
      if (onConfirmSubject && !hasSubject(draft)) {
        onConfirmSubject(at);
        return;
      }
      onSend(at);
    },
    [draft, onSend, onConfirmSubject],
  );

  /**
   * What Escape does, and what the footer's `close` does, which must be one
   * thing: the schedule row is the nearer of the two surfaces, so it goes
   * first and the panel stays.
   */
  const dismiss = useCallback(() => {
    if (scheduling) setScheduling(false);
    else onClose();
  }, [scheduling, onClose]);

  /**
   * Hand the caret up on the way out.
   *
   * Read here rather than in the dock because the editor is this component's:
   * by the time the dock has re-rendered with the other presentation, this
   * editor has been unmounted and its selection has gone with the element.
   */
  const popOut = useCallback(() => {
    onPopOut?.(editor.current?.caret() ?? null);
  }, [onPopOut]);

  /* ------------------------------------------------------- the questions */

  /**
   * Which question the footer is being, if it is being one.
   *
   * One value rather than two flags, because there is one row: two questions in
   * the same box at the same time is not a question, it is a race, and the
   * writer would answer whichever one his hands were already moving towards.
   * Neither can raise the other — the key that would is declined while the
   * other is up, and the control that would is not on screen, since the band
   * has replaced the whole legend. If both ever arrived anyway the
   * unrecoverable one wins: a draft that has been asked about is a draft that
   * may be about to stop existing, and a send can wait for that to be settled.
   *
   * Everything below is shared by both: one arming guard, one claim on the
   * keyboard, one record of where the caret was, one band.
   */
  const asking: "discard" | "subject" | null = confirmingDiscard
    ? "discard"
    : confirmingSubject
      ? "subject"
      : null;

  const keymap = useKeymap();
  /**
   * The answer that takes the focus.
   *
   * The affirmative either way — Discard, or Send — because that is the answer
   * the question was raised *by*: he pressed the key, the panel asked, and the
   * key he would press to mean it is under his hands already. What makes that
   * safe is `armed`, not distance.
   */
  const answerButton = useRef<HTMLButtonElement>(null);
  /**
   * Whether the question may yet be answered *yes*.
   *
   * The affirmative takes the focus, because he asked for it: the question is
   * raised by a key, answered by a key, and a confirmation whose default answer
   * is unreachable without moving the hands is a confirmation he has to click.
   * That makes ⏎ destroy the draft — and ⏎ is one of the keys that can *raise*
   * the question, by activating the footer's own `discard` while it has focus.
   * So can ⇧⌘⌫, by repeating: hold either down and the OS sends a second event
   * a quarter of a second later, at the button that has meanwhile taken the
   * focus. ⌘⏎ and the subject question are the same shape exactly — held down,
   * it repeats at a Send button that was not there when the finger went down.
   *
   * The guard is not a delay. A delay long enough to cover the repeat interval
   * is long enough to swallow a deliberate second press, which is the thing
   * this row is fastest at. It is instead that a press still being held down
   * cannot be a new decision:
   *
   *  * the *key* the question is about answers any press that is not the OS
   *    repeating one already down — see `KeyEventLike.repeat`. Read from the
   *    event rather than from here, because the keymap's own listener is on
   *    the window ahead of this one and would see a stale flag;
   *  * everything else — the button, under ⏎ or a click — waits for this,
   *    which the first keyup of a real key sets, and so does any pointer press,
   *    which is what a mouse must do to reach the button at all.
   *
   * Either way a second ⇧⌘⌫ — or a second ⌘⏎ — still means it with no wait, and
   * a held one never gets past the question.
   */
  const armed = useRef(false);
  /**
   * What had the caret when the question was asked.
   *
   * The answer has to be reachable by keyboard, so the safe button takes
   * focus — and a confirmation that takes the caret out of a half-written
   * message and does not put it back is the bug it was meant to prevent,
   * wearing a different hat. This is the way back.
   */
  const askedFrom = useRef<HTMLElement | null>(null);
  /**
   * And *where* in the message it was, if that is where it was.
   *
   * Focus and caret are two different things to give back. WebKit hands a
   * contenteditable back at character zero however far into the paragraph the
   * writer had got, so keeping only the element means Keep drops them at the
   * top of their own draft and the next word they type lands there. An offset,
   * for the reason `initialCaret` takes one — see `caret-offset.ts`.
   */
  const askedAt = useRef<number | null>(null);

  /*
   * A question owns the keyboard while it is up.
   *
   * Focus is on a button rather than in the message, so `isTypingTarget` is
   * false and every shell binding — `e` archives, `x` ticks a row — would be
   * live behind a question the writer has not answered yet. The claim silences
   * everything below the overlay floor, which is the same thing every dialog in
   * the app does through `Overlay`. The composer's own keys sit at or above
   * that floor and survive: ⇧⌘⌫ again is the second press that means it, and so
   * is ⌘⏎.
   *
   * Owning the keyboard is not trapping focus. ⇥ leaves, the window is still
   * the window, and Escape is always the way out — which matters more for the
   * subject question than for the discard, because that one is standing in
   * front of an act that was already undoable.
   *
   * A layout effect, for the reason the overlay uses one: the claim has to be
   * in force before anything can be typed at the row that has just appeared.
   */
  useLayoutEffect(() => {
    if (!asking) return;
    return keymap.claimKeyboard();
  }, [asking, keymap]);

  /*
   * Disarmed the moment the question goes up, and armed by the first thing the
   * writer *releases* or presses with the pointer. In the capture phase and on
   * the window, so it is reached whatever has focus and whatever else claims
   * the event; a modifier coming up on its own is not a release, or letting go
   * of ⇧ while ⌘⌫ repeats would arm it.
   *
   * Neither key the questions are about consults this — see `destroy` and
   * `sendAnyway`.
   */
  useLayoutEffect(() => {
    if (!asking) return;
    armed.current = false;
    const MODIFIERS = new Set(["Shift", "Meta", "Control", "Alt"]);
    const onKeyUp = (event: KeyboardEvent) => {
      if (!MODIFIERS.has(event.key)) armed.current = true;
    };
    const onPointerDown = () => {
      armed.current = true;
    };
    window.addEventListener("keyup", onKeyUp, true);
    window.addEventListener("pointerdown", onPointerDown, true);
    window.addEventListener("mousedown", onPointerDown, true);
    return () => {
      window.removeEventListener("keyup", onKeyUp, true);
      window.removeEventListener("pointerdown", onPointerDown, true);
      window.removeEventListener("mousedown", onPointerDown, true);
    };
  }, [asking]);

  useEffect(() => {
    if (!asking) return;
    const had = document.activeElement as HTMLElement | null;
    // Not the button this is about to focus: in StrictMode the effect runs
    // twice on mount, and the second pass would record its own answer.
    if (had !== answerButton.current) {
      askedFrom.current = had;
      // Read while the caret is still in the message. A moment later it is on
      // a button and there is nothing left to read.
      askedAt.current = editor.current?.caret() ?? null;
    }
    answerButton.current?.focus();
  }, [asking]);

  /**
   * Yes — if this is a decision rather than the press that asked. See `armed`.
   *
   * Both ways of saying yes come through here. `decided` is the discard key
   * saying it already knows: a press that is not a repeat. Everything else asks
   * whether anything has been released or pressed since the question went up.
   * Refusing is silent, and has to be: the only presses it refuses are ones the
   * writer never made.
   */
  const destroy = useCallback(
    (decided = false) => {
      if (!decided && !armed.current) return;
      onDiscard();
    },
    [onDiscard],
  );

  /**
   * The same yes, for the other question. Same guard, and it has to be:
   * ⌘⏎ raises this one and ⌘⏎ answers it, so the press that asked would
   * otherwise send the message it was asking about.
   *
   * There is nothing after this. The dock sends on the way out of the handler —
   * the queue, the ten seconds, the undo pill — because a question the writer
   * has just answered yes to and is then asked again in another form is a
   * question he stops reading.
   */
  const sendAnyway = useCallback(
    (decided = false) => {
      if (!decided && !armed.current) return;
      onSendAnyway?.();
    },
    [onSendAnyway],
  );

  /**
   * Put the caret back where the question took it from.
   *
   * An address field can have itself back directly — an input restores its own
   * selection, and there are forty characters in it either way. The message
   * cannot: it goes through the editor's handle, because Squire has to be
   * *told* it has focus (see `focusAndAnnounce`) and because the offset is the
   * only part of "where it was" that survives the round trip.
   */
  const restore = useCallback(() => {
    const was = askedFrom.current;
    const at = askedAt.current;
    askedFrom.current = null;
    askedAt.current = null;
    // A field by its tag rather than by `isContentEditable`, which is the same
    // call `isTypingTarget` makes and the only one jsdom can answer.
    const tag = was?.tagName?.toUpperCase();
    const field =
      was?.isConnected &&
      was.closest?.("[data-mach-composer]") &&
      (tag === "INPUT" || tag === "TEXTAREA")
        ? was
        : null;
    if (field) field.focus();
    else editor.current?.focus(at);
  }, []);

  /** Keep the draft, and leave everything else exactly as it was. */
  const keep = useCallback(() => {
    onKeepDraft?.();
    restore();
  }, [onKeepDraft, restore]);

  /**
   * No — and this one does not hand the caret back where it found it.
   *
   * The discard question has to, because "keep this draft" is an answer that
   * changes nothing and a caret moved by a question the writer cancelled is the
   * bug that question was meant to prevent. This question is different: it was
   * asked *about a field*, and "no" almost always means "let me write one". So
   * the field it is about takes the caret and the next thing typed is the
   * subject — one input from where they were, which ⇥ is not.
   *
   * A reply's subject is a heading rather than a field, and there is nothing to
   * focus. Close to unreachable — `replySubject` returns `Re:` at its very
   * emptiest, so a reply always has one — and the fallback is the discard
   * question's answer: back where they were, offset and all.
   */
  const keepWriting = useCallback(() => {
    onKeepWriting?.();
    if (subjectField.current) {
      subjectField.current.focus();
      askedFrom.current = null;
      askedAt.current = null;
    } else restore();
  }, [onKeepWriting, restore]);

  /* ------------------------------------------------------------ completion */

  const setField = (which: "to" | "cc" | "bcc") => (value: string) => {
    setRaw((current) => ({ ...current, [which]: value }));
    onChange({ ...draft, [which]: parseRecipients(value) });
  };

  const to = useAddressTypeahead({
    value: raw.to,
    contacts,
    onChange: setField("to"),
    fieldRef: toField,
    enabled: !busy,
  });

  const subjectGhost = useGhostText({
    kind: "emailSubject",
    value: draft.subject,
    context: ghostContext,
    active: !busy && !isReply && focus === "subject" && caretAtEnd,
  });

  /*
   * `!asking` on the keys below, and it is not redundant with the claim above.
   *
   * The claim silences everything under the overlay floor; the composer's own
   * keys sit at or above it and survive, which is right for the two keys the
   * two questions are about — each one is its own second press that means it —
   * and wrong for the rest. A question about whether this draft should exist
   * that ⌘⏎ can send the draft out from under is not a question, and neither is
   * a question about the subject that ⇧⌘⌫ can throw the draft away behind. So
   * every key that is not an answer waits for one.
   */
  useKeyBindings([
    {
      keys: COMPOSER_KEYS.send,
      group: "Composer",
      description: "Send",
      allowInInput: true,
      priority: 100,
      // Live while the subject question is up, because there it *is* the
      // answer — the same "the key that asked is the key that means it" the
      // discard key makes one row down.
      when: () => active && !busy && !confirmingDiscard,
      handler: (event) =>
        confirmingSubject ? sendAnyway(event.repeat !== true) : send(),
    },
    {
      keys: COMPOSER_KEYS.schedule,
      group: "Composer",
      description: "Schedule send",
      allowInInput: true,
      priority: 100,
      when: () => active && !busy && !asking,
      handler: () => setScheduling((open) => !open),
    },
    {
      keys: COMPOSER_KEYS.attach,
      group: "Composer",
      description: "Attach files",
      allowInInput: true,
      priority: 100,
      when: () => active && !busy && !asking,
      handler: () => onAttach(),
    },
    {
      keys: COMPOSER_KEYS.discard,
      group: "Composer",
      description: "Discard this draft",
      allowInInput: true,
      priority: 100,
      when: () => active && !busy && !confirmingSubject,
      // The second press that means it, and only if it is a second press:
      // holding the key down must not answer its own question. See `armed`.
      handler: (event) =>
        confirmingDiscard ? destroy(event.repeat !== true) : onDiscard(),
    },
    {
      keys: COMPOSER_KEYS.popOut,
      group: "Composer",
      description: "Pop out, or put back",
      allowInInput: true,
      // Above the floor the overlay claims, because half of what this key does
      // is done from inside that overlay.
      priority: 110,
      when: () => active && onPopOut !== undefined,
      handler: () => popOut(),
    },
    /*
     * Escape answers the question before it answers anything else.
     *
     * Above the composer's own Escape, which closes the panel: while the
     * question is up, the thing on screen that Escape is aimed at is the
     * question. It cancels — a key that could confirm a discard by being
     * pressed at the wrong moment is not a key, it is a trap — and hands the
     * caret back. `allowInInput` because the answer must arrive whether the
     * writer left the caret in the message or is on the button itself.
     *
     * One key out of either question, which is what keeps the subject warning
     * cheap enough to be worth having: the message it stands in front of was
     * already recallable, so the question may cost one keystroke and not two.
     */
    {
      keys: COMPOSER_KEYS.close,
      group: "Composer",
      description: "Keep the draft",
      allowInInput: true,
      priority: OVERLAY_KEY_FLOOR + 30,
      when: () => active && asking !== null,
      handler: () => (confirmingDiscard ? keep() : keepWriting()),
    },
    {
      keys: COMPOSER_KEYS.close,
      group: "Composer",
      description: "Close, keeping the draft",
      allowInInput: true,
      priority: 120,
      // A suggestion list is a popup, and the keymap runs in the capture phase
      // — declining here is what lets one Escape dismiss the list and the next
      // one close the composer. See `lib/popups.ts`.
      when: () => active && !anyPopupOpen(),
      handler: () => dismiss(),
    },
  ]);

  /** ⇥ takes the grey text; Escape refuses it without closing anything. */
  const ghostKeys =
    (suggestion: string, accept: () => string | null, dismiss: () => void, apply: (v: string) => void) =>
    (event: React.KeyboardEvent) => {
      if (!suggestion) return;
      if (event.key === "Tab" && !event.shiftKey && !event.metaKey && !event.ctrlKey) {
        event.preventDefault();
        event.stopPropagation();
        const next = accept();
        if (next !== null) apply(next);
        return;
      }
      if (event.key === "Escape") {
        event.preventDefault();
        event.stopPropagation();
        dismiss();
      }
    };

  const trackCaret = (event: { currentTarget: HTMLInputElement | HTMLTextAreaElement }) => {
    const node = event.currentTarget;
    setCaretAtEnd((node.selectionStart ?? 0) === node.value.length && node.selectionStart === node.selectionEnd);
  };

  const overlay = presentation === "overlay";

  return (
    // The overlay's panel already provides the surface, the border and the
    // shadow, and it is not attached to anything above it — so it gets neither
    // the top rule that joins the dock to the reading pane nor the reading
    // pane's own measure.
    <div
      // Says "the keyboard is in a composer" to the shell — see
      // `keyboardInComposer`. Nothing styles off it.
      data-mach-composer=""
      data-mach-drop-target=""
      className={cn(
        // In the overlay the panel above this one has a maximum height and
        // clips what does not fit, so this has to be a column that can
        // shrink — see COMPOSER_COLUMN for what was cut off when it wasn't.
        overlay ? COMPOSER_COLUMN : "shrink-0 border-t border-border bg-surface",
        // A drop target has to say so before the file is let go, or the only
        // feedback is whether it worked. Only for the region that means "beside
        // the message" — a drop headed for the body is drawn on the body, and
        // ringing both boxes at once would say the two outcomes are one.
        dropTarget === "composer" && "ring-1 ring-inset ring-accent",
      )}
    >
      <div
        className={cn(
          COMPOSER_COLUMN,
          "flex-1 px-5",
          overlay ? "py-4" : "mx-auto max-w-[72ch] py-3",
        )}
      >
        <div className={cn(COMPOSER_FIXED_ROW, "flex items-baseline gap-2")}>
          {isReply ? (
            <h2 className="min-w-0 flex-1 truncate text-list font-medium text-foreground">
              {draft.subject}
            </h2>
          ) : (
            <div className="relative min-w-0 flex-1">
              <GhostText
                value={draft.subject}
                suggestion={subjectGhost.suggestion}
                typography={SUBJECT_TYPE}
                multiline={false}
              />
              <input
                id="composer-subject"
                ref={subjectField}
                value={draft.subject}
                disabled={busy}
                onChange={(event) => {
                  onChange({ ...draft, subject: event.target.value });
                  trackCaret(event);
                }}
                onSelect={trackCaret}
                onFocus={(event) => {
                  setFocus("subject");
                  trackCaret(event);
                }}
                onBlur={() => setFocus((current) => (current === "subject" ? null : current))}
                onKeyDown={ghostKeys(
                  subjectGhost.suggestion,
                  subjectGhost.accept,
                  subjectGhost.dismiss,
                  (value) => onChange({ ...draft, subject: value }),
                )}
                placeholder="Subject"
                // The placeholder is the only thing naming this field, and a
                // placeholder disappears the moment there is a subject.
                aria-label="Subject"
                className={cn(
                  "w-full bg-transparent text-foreground",
                  SUBJECT_TYPE,
                  "placeholder:text-faint-foreground focus:outline-none",
                )}
              />
            </div>
          )}
          {!showCc && (
            <button
              type="button"
              onClick={() => setShowCc(true)}
              className="shrink-0 text-micro text-faint-foreground hover:text-foreground"
            >
              cc / bcc
            </button>
          )}
        </div>

        <div className={cn(COMPOSER_FIXED_ROW, "mt-2 space-y-1 border-b border-border pb-2")}>
          <AddressField label="To" typeahead={to} value={raw.to} disabled={busy} inputRef={toField} />
          {showCc && (
            <>
              <CompletedAddressField
                label="Cc"
                value={raw.cc}
                onChange={setField("cc")}
                contacts={contacts}
                disabled={busy}
              />
              <CompletedAddressField
                label="Bcc"
                value={raw.bcc}
                onChange={setField("bcc")}
                contacts={contacts}
                disabled={busy}
              />
            </>
          )}
        </div>

        <RichTextEditor
          docKey={draft.id}
          initialHtml={html}
          onChange={onBodyChange}
          disabled={busy}
          active={active}
          handle={editor}
          height={bodyHeight}
          initialCaret={initialCaret}
          bodyRef={bodyRef}
          inlineImages={inlineImages}
          dropping={dropTarget === "body"}
        />

        {/*
          Where the files have to land, and what letting go there will do.
          Drawn while anything is being dragged over the window, not only while
          it is over the composer: a target that appears once the pointer is
          already on it has told the user nothing they could act on. The ring —
          on the writing area or on the composer's own edge — is the second
          half; see `dropTarget` above.
        */}
        {dragging && (
          <div
            className={cn(
              COMPOSER_FIXED_ROW,
              "mt-2 rounded-[var(--radius)] border border-dashed px-3 py-2 text-micro",
              dropTarget
                ? "border-accent text-foreground"
                : "border-border text-faint-foreground",
            )}
          >
            {dropTarget === "body" && draggingInlinable
              ? "Drop in the message"
              : "Drop to attach"}
          </div>
        )}

        {(attachments.length > 0 || pending.length > 0) && (
          <ul className={cn(COMPOSER_FIXED_ROW, "mt-2 flex flex-wrap gap-1.5")}>
            {attachments.map((file) => (
              <AttachmentChip
                key={file.id}
                file={file}
                disabled={busy}
                onRemove={() => onRemoveAttachment(file.id)}
                onSetInline={onSetInline}
              />
            ))}
            {/*
              Last, because they are the newest and because the reconciled chip
              takes the same place in the row the pending one held.
            */}
            {pending.map((file) => (
              <PendingChip key={file.key} file={file} />
            ))}
          </ul>
        )}

        {scheduling && (
          <div
            className={cn(
              COMPOSER_FIXED_ROW,
              "mt-2 flex flex-wrap items-center gap-1.5 border-t border-border pt-2",
            )}
          >
            <span className="text-micro text-faint-foreground">Send</span>
            {options.map((option) => (
              <button
                key={option.label}
                type="button"
                onClick={() => send(option.at)}
                className={cn(
                  "rounded-[var(--radius)] border border-border px-2 py-0.5 text-micro",
                  "text-muted-foreground hover:border-border-strong hover:text-foreground",
                )}
              >
                {option.label}
              </button>
            ))}
          </div>
        )}

        {/*
          The footer: every act this panel offers, and the key for each one.
          It keeps its size while the message gives up as much as the panel
          asks for — see COMPOSER_FIXED_ROW. Named in the DOM because "is the
          footer still inside the panel" is a question worth being able to ask.

          It has a second state, and it is the same row rather than a row of
          its own: the question `discard` raises, and the one a send with no
          subject raises. Same box, same measure, same height — so answering it
          costs no layout and the writer's eye does not have to go anywhere.
          See `confirmingDiscard`, `confirmingSubject` and `asking`.

          The band is the half a swap of words cannot do. Both states are a
          line of micro type along the same edge, and read at a glance — which
          is the only way this row is ever read — one line of small grey text
          looks like the other. The surface changes under the whole row, so the
          eye that was on `discard` when it was clicked has an answer at the
          point it was looking.

          It changed colour rather than surface until he saw it: a red strip,
          which is the alarm a mail client raises for a message that failed to
          send, spent on a question he asked for. The band is a step of the
          gray ramp now and the only red in it is on the button that does the
          destroying. It carries no vertical padding either way: the row may
          not change height, or the message above it moves while being written.

          `-mx-2` pays back the padding the buttons carry, so the first key
          chip sits on the same left edge as the fields above it.
        */}
        <div
          data-mach-composer-footer=""
          className={cn(
            COMPOSER_FIXED_ROW,
            "-mx-2 mt-2 flex items-center gap-1 text-micro",
            asking
              ? "rounded-[var(--radius)] bg-surface-raised text-muted-foreground ring-1 ring-inset ring-border"
              : "text-faint-foreground",
          )}
        >
          {asking === "discard" ? (
            /*
              Not where the pointer is. It has just clicked `discard`, a third
              of the way along this row, and a destructive button under a
              pointer that is already moving is a draft lost to a double click.
              The answers sit at the far end of the same row, which is also
              where the bar this replaced put them.

              Discard has the focus, which the safe answer used to have — his
              call, and the right one for a question raised and answered by
              key. What makes that safe is `armed`, not distance: ⏎ can only
              reach it once the press that raised the question has been let go
              of. Escape still keeps the draft, from anywhere.
            */
            <ConfirmBand question="Discard this draft?">
              <FooterAction
                ref={answerButton}
                keys={COMPOSER_KEYS.discard}
                label="Discard"
                tone="destroy"
                // With nothing: `destroy(decided)` handed anything truthy is
                // told the press was a decision. See `armed`.
                onClick={() => destroy()}
              />
              <FooterAction
                keys={COMPOSER_KEYS.close}
                label="Keep"
                tone="answer"
                onClick={keep}
              />
            </ConfirmBand>
          ) : asking === "subject" ? (
            /*
              The same band, and that is the point of it: the second question a
              composer can ask arrives in the box the eye already knows, at the
              same height, with the answers at the same end.

              `Send` keeps the accent fill it wears in the legend two states
              back. It is the act the panel exists for either way, it is not
              destructive — the message still waits out its window with ⌘Z on
              offer — and the fill is what makes the answer findable in the
              hundred milliseconds this row is looked at for.
            */
            <ConfirmBand question="Send without a subject?">
              <FooterAction
                ref={answerButton}
                keys={COMPOSER_KEYS.send}
                label="Send"
                tone="primary"
                // With nothing, for the reason `destroy` is: a MouseEvent read
                // as `decided` would tell every click it was a decision.
                onClick={() => sendAnyway()}
              />
              <FooterAction
                keys={COMPOSER_KEYS.close}
                label="Add one"
                tone="answer"
                onClick={keepWriting}
              />
            </ConfirmBand>
          ) : (
            <>
              <FooterAction
                keys={COMPOSER_KEYS.send}
                label="send"
                tone="primary"
                disabled={busy}
                onClick={() => send()}
              />
              <FooterAction
                keys={COMPOSER_KEYS.schedule}
                label="later"
                disabled={busy}
                expanded={scheduling}
                onClick={() => setScheduling((open) => !open)}
              />
              {/* Not `onClick={onAttach}`: that hands the click event to `inline`. */}
              <FooterAction
                keys={COMPOSER_KEYS.attach}
                label="attach"
                disabled={busy}
                onClick={() => onAttach()}
              />
              <FooterAction
                keys={COMPOSER_KEYS.discard}
                label="discard"
                tone="danger"
                disabled={busy}
                onClick={onDiscard}
              />
              {onPopOut && (
                <FooterAction
                  keys={COMPOSER_KEYS.popOut}
                  label={poppedOut ? "put back" : "pop out"}
                  onClick={popOut}
                />
              )}
              <FooterAction keys={COMPOSER_KEYS.close} label="close" onClick={dismiss} />
              {/*
                This used to read "draft saved as you type" whenever nothing was
                happening — the software applauding itself for doing the one
                thing a draft is for. A draft that saves itself should just save
                itself. What is left is the state you cannot see and might act
                on: a send already in flight, which is why the fields have gone
                dead.
              */}
              {/*
                A draft is written here and pushed to Gmail in the background,
                so it is on his phone as well. When that push fails it is on
                this Mac and nowhere else — which he would otherwise have no way
                of knowing, and would find out by opening Gmail somewhere else
                and seeing nothing. Google's own words on hover, because "why"
                is a second question and only the first one is worth a line.
              */}
              {isLocalOnly(draft) && (
                <span
                  className="truncate px-2 text-danger"
                  title={draft.remote?.error ?? undefined}
                >
                  Not in Gmail
                </span>
              )}
              {/* Not a legend: it says a suggestion is waiting, which is news. */}
              <GhostHint shown={subjectGhost.suggestion !== ""} />
              <span className="ml-auto truncate px-2">{busy ? "Sending" : null}</span>
            </>
          )}
        </div>
      </div>
    </div>
  );
}

/**
 * The footer, being a question.
 *
 * One component for both of them, because the two must not drift: the whole
 * reason the second question reads at a glance is that it is the first one
 * again — same box, same measure, same height, answers at the same end. The
 * copy takes the measure with `flex-1`, which is what pushes them there and
 * what keeps a destructive answer out from under the pointer that just clicked.
 *
 * The band's surface is on the footer row itself rather than here: the row may
 * not change height when a question replaces the legend, and a wrapper with its
 * own box is the easiest way to make it.
 */
function ConfirmBand({ question, children }: { question: string; children: ReactNode }) {
  return (
    <>
      <span className="min-w-0 flex-1 truncate px-2 text-foreground">{question}</span>
      {children}
    </>
  );
}

/**
 * How much weight one footer control carries.
 *
 * Four steps, and the row uses three of them at a time. `primary` is the app's
 * one filled button, spent on the act the panel exists for — in the legend, and
 * again as the affirmative answer to the subject question, which is the same
 * act with one more keystroke in front of it; `danger` is quiet until the
 * pointer is on it, because a resting row of six controls with a red one in it
 * reads as an error; `destroy` and `answer` belong to the questions, where the
 * band underneath has already moved a step up the ramp and the controls have to
 * move with it.
 *
 * The chip moves too. `Kbd` sits on `surface-raised`, which is the band's own
 * colour and the hover fill's neighbour — left alone it would dissolve into
 * both exactly when the eye is on it.
 */
type FooterTone = "quiet" | "primary" | "danger" | "destroy" | "answer";

const FOOTER_TONES: Record<FooterTone, { button: string; chip: string }> = {
  quiet: {
    button: "text-faint-foreground hover:bg-row-hover hover:text-foreground active:bg-border",
    chip: "group-hover:border-border-strong group-hover:bg-background",
  },
  primary: {
    button: "bg-accent text-accent-foreground hover:brightness-110 active:brightness-95",
    chip: "border-accent-foreground/30 bg-accent-foreground/15 text-accent-foreground",
  },
  danger: {
    button: "text-faint-foreground hover:bg-danger/10 hover:text-danger active:bg-danger/20",
    chip: "group-hover:border-danger/40 group-hover:bg-background group-hover:text-danger",
  },
  destroy: {
    button: "bg-danger/10 text-danger hover:bg-danger/20 active:bg-danger/25",
    chip: "border-danger/30 bg-background text-danger",
  },
  answer: {
    button: "text-muted-foreground hover:bg-border hover:text-foreground active:bg-border-strong",
    chip: "border-border-strong bg-background",
  },
};

/**
 * One control on the footer row.
 *
 * The whole point is that every one of them is the same box: 24px tall, 8px of
 * padding either side, a key chip and a word. A row where the hit target is the
 * glyphs and nothing else is a row you have to aim at — and where half the
 * items were not controls at all, aiming at them did nothing.
 *
 * `cursor-pointer` against the app's own grain: `globals.css` sets the arrow
 * cursor on everything, which is what a Mac application does, and no button in
 * the app asks for the hand. He asked for it here by name, and the row he asked
 * about is the one place where the question "is this a control" was live.
 *
 * The focus ring is the app's, from `:focus-visible` in `globals.css` — inset,
 * so it is drawn inside a 24px box instead of over the row above it.
 */
function FooterAction({
  keys,
  label,
  tone = "quiet",
  disabled,
  expanded,
  onClick,
  ref,
}: {
  keys: string;
  label: string;
  tone?: FooterTone;
  disabled?: boolean;
  /** For a control that opens a row of its own — the schedule options. */
  expanded?: boolean;
  onClick: () => void;
  ref?: Ref<HTMLButtonElement>;
}) {
  const { button, chip } = FOOTER_TONES[tone];
  return (
    <button
      ref={ref}
      type="button"
      disabled={disabled}
      aria-expanded={expanded}
      // Called with nothing rather than handed the click: a handler that takes
      // an optional argument — `destroy(decided)` — would read the MouseEvent
      // as that argument and be told yes by every click. See `armed`.
      onClick={() => onClick()}
      className={cn(
        "group inline-flex h-6 shrink-0 cursor-pointer select-none items-center gap-1",
        "rounded-[var(--radius)] px-2 transition-colors",
        // Not dimmed and still hoverable: a disabled control that lights up
        // under the pointer is a control that looks broken rather than busy.
        "disabled:pointer-events-none disabled:opacity-40",
        button,
        // The same step the press itself paints, held for as long as the row
        // it opened is up. A hover-weight fill was invisible on a surface one
        // step away from it.
        expanded === true && "bg-border text-foreground",
      )}
    >
      <Kbd keys={keys} className={cn("transition-colors", chip)} />
      {label}
    </button>
  );
}

/**
 * One file, with the two things that can be done to it.
 *
 * The inline control is drawn only on images, because it is the only file type
 * where the choice exists — see `compose::attach::Attachment::inline`. It is a
 * button rather than a menu so it is one ⇥ and one ⏎ away, which is the whole
 * requirement: a message with a picture in it cannot be assembled by mouse
 * alone.
 */
function AttachmentChip({
  file,
  disabled,
  onRemove,
  onSetInline,
}: {
  file: DraftAttachment;
  disabled?: boolean;
  onRemove: () => void;
  onSetInline?: (attachmentId: string, inline: boolean) => void;
}) {
  const inline = file.inline === true;
  const choosable = onSetInline !== undefined && isInlinableImage(file);
  return (
    <li className={CHIP}>
      {inline ? (
        <ImageIcon className="size-3 shrink-0" />
      ) : (
        <Paperclip className="size-3 shrink-0" />
      )}
      <span className="min-w-0 truncate" title={file.filename}>
        {file.filename}
      </span>
      <span className="shrink-0 text-faint-foreground">{humanSize(file.sizeBytes)}</span>
      {choosable && (
        /*
         * The button is labelled with the *other* place the file could be, not
         * with where it is.
         *
         * It used to read "attached" on a chip that was itself the attachment —
         * the software saying out loud what the row it is drawn in already
         * says, which is the thing CLAUDE.md bans by name. Where the file
         * stands is carried by the icon and, for an inline image, by the
         * picture visible in the message a few pixels above; what a button owes
         * the reader is what pressing it will do.
         */
        <button
          type="button"
          disabled={disabled}
          onClick={() => onSetInline?.(file.id, !inline)}
          aria-label={
            inline
              ? `Attach ${file.filename} as a file`
              : `Show ${file.filename} in the message`
          }
          className="shrink-0 text-faint-foreground hover:text-foreground"
        >
          {inline ? "As file" : "In message"}
        </button>
      )}
      <button
        type="button"
        disabled={disabled}
        onClick={onRemove}
        aria-label={`Remove ${file.filename}`}
        className="shrink-0 text-faint-foreground hover:text-danger"
      >
        <X className="size-3" />
      </button>
    </li>
  );
}

/**
 * A file the store has not answered for yet.
 *
 * The same chip, from the only two things known before the round trip: the name
 * off the dropped path and which way it was headed. No size, because there is
 * no honest number to show yet — a chip that guessed one and corrected itself a
 * moment later would be worse than a chip that waits to say.
 *
 * No remove control either. The id Rust will give this file does not exist yet,
 * so there is nothing to send `attachRemove`; the chip is on screen for as long
 * as the attach takes and is replaced by one that *can* be removed.
 */
function PendingChip({ file }: { file: PendingAttachment }) {
  return (
    <li className={CHIP} aria-busy="true">
      <LoaderCircle
        className="size-3 shrink-0 animate-spin text-faint-foreground"
        aria-hidden
      />
      <span className="min-w-0 truncate" title={file.filename}>
        {file.filename}
      </span>
    </li>
  );
}

/**
 * Wide enough for a real filename next to a size and two controls. Below this
 * the name is the thing that gives way, and a chip reading "scree…" is a chip
 * that has stopped saying which file it is.
 */
const CHIP = cn(
  "inline-flex max-w-[40ch] items-center gap-1.5 rounded-[var(--radius)]",
  "border border-border px-2 py-0.5 text-micro text-muted-foreground",
);

/**
 * Cc and Bcc, which unlike To are not held by the parent — each needs its own
 * typeahead, and a hook cannot be called in a loop, so this is the wrapper that
 * gives one field one of everything.
 */
function CompletedAddressField({
  label,
  value,
  onChange,
  contacts,
  disabled,
}: {
  label: string;
  value: string;
  onChange: (value: string) => void;
  contacts: readonly Contact[];
  disabled?: boolean;
}) {
  const inputRef = useRef<HTMLInputElement>(null);
  const typeahead = useAddressTypeahead({
    value,
    contacts,
    onChange,
    fieldRef: inputRef,
    enabled: !disabled,
  });
  return (
    <AddressField
      label={label}
      value={value}
      typeahead={typeahead}
      disabled={disabled}
      inputRef={inputRef}
    />
  );
}

function AddressField({
  label,
  value,
  typeahead,
  disabled,
  inputRef,
}: {
  label: string;
  value: string;
  typeahead: ReturnType<typeof useAddressTypeahead>;
  disabled?: boolean;
  inputRef: RefObject<HTMLInputElement | null>;
}) {
  return (
    <div className="relative">
      <label className="flex min-w-0 items-baseline gap-2">
        <span className="w-7 shrink-0 text-micro uppercase tracking-wide text-faint-foreground">
          {label}
        </span>
        <input
          ref={inputRef}
          value={value}
          disabled={disabled}
          spellCheck={false}
          placeholder="name@example.com"
          {...typeaheadFieldProps(typeahead)}
          className={cn(
            "min-w-0 flex-1 bg-transparent text-body text-foreground",
            "placeholder:text-faint-foreground focus:outline-none disabled:opacity-60",
          )}
        />
      </label>
      <AddressSuggestions typeahead={typeahead} className="left-9" />
    </div>
  );
}
