import {
  AlertTriangle,
  Calendar as CalendarIcon,
  Copy,
  ExternalLink,
  Layers,
  LoaderCircle,
  Lock,
  MapPin,
  Paperclip,
  Phone,
  Repeat,
  Trash2,
  UserPen,
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
  type ReactNode,
} from "react";
import type {
  Account,
  Calendar,
  CalendarEvent,
  CalendarId,
  EventConference,
  EventGuest,
  Rsvp,
} from "@/types";
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
import { calendarFill, toneFor, type CalendarColor } from "@/lib/calendar-palette";
import {
  conferenceLink,
  dialUrl,
  entryLabel,
  googleCalendarUrl,
  joinUrl,
  mapsUrl,
} from "@/lib/calendar-links";
import type { MergedEvent } from "@/lib/calendar-merge";
import {
  REMINDER_CHOICES,
  RECURRENCE_CHOICES,
  SHOW_AS_CHOICES,
  describeReminders,
  describeRules,
  emptyForm,
  formFromEvent,
  formPatch,
  formTimes,
  hasMeet,
  isFormError,
  parseAttendees,
  reminderChoiceById,
  reminderChoiceId,
  reminderMinutesOf,
  wouldNotifyGuests,
  type EventForm,
} from "@/lib/calendar-edit";
import type { Notify } from "@/lib/data";
import { fullDate } from "@/lib/time";
import { calendarLabel } from "./CalendarSidebar";

export type ModalTarget =
  | { mode: "view"; event: CalendarEvent }
  | { mode: "create"; start: number; end: number; allDay?: boolean; title?: string };

export interface EventModalProps {
  target: ModalTarget | null;
  calendars: Calendar[];
  accounts: Account[];
  colorFor: (id: CalendarId) => CalendarColor;
  dark: boolean;
  merged: MergedEvent | null;
  /** Default calendar for a new event — the first visible one. */
  defaultCalendarId: CalendarId | null;
  /** Whether this event is one of a series — see `looksRecurring`. */
  recurring: boolean;
  /**
   * Whether Google would accept a write to this event at all — see
   * `canEditEvent`. False turns the modal into a viewer with an RSVP on it.
   */
  canEdit: boolean;
  /**
   * Why the last save or delete did not happen, if it did not.
   *
   * The modal stays open on a failure and says so here. The status bar alone
   * was not enough: it is a 24px rail, it truncates, it clears itself after six
   * seconds, and the modal used to close *before* the command ran — so a
   * refused save looked exactly like a successful one, which is the whole
   * reason "I tried to create an event and nothing happened" was hard to chase.
   */
  error: string | null;
  busy: boolean;
  /** Everyone the app has seen, for completing the guest list. */
  contacts?: readonly Contact[];
  onClose: () => void;
  onSave: (form: EventForm, scope: EventScope) => void;
  /*
   * `notify` is the answer to "email the guests about this?". Absent means the
   * question was never asked — an event with no guests, or a scope prompt that
   * resolved without one — and the command layer then tells them, which is
   * Google Calendar's own default for a cancellation.
   */
  onDelete: (scope: EventScope, notify?: Notify) => void;
  onDuplicate: () => void;
  onRsvp: (response: Rsvp) => void;
  onOpenExternal: (url: string) => void;
}

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
  colorFor,
  dark,
  merged,
  defaultCalendarId,
  recurring,
  canEdit,
  error,
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
  /** Set when a save would email guests and has not been told whether to. */
  const [notifyPrompt, setNotifyPrompt] = useState<{ scope: EventScope } | null>(null);
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
    setNotifyPrompt(null);
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
    if (!form || busy || timeError || !canEdit) return;
    const active = document.activeElement;
    if (active instanceof HTMLElement && active !== document.body) active.blur();
    setSaveNonce((n) => n + 1);
  };

  /**
   * The last gate before a save: does this email anybody?
   *
   * Google Calendar asks the same question with a dialog, because both answers
   * are things people want — a moved meeting has to reach the room, and a typo
   * fixed in the notes of a thirty-person standup does not. Neither is a default
   * this component gets to pick.
   *
   * It is only asked where it is a question. A new event invites the people it
   * names, the way it does in Google Calendar; an event with no guests has
   * nobody to tell; and a change nobody can see from outside this window
   * (`wouldNotifyGuests`) is not a change worth mailing.
   */
  const saveWithScope = (current: EventForm, scope: EventScope) => {
    if (!event) {
      onSave(current, scope);
      return;
    }
    const patch = formPatch(event, current);
    if (patch && wouldNotifyGuests(event, patch)) {
      setScopePrompt(null);
      setNotifyPrompt({ scope });
      return;
    }
    onSave(current, scope);
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
    saveWithScope(form, "this");
    // Only the nonce may start a save; every other value here is read fresh
    // from the render the nonce caused.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [saveNonce]);

  const remove = () => {
    if (!event || busy || !canEdit) return;
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
      description: "Save",
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
        if (notifyPrompt !== null) {
          setNotifyPrompt(null);
          return;
        }
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
      description: "Delete",
      when: () => live() && event !== null,
      handler: remove,
    },
    {
      keys: "mod+d",
      priority: 120,
      allowInInput: true,
      group: "Event",
      description: "Duplicate",
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

  /** ⇥ takes the grey text. Escape closes the modal — it is not a second mode. */
  const ghostKeys =
    (ghost: { suggestion: string; accept: () => string | null; dismiss: () => void }, apply: (value: string) => void) =>
    (keyEvent: ReactKeyboardEvent) => {
      if (!ghost.suggestion) return;
      if (keyEvent.key === "Tab" && !keyEvent.shiftKey && !keyEvent.metaKey && !keyEvent.ctrlKey) {
        keyEvent.preventDefault();
        keyEvent.stopPropagation();
        const next = ghost.accept();
        if (next !== null) apply(next);
      }
    };

  if (!open || !form) return null;

  const account = event ? accounts.find((a) => a.id === event.accountId) : null;
  const conference = event ? conferenceLink(event) : null;
  // Read off the field rather than off the event, so an address pasted a second
  // ago has its map without waiting for a save.
  const map = mapsUrl(form.location);
  const color = colorFor(form.calendarId);
  const tone = event ? toneFor(event.rsvp) : "solid";
  const set = (patch: Partial<EventForm>) => setForm((current) => ({ ...current!, ...patch }));
  // Read off the field rather than off the event, so somebody added a moment
  // ago is counted in the question about whether to tell them.
  const guestCount = parseAttendees(form.attendees).length;

  // A rule this form cannot author still has to be *shown*, and shown as
  // itself. It leads the list so the trigger has a label, and picking anything
  // else replaces it — which is the only destructive thing the picker can do
  // and now the only way to do it.
  const repeatChoices = RECURRENCE_CHOICES.map((choice) => ({
    value: choice.id,
    label: choice.label,
  }));
  // `series` is the occurrence of a synced series whose rule lives on the master
  // Google never returns. It led the list saying "Does not repeat", which is the
  // opposite of the truth for the most common recurring event there is.
  const repeatItems =
    form.recurrence === "custom"
      ? [{ value: "custom", label: describeRules(form.recurrenceRules) }, ...repeatChoices]
      : form.recurrence === "series"
        ? [{ value: "series", label: "Repeats (rule kept in Google)" }, ...repeatChoices]
        : repeatChoices;

  const showAsItems = SHOW_AS_CHOICES.map((choice) => ({
    value: choice.id,
    label: choice.label,
  }));
  const alertChoices = REMINDER_CHOICES.map((choice) => ({
    value: choice.id,
    label: choice.label,
  }));
  const alertItems =
    reminderChoiceId(form.reminderMinutes) === "custom"
      ? [
          { value: "custom", label: describeReminders(form.reminderMinutes) },
          ...alertChoices,
        ]
      : alertChoices;

  return (
    <Overlay open onClose={onClose} align="center" className="max-w-[36rem]" labelledBy="event-title">
      <div className="flex items-center gap-2 border-b border-border px-3 py-2">
        <span
          className="h-2.5 w-2.5 shrink-0 rounded-[3px]"
          style={{ backgroundColor: calendarFill(color, dark) }}
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
        {/* Two words that change what the event means rather than how it looks:
            whether it defends the time, and whether anyone sharing this calendar
            can read it. Shown only when they are not the default. */}
        {form.transparency === "transparent" && (
          <span className="text-micro text-faint-foreground">free</span>
        )}
        {(event?.visibility === "private" || event?.visibility === "confidential") && (
          <span className="text-micro text-faint-foreground">private</span>
        )}
        {/* `⌘⏎ save · Esc close` sat here. The footer of this same panel draws
            both as buttons, and the Save button carries the ⌘⏎ chip itself.
            `GhostHint` stays: it reports that a suggestion is waiting, which is
            news rather than a legend. */}
        <GhostHint shown={titleGhost.suggestion !== "" || notesGhost.suggestion !== ""} />
      </div>

      <div className="flex min-h-0 flex-1 flex-col gap-3 overflow-x-hidden overflow-y-auto p-3">
        {/* A failure gets the top of the panel and stays there. It is the one
            thing in this modal that must not be missable. */}
        {error && (
          <p
            role="alert"
            className="flex items-start gap-2 rounded-sm border border-danger/40 bg-danger/10 px-2 py-1 text-micro text-danger"
          >
            <AlertTriangle size={12} strokeWidth={1.75} className="mt-[1px] shrink-0" />
            <span className="min-w-0">{error}</span>
          </p>
        )}

        {/* Google refuses a write from anyone but the organizer, so the modal
            says the fields are inert rather than letting the user type into
            them and discover it from a red line at the bottom of the window.
            A label, not a paragraph: the two things it used to explain — that
            you can still RSVP, and that Google Calendar can edit it — are both
            visible controls a few rows down. */}
        {!canEdit && (
          <p className="flex items-center gap-2 rounded-sm border border-border bg-surface-raised px-2 py-1 text-micro text-muted-foreground">
            <Lock size={12} strokeWidth={1.75} className="shrink-0" />
            <span className="min-w-0 truncate">
              {event?.organizer?.email
                ? `Organized by ${event.organizer.email}`
                : "Organized by someone else"}
            </span>
            <span className="shrink-0 text-faint-foreground">Read-only</span>
          </p>
        )}

        {/* Ghost text is offered only where a write would be accepted — an
            organizer's event is read-only, and a suggestion you cannot take is
            an invitation to a red line at the bottom of the window. */}
        <div className="relative">
          {canEdit && (
            <GhostText
              value={form.title}
              suggestion={titleGhost.suggestion}
              typography="text-reading"
              frame="px-2 border border-transparent flex items-center"
              multiline={false}
            />
          )}
          <Input
            ref={titleField}
            id={ids.title}
            value={form.title}
            placeholder="Add a title"
            aria-label="Title"
            readOnly={!canEdit}
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
            onKeyDown={canEdit ? ghostKeys(titleGhost, (title) => set({ title })) : undefined}
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
                disabled={!canEdit}
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
                disabled={!canEdit}
              />
              <div className="flex items-center gap-3">
                <Label className="cursor-pointer gap-2 text-micro text-muted-foreground">
                  {/* The end date means the same thing on both sides of this
                      tick — the last day the event covers — so it is left
                      alone. Collapsing it to the start date, which this used
                      to do, silently shortened every multi-day event that was
                      switched to all-day. */}
                  <Checkbox
                    checked={form.allDay}
                    disabled={!canEdit}
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
              items={repeatItems}
              value={form.recurrence}
              disabled={!canEdit}
              onValueChange={(value) => {
                if (value !== null) set({ recurrence: value as EventForm["recurrence"] });
              }}
            >
              <SelectTrigger id={ids.repeat} aria-label="Repeat">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {repeatItems.map((choice) => (
                  <SelectItem key={choice.value} value={choice.value}>
                    {choice.label}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            {/* The one thing the picker cannot say for itself: the rule belongs
                to the series, so changing it here is never local to the day you
                opened. A fragment, not the paragraph that used to be here. */}
            {recurring && form.recurrence !== "none" && (
              <FieldDescription>Applies to every occurrence</FieldDescription>
            )}
          </Field>

          <Field orientation="row">
            <FieldLabel htmlFor={ids.alert}>Alert</FieldLabel>
            <Select
              items={alertItems}
              value={reminderChoiceId(form.reminderMinutes)}
              disabled={!canEdit}
              onValueChange={(value) => {
                if (value === null) return;
                const choice = reminderChoiceById(value);
                // `custom` has no choice behind it — it is the label for what
                // the event already is, so picking it means "leave it alone".
                if (choice) set({ reminderMinutes: choice.minutes });
              }}
            >
              <SelectTrigger id={ids.alert} aria-label="Reminder">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {alertItems.map((choice) => (
                  <SelectItem key={choice.value} value={choice.value}>
                    {choice.label}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            {form.reminderMinutes === null && event !== null && reminderMinutesOf(event) !== null && (
              <FieldDescription>“Calendar default” can only be restored in Google Calendar</FieldDescription>
            )}
          </Field>

          <Field orientation="row">
            <FieldLabel htmlFor={ids.showAs}>Show as</FieldLabel>
            <Select
              items={showAsItems}
              value={form.transparency}
              disabled={!canEdit}
              onValueChange={(value) => {
                if (value !== null) set({ transparency: value as EventForm["transparency"] });
              }}
            >
              <SelectTrigger id={ids.showAs} aria-label="Show as">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {showAsItems.map((choice) => (
                  <SelectItem key={choice.value} value={choice.value}>
                    {choice.label}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </Field>

          {/* The call comes before the room. A meeting you cannot join from is
              a calendar entry, and this is the row that stops it being one.

              The tick is what makes it editable at all. Google mints the code —
              there is no link to paste — so "add a Meet link" has to be a
              request, and until now the only way to make one was to leave for
              Google Calendar. */}
          {(event?.conference || conference || canEdit) && (
            <Field orientation="row">
              <RowCaption>
                <Video size={11} strokeWidth={1.75} />
                Call
              </RowCaption>
              <div className="flex min-w-0 flex-1 flex-col gap-1">
                {(event?.conference || conference) && (
                  <ConferenceBlock
                    conference={event?.conference}
                    fallback={conference}
                    onOpenExternal={onOpenExternal}
                  />
                )}
                {canEdit && (
                  <Label className="cursor-pointer gap-2 text-micro text-muted-foreground">
                    <Checkbox
                      aria-label="Google Meet"
                      checked={form.meet}
                      onCheckedChange={(on) => set({ meet: on })}
                    />
                    Google Meet
                  </Label>
                )}
                {/* Removing a call is not reversible: Google issues a new code
                    every time, so the link that goes is gone. */}
                {canEdit && !form.meet && event !== null && hasMeet(event) && (
                  <FieldDescription>Removing the call retires this link</FieldDescription>
                )}
              </div>
            </Field>
          )}

          <Field orientation="row">
            <FieldLabel htmlFor={ids.location}>
              <MapPin size={11} strokeWidth={1.75} />
              Where
            </FieldLabel>
            {/* The button is beside the field rather than replacing the text,
                because the text is still the answer to "where is this" for the
                locations no map can take — a room, a floor, a person's desk.
                It is an ordinary button in the row's tab order, one stop after
                the field it is about, so ⇥ from the address reaches it.

                Absent, not disabled, when the location is not a place: a greyed
                Map on "Room 2" is a control promising something it will never
                do. See `looksLikeAddress` for what counts. */}
            <div className="flex min-w-0 items-center gap-1">
              <Input
                id={ids.location}
                value={form.location}
                placeholder="Add a place, or a call link"
                aria-label="Location"
                readOnly={!canEdit}
                className="min-w-0 flex-1"
                onChange={(e) => set({ location: e.target.value })}
              />
              {map && (
                <Button
                  size="sm"
                  className="shrink-0"
                  onClick={() => onOpenExternal(map)}
                  title="Open in Google Maps"
                >
                  <MapPin size={12} strokeWidth={1.75} />
                  Map
                </Button>
              )}
            </div>
          </Field>

          <Field orientation="row">
            <FieldLabel htmlFor={ids.attendees}>
              <Users size={11} strokeWidth={1.75} />
              Who
            </FieldLabel>
            {/* `relative` is what `AddressSuggestions` positions against. */}
            <div className="relative flex min-w-0 flex-1 flex-col gap-2">
              <Textarea
                ref={guestField}
                id={ids.attendees}
                autoSize
                rows={2}
                maxRows={5}
                value={form.attendees}
                aria-label="Guests"
                placeholder="ada@example.com, bob@example.com"
                readOnly={!canEdit}
                // A read-only field takes no typing, so it needs no change
                // handler and has nothing to complete against.
                {...(canEdit ? typeaheadFieldProps(guests) : {})}
              />
              {canEdit && <AddressSuggestions typeahead={guests} />}
              {event && <GuestList guests={event.guests ?? []} />}
            </div>
          </Field>

          {/* Only when they differ. The same name under two labels is noise,
              and they agree on most events. */}
          {event?.creator &&
            event.creator.email.toLowerCase() !== (event.organizer?.email ?? "").toLowerCase() && (
              <Field orientation="row">
                <RowCaption>
                  <UserPen size={11} strokeWidth={1.75} />
                  Created by
                </RowCaption>
                <span className="min-w-0 truncate text-micro text-muted-foreground">
                  {event.creator.name
                    ? `${event.creator.name} · ${event.creator.email}`
                    : event.creator.email}
                </span>
              </Field>
            )}

          {event && (event.attachments?.length ?? 0) > 0 && (
            <Field orientation="row">
              <RowCaption>
                <Paperclip size={11} strokeWidth={1.75} />
                Files
              </RowCaption>
              <div className="flex min-w-0 flex-col gap-1">
                {event.attachments?.map((file) => {
                  const url = joinUrl(file.url);
                  return url ? (
                    <button
                      key={file.url}
                      type="button"
                      onClick={() => onOpenExternal(url)}
                      className="truncate text-left text-micro text-accent hover:underline"
                    >
                      {file.title}
                    </button>
                  ) : (
                    <span key={file.url} className="truncate text-micro text-muted-foreground">
                      {file.title}
                    </span>
                  );
                })}
              </div>
            </Field>
          )}

          <Field orientation="row">
            <FieldLabel htmlFor={ids.calendar}>
              <CalendarIcon size={11} strokeWidth={1.75} />
              Calendar
            </FieldLabel>
            <Select
              items={calendarItems}
              value={form.calendarId}
              disabled={!canEdit}
              onValueChange={(value) => {
                if (value !== null) set({ calendarId: value });
              }}
            >
              <SelectTrigger id={ids.calendar} aria-label="Calendar">
                <span
                  className="h-2 w-2 shrink-0 rounded-[2px]"
                  style={{ backgroundColor: calendarFill(color, dark) }}
                />
                <SelectValue />
              </SelectTrigger>
              <SelectContent className="max-w-[26rem]">
                {calendars.map((calendar) => {
                  const owner = accounts.find((a) => a.id === calendar.accountId);
                  return (
                    <SelectItem key={calendar.id} value={calendar.id}>
                      <span className="flex min-w-0 items-center gap-1">
                        <span
                          className="h-2 w-2 shrink-0 rounded-[2px]"
                          style={{ backgroundColor: calendarFill(colorFor(calendar.id), dark) }}
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
              <FieldDescription>Re-creates the event, which re-invites its guests</FieldDescription>
            )}
          </Field>

          <Field orientation="row">
            <FieldLabel htmlFor={ids.notes}>Notes</FieldLabel>
            <div className="relative min-w-0 flex-1">
              {canEdit && (
                <GhostText
                  value={form.description}
                  suggestion={notesGhost.suggestion}
                  typography="text-body leading-snug"
                  frame="px-2 py-1 border border-transparent"
                />
              )}
              <Textarea
                readOnly={!canEdit}
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
                onKeyDown={
                  canEdit ? ghostKeys(notesGhost, (description) => set({ description })) : undefined
                }
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
                {" "}· edits apply to this copy
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

      {notifyPrompt !== null ? (
        <NotifyPrompt
          count={guestCount}
          busy={busy}
          onChoose={(notify) => {
            setNotifyPrompt(null);
            onSave({ ...form, notify }, notifyPrompt.scope);
          }}
          onCancel={() => setNotifyPrompt(null)}
        />
      ) : scopePrompt !== null ? (
        <ScopePrompt
          action={scopePrompt}
          busy={busy}
          guests={scopePrompt === "delete" ? guestCount : 0}
          onChoose={(scope) =>
            scopePrompt === "save" ? saveWithScope(form, scope) : onDelete(scope)
          }
          onCancel={() => setScopePrompt(null)}
        />
      ) : (
        <div className="flex items-center gap-2 border-t border-border px-3 py-2">
          {event && canEdit && (
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
              {/* Deleting a meeting cancels it *with* its guests. Said here
                  rather than asked, because the answer is not the modal's to
                  give — see `GuestNotice`. */}
              {confirmDelete && guestCount > 0 && (
                <span className="text-micro text-danger">
                  Cancels with {guestCount} {guestCount === 1 ? "guest" : "guests"}
                </span>
              )}
            </>
          )}
          {event && (
            <>
              {/* Duplicating is always allowed: the copy lands on a calendar the
                  user owns, so it is a create, not a write to someone else's
                  event. It is also the way out for a guest who wants their own
                  version of a meeting they cannot edit. */}
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
          <div className="ml-auto flex items-center gap-2">
            <Button size="sm" onClick={onClose}>
              {canEdit ? "Cancel" : "Close"}
            </Button>
            {/* Not disabled on `!dirty`. A date field holds its typed text
                until it commits, so "nothing has changed yet" and "you have not
                finished typing" look identical from here — and a greyed-out
                Save is the worst possible answer to the second one. An
                unchanged event just closes.

                It *is* absent when the event is not ours: a Save that Google
                can only refuse is not a control, it is a trap. */}
            {canEdit && (
              <Button
                size="sm"
                variant="default"
                disabled={busy || Boolean(timeError)}
                onClick={submit}
              >
                {/* The panel deliberately stays up until the write lands — it is
                    holding everything that was typed, and a refused save has to
                    have somewhere to put the reason. That makes the button the
                    only thing on screen that can say the press was heard, and a
                    control that only greys out says it too quietly: a disabled
                    Save and a Save you have not pressed look the same at a
                    glance. */}
                {busy && <LoaderCircle size={11} strokeWidth={1.75} className="animate-spin" />}
                {event ? "Save" : "Create"}
                {/* Naked glyph, not a chip: Kbd's muted ink is for a raised
                    surface, and it disappears on accent. Inherit the button. */}
                <Kbd keys="mod+enter" className="border-none bg-transparent px-0 text-current" />
              </Button>
            )}
          </div>
        </div>
      )}
    </Overlay>
  );
}

/**
 * A label-shaped caption for a row that labels nothing.
 *
 * `FieldLabel` renders a `<label>`, which wants a control to point at. The call,
 * the creator and the attachments are read-only rows, and a `<label>` with no
 * target is a promise to a screen reader that nothing keeps. The `data-slot` is
 * what `Field`'s row grid aligns on, so the gutter stays the same width.
 */
function RowCaption({ children }: { children: ReactNode }) {
  return (
    <span
      data-slot="field-label"
      className="flex items-center gap-1 text-micro leading-none text-faint-foreground select-none"
    >
      {children}
    </span>
  );
}

/**
 * The call: how to join it, what to read out, and how to dial in.
 *
 * Everything here is a string an attacker chose — an invitation is an
 * unauthenticated write into the store from anyone who knows the address — so
 * two rules hold throughout. Every value is rendered as *text*, never as
 * markup, so a label of `<img onerror=…>` is a strange-looking meeting and
 * nothing else. And nothing is ever *followed* without passing `joinUrl` or
 * `dialUrl` first, which is why a URI that fails those is shown as plain text
 * rather than as a button that would refuse: the information is still true, it
 * is only the action that is withheld.
 *
 * `fallback` is the link scraped out of the description or the location — the
 * only way Zoom, Teams and Webex ever appear, since Google mints
 * `conferenceData` for Meet alone.
 */
function ConferenceBlock({
  conference,
  fallback,
  onOpenExternal,
}: {
  conference: EventConference | undefined;
  fallback: { provider: string; url: string } | null;
  onOpenExternal: (url: string) => void;
}) {
  const [copied, setCopied] = useState<"done" | "failed" | null>(null);

  const video = conference?.entryPoints.find((entry) => entry.kind === "video");
  const join = joinUrl(video?.uri) ?? fallback?.url ?? null;
  const provider = conference?.name ?? fallback?.provider ?? "call";
  const shown = video ? entryLabel(video) : (fallback?.url.replace(/^https:\/\//i, "") ?? null);

  const phones = (conference?.entryPoints ?? []).filter((entry) => entry.kind === "phone");
  const more = conference?.entryPoints.find((entry) => entry.kind === "more");
  const moreUrl = joinUrl(more?.uri);

  const copy = () => {
    if (!join) return;
    // The clipboard can refuse — no permission, no secure context — and a copy
    // button that silently does nothing is the failure this app has paid for
    // most often. So the result is said either way.
    navigator.clipboard
      ?.writeText(join)
      .then(() => setCopied("done"))
      .catch(() => setCopied("failed")) ?? setCopied("failed");
  };

  return (
    <div className="flex min-w-0 flex-col gap-1">
      <div className="flex min-w-0 items-center gap-1">
        {join && (
          <Button size="sm" variant="default" onClick={() => onOpenExternal(join)}>
            <Video size={12} strokeWidth={1.75} />
            Join {provider}
          </Button>
        )}
        {shown && <span className="min-w-0 truncate text-micro text-muted-foreground">{shown}</span>}
        {join && (
          <Button size="sm" variant="ghost" onClick={copy} title="Copy link" aria-label="Copy link">
            <Copy size={12} strokeWidth={1.75} />
            {copied === "done" ? "Copied" : copied === "failed" ? "Copy failed" : ""}
          </Button>
        )}
      </div>

      {conference?.id && (
        <span className="text-micro text-faint-foreground">Code {conference.id}</span>
      )}

      {phones.map((phone) => {
        const dial = dialUrl(phone.uri);
        const label = entryLabel(phone);
        return (
          <span key={phone.uri} className="flex min-w-0 items-center gap-1 text-micro">
            <Phone size={11} strokeWidth={1.75} className="shrink-0 text-faint-foreground" />
            {dial ? (
              <button
                type="button"
                onClick={() => onOpenExternal(dial)}
                className="truncate text-accent hover:underline"
              >
                {label}
              </button>
            ) : (
              <span className="truncate text-muted-foreground">{label}</span>
            )}
            {phone.pin && <span className="shrink-0 text-faint-foreground">PIN {phone.pin}</span>}
            {phone.regionCode && (
              <span className="shrink-0 text-faint-foreground">{phone.regionCode}</span>
            )}
          </span>
        );
      })}

      {moreUrl && (
        <button
          type="button"
          onClick={() => onOpenExternal(moreUrl)}
          className="self-start text-micro text-accent hover:underline"
        >
          More phone numbers
        </button>
      )}

      {conference?.notes && (
        <span className="text-micro text-faint-foreground">{conference.notes}</span>
      )}
    </div>
  );
}

/**
 * Who is coming, and what they said.
 *
 * The counts are Google's, in Google's order, because the owner reads them there
 * every day and a second vocabulary for the same four states would be a second
 * thing to learn. "Awaiting" covers both guests who have not answered and guests
 * Google told us nothing about: from the room they look the same, and the two
 * are only distinguishable further down the stack.
 *
 * A declining guest's comment gets its own line rather than a tooltip. It is
 * usually the entire content of the notification the decline generated, and a
 * reason you have to hover to find is a reason nobody reads.
 */
function GuestList({ guests }: { guests: readonly EventGuest[] }) {
  if (guests.length === 0) return null;

  const count = (response: Rsvp) => guests.filter((g) => g.response === response).length;
  const awaiting = guests.filter(
    (g) => g.response === undefined || g.response === "needsAction",
  ).length;
  const parts = [
    [count("accepted"), "yes"] as const,
    [count("declined"), "no"] as const,
    [count("tentative"), "maybe"] as const,
    [awaiting, "awaiting"] as const,
  ]
    .filter(([n]) => n > 0)
    .map(([n, word]) => `${n} ${word}`);

  return (
    <div className="flex min-w-0 flex-col gap-1">
      <span className="text-micro text-faint-foreground">
        {guests.length} {guests.length === 1 ? "guest" : "guests"}
        {parts.length > 0 ? ` · ${parts.join(", ")}` : ""}
      </span>
      <ul className="flex min-w-0 flex-col gap-1">
        {guests.map((guest) => (
          <li key={guest.email} className="min-w-0 text-micro">
            <span className="flex min-w-0 items-center gap-1">
              <span className="min-w-0 truncate text-muted-foreground">
                {guest.name || guest.email}
              </span>
              {guest.organizer && <span className="shrink-0 text-faint-foreground">organizer</span>}
              {guest.optional && <span className="shrink-0 text-faint-foreground">optional</span>}
              <span className="ml-auto shrink-0 text-faint-foreground">{answer(guest)}</span>
            </span>
            {guest.comment && (
              <span className="block truncate text-faint-foreground">{guest.comment}</span>
            )}
          </li>
        ))}
      </ul>
    </div>
  );
}

/** One word for a guest's answer, or none where there is no answer to give. */
function answer(guest: EventGuest): string {
  switch (guest.response) {
    case "accepted":
      return "yes";
    case "declined":
      return "no";
    case "tentative":
      return "maybe";
    default:
      return guest.resource ? "room" : "awaiting";
  }
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
  disabled,
}: {
  caption: string;
  which: "Start" | "End";
  dateId: string;
  date: string;
  onDate: (value: string) => void;
  time: string;
  onTime: (value: string) => void;
  allDay: boolean;
  disabled: boolean;
}) {
  return (
    <div className="flex items-center gap-2">
      <span className="w-6 shrink-0 text-micro text-faint-foreground">{caption}</span>
      <DateField
        id={dateId}
        label={`${which} date`}
        value={date}
        onChange={onDate}
        disabled={disabled}
        className="min-w-0 flex-1"
      />
      {!allDay && (
        <TimeField
          id={`${dateId}-time`}
          label={`${which} time`}
          value={time}
          onChange={onTime}
          disabled={disabled}
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
    showAs: `${base}-show-as`,
    location: `${base}-location`,
    attendees: `${base}-attendees`,
    calendar: `${base}-calendar`,
    notes: `${base}-notes`,
  };
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
  guests,
  onChoose,
  onCancel,
}: {
  action: "save" | "delete";
  busy: boolean;
  /** How many people a delete would cancel with. Zero says nothing. */
  guests: number;
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
        {guests > 0 && (
          <span className="text-danger">
            {" "}
            Cancels with {guests} {guests === 1 ? "guest" : "guests"}.
          </span>
        )}
      </p>
      <div className="flex items-center gap-2">
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
      {/* Why there is no third button is in this component's doc comment. The
          interface only has to say where that choice lives. */}
      <p className="text-micro text-faint-foreground">
        “This and following” — in Google Calendar
      </p>
    </div>
  );
}

/**
 * "Tell the guests?"
 *
 * The question Google Calendar asks with a dialog, and the reason it asks: both
 * answers are things people want. A meeting that moved has to reach the room.
 * A typo fixed in the notes of a thirty-person standup does not, and mailing
 * thirty people about it is a cost the software should not choose on its own.
 *
 * **Send is the first button and takes focus**, so ⏎ is the safe answer. The
 * failure this whole change exists to fix is silence, and a prompt whose default
 * is silence would reintroduce it one keystroke lower down.
 *
 * Same shape as `ScopePrompt` — an inline bar, not a second modal — for the same
 * reason: a dialog on top of a dialog is where keyboard focus goes to die.
 */
function NotifyPrompt({
  count,
  busy,
  onChoose,
  onCancel,
}: {
  count: number;
  busy: boolean;
  onChoose: (notify: Notify) => void;
  onCancel: () => void;
}) {
  const first = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    first.current?.focus();
  }, []);

  return (
    <div className="flex flex-col gap-2 border-t border-border-strong bg-surface-raised px-3 py-2">
      <p className="text-micro text-muted-foreground">
        Email {count} {count === 1 ? "guest" : "guests"} about this change?
      </p>
      <div className="flex items-center gap-2">
        <Button
          ref={first}
          size="sm"
          variant="default"
          disabled={busy}
          onClick={() => onChoose("guests")}
        >
          Send
        </Button>
        <Button size="sm" variant="subtle" disabled={busy} onClick={() => onChoose("nobody")}>
          Don't send
        </Button>
        <Button size="sm" disabled={busy} onClick={onCancel} className="ml-auto">
          Back
        </Button>
      </div>
    </div>
  );
}
