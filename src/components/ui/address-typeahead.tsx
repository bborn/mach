import {
  useCallback,
  useId,
  useMemo,
  useState,
  type ChangeEvent,
  type KeyboardEvent,
  type RefObject,
  type SyntheticEvent,
} from "react";
import { usePopupRegistration } from "./popup";
import { cn } from "@/lib/utils";
import { contactValue, rankContacts, type Contact } from "@/lib/contacts";
import { committedAddresses, replaceToken, tokenQuery } from "@/lib/typeahead";

/**
 * Address completion, for every field in the app that holds a list of people.
 *
 * Two of those exist — the composer's To/Cc/Bcc and the event modal's guest
 * list — and they are built out of different primitives (a bare `<input>`, a
 * growing `<Textarea>`), so this is a hook plus a list rather than a component
 * that owns the field. The hook holds the caret, the matches and the keys; the
 * list draws them.
 *
 * # The keys
 *
 * ↑↓ move, ⏎ and ⇥ accept, Escape dismisses. Every one of those is a key
 * something else in Mach also wants, which is why the list registers itself
 * with `lib/popups.ts` for as long as it is on screen: the keymap listens in
 * the capture phase, so an Escape that closed the composer would never reach
 * this field. Bindings that decline while a popup is open are what make one
 * press dismiss the list and the next press close the composer.
 *
 * # Why it never reorders under your fingers
 *
 * Matching is prefix-first and never fuzzy (see `lib/contacts.ts`), and the
 * highlighted row resets to the top on every keystroke. The row under the
 * cursor is one ⏎ away from being a recipient; a list that shuffles is how the
 * wrong person gets a message.
 */

export interface AddressTypeahead {
  matches: Contact[];
  open: boolean;
  index: number;
  listId: string;
  activeId: string | undefined;
  setIndex: (index: number) => void;
  accept: (contact: Contact) => void;
  /** Wire to the field's `onChange` — it forwards the value and takes the caret. */
  handleChange: (event: ChangeEvent<HTMLInputElement | HTMLTextAreaElement>) => void;
  /** Wire to `onSelect`, so clicking into the middle of the line re-matches. */
  handleSelect: (event: SyntheticEvent<HTMLInputElement | HTMLTextAreaElement>) => void;
  handleKeyDown: (event: KeyboardEvent<HTMLInputElement | HTMLTextAreaElement>) => void;
  handleFocus: () => void;
  handleBlur: () => void;
}

export interface AddressTypeaheadOptions {
  value: string;
  contacts: readonly Contact[];
  onChange: (value: string) => void;
  fieldRef: RefObject<HTMLInputElement | null> | RefObject<HTMLTextAreaElement | null>;
  enabled?: boolean;
  limit?: number;
}

export function useAddressTypeahead({
  value,
  contacts,
  onChange,
  fieldRef,
  enabled = true,
  limit = 6,
}: AddressTypeaheadOptions): AddressTypeahead {
  const listId = useId();
  const [caret, setCaret] = useState(value.length);
  const [focused, setFocused] = useState(false);
  const [index, setIndex] = useState(0);
  // Escape means "not this time", not "not ever": it holds until the field
  // changes again, so dismissing does not disable the feature for the session.
  const [dismissedAt, setDismissedAt] = useState<string | null>(null);

  const query = useMemo(() => tokenQuery(value, caret), [value, caret]);

  const matches = useMemo(() => {
    if (!enabled || !focused) return [];
    // Nothing typed, nothing offered. Listing your six most-written-to people
    // the instant the field takes focus sounds helpful and is not: it puts a
    // panel over the message on every reply, and it makes ⇥ — which has always
    // meant "on to the body" — accept a recipient instead.
    if (!query) return [];
    return rankContacts(contacts, query, {
      limit,
      exclude: committedAddresses(value, caret),
    });
  }, [enabled, focused, contacts, query, value, caret, limit]);

  const open = matches.length > 0 && dismissedAt !== value;
  const active = open ? matches[Math.min(index, matches.length - 1)] : undefined;

  usePopupRegistration(open);

  const accept = useCallback(
    (contact: Contact) => {
      const next = replaceToken(value, caret, contactValue(contact));
      onChange(next.value);
      setCaret(next.caret);
      setIndex(0);
      // The caret has to land after the address that was just inserted, and
      // React will have repainted the field by the time this runs.
      const field = fieldRef.current;
      requestAnimationFrame(() => {
        if (!field) return;
        field.focus();
        field.setSelectionRange(next.caret, next.caret);
      });
    },
    [value, caret, onChange, fieldRef],
  );

  const handleChange = useCallback(
    (event: ChangeEvent<HTMLInputElement | HTMLTextAreaElement>) => {
      const target = event.target;
      setCaret(target.selectionStart ?? target.value.length);
      setIndex(0);
      setDismissedAt(null);
      onChange(target.value);
    },
    [onChange],
  );

  const handleSelect = useCallback(
    (event: SyntheticEvent<HTMLInputElement | HTMLTextAreaElement>) => {
      const target = event.currentTarget;
      setCaret(target.selectionStart ?? target.value.length);
    },
    [],
  );

  const handleKeyDown = useCallback(
    (event: KeyboardEvent<HTMLInputElement | HTMLTextAreaElement>) => {
      if (!open) return;
      if (event.key === "ArrowDown" || event.key === "ArrowUp") {
        event.preventDefault();
        event.stopPropagation();
        const step = event.key === "ArrowDown" ? 1 : matches.length - 1;
        setIndex((current) => (current + step) % matches.length);
        return;
      }
      if (event.key === "Enter" || event.key === "Tab") {
        // ⌘⏎ is send, and send must win even with a list on screen.
        if (event.metaKey || event.ctrlKey) return;
        event.preventDefault();
        event.stopPropagation();
        if (active) accept(active);
        return;
      }
      if (event.key === "Escape") {
        event.preventDefault();
        event.stopPropagation();
        setDismissedAt(value);
      }
    },
    [open, matches.length, active, accept, value],
  );

  const handleFocus = useCallback(() => setFocused(true), []);
  // Deferred, because picking a row with the mouse blurs the field first and a
  // list that vanishes on mousedown can never be clicked.
  const handleBlur = useCallback(() => {
    window.setTimeout(() => setFocused(false), 120);
  }, []);

  return {
    matches,
    open,
    index: open ? Math.min(index, matches.length - 1) : 0,
    listId,
    activeId: active ? `${listId}-${active.email}` : undefined,
    setIndex,
    accept,
    handleChange,
    handleSelect,
    handleKeyDown,
    handleFocus,
    handleBlur,
  };
}

/** The props every completed field needs, so no call site forgets one. */
export function typeaheadFieldProps(typeahead: AddressTypeahead) {
  return {
    role: "combobox" as const,
    "aria-expanded": typeahead.open,
    "aria-controls": typeahead.open ? typeahead.listId : undefined,
    "aria-activedescendant": typeahead.activeId,
    "aria-autocomplete": "list" as const,
    autoComplete: "off",
    onChange: typeahead.handleChange,
    onSelect: typeahead.handleSelect,
    onKeyDown: typeahead.handleKeyDown,
    onFocus: typeahead.handleFocus,
    onBlur: typeahead.handleBlur,
  };
}

/**
 * The list itself. Absolutely positioned, so the caller wraps the field in
 * something `relative` and nothing reflows when a suggestion appears.
 */
export function AddressSuggestions({
  typeahead,
  className,
}: {
  typeahead: AddressTypeahead;
  className?: string;
}) {
  if (!typeahead.open) return null;

  return (
    <ul
      id={typeahead.listId}
      role="listbox"
      aria-label="Matching people"
      className={cn(
        "absolute left-0 top-full z-50 mt-1 max-h-56 w-full min-w-[18rem] max-w-[28rem]",
        "overflow-y-auto rounded-[var(--radius)] border border-border-strong bg-surface py-1",
        "shadow-[0_16px_48px_-12px_rgba(0,0,0,0.35)]",
        className,
      )}
    >
      {typeahead.matches.map((contact, at) => (
        <li
          key={contact.email}
          id={`${typeahead.listId}-${contact.email}`}
          role="option"
          aria-selected={at === typeahead.index}
          onMouseMove={() => typeahead.setIndex(at)}
          // `mousedown` rather than `click`: the field blurs on mousedown, and
          // by the time a click lands the list would be closing.
          onMouseDown={(event) => {
            event.preventDefault();
            typeahead.accept(contact);
          }}
          className={cn(
            "flex cursor-default items-baseline gap-2 px-2.5 py-1 text-list",
            at === typeahead.index ? "bg-surface-raised text-foreground" : "text-muted-foreground",
          )}
        >
          <span className="min-w-0 shrink truncate">{contact.name ?? contact.email}</span>
          {contact.name && (
            <span className="min-w-0 flex-1 truncate text-micro text-faint-foreground">
              {contact.email}
            </span>
          )}
          {contact.self && <span className="ml-auto shrink-0 text-micro text-faint-foreground">you</span>}
        </li>
      ))}
    </ul>
  );
}
