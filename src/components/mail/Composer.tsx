import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type Ref,
  type RefObject,
} from "react";
import { useKeyBindings } from "@/hooks/useKeymap";
import { Kbd } from "@/components/ui/kbd";
import { cn } from "@/lib/utils";
import {
  COMPOSER_KEYS,
  formatRecipients,
  isLocalOnly,
  parseRecipients,
  scheduleOptions,
  type Draft,
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
  onChange: (draft: Draft) => void;
  onSend: (scheduleAt?: number) => void;
  onClose: () => void;
  /** Set while the send is in flight — the fields go read-only, not blank. */
  busy?: boolean;
  presentation?: ComposerPresentation;
  /**
   * Somewhere for the surface around this to hold on to the To field.
   *
   * The overlay traps focus and places it, and its effect runs *after* this
   * component's — so the composer focusing the field itself is not enough, and
   * the overlay's own fallback ("the first input in the panel") lands on the
   * subject. This is how it is told which field is the one to start in.
   */
  toRef?: RefObject<HTMLInputElement | null>;
}

/** Two-line minimum, then it grows; past this it scrolls instead. */
const MAX_EDITOR_HEIGHT = 15 * 22;

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
 */
export function Composer({
  draft,
  onChange,
  onSend,
  onClose,
  busy = false,
  presentation = "dock",
  toRef,
}: ComposerProps) {
  const editor = useRef<HTMLTextAreaElement>(null);
  const ownToField = useRef<HTMLInputElement>(null);
  const toField = toRef ?? ownToField;
  const [showCc, setShowCc] = useState(draft.cc.length > 0 || draft.bcc.length > 0);
  const [scheduling, setScheduling] = useState(false);

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
      description: "Close, keeping the draft",
      allowInInput: true,
      priority: 120,
      handler: () => {
        if (scheduling) setScheduling(false);
        else onClose();
      },
    },
  ]);

  const setField = (which: "to" | "cc" | "bcc") => (value: string) => {
    setRaw((current) => ({ ...current, [which]: value }));
    onChange({ ...draft, [which]: parseRecipients(value) });
  };

  const overlay = presentation === "overlay";

  return (
    // The overlay's panel already provides the surface, the border and the
    // shadow, and it is not attached to anything above it — so it gets neither
    // the top rule that joins the dock to the reading pane nor the reading
    // pane's own measure.
    <div className={cn(!overlay && "shrink-0 border-t border-border bg-surface")}>
      <div className={cn("px-5", overlay ? "py-4" : "mx-auto max-w-[72ch] py-3")}>
        <div className="flex items-baseline gap-2">
          {isReply ? (
            <h2 className="min-w-0 flex-1 truncate text-list font-medium text-foreground">
              {draft.subject}
            </h2>
          ) : (
            <input
              id="composer-subject"
              value={draft.subject}
              disabled={busy}
              onChange={(event) => onChange({ ...draft, subject: event.target.value })}
              placeholder="Subject"
              className={cn(
                "min-w-0 flex-1 bg-transparent text-list font-medium text-foreground",
                "placeholder:text-faint-foreground focus:outline-none",
              )}
            />
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
          <AddressField
            label="To"
            value={raw.to}
            onChange={setField("to")}
            disabled={busy}
            inputRef={toField}
          />
          {showCc && (
            <>
              <AddressField label="Cc" value={raw.cc} onChange={setField("cc")} disabled={busy} />
              <AddressField
                label="Bcc"
                value={raw.bcc}
                onChange={setField("bcc")}
                disabled={busy}
              />
            </>
          )}
        </div>

        <textarea
          ref={editor}
          value={draft.body}
          disabled={busy}
          spellCheck
          rows={4}
          onChange={(event) => onChange({ ...draft, body: event.target.value })}
          onInput={resize}
          placeholder="**bold**, `code`, - lists"
          className={cn(
            "mt-3 block w-full resize-none overflow-y-auto bg-transparent",
            "text-reading leading-[1.6] text-foreground",
            "placeholder:text-faint-foreground focus:outline-none disabled:opacity-60",
          )}
        />

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
          <span className="ml-auto truncate">{busy ? "Sending" : null}</span>
        </div>
      </div>
    </div>
  );
}

function AddressField({
  label,
  value,
  onChange,
  disabled,
  inputRef,
}: {
  label: string;
  value: string;
  onChange: (value: string) => void;
  disabled?: boolean;
  inputRef?: Ref<HTMLInputElement>;
}) {
  return (
    <label className="flex min-w-0 items-baseline gap-2">
      <span className="w-7 shrink-0 text-micro uppercase tracking-wide text-faint-foreground">
        {label}
      </span>
      <input
        ref={inputRef}
        value={value}
        disabled={disabled}
        spellCheck={false}
        autoComplete="off"
        onChange={(event) => onChange(event.target.value)}
        placeholder="name@example.com"
        className={cn(
          "min-w-0 flex-1 bg-transparent text-body text-foreground",
          "placeholder:text-faint-foreground focus:outline-none disabled:opacity-60",
        )}
      />
    </label>
  );
}
