import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type RefObject,
} from "react";
import { Paperclip, X } from "lucide-react";
import { useKeyBindings } from "@/hooks/useKeymap";
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
import {
  COMPOSER_KEYS,
  formatRecipients,
  humanSize,
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
  onAttach: () => void;
  onRemoveAttachment: (attachmentId: string) => void;
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
  /** Files are being dragged over this composer. */
  dropping?: boolean;
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
 *  * **The footer is a legend, not a button bar.** The two exceptions are the
 *    two acts with consequences outside this panel: attaching a file, and
 *    throwing the draft away.
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
  onAttach,
  onRemoveAttachment,
  onPopOut,
  poppedOut = false,
  bodyHeight,
  initialCaret = null,
  busy = false,
  presentation = "dock",
  active = true,
  dropping = false,
  toRef,
  bodyRef,
  contacts = [],
  ghostContext,
}: ComposerProps) {
  const editor = useRef<RichTextEditorHandle>(null);
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

  const send = useCallback(
    (at?: number) => {
      setScheduling(false);
      onSend(at);
    },
    [onSend],
  );

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

  useKeyBindings([
    {
      keys: COMPOSER_KEYS.send,
      group: "Composer",
      description: "Send",
      allowInInput: true,
      priority: 100,
      when: () => active,
      handler: () => send(),
    },
    {
      keys: COMPOSER_KEYS.schedule,
      group: "Composer",
      description: "Schedule send",
      allowInInput: true,
      priority: 100,
      when: () => active,
      handler: () => setScheduling((open) => !open),
    },
    {
      keys: COMPOSER_KEYS.attach,
      group: "Composer",
      description: "Attach files",
      allowInInput: true,
      priority: 100,
      when: () => active && !busy,
      handler: () => onAttach(),
    },
    {
      keys: COMPOSER_KEYS.discard,
      group: "Composer",
      description: "Discard this draft",
      allowInInput: true,
      priority: 100,
      when: () => active && !busy,
      handler: () => onDiscard(),
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
      handler: () => {
        if (scheduling) setScheduling(false);
        else onClose();
      },
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
      className={cn(
        !overlay && "shrink-0 border-t border-border bg-surface",
        // A drop target has to say so before the file is let go, or the only
        // feedback is whether it worked.
        dropping && "ring-1 ring-inset ring-accent",
      )}
    >
      <div className={cn("px-5", overlay ? "py-4" : "mx-auto max-w-[72ch] py-3")}>
        <div className="flex items-baseline gap-2">
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

        <div className="mt-2 space-y-1 border-b border-border pb-2">
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
        />

        {attachments.length > 0 && (
          <ul className="mt-2 flex flex-wrap gap-1.5">
            {attachments.map((file) => (
              <AttachmentChip
                key={file.id}
                file={file}
                disabled={busy}
                onRemove={() => onRemoveAttachment(file.id)}
              />
            ))}
          </ul>
        )}

        {scheduling && (
          <div className="mt-2 flex flex-wrap items-center gap-1.5 border-t border-border pt-2">
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

        <div className="mt-2 flex items-center gap-3 text-micro text-faint-foreground">
          <span className="inline-flex items-center gap-1">
            <Kbd keys={COMPOSER_KEYS.send} /> send
          </span>
          <span className="inline-flex items-center gap-1">
            <Kbd keys={COMPOSER_KEYS.schedule} /> later
          </span>
          <button
            type="button"
            disabled={busy}
            onClick={onAttach}
            className="inline-flex items-center gap-1 hover:text-foreground"
          >
            <Paperclip className="size-3" /> attach
          </button>
          <button
            type="button"
            disabled={busy}
            onClick={onDiscard}
            className="inline-flex items-center gap-1 hover:text-danger"
          >
            discard
          </button>
          {onPopOut && (
            <button
              type="button"
              onClick={popOut}
              className="inline-flex items-center gap-1 hover:text-foreground"
            >
              <Kbd keys={COMPOSER_KEYS.popOut} /> {poppedOut ? "put back" : "pop out"}
            </button>
          )}
          <span className="inline-flex items-center gap-1">
            <Kbd keys={COMPOSER_KEYS.close} /> close
          </span>
          {/*
            This used to read "draft saved as you type" whenever nothing was
            happening — the software applauding itself for doing the one thing
            a draft is for. A draft that saves itself should just save itself.
            What is left is the state you cannot see and might act on: a send
            already in flight, which is why the fields have gone dead.
          */}
          {/*
            A draft is written here and pushed to Gmail in the background, so
            it is on his phone as well. When that push fails it is on this Mac
            and nowhere else — which he would otherwise have no way of knowing,
            and would find out by opening Gmail somewhere else and seeing
            nothing. Google's own words on hover, because "why" is a second
            question and only the first one is worth a line.
          */}
          {isLocalOnly(draft) && (
            <span className="truncate text-danger" title={draft.remote?.error ?? undefined}>
              Not in Gmail
            </span>
          )}
          {/* Not a legend: it says a suggestion is waiting, which is news. */}
          <GhostHint shown={subjectGhost.suggestion !== ""} />
          <span className="ml-auto truncate">{busy ? "Sending" : null}</span>
        </div>
      </div>
    </div>
  );
}

function AttachmentChip({
  file,
  disabled,
  onRemove,
}: {
  file: DraftAttachment;
  disabled?: boolean;
  onRemove: () => void;
}) {
  return (
    <li
      className={cn(
        "inline-flex max-w-[22ch] items-center gap-1.5 rounded-[var(--radius)]",
        "border border-border px-2 py-0.5 text-micro text-muted-foreground",
      )}
    >
      <Paperclip className="size-3 shrink-0" />
      <span className="min-w-0 truncate" title={file.filename}>
        {file.filename}
      </span>
      <span className="shrink-0 text-faint-foreground">{humanSize(file.sizeBytes)}</span>
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
