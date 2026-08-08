import { useCallback, useEffect, useMemo, useRef, useState, type RefObject } from "react";
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
import type { Contact } from "@/lib/contacts";
import {
  COMPOSER_KEYS,
  formatRecipients,
  parseRecipients,
  scheduleOptions,
  type Draft,
} from "@/lib/compose";

interface ComposerProps {
  draft: Draft;
  onChange: (draft: Draft) => void;
  onSend: (scheduleAt?: number) => void;
  onClose: () => void;
  /** Set while the send is in flight — the fields go read-only, not blank. */
  busy?: boolean;
  /** Everyone the app has seen, for the address fields. */
  contacts?: readonly Contact[];
  /** Lines describing what is being answered, for the ghost completions. */
  ghostContext?: readonly string[];
}

/** Two-line minimum, then it grows; past this it scrolls instead. */
const MAX_EDITOR_HEIGHT = 15 * 22;

/** Shared by the editor and the ghost mirror behind it. They must not drift. */
const EDITOR_TYPE = "text-reading leading-[1.6]";
const SUBJECT_TYPE = "text-list font-medium";

/**
 * The composer.
 *
 * A `<textarea>`, three address fields, and two keys. There is no toolbar
 * because a toolbar is a confession that the editor cannot simply be typed
 * into, and no rich-text surface because the source of truth is the characters
 * you typed: `**bold**` stays `**bold**` in the plain-text part of the message
 * and becomes `<strong>` in the HTML one, which is the only behaviour that is
 * honest in both.
 *
 * The decisions worth knowing:
 *
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
 *  * **The editor grows with the text.** Growing rather than scrolling is what
 *    keeps the message being answered on screen, which is the entire reason the
 *    composer is inline.
 *  * **The footer is a legend, not a button bar.**
 *
 * # Completion
 *
 * Two kinds, and they are not the same feature. The address fields complete
 * from people the app has already seen — local, instant, and always available.
 * The subject and the body offer a grey continuation from the model, which
 * exists only when `ANTHROPIC_API_KEY` does; without it every one of those code
 * paths resolves to the empty string and the composer is exactly what it was.
 * Both are taken with ⇥ and refused by carrying on typing.
 */
export function Composer({
  draft,
  onChange,
  onSend,
  onClose,
  busy = false,
  contacts = [],
  ghostContext,
}: ComposerProps) {
  const editor = useRef<HTMLTextAreaElement>(null);
  const subjectField = useRef<HTMLInputElement>(null);
  const toField = useRef<HTMLInputElement>(null);
  const [showCc, setShowCc] = useState(draft.cc.length > 0 || draft.bcc.length > 0);
  const [scheduling, setScheduling] = useState(false);
  const [focus, setFocus] = useState<"body" | "subject" | null>(null);
  const [caretAtEnd, setCaretAtEnd] = useState(true);
  // The editor scrolls once the message outgrows fifteen lines; the ghost
  // mirror has to scroll with it or the suggestion lands in the wrong place.
  const [bodyScroll, setBodyScroll] = useState(0);

  const isReply = draft.kind === "reply" || draft.kind === "replyAll";

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
    if (draft.to.length === 0 && toField.current) toField.current.focus();
    else editor.current?.focus();
  }, [draft.id]); // eslint-disable-line -- on open, not on every keystroke

  // Grow to fit. Measured rather than counted, because a wrapped line is not a
  // newline.
  const resize = useCallback(() => {
    const node = editor.current;
    if (!node) return;
    node.style.height = "auto";
    node.style.height = `${Math.min(node.scrollHeight, MAX_EDITOR_HEIGHT)}px`;
  }, []);
  useEffect(resize, [draft.body, showCc, resize]);

  const options = useMemo(() => scheduleOptions(), [scheduling]);

  const send = useCallback(
    (at?: number) => {
      setScheduling(false);
      onSend(at);
    },
    [onSend],
  );

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

  // What the model is told about the message being written. Recipients and
  // subject only — the body it is completing is the prompt, and the thread
  // behind it arrives from the dock, which is the only place that has it.
  const bodyContext = useMemo(() => {
    const lines = [...(ghostContext ?? [])];
    if (draft.subject) lines.push(`Subject: ${draft.subject}`);
    const names = draft.to.map((m) => m.name ?? m.email).filter(Boolean);
    if (names.length > 0) lines.push(`Writing to: ${names.join(", ")}`);
    return lines;
  }, [ghostContext, draft.subject, draft.to]);

  const bodyGhost = useGhostText({
    kind: "emailBody",
    value: draft.body,
    context: bodyContext,
    active: !busy && focus === "body" && caretAtEnd,
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
      handler: () => send(),
    },
    {
      keys: COMPOSER_KEYS.schedule,
      group: "Composer",
      description: "Schedule send",
      allowInInput: true,
      priority: 100,
      handler: () => setScheduling((open) => !open),
    },
    {
      keys: COMPOSER_KEYS.close,
      group: "Composer",
      description: "Close the composer, keeping the draft",
      allowInInput: true,
      priority: 120,
      // A suggestion list is a popup, and the keymap runs in the capture phase
      // — declining here is what lets one Escape dismiss the list and the next
      // one close the composer. See `lib/popups.ts`.
      when: () => !anyPopupOpen(),
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

  return (
    <div className="shrink-0 border-t border-border bg-surface">
      <div className="mx-auto max-w-[72ch] px-5 py-3">
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

        <div className="relative mt-3">
          <GhostText
            value={draft.body}
            suggestion={bodyGhost.suggestion}
            typography={EDITOR_TYPE}
            scrollTop={bodyScroll}
          />
          <textarea
            ref={editor}
            value={draft.body}
            disabled={busy}
            spellCheck
            rows={4}
            onChange={(event) => {
              onChange({ ...draft, body: event.target.value });
              trackCaret(event);
            }}
            onSelect={trackCaret}
            onFocus={(event) => {
              setFocus("body");
              trackCaret(event);
            }}
            onBlur={() => setFocus((current) => (current === "body" ? null : current))}
            onKeyDown={ghostKeys(
              bodyGhost.suggestion,
              bodyGhost.accept,
              bodyGhost.dismiss,
              (value) => onChange({ ...draft, body: value }),
            )}
            onInput={resize}
            onScroll={(event) => setBodyScroll(event.currentTarget.scrollTop)}
            placeholder="Markdown-ish. **bold**, `code`, - lists."
            className={cn(
              "block w-full resize-none overflow-y-auto bg-transparent",
              EDITOR_TYPE,
              "text-foreground placeholder:text-faint-foreground focus:outline-none disabled:opacity-60",
            )}
          />
        </div>

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
          <span className="inline-flex items-center gap-1">
            <Kbd keys={COMPOSER_KEYS.close} /> close
          </span>
          <GhostHint shown={bodyGhost.suggestion !== "" || subjectGhost.suggestion !== ""} />
          <span className="ml-auto truncate">
            {busy ? "sending…" : "draft saved as you type"}
          </span>
        </div>
      </div>
    </div>
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
