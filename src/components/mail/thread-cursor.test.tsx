// @vitest-environment jsdom

/**
 * Which message `r` answers.
 *
 * A conversation of eleven is eleven questions, and the reply keys used to mean
 * the last of them wherever the reader was standing. The cursor is *focus* —
 * every message header is a real button — so these tests are about the DOM: put
 * the keyboard on a message, ask what a reply verb would aim at, and check the
 * draft that comes back is built from that message and not from the newest one.
 */

import { renderToStaticMarkup } from "react-dom/server";
import { beforeEach, describe, expect, it } from "vitest";
import type { Message } from "@/types";
import * as fixtures from "@/lib/fixtures";
import { prepareDraft, replyRecipients } from "@/lib/compose";
import { ThreadMessage } from "./ThreadMessage";
import {
  MESSAGE_CURSOR,
  MESSAGE_ROW,
  focusedMessageId,
  messageRows,
  moveMessageCursor,
  replyTarget,
} from "./thread-cursor";

function message(id: number, over: Partial<Message> = {}): Message {
  return {
    id,
    threadId: 1,
    accountId: 1,
    from: { name: `Sender ${id}`, email: `s${id}@example.com` },
    to: [{ name: "Bruno Bornsztein", email: "bruno@example.com" }],
    cc: [],
    timestamp: Date.UTC(2026, 7, 9, 12, id),
    snippet: `message ${id}`,
    bodyText: `message ${id}`,
    attachments: [],
    isDraft: false,
    ...over,
  };
}

/** The conversation as the reading pane draws it, in a real document. */
function conversation(messages: Message[]): void {
  document.body.innerHTML = messages
    .map((m) =>
      renderToStaticMarkup(
        <ThreadMessage
          message={m}
          live={false}
          expanded={false}
          onToggle={() => {}}
          onOpenDraft={() => {}}
          onMenu={() => {}}
        />,
      ),
    )
    .join("");
}

const three = [message(1), message(2), message(3)];

describe("the cursor inside a conversation", () => {
  beforeEach(() => {
    document.body.innerHTML = "";
  });

  it("draws every message as a row that can hold the keyboard", () => {
    conversation(three);
    expect(messageRows().map((row) => row.getAttribute(MESSAGE_ROW))).toEqual(["1", "2", "3"]);
    // The focus stop is the header, and it is a real button — no tabIndex, no
    // role, nothing that gets most of a keyboard affordance and loses the rest.
    for (const row of messageRows()) {
      const stop = row.querySelector(`[${MESSAGE_CURSOR}]`);
      expect(stop?.tagName).toBe("BUTTON");
    }
  });

  it("aims at nothing while the keyboard is elsewhere", () => {
    conversation(three);
    expect(focusedMessageId()).toBeNull();
    // Which is the fallback: no message named means the newest one, the same
    // answer the strip under the conversation has always given.
    expect(replyTarget()).toBeNull();
  });

  it("starts from the newest message and steps back through the thread", () => {
    conversation(three);
    expect(moveMessageCursor(-1)).toBe(3);
    expect(replyTarget()).toBe(3);
    expect(moveMessageCursor(-1)).toBe(2);
    expect(moveMessageCursor(-1)).toBe(1);
    // No wrapping: running off the end of a long conversation and silently
    // reappearing at the other end is how a reader loses their place.
    expect(moveMessageCursor(-1)).toBe(1);
    expect(moveMessageCursor(1)).toBe(2);
  });

  it("answers for the message the keyboard is anywhere inside", () => {
    conversation(three);
    moveMessageCursor(-1);
    moveMessageCursor(-1);
    // The ⋮ beside the timestamp is in the same row; focusing it must not
    // change which message a reply verb means.
    const menuButton = document
      .querySelector(`[${MESSAGE_ROW}="2"]`)
      ?.querySelector<HTMLElement>('button[aria-haspopup="menu"]');
    menuButton?.focus();
    expect(replyTarget()).toBe(2);
  });

  it("never answers a draft row, which is your own unsent text", () => {
    conversation([message(1), message(2, { isDraft: true })]);
    const draftRow = document.querySelector<HTMLElement>(`[${MESSAGE_ROW}="2"] button`);
    draftRow?.focus();
    expect(focusedMessageId()).toBe(2);
    // …and yet nothing to answer: `r` there falls back to the conversation.
    expect(replyTarget()).toBeNull();
    // It has no menu either. Reply / reply all / forward mean nothing on it.
    expect(
      document.querySelector(`[${MESSAGE_ROW}="2"] button[aria-haspopup="menu"]`),
    ).toBeNull();
  });
});

describe("the draft a reply verb prepares", () => {
  /** A fixture conversation with more than one message in it. */
  const threadId = [...fixtures.messagesByThread.entries()].find(
    ([, messages]) => messages.filter((m) => !m.isDraft).length > 2,
  )![0];
  const messages = fixtures.messagesByThread.get(threadId)!.filter((m) => !m.isDraft);

  it("answers the newest message when none was named", async () => {
    const draft = await prepareDraft(threadId, "reply");
    expect(draft.replyToId).toBe(messages[messages.length - 1].id);
  });

  it("answers the message that was named, from the middle of the thread", async () => {
    const middle = messages[messages.length - 2];
    const draft = await prepareDraft(threadId, "reply", middle.id);

    expect(draft.replyToId).toBe(middle.id);
    // And it is addressed from *that* message: the recipients are what
    // `replyRecipients` makes of it alone. Every fixture conversation is
    // two-party, so this pins the derivation rather than discriminating
    // between two answers — the case where the sets genuinely differ is in
    // `src-tauri/tests/compose.rs`, against a three-party thread.
    const account = fixtures.accounts.find((a) => a.id === draft.accountId)!;
    const expected = replyRecipients(
      { from: middle.from, to: middle.to, cc: middle.cc },
      account.email,
      fixtures.accounts.map((a) => a.email),
      false,
    );
    expect(draft.to).toEqual(expected.to);
  });

  it("carries reply-all's wider set from the named message", async () => {
    const middle = messages[messages.length - 2];
    const draft = await prepareDraft(threadId, "replyAll", middle.id);
    const account = fixtures.accounts.find((a) => a.id === draft.accountId)!;
    const expected = replyRecipients(
      { from: middle.from, to: middle.to, cc: middle.cc },
      account.email,
      fixtures.accounts.map((a) => a.email),
      true,
    );
    expect(draft.replyToId).toBe(middle.id);
    expect(draft.to).toEqual(expected.to);
    expect(draft.cc).toEqual(expected.cc);
  });

  it("falls back rather than threading onto a draft row", async () => {
    const withDraft = fixtures.messagesByThread.get(fixtures.DRAFT_THREAD_ID)!;
    const draftRow = withDraft.find((m) => m.isDraft)!;
    const newest = [...withDraft].reverse().find((m) => !m.isDraft)!;

    const draft = await prepareDraft(fixtures.DRAFT_THREAD_ID, "reply", draftRow.id);
    expect(draft.replyToId).toBe(newest.id);
  });
});
