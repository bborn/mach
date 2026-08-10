import { useEffect, useId, useState } from "react";
import type { Account, ColorIndex, Label, MailFilter } from "@/types";
import { getDataSource } from "@/lib/data";
import { ACCOUNT_BG } from "@/lib/colors";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Field, FieldContent, FieldDescription, FieldError, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";

/**
 * Gmail filters, in Preferences → Mail.
 *
 * # Why this exists at all
 *
 * The agent can make one, and the agent is not the only way in. Every capability
 * the app grows has to be reachable by a person who does not want to have a
 * conversation about it — otherwise the answer to "filter these out" is still
 * "go to Gmail's web settings", which is where this started.
 *
 * # Why it waits on Google
 *
 * Everything else in Mach renders from SQLite and the network is a background
 * loop. A filter has never been a local row: it is not mail, there is nothing
 * to render it beside, and Google offers no change feed for it, so a cached
 * copy could only be refreshed by fetching the whole list — which is what this
 * does. The cost of caching would be paid on the one operation that must not be
 * wrong: a delete addresses an id, and an id from a stale list is either gone
 * or somebody else's rule. See `src-tauri/src/commands/filters.rs`.
 *
 * So this is the one surface in the app with a loading state, and it says so
 * only while it has nothing to show.
 *
 * # Keyboard
 *
 * Nothing here is reachable by mouse alone. The rows are in the DOM in reading
 * order, each has a Remove button, and the form is Inputs, Checkboxes and one
 * Select — all Base UI primitives, all tab-reachable, all operable with Space
 * and Enter. `PreferencesDialog` hands Tab back to the browser while it is
 * open, which is what makes that true.
 *
 * # Why creating is a form and not a text box
 *
 * Gmail's own filter dialog is this list of fields, and the vocabulary is worth
 * matching rather than inventing. The three checkboxes are the three things
 * people actually make filters for, and each is a label movement Gmail spells
 * strangely — which is why they are checkboxes with plain names rather than a
 * label picker that would make the owner know that "skip the inbox" means
 * removing INBOX.
 */

/** A new filter's fields, before it is anything. */
interface Draft {
  accountId: number | null;
  from: string;
  subject: string;
  query: string;
  skipInbox: boolean;
  markRead: boolean;
  trash: boolean;
  labelId: string;
}

/** The Select's value for "no label", which is not a label id. */
const NO_LABEL = "none";

function blankDraft(accountId: number | null): Draft {
  return {
    accountId,
    from: "",
    subject: "",
    query: "",
    skipInbox: false,
    markRead: false,
    trash: false,
    labelId: NO_LABEL,
  };
}

export function Filters({
  accounts,
  labels,
  missingScope,
}: {
  accounts: readonly Account[];
  labels: readonly Label[];
  /** Accounts whose grant does not cover filters yet. */
  missingScope: readonly string[];
}) {
  const ids = useIds();
  const [filters, setFilters] = useState<MailFilter[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [draft, setDraft] = useState<Draft | null>(null);
  const [busy, setBusy] = useState(false);

  const load = () => {
    setError(null);
    void getDataSource()
      .listFilters()
      .then(setFilters)
      .catch((caught: unknown) => {
        // Silent failure is what this project has paid most for. An empty list
        // and a failed list are different claims.
        setFilters([]);
        setError(message(caught));
      });
  };

  useEffect(load, []);

  const create = (draft: Draft) => {
    const accountId = draft.accountId ?? accounts[0]?.id;
    if (accountId == null) return;
    setBusy(true);
    setError(null);
    void getDataSource()
      .createFilter(accountId, criteriaOf(draft), actionOf(draft))
      .then((created) => {
        setFilters((was) => [...(was ?? []), created]);
        setDraft(null);
      })
      .catch((caught: unknown) => setError(message(caught)))
      .finally(() => setBusy(false));
  };

  const remove = (filter: MailFilter) => {
    setError(null);
    void getDataSource()
      .deleteFilter(filter.accountId, filter.id)
      .then(() => setFilters((was) => (was ?? []).filter((f) => f.id !== filter.id)))
      .catch((caught: unknown) => setError(message(caught)));
  };

  return (
    <Field orientation="row">
      <FieldLabel>Filters</FieldLabel>
      <FieldContent>
        {filters === null ? (
          <FieldDescription>Reading filters from Gmail…</FieldDescription>
        ) : filters.length === 0 ? (
          <FieldDescription>No filters</FieldDescription>
        ) : (
          filters.map((filter) => (
            <div key={`${filter.accountId}:${filter.id}`} className="flex min-w-0 items-start gap-2">
              <span
                className={cn(
                  "mt-1 h-4 w-[3px] shrink-0 rounded-full",
                  ACCOUNT_BG[colorOf(accounts, filter.accountId)],
                )}
              />
              <span className="min-w-0 flex-1 text-body leading-snug text-foreground">
                {filter.description}
              </span>
              <Button
                size="sm"
                variant="ghost"
                aria-label={`Remove filter: ${filter.description}`}
                onClick={() => remove(filter)}
              >
                Remove
              </Button>
            </div>
          ))
        )}

        {/* The one thing the owner has to act on: a grant that predates the
            permission. Kept because he would otherwise meet it as a failure
            every time he pressed New filter. */}
        {missingScope.length > 0 && (
          <FieldError>
            {missingScope.join(", ")} must be removed and added again before filters work.
          </FieldError>
        )}

        <FieldError>{error}</FieldError>

        {draft === null ? (
          <div>
            <Button
              variant="subtle"
              disabled={accounts.length === 0}
              onClick={() => setDraft(blankDraft(accounts[0]?.id ?? null))}
            >
              New filter
            </Button>
          </div>
        ) : (
          <div className="flex flex-col gap-2 rounded-[var(--radius)] border border-border bg-surface-raised p-2">
            {accounts.length > 1 && (
              <Row label="Account" htmlFor={ids.account}>
                <Select
                  items={accounts.map((a) => ({ value: String(a.id), label: a.email }))}
                  value={String(draft.accountId ?? accounts[0]?.id ?? "")}
                  onValueChange={(value) => {
                    if (value === null) return;
                    setDraft({ ...draft, accountId: Number(value), labelId: NO_LABEL });
                  }}
                >
                  <SelectTrigger id={ids.account} aria-label="Account">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    {accounts.map((account) => (
                      <SelectItem key={account.id} value={String(account.id)}>
                        {account.email}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </Row>
            )}

            <Row label="From" htmlFor={ids.from}>
              <Input
                id={ids.from}
                spellCheck={false}
                placeholder="no-reply@okta.com"
                value={draft.from}
                onChange={(event) => setDraft({ ...draft, from: event.target.value })}
              />
            </Row>

            <Row label="Subject" htmlFor={ids.subject}>
              <Input
                id={ids.subject}
                spellCheck={false}
                value={draft.subject}
                onChange={(event) => setDraft({ ...draft, subject: event.target.value })}
              />
            </Row>

            <Row label="Search" htmlFor={ids.query}>
              <Input
                id={ids.query}
                spellCheck={false}
                placeholder="has:attachment older_than:1y"
                value={draft.query}
                onChange={(event) => setDraft({ ...draft, query: event.target.value })}
              />
            </Row>

            <Row label="Then" htmlFor={ids.skipInbox}>
              <div className="flex min-w-0 flex-col gap-1">
                <Toggle
                  id={ids.skipInbox}
                  label="Skip the inbox"
                  checked={draft.skipInbox}
                  onChange={(on) => setDraft({ ...draft, skipInbox: on })}
                />
                <Toggle
                  id={ids.markRead}
                  label="Mark as read"
                  checked={draft.markRead}
                  onChange={(on) => setDraft({ ...draft, markRead: on })}
                />
                <Toggle
                  id={ids.trash}
                  label="Delete it"
                  checked={draft.trash}
                  onChange={(on) => setDraft({ ...draft, trash: on })}
                />
              </div>
            </Row>

            <Row label="Label" htmlFor={ids.label}>
              <Select
                items={labelItems(labels, draft.accountId)}
                value={draft.labelId}
                onValueChange={(value) => {
                  if (value === null) return;
                  setDraft({ ...draft, labelId: value });
                }}
              >
                <SelectTrigger id={ids.label} aria-label="Apply a label">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {labelItems(labels, draft.accountId).map((item) => (
                    <SelectItem key={item.value} value={item.value}>
                      {item.label}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </Row>

            {/* Not a description of the software: the thing a person coming
                from Gmail's dialog will assume and be wrong about. */}
            <FieldDescription>Applies to mail that arrives from now on</FieldDescription>

            <div className="flex items-center gap-2">
              <Button size="sm" disabled={busy || !isUsable(draft)} onClick={() => create(draft)}>
                Create filter
              </Button>
              <Button size="sm" variant="subtle" onClick={() => setDraft(null)}>
                Cancel
              </Button>
            </div>
          </div>
        )}
      </FieldContent>
    </Field>
  );
}

/** One labelled control inside the create form. */
function Row({
  label,
  htmlFor,
  children,
}: {
  label: string;
  htmlFor: string;
  children: React.ReactNode;
}) {
  return (
    <div className="flex min-w-0 items-start gap-2">
      <label
        htmlFor={htmlFor}
        className="w-20 shrink-0 pt-1 text-micro text-faint-foreground"
      >
        {label}
      </label>
      <div className="min-w-0 flex-1">{children}</div>
    </div>
  );
}

function Toggle({
  id,
  label,
  checked,
  onChange,
}: {
  id: string;
  label: string;
  checked: boolean;
  onChange: (checked: boolean) => void;
}) {
  return (
    <label
      htmlFor={id}
      className="flex min-w-0 cursor-default items-center gap-2 text-body text-foreground"
    >
      <Checkbox id={id} checked={checked} onCheckedChange={onChange} />
      <span className="min-w-0 truncate">{label}</span>
    </label>
  );
}

/**
 * The draft as Gmail's criteria.
 *
 * Empty fields are omitted rather than sent as empty strings: Gmail treats
 * `{"from": ""}` as a criterion that matches nothing useful, and Rust refuses a
 * filter whose criteria are all absent, which is the check that stops an empty
 * form becoming a rule over every message.
 */
export function criteriaOf(draft: Draft) {
  const criteria: Record<string, string> = {};
  if (draft.from.trim()) criteria.from = draft.from.trim();
  if (draft.subject.trim()) criteria.subject = draft.subject.trim();
  if (draft.query.trim()) criteria.query = draft.query.trim();
  return criteria;
}

/**
 * The draft as Gmail's action.
 *
 * The three checkboxes are label movements: removing INBOX is "skip the inbox",
 * removing UNREAD is "mark as read", and adding TRASH is "delete it".
 */
export function actionOf(draft: Draft) {
  const addLabelIds: string[] = [];
  const removeLabelIds: string[] = [];
  if (draft.skipInbox) removeLabelIds.push("INBOX");
  if (draft.markRead) removeLabelIds.push("UNREAD");
  if (draft.trash) addLabelIds.push("TRASH");
  if (draft.labelId !== NO_LABEL) addLabelIds.push(draft.labelId);
  return { addLabelIds, removeLabelIds };
}

/** A rule needs something to match and something to do. */
export function isUsable(draft: Draft): boolean {
  const criteria = criteriaOf(draft);
  const action = actionOf(draft);
  return (
    Object.keys(criteria).length > 0 &&
    action.addLabelIds.length + action.removeLabelIds.length > 0
  );
}

/**
 * The user labels on one account, with "no label" first.
 *
 * `accountId: null` means the label id is shared across every account — see
 * `mapLabels` — so it belongs on every list rather than none.
 */
function labelItems(labels: readonly Label[], accountId: number | null) {
  return [
    { value: NO_LABEL, label: "None" },
    ...labels
      .filter(
        (label) =>
          label.kind === "user" &&
          (label.accountId === null || accountId === null || label.accountId === accountId),
      )
      .map((label) => ({ value: label.id, label: label.name })),
  ];
}

function colorOf(accounts: readonly Account[], accountId: number): ColorIndex {
  return accounts.find((a) => a.id === accountId)?.colorIndex ?? 1;
}

function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function useIds() {
  const prefix = useId();
  return {
    account: `${prefix}-account`,
    from: `${prefix}-from`,
    subject: `${prefix}-subject`,
    query: `${prefix}-query`,
    skipInbox: `${prefix}-skip-inbox`,
    markRead: `${prefix}-mark-read`,
    trash: `${prefix}-trash`,
    label: `${prefix}-label`,
  };
}
