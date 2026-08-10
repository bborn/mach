/**
 * What a selection is offered, per mailbox — and that the offer is real.
 *
 * Two claims, and the second is the one that matters. The first is the table
 * itself: Drafts offers a discard and not an archive, Trash offers a way out,
 * Spam offers "not spam". The second is that every entry in that table is the
 * same command as the key printed beside it — asserted by registering the real
 * bindings against a real keymap, pressing each action's key, and calling each
 * action's handler, and requiring the two to reach the same method of
 * `MachActions`.
 *
 * That is what keeps the bar from becoming a second implementation. A button
 * that archived by a different route would pass a test asserting "clicking
 * Archive archives"; it cannot pass this one.
 */

import { describe, expect, it, vi } from "vitest";
import type { MachActions } from "@/hooks/useMach";
import { createKeymap, type Keymap } from "@/lib/keymap";
import { keyEventFromToken } from "@/lib/menu";
import { mailActionBindings, mailActionHandlers } from "./mail-bindings";
import { mailboxOffers, putBackLabel, selectionActions } from "./selection-actions";

const MIXED = { allStarred: false, anyUnread: true };
const READ_AND_STARRED = { allStarred: true, anyUnread: false };

const labels = (id: string, marks = MIXED) =>
  selectionActions(id, marks).map((action) => action.label);

const keys = (id: string, marks = MIXED) =>
  selectionActions(id, marks).map((action) => action.keys);

describe("what each mailbox offers a selection", () => {
  it("gives the inbox the triage set", () => {
    expect(labels("INBOX")).toEqual(["Archive", "Snooze", "Star", "Mark read", "Trash"]);
    expect(keys("INBOX")).toEqual(["e", "b", "s", "shift+i", "#"]);
  });

  it("gives a user label the same set as the inbox", () => {
    // Everything the rail does not name falls here, and archive still means
    // something in a label: the conversation leaves the inbox and keeps the
    // label, so the row stays and the mailbox behind it changes.
    expect(labels("Label_17")).toEqual(labels("INBOX"));
  });

  it("offers Drafts one verb, and it is the one the report asked for", () => {
    // Archive removes INBOX from a thread that never had it — an empty diff,
    // no request, six rows exactly where they were. Snooze hides a thread from
    // an inbox a draft is not in. Read and unread are states a draft has not
    // got. What is left is the thing you came here to do.
    expect(labels("DRAFT")).toEqual(["Discard"]);
    expect(keys("DRAFT")).toEqual(["#"]);
  });

  it("offers Trash the way out", () => {
    expect(labels("TRASH")).toEqual(["Restore"]);
    expect(keys("TRASH")).toEqual(["shift+e"]);
  });

  it("offers Spam 'not spam' first and the bin second", () => {
    expect(labels("SPAM")).toEqual(["Not spam", "Trash"]);
    expect(keys("SPAM")).toEqual(["shift+e", "#"]);
  });

  it("offers Archive the way back, and Sent neither way", () => {
    expect(labels("ARCHIVE")).toEqual(["Move to inbox", "Star", "Mark read", "Trash"]);
    // Nothing in Sent is in the inbox and nothing in it is unread.
    expect(labels("SENT")).toEqual(["Star", "Trash"]);
  });

  it("never offers archive where archive would do nothing", () => {
    for (const id of ["DRAFT", "TRASH", "SPAM", "SENT", "ARCHIVE", "SNOOZED"]) {
      expect(labels(id)).not.toContain("Archive");
      expect(mailboxOffers(id, "archive")).toBe(false);
    }
    expect(mailboxOffers("INBOX", "archive")).toBe(true);
  });

  it("puts the destructive verb last wherever there is one", () => {
    for (const id of ["INBOX", "SPAM", "SENT", "ARCHIVE", "SNOOZED", "DRAFT"]) {
      const actions = selectionActions(id, MIXED);
      const danger = actions.filter((a) => a.tone === "danger");
      expect(danger).toHaveLength(1);
      expect(actions[actions.length - 1]).toBe(danger[0]);
    }
  });

  it("names the direction the selection is actually going", () => {
    // The same rule `starSelected` and `markReadSelected` follow: a mixed set
    // gets starred and gets marked read, so the label has to say which.
    expect(labels("INBOX", MIXED)).toContain("Star");
    expect(labels("INBOX", MIXED)).toContain("Mark read");
    expect(labels("INBOX", READ_AND_STARRED)).toContain("Unstar");
    expect(labels("INBOX", READ_AND_STARRED)).toContain("Mark unread");
    expect(keys("INBOX", READ_AND_STARRED)).toContain("shift+u");
  });

  it("asks before it discards, and only before it discards", () => {
    // A discard ends at `drafts.delete` and there is no inverse for ⌘Z, which
    // is the whole reason this one flag exists. Everything else is undoable
    // and confirming it would be furniture.
    expect(selectionActions("DRAFT", MIXED)[0]?.confirm).toBe(true);
    for (const id of ["INBOX", "TRASH", "SPAM", "SENT", "ARCHIVE", "SNOOZED"]) {
      expect(selectionActions(id, MIXED).some((a) => a.confirm)).toBe(false);
    }
  });

  it("has a word for the put-back only where there is somewhere to go", () => {
    expect(putBackLabel("TRASH")).toBe("Restore");
    expect(putBackLabel("SPAM")).toBe("Not spam");
    expect(putBackLabel("ARCHIVE")).toBe("Move to inbox");
    expect(putBackLabel("INBOX")).toBeNull();
    expect(putBackLabel("DRAFT")).toBeNull();
  });
});

describe("a button and its key are the same command", () => {
  /** A `MachActions` that only records which method was reached. */
  function spyActions() {
    const called: string[] = [];
    const record =
      (name: string) =>
      () => {
        called.push(name);
      };
    const actions = {
      archiveSelected: vi.fn(record("archiveSelected")),
      trashSelected: vi.fn(record("trashSelected")),
      starSelected: vi.fn(record("starSelected")),
      markReadSelected: vi.fn((read: boolean) =>
        called.push(read ? "markReadSelected(true)" : "markReadSelected(false)"),
      ),
      putBackSelected: vi.fn(record("putBackSelected")),
      discardSelected: vi.fn(record("discardSelected")),
      setSnooze: vi.fn((open: boolean) => called.push(`setSnooze(${String(open)})`)),
      toggleFavoriteFocused: vi.fn(record("toggleFavoriteFocused")),
      undo: vi.fn(record("undo")),
    } as unknown as MachActions;
    return { actions, called };
  }

  function registryFor(mailbox: string, actions: MachActions): Keymap {
    const keymap = createKeymap("meta");
    for (const binding of mailActionBindings(
      { active: () => true, mail: () => true, mailbox: () => mailbox },
      mailActionHandlers(actions),
    )) {
      keymap.register(binding);
    }
    return keymap;
  }

  const MAILBOXES = ["INBOX", "DRAFT", "TRASH", "SPAM", "SENT", "ARCHIVE", "SNOOZED"];

  it("reaches the same method whichever way the action is taken", () => {
    for (const mailbox of MAILBOXES) {
      for (const action of selectionActions(mailbox, MIXED)) {
        const viaKey = spyActions();
        const keymap = registryFor(mailbox, viaKey.actions);
        expect(keymap.handle(keyEventFromToken(action.keys))).toBe(true);

        const viaButton = spyActions();
        mailActionHandlers(viaButton.actions)[action.handler]();

        expect(viaKey.called).toHaveLength(1);
        expect(viaButton.called).toEqual(viaKey.called);
      }
    }
  });

  it("registers a live binding for every button, and no button without one", () => {
    for (const mailbox of MAILBOXES) {
      const { actions } = spyActions();
      const live = registryFor(mailbox, actions)
        .active()
        .filter((b) => b.group === "Actions");
      const offered = new Set(selectionActions(mailbox, MIXED).map((a) => a.keys));

      for (const key of offered) {
        expect(live.some((b) => b.keys === key)).toBe(true);
      }
      // The other way round, for the keys the bar is responsible for. ⇧F, `z`
      // and the ⇧U half of the read pair are the actions' own and are not
      // buttons: favouriting and undo act on the conversation, not the
      // selection, and ⇧U is drawn only when it is the honest direction.
      const notButtons = new Set(["shift+f", "z", "h", "mod+backspace", "shift+u", "shift+i"]);
      for (const binding of live) {
        if (notButtons.has(binding.keys)) continue;
        expect(offered.has(binding.keys)).toBe(true);
      }
    }
  });
});
