/**
 * `ctx.mach` — the entire API surface, implemented on the host side.
 *
 * Six namespaces, about twenty functions. It is small on purpose: every
 * function here is something the core promises not to break for a whole major
 * version, and the way to keep that promise is to promise less.
 *
 * # This file is the DTO layer
 *
 * `docs/plugins.md` §6 names what is *never* public API: `Thread`, `Message`,
 * `CalendarEvent` as internal shapes, `MachDataSource`, the SQLite schema, the
 * `src/lib/ipc.ts` wire mapping. So nothing internal crosses the boundary —
 * every value a plugin sees is built here, from a narrower vocabulary. That
 * mapping is the whole cost of being able to refactor the core freely, and it
 * is the cheapest insurance in the design.
 *
 * The most load-bearing line in the file is in `read.threads`: it does **not**
 * copy the body. `threads.metadata` is a genuinely smaller ask than `threads`,
 * the install prompt says so out loud, and that would be a lie if the metadata
 * read leaked a snippet of body text the user did not agree to.
 */

import type { Command, CommandResult, MachDataSource } from "@/lib/data";
import type { Participant, Thread, ThreadId } from "@/types";
import type { PluginId } from "./types";

/* -------------------------------------------------------------------------- */
/* What a plugin sees                                                          */
/* -------------------------------------------------------------------------- */

export interface PluginPerson {
  name: string;
  email: string;
}

/** Subjects, participants, labels, dates, snippets. **Not bodies.** */
export interface PluginThreadSummary {
  id: ThreadId;
  accountId: number;
  subject: string;
  snippet: string;
  participants: PluginPerson[];
  at: number;
  isUnread: boolean;
  isStarred: boolean;
  labelIds: string[];
  messageCount: number;
}

/** The above, plus message bodies as plain text. Needs `read: ["threads"]`. */
export interface PluginThreadDetail extends PluginThreadSummary {
  messages: {
    id: number;
    from: PluginPerson;
    to: PluginPerson[];
    cc: PluginPerson[];
    at: number;
    /** Plain text. Never HTML: a plugin has no DOM to render it in anyway. */
    text: string;
    attachments: { filename: string; mimeType: string; sizeBytes: number }[];
  }[];
}

export interface PluginEvent {
  id: number;
  calendarId: string;
  accountId: number;
  title: string;
  start: number;
  end: number;
  isAllDay: boolean;
  location?: string;
  organizer?: PluginPerson;
  attendees: PluginPerson[];
  rsvp?: string;
}

export interface PluginLabel {
  id: string;
  accountId: number | null;
  name: string;
  kind: "system" | "user";
}

export interface PluginAccount {
  id: number;
  email: string;
  name: string;
}

/* -------------------------------------------------------------------------- */
/* What the host has to supply                                                 */
/* -------------------------------------------------------------------------- */

/** Host-rendered prompts. A plugin never draws a dialog itself. */
export interface AskHost {
  pick(o: {
    pluginName: string;
    title: string;
    items: { id: string; title: string; subtitle?: string; value: unknown }[];
  }): Promise<unknown | null>;
  text(o: {
    pluginName: string;
    title: string;
    placeholder?: string;
    initial?: string;
  }): Promise<string | null>;
  confirm(o: {
    pluginName: string;
    title: string;
    body?: string;
    danger?: boolean;
  }): Promise<boolean>;
}

export interface ApiOptions {
  id: PluginId;
  name: string;
  source: MachDataSource;
  ask: AskHost;
  notify: (message: string, tone: "info" | "error") => void;
  log: (...args: unknown[]) => void;
  /** Called with every `CommandResult` so one action can undo as one group. */
  onRun?: (command: Command, result: CommandResult) => void;
  store?: PluginStore;
  now?: () => number;
}

/* -------------------------------------------------------------------------- */
/* The private key–value namespace                                             */
/* -------------------------------------------------------------------------- */

/** A few hundred kilobytes, per the design. Enough for a list of label ids. */
export const STORE_LIMIT_BYTES = 256 * 1024;

export interface PluginStore {
  get(key: string): Promise<unknown>;
  set(key: string, value: unknown): Promise<void>;
}

/**
 * `mach.store`, on the host side of the wall.
 *
 * Host-side rather than in the guest's own `localStorage` even though the guest
 * has its own origin and therefore its own partition — because a plugin's data
 * should survive the guest being torn down and rebuilt, and because a bounded
 * store needs someone to enforce the bound.
 */
export function localPluginStore(id: PluginId): PluginStore {
  const prefix = `mach.plugin.${id}.`;
  return {
    async get(key) {
      try {
        const raw = window.localStorage.getItem(prefix + key);
        return raw === null ? null : JSON.parse(raw);
      } catch {
        return null;
      }
    },
    async set(key, value) {
      const body = JSON.stringify(value ?? null);
      if (body.length > STORE_LIMIT_BYTES) {
        throw new Error(
          `${id} tried to store ${body.length} bytes under "${key}"; the limit is ${STORE_LIMIT_BYTES}`,
        );
      }
      window.localStorage.setItem(prefix + key, body);
    },
  };
}

/* -------------------------------------------------------------------------- */
/* The API                                                                     */
/* -------------------------------------------------------------------------- */

/**
 * Build the flat `mach.*` implementation table for one plugin.
 *
 * Keyed by the same dotted names the worker shim posts and `capability.ts`
 * checks, so there is exactly one spelling of every method in the system.
 */
export function createHostApi(options: ApiOptions): Record<string, (...args: never[]) => unknown> {
  const { source } = options;
  const store = options.store ?? localPluginStore(options.id);

  return {
    /**
     * Dispatch a core command.
     *
     * `source` is what makes the audit trail real: the command layer is told
     * which plugin asked, checks the grant a second time, and rate limits per
     * plugin. A plugin has no other way to change anything.
     */
    async run(command: Command) {
      const result = await source.execute(command, `plugin:${options.id}`);
      options.onRun?.(command, result);
      return result;
    },

    async "read.threads"(query: {
      labelId?: string;
      accountId?: number | null;
      unreadOnly?: boolean;
      limit?: number;
    }) {
      const page = await source.listThreads({
        labelId: query?.labelId ?? "INBOX",
        accountId: query?.accountId ?? null,
        unreadOnly: query?.unreadOnly ?? false,
        limit: Math.min(Math.max(query?.limit ?? 50, 1), 200),
      });
      return page.threads.map(summary);
    },

    async "read.thread"(threadId: ThreadId) {
      const detail = await source.getThread(threadId);
      if (!detail) return null;
      const out: PluginThreadDetail = {
        ...summary(detail.thread),
        messages: detail.messages.map((message) => ({
          id: message.id,
          from: person(message.from),
          to: message.to.map(person),
          cc: message.cc.map(person),
          at: message.timestamp,
          // Plain text only. The sanitizer's output is not a plugin API — see
          // "message-body transforms: never" in the design.
          text: message.bodyText ?? "",
          attachments: message.attachments.map((a) => ({
            filename: a.filename,
            mimeType: a.mimeType,
            sizeBytes: a.sizeBytes,
          })),
        })),
      };
      return out;
    },

    async "read.events"(range: { start: number; end: number }) {
      const events = await source.listEvents({
        start: Number(range?.start ?? 0),
        end: Number(range?.end ?? 0),
      });
      return events.map<PluginEvent>((event) => ({
        id: event.id,
        calendarId: event.calendarId,
        accountId: event.accountId,
        title: event.title,
        start: event.start,
        end: event.end,
        // `isAllDay`, not `allDay`: the plugin vocabulary follows the command
        // catalogue's spelling, which is the one that is public API.
        isAllDay: event.allDay,
        location: event.location,
        organizer: event.organizer ? person(event.organizer) : undefined,
        attendees: event.attendees.map(person),
        rsvp: event.rsvp,
      }));
    },

    async "read.labels"(accountId?: number | null) {
      const labels = await source.listLabels(accountId ?? null);
      return labels.map<PluginLabel>((label) => ({
        id: label.id,
        accountId: label.accountId,
        name: label.name,
        kind: label.kind,
      }));
    },

    async "read.accounts"() {
      const accounts = await source.listAccounts();
      return accounts.map<PluginAccount>((account) => ({
        id: account.id,
        email: account.email,
        name: account.name,
      }));
    },

    "ask.pick"(o: {
      title: string;
      items: { id: string; title: string; subtitle?: string; value: unknown }[];
    }) {
      return options.ask.pick({
        pluginName: options.name,
        title: String(o?.title ?? ""),
        items: (o?.items ?? []).slice(0, 500),
      });
    },

    "ask.text"(o: { title: string; placeholder?: string; initial?: string }) {
      return options.ask.text({ pluginName: options.name, ...o });
    },

    "ask.confirm"(o: { title: string; body?: string; danger?: boolean }) {
      return options.ask.confirm({ pluginName: options.name, ...o });
    },

    notify(message: string, tone?: "info" | "error") {
      options.notify(String(message ?? "").slice(0, 200), tone === "error" ? "error" : "info");
      return null;
    },

    "store.get"(key: string) {
      return store.get(String(key));
    },

    "store.set"(key: string, value: unknown) {
      return store.set(String(key), value);
    },

    log(...args: unknown[]) {
      options.log(...args);
      return null;
    },
  };
}

function summary(thread: Thread): PluginThreadSummary {
  return {
    id: thread.id,
    accountId: thread.accountId,
    subject: thread.subject,
    snippet: thread.snippet,
    participants: thread.participants.map(person),
    at: thread.timestamp,
    isUnread: thread.unread,
    isStarred: thread.starred,
    labelIds: [...thread.labelIds],
    messageCount: thread.messageCount,
  };
}

function person(p: Participant): PluginPerson {
  return { name: p.name, email: p.email };
}
