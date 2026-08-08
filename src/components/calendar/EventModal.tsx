import {
  Calendar as CalendarIcon,
  Copy,
  ExternalLink,
  Layers,
  MapPin,
  Repeat,
  Trash2,
  Users,
  Video,
} from "lucide-react";
import {
  useEffect,
  useId,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
} from "react";
import type { Account, Calendar, CalendarEvent, CalendarId, Rsvp } from "@/types";
import { Overlay } from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { DateField, TimeField } from "@/components/ui/date-field";
import {
  Field,
  FieldContent,
  FieldDescription,
  FieldError,
  FieldGroup,
  FieldLabel,
} from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { Kbd } from "@/components/ui/kbd";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Textarea } from "@/components/ui/textarea";
import { GhostHint, GhostText } from "@/components/ui/ghost";
import {
  AddressSuggestions,
  typeaheadFieldProps,
  useAddressTypeahead,
} from "@/components/ui/address-typeahead";
import { useKeyBindings } from "@/hooks/useKeymap";
import { useGhostText } from "@/hooks/useGhostText";
import type { Contact } from "@/lib/contacts";
import { anyPopupOpen } from "@/lib/popups";
import type { EventScope } from "@/lib/data";
import { calendarFill, toneFor, type HueIndex } from "@/lib/calendar-palette";
import { conferenceLink, googleCalendarUrl } from "@/lib/calendar-links";
import type { MergedEvent } from "@/lib/calendar-merge";
import {
  REMINDER_CHOICES,
  RECURRENCE_CHOICES,
  emptyForm,
  formFromEvent,
  formPatch,
  formTimes,
  isFormError,
  type EventForm,
} from "@/lib/calendar-edit";
import { fullDate } from "@/lib/time";
import { calendarLabel } from "./CalendarSidebar";

export type ModalTarget =
  | { mode: "view"; event: CalendarEvent }
  | { mode: "create"; start: number; end: number; allDay?: boolean; title?: string };

export interface EventModalProps {
  target: ModalTarget | null;
  calendars: Calendar[];
  accounts: Account[];
  hueFor: (id: CalendarId) => HueIndex;
  dark: boolean;
  merged: MergedEvent | null;
  /** Default calendar for a new event — the first visible one. */
  defaultCalendarId: CalendarId | null;
  /** Whether this event is one of a series — see `looksRecurring`. */
  recurring: boolean;
  busy: boolean;
  /** Everyone the app has seen, for completing the guest list. */
  contacts?: readonly Contact[];
  onClose: () => void;
  onSave: (form: EventForm, scope: EventScope) => void;
  onDelete: (scope: EventScope) => void;
  onDuplicate: () => void;
  onRsvp: (response: Rsvp) => void;
  onOpenExternal: (url: string) => void;
}

/** "Calendar default" is a `null` offset; the select needs a string for it. */
const REMINDER_DEFAULT = "default";

/**
 * The event editor.
 *
 * A **modal**, not a side rail — asked for directly, and it is the right call:
 * a 256px rail could show a title and a time, so every real edit meant leaving
 * for Google Calendar. The rail was a viewer wearing an editor's name.
 *
 * # Not one native control
 *
 * Every field is a Base UI primitive from `components/ui` wearing the app's
 * tokens. That is not a purity exercise. The first version of this modal used
 * `<select>`, `<textarea>` and `<input type="date">`, and WebKit drew them with
 * stepper arrows, resize grips and its own metrics — so a dense, near
 * monochrome app had one panel in it that looked like a web form from 2011.
 * Nothing in a stylesheet reaches inside those controls, which is the whole
 * reason the component library exists.
 *
 * # Keyboard first
 *
 * Everything here is reachable and completable without a mouse. Tab walks the
 * fields in reading order, `⌘⏎` saves from anywhere including mid-field, and
 * Escape cancels. Both are registered with the keymap at overlay priority and
 * `allowInInput`, so they fire while the caret is in a text field — where a
 * plain window listener would not, and where a per-input `onKeyDown` would have
 * to be repeated on nine fields and would drift.
 *
 * The one addition is `anyPopupOpen()` on each binding. The keymap listens in
 * the capture phase, so without it the first Escape out of an open select menu
 * would close the menu *and* discard the edit. Declining hands the key back to
 * Base UI, which closes only the menu. See `lib/popups.ts`.
 *
 * # The recurring-edit prompt
 *
 * Saving or deleting an occurrence of a series asks which occurrences it means,
 * as an inline two-button prompt rather than a second modal — a dialog on top
 * of a dialog is where keyboard focus goes to die. The prompt is a pair of
 * buttons in the tab order, so the choice is as reachable as the thing that
 * raised it.
 */
export function EventModal({
  target,
  calendars,
  accounts,
  hueFor,
  dark,
  merged,
  defaultCalendarId,
  recurring,
  busy,
  contacts = [],
  onClose,
  onSave,
  onDelete,
  onDuplicate,
  onRsvp,
  onOpenExternal,
}: EventModalProps) {
  const open = target !== null;
  const event = target?.mode === "view" ? target.event : null;
  const [form, setForm] = useState<EventForm | null>(null);
  /** Set when a series edit needs the this/all answer before it can run. */
  const [scopePrompt, setScopePrompt] = useState<"save" | "delete" | null>(null);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const titleField = useRef<HTMLInputElement>(null);
  const guestField = useRef<HTMLTextAreaElement>(null);
  const [ghostFocus, setGhostFocus] = useState<"title" | "notes" | null>(null);
  const [caretAtEnd, setCaretAtEnd] = useState(true);
  const ids = useFieldIds();

  // Rebuild the form whenever the modal points at something new. Keyed on the
  // event id and mode rather than on the object, so a background sync that
  // re-fetches the same event does not throw away what is being typed.
  const key = target === null ? null : target.mode === "view" ? `v${target.event.id}` : "create";
  useEffect(() => {
    if (target === null) {
      setForm(null);
      return;
    }
    setScopePrompt(null);
    setConfirmDelete(false);
    if (target.mode === "view") {
      setForm(formFromEvent(target.event));
    } else {
      setForm({
        ...emptyForm({
          start: target.start,
          end: target.end,
          allDay: target.allDay,
          calendarId: defaultCalendarId ?? "",
        }),
        title: target.title ?? "",
      });
    }
    // The title is where a new event starts and where an edit usually goes.
    window.requestAnimationFrame(() => titleField.current?.select());
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key]);

  const times = form ? formTimes(form) : null;
  const timeError = times && isFormError(times) ? times.error : null;
  const dirty = Boolean(
    form && (event ? formPatch(event, form) !== undefined || form.calendarId !== event.calendarId : true),
  );

  // The trigger shows a label, so Base UI needs the value→label map up front —
  // it cannot read one out of a popup that has never been mounted.
  const calendarItems = useMemo(
    () =>
      calendars.map((calendar) => {
        const owner = accounts.find((a) => a.id === calendar.accountId);
        return {
          value: calendar.id,
          label: `${calendarLabel(calendar)}${owner ? ` · ${owner.email}` : ""}`,
        };
      }),
    [calendars, accounts],
  );

  /**
   * Save — but only after whatever a field is still holding.
   *
   * `DateField` and `TimeField` keep the text you typed until it is *committed*,
   * on Enter or on blur. The keymap listens in the **capture** phase, so `⌘⏎`
   * stops the event before the field's own Enter handler ever runs: saving
   * straight from here sent the value from before the last thing typed, closed
   * the modal, and looked like it had worked. Clicking Save had the same shape —
   * the button was disabled until the field committed, so the first click only
   * blurred the field and the second one saved.
   *
   * So `submit` does not save. It blurs whatever is focused, which flushes the
   * field, and bumps a nonce. Both land in the same React batch, so the effect
   * below runs one render later — against the committed form rather than the
   * stale one.
   */
  const [saveNonce, setSaveNonce] = useState(0);
  const submit = () => {
    if (!form || busy || timeError) return;
    const active = document.activeElement;
    if (active instanceof HTMLElement && active !== document.body) active.blur();
    setSaveNonce((n) => n + 1);
  };

  useEffect(() => {
    if (saveNonce === 0 || !form || busy) return;
    if (timeError) return;
    if (event && !dirty) {
      onClose();
      return;
    }
    if (event && recurring) {
      setScopePrompt("save");
      return;
    }
    onSave(form, "this");
    // Only the nonce may start a save; every other value here is read fresh
    // from the render the nonce caused.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [saveNonce]);

  const remove = () => {
    if (!event || busy) return;
    if (recurring) {
      setScopePrompt("delete");
      return;
    }
    if (!confirmDelete) {
      setConfirmDelete(true);
      return;
    }
    onDelete("this");
  };

  // A select menu or a date grid owns the keyboard for as long as it is up: its
  // Escape closes it, and ⌘⏎ must not save out from under a half-made choice.
  const live = () => open && !anyPopupOpen();

  useKeyBindings([
    {
      keys: "mod+enter",
      priority: 120,
      allowInInput: true,
      group: "Event",
      description: "Save the event",
      when: live,
      handler: submit,
    },
    {
      keys: "escape",
      priority: 120,
      allowInInput: true,
      group: "Event",
      description: "Close without saving",
      when: live,
      handler: () => {
        if (scopePrompt !== null) {
          setScopePrompt(null);
          return;
        }
        if (confirmDelete) {
          setConfirmDelete(false);
          return;
        }
        onClose();
      },
    },
    {
      keys: "mod+backspace",
      priority: 120,
      allowInInput: true,
      group: "Event",
      description: "Delete the event",
      when: () => live() && event !== null,
      handler: remove,
    },
    {
      keys: "mod+d",
      priority: 120,
      allowInInput: true,
      group: "Event",
      description: "Duplicate the event",
      when: () => live() && event !== null,
      handler: () => onDuplicate(),
    },
  ]);

  /* ------------------------------------------------------------ completion */

  // Hooks, so they run before the `!form` bail-out below. Both are inert while
  // the modal is closed: no matches, no requests.
  const guests = useAddressTypeahead({
    value: form?.attendees ?? "",
    contacts,
    onChange: (attendees) =>
      setForm((current) => (current ? { ...current, attendees } : current)),
    fieldRef: guestField,
    enabled: open && !busy,
  });

  /** What the model is told about the event whose text it is finishing. */
  const eventContext = useMemo(() => {
    if (!form) return undefined;
    const lines = [`A calendar event on ${form.startDate}, ${form.startTime}–${form.endTime}.`];
    if (form.location) lines.push(`Location: ${form.location}`);
    if (form.attendees) lines.push(`Guests: ${form.attendees}`);
    return lines;
  }, [form]);

  const titleGhost = useGhostText({
    kind: "eventTitle",
    value: form?.title ?? "",
    context: eventContext,
    active: open && !busy && ghostFocus === "title" && caretAtEnd,
  });

  const notesGhost = useGhostText({
    kind: "eventDescription",
    value: form?.description ?? "",
    context: eventContext ? [...eventContext, `Title: ${form?.title ?? ""}`] : undefined,
    active: open && !busy && ghostFocus === "notes" && caretAtEnd,
  });

  const trackCaret = (event: { currentTarget: HTMLInputElement | HTMLTextAreaElement }) => {
    const node = event.currentTarget;
    setCaretAtEnd(
      node.selectionStart === node.selectionEnd &&
        (node.selectionStart ?? 0) === node.value.length,
    );
  };

  /** ⇥ takes the grey text; Escape refuses it without closing the modal. */
  const ghostKeys =
    (ghost: { suggestion: string; accept: () => string | null; dismiss: () => void }, apply: (value: string) => void) =>
    (keyEvent: ReactKeyboardEvent) => {
      if (!ghost.suggestion) return;
      if (keyEvent.key === "Tab" && !keyEvent.shiftKey && !keyEvent.metaKey && !keyEvent.ctrlKey) {
        keyEvent.preventDefault();
        keyEvent.stopPropagation();
        const next = ghost.accept();
        if (next !== null) apply(next);
        return;
      }
      if (keyEvent.key === "Escape") {
        keyEvent.preventDefault();
        keyEvent.stopPropagation();
        ghost.dismiss();
      }
    };

  if (!open || !form) return null;

  const account = event ? accounts.find((a) => a.id === event.accountId) : null;
  const conference = event ? conferenceLink(event) : null;
  const hue = hueFor(form.calendarId);
  const tone = event ? toneFor(event.rsvp) : "solid";
  const set = (patch: Partial<EventForm>) => setForm((current) => ({ ...current!, ...patch }));

  return (
    <Overlay open onClose={onClose} align="center" className="max-w-[36rem]" labelledBy="event-title">
      <div className="flex items-center gap-2 border-b border-border px-3 py-2">
        <span
          className="h-2.5 w-2.5 shrink-0 rounded-[3px]"
          style={{ backgroundColor: calendarFill(hue, dark) }}
        />
        <h2 id="event-title" className="min-w-0 flex-1 truncate text-list font-medium text-foreground">
          {event ? "Event" : "New event"}
        </h2>
        {recurring && (
          <span className="inline-flex items-center gap-1 text-micro text-faint-foreground">
            <Repeat size={11} strokeWidth={1.75} />
            repeats
          </span>
        )}
        <span className="inline-flex items-center gap-1.5 text-micro text-faint-foreground">
          <span>
            <Kbd keys="mod+enter" /> save · <Kbd keys="escape" /> close
          </span>
          <GhostHint shown={titleGhost.suggestion !== "" || notesGhost.suggestion !== ""} />
        </span>
      </div>

      <div className="flex min-h-0 flex-1 flex-col gap-3 overflow-x-hidden overflow-y-auto p-3">
        <div className="relative">
          <GhostText
            value={form.title}
            suggestion={titleGhost.suggestion}
            typography="text-reading"
            frame="px-2 border border-transparent flex items-center"
            multiline={false}
          />
          <Input
            ref={titleField}
            id={ids.title}
            value={form.title}
            placeholder="Add a title"
            aria-label="Title"
            className="h-8 text-reading"
            onChange={(e) => {
              set({ title: e.target.value });
              trackCaret(e);
            }}
            onSelect={trackCaret}
            onFocus={(e) => {
              setGhostFocus("title");
              trackCaret(e);
            }}
            onBlur={() => setGhostFocus((current) => (current === "title" ? null : current))}
            onKeyDown={ghostKeys(titleGhost, (title) => set({ title }))}
          />
        </div>

        <FieldGroup>
          <Field orientation="row" data-invalid={timeError !== null || undefined}>
            <FieldLabel htmlFor={ids.startDate}>When</FieldLabel>
            <FieldContent>
              {/* One row per end. Putting all four controls on one line reads as
                  "date time to time date", which is not a sentence anyone parses;
                  start-above-end is how every calendar states a range. */}
              <EndRow
                caption="from"
                which="Start"
                dateId={ids.startDate}
                date={form.startDate}
                onDate={(startDate) => set({ startDate })}
                time={form.startTime}
                onTime={(startTime) => set({ startTime })}
                allDay={form.allDay}
              />
              <EndRow
                caption="to"
                which="End"
                dateId={ids.endDate}
                date={form.endDate}
                onDate={(endDate) => set({ endDate })}
                time={form.endTime}
                onTime={(endTime) => set({ endTime })}
                allDay={form.allDay}
              />
              <div className="flex items-center gap-3">
                <Label className="cursor-pointer gap-1.5 text-micro text-muted-foreground">
                  {/* The end date means the same thing on both sides of this
                      tick — the last day the event covers — so it is left
                      alone. Collapsing it to the start date, which this used
                      to do, silently shortened every multi-day event that was
                      switched to all-day. */}
                  <Checkbox
                    checked={form.allDay}
                    onCheckedChange={(on) => set({ allDay: on })}
                  />
                  All day
                </Label>
                {timeError === null && times !== null && !isFormError(times) && (
                  <span className="text-micro text-faint-foreground">{fullDate(times.start)}</span>
                )}
              </div>
            </FieldContent>
            <FieldError>{timeError}</FieldError>
          </Field>

          <Field orientation="row">
            <FieldLabel htmlFor={ids.repeat}>
              <Repeat size={11} strokeWidth={1.75} />
              Repeat
            </FieldLabel>
            <Select
              items={RECURRENCE_CHOICES.map((choice) => ({ value: choice.id, label: choice.label }))}
              value={form.recurrence}
              onValueChange={(value) => {
                if (value !== null) set({ recurrence: value as EventForm["recurrence"] });
              }}
            >
              <SelectTrigger id={ids.repeat} aria-label="Repeat">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {RECURRENCE_CHOICES.map((choice) => (
                  <SelectItem key={choice.id} value={choice.id}>
                    {choice.label}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            {/* Recurrence is write-only: the store keeps expanded occurrences, not
                the rule, so an existing series' rule cannot be read back. Saying
                so beats showing "does not repeat" over a weekly meeting. */}
            {recurring && (
              <FieldDescription>
                This is one occurrence of a series. Leave this alone to keep the existing rule —
                choosing one replaces it.
              </FieldDescription>
            )}
          </Field>

          <Field orientation="row">
            <FieldLabel htmlFor={ids.alert}>Alert</FieldLabel>
            <Select
              items={REMINDER_CHOICES.map((choice) => ({
                value: reminderValue(choice.minutes),
                label: choice.label,
              }))}
              value={reminderValue(form.reminderMinutes)}
              onValueChange={(value) => {
                if (value === null) return;
                set({ reminderMinutes: value === REMINDER_DEFAULT ? null : Number(value) });
              }}
            >
              <SelectTrigger id={ids.alert} aria-label="Reminder">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {REMINDER_CHOICES.map((choice) => (
                  <SelectItem
                    key={reminderValue(choice.minutes)}
                    value={reminderValue(choice.minutes)}
                  >
                    {choice.label}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </Field>

          <Field orientation="row">
            <FieldLabel htmlFor={ids.location}>
              <MapPin size={11} strokeWidth={1.75} />
              Where
            </FieldLabel>
            <div className="min-w-0">
              <Input
                id={ids.location}
                value={form.location}
                placeholder="Add a place, or a call link"
                aria-label="Location"
                onChange={(e) => set({ location: e.target.value })}
              />
              {conference && (
                <button
                  type="button"
                  onClick={() => onOpenExternal(conference.url)}
                  className="mt-1 inline-flex items-center gap-1 text-micro text-accent hover:underline"
                >
                  <Video size={11} strokeWidth={1.75} />
                  Join {conference.provider}
                </button>
              )}
            </div>
          </Field>

          <Field orientation="row">
            <FieldLabel htmlFor={ids.attendees}>
              <Users size={11} strokeWidth={1.75} />
              Who
            </FieldLabel>
            <div className="relative min-w-0 flex-1">
              <Textarea
                ref={guestField}
                id={ids.attendees}
                autoSize
                rows={2}
                maxRows={5}
                value={form.attendees}
                aria-label="Guests"
                placeholder="ada@example.com, bob@example.com"
                {...typeaheadFieldProps(guests)}
              />
              <AddressSuggestions typeahead={guests} />
            </div>
          </Field>

          <Field orientation="row">
            <FieldLabel htmlFor={ids.calendar}>
              <CalendarIcon size={11} strokeWidth={1.75} />
              Calendar
            </FieldLabel>
            <Select
              items={calendarItems}
              value={form.calendarId}
              onValueChange={(value) => {
                if (value !== null) set({ calendarId: value });
              }}
            >
              <SelectTrigger id={ids.calendar} aria-label="Calendar">
                <span
                  className="h-2 w-2 shrink-0 rounded-[2px]"
                  style={{ backgroundColor: calendarFill(hue, dark) }}
                />
                <SelectValue />
              </SelectTrigger>
              <SelectContent className="max-w-[26rem]">
                {calendars.map((calendar) => {
                  const owner = accounts.find((a) => a.id === calendar.accountId);
                  return (
                    <SelectItem key={calendar.id} value={calendar.id}>
                      <span className="flex min-w-0 items-center gap-1.5">
                        <span
                          className="h-2 w-2 shrink-0 rounded-[2px]"
                          style={{ backgroundColor: calendarFill(hueFor(calendar.id), dark) }}
                        />
                        <span className="truncate">{calendarLabel(calendar)}</span>
                        {owner && (
                          <span className="truncate text-micro text-faint-foreground">
                            {owner.email}
                          </span>
                        )}
                      </span>
                    </SelectItem>
                  );
                })}
              </SelectContent>
            </Select>
            {event && form.calendarId !== event.calendarId && (
              <FieldDescription>
                Moving a meeting to another calendar re-creates it there, which re-invites its
                guests.
              </FieldDescription>
            )}
          </Field>

          <Field orientation="row">
            <FieldLabel htmlFor={ids.notes}>Notes</FieldLabel>
            <div className="relative min-w-0 flex-1">
              <GhostText
                value={form.description}
                suggestion={notesGhost.suggestion}
                typography="text-body leading-snug"
                frame="px-2 py-1 border border-transparent"
              />
              <Textarea
                id={ids.notes}
                autoSize
                rows={3}
                maxRows={10}
                value={form.description}
                aria-label="Description"
                placeholder="Anything else"
                onChange={(e) => {
                  set({ description: e.target.value });
                  trackCaret(e);
                }}
                onSelect={trackCaret}
                onFocus={(e) => {
                  setGhostFocus("notes");
                  trackCaret(e);
                }}
                onBlur={() => setGhostFocus((current) => (current === "notes" ? null : current))}
                onKeyDown={ghostKeys(notesGhost, (description) => set({ description }))}
              />
            </div>
          </Field>

          {merged && merged.copies.length > 1 && (
            <p className="inline-flex items-start gap-1 text-micro text-faint-foreground">
              <Layers size={11} strokeWidth={1.75} className="mt-[2px] shrink-0" />
              <span>
                Also on{" "}
                {merged.copies
                  .map((copy) => {
                    const calendar = calendars.find((c) => c.id === copy.calendarId);
                    return calendar ? calendarLabel(calendar) : copy.calendarId;
                  })
                  .join(", ")}
                . Edits here apply to this copy only.
              </span>
            </p>
          )}

          {event && (tone === "outline" || tone === "tentative") && (
            <Field orientation="row">
              <FieldLabel>Going?</FieldLabel>
              <div className="flex gap-1">
                {(["accepted", "tentative", "declined"] as const).map((response) => (
                  <Button
                    key={response}
                    size="sm"
                    variant="subtle"
                    disabled={busy}
                    onClick={() => onRsvp(response)}
                  >
                    {response === "accepted" ? "Yes" : response === "tentative" ? "Maybe" : "No"}
                  </Button>
                ))}
              </div>
            </Field>
          )}
        </FieldGroup>
      </div>

      {scopePrompt !== null ? (
        <ScopePrompt
          action={scopePrompt}
          busy={busy}
          onChoose={(scope) =>
            scopePrompt === "save" ? onSave(form, scope) : onDelete(scope)
          }
          onCancel={() => setScopePrompt(null)}
        />
      ) : (
        <div className="flex items-center gap-1.5 border-t border-border px-3 py-2">
          {event && (
            <>
              <Button
                size="sm"
                variant={confirmDelete ? "danger" : "ghost"}
                disabled={busy}
                onClick={remove}
                title="Delete (⌘⌫)"
              >
                <Trash2 size={12} strokeWidth={1.75} />
                {confirmDelete ? "Really delete" : "Delete"}
              </Button>
              <Button size="sm" disabled={busy} onClick={onDuplicate} title="Duplicate (⌘D)">
                <Copy size={12} strokeWidth={1.75} />
                Duplicate
              </Button>
              <Button
                size="sm"
                onClick={() => onOpenExternal(googleCalendarUrl(event, account?.email))}
                title="Open in Google Calendar"
              >
                <ExternalLink size={12} strokeWidth={1.75} />
                Google
              </Button>
            </>
          )}
          <div className="ml-auto flex items-center gap-1.5">
            <Button size="sm" onClick={onClose}>
              Cancel
            </Button>
            {/* Not disabled on `!dirty`. A date field holds its typed text
                until it commits, so "nothing has changed yet" and "you have not
                finished typing" look identical from here — and a greyed-out
                Save is the worst possible answer to the second one. An
                unchanged event just closes. */}
            <Button size="sm" variant="default" disabled={busy || Boolean(timeError)} onClick={submit}>
              {event ? "Save" : "Create"}
              <Kbd keys="mod+enter" className="border-none bg-transparent px-0" />
            </Button>
          </div>
        </div>
      )}
    </Overlay>
  );
}

/** One end of the range: a caption, a date, and a time unless it is all day. */
function EndRow({
  caption,
  which,
  dateId,
  date,
  onDate,
  time,
  onTime,
  allDay,
}: {
  caption: string;
  which: "Start" | "End";
  dateId: string;
  date: string;
  onDate: (value: string) => void;
  time: string;
  onTime: (value: string) => void;
  allDay: boolean;
}) {
  return (
    <div className="flex items-center gap-1.5">
      <span className="w-6 shrink-0 text-micro text-faint-foreground">{caption}</span>
      <DateField
        id={dateId}
        label={`${which} date`}
        value={date}
        onChange={onDate}
        className="min-w-0 flex-1"
      />
      {!allDay && (
        <TimeField
          id={`${dateId}-time`}
          label={`${which} time`}
          value={time}
          onChange={onTime}
          className="w-[7.5rem] shrink-0"
        />
      )}
    </div>
  );
}

/** Stable ids, so every label actually points at its own control. */
function useFieldIds() {
  const base = useId();
  return {
    title: `${base}-title`,
    startDate: `${base}-start-date`,
    endDate: `${base}-end-date`,
    repeat: `${base}-repeat`,
    alert: `${base}-alert`,
    location: `${base}-location`,
    attendees: `${base}-attendees`,
    calendar: `${base}-calendar`,
    notes: `${base}-notes`,
  };
}

function reminderValue(minutes: number | null): string {
  return minutes === null ? REMINDER_DEFAULT : String(minutes);
}

/**
 * "This one, or all of them?"
 *
 * Two choices, not three. `this and following` needs an endpoint Google does
 * not have — it means rewriting the series' rule with an `UNTIL` and inserting
 * a second series, three calls whose partial failure leaves a split series
 * behind. Saying so is better than offering a button that quietly does
 * something else.
 */
function ScopePrompt({
  action,
  busy,
  onChoose,
  onCancel,
}: {
  action: "save" | "delete";
  busy: boolean;
  onChoose: (scope: EventScope) => void;
  onCancel: () => void;
}) {
  const first = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    first.current?.focus();
  }, []);

  return (
    <div className="flex flex-col gap-2 border-t border-border-strong bg-surface-raised px-3 py-2">
      <p className="text-micro text-muted-foreground">
        This event repeats. {action === "delete" ? "Delete" : "Change"} which occurrences?
      </p>
      <div className="flex items-center gap-1.5">
        <Button
          ref={first}
          size="sm"
          variant="subtle"
          disabled={busy}
          onClick={() => onChoose("this")}
        >
          This one
        </Button>
        <Button
          size="sm"
          variant={action === "delete" ? "danger" : "subtle"}
          disabled={busy}
          onClick={() => onChoose("all")}
        >
          All of them
        </Button>
        <Button size="sm" disabled={busy} onClick={onCancel} className="ml-auto">
          Cancel
        </Button>
      </div>
      <p className="text-micro text-faint-foreground">
        “This and following” is not offered: Google's API has no such call, and faking it can leave
        a series split in half. Use Google Calendar for that one.
      </p>
    </div>
  );
}
