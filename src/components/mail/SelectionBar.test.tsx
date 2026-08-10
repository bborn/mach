// @vitest-environment jsdom

/**
 * The bar, as markup.
 *
 * What `selection-actions.test.ts` cannot see is whether any of that reaches
 * the screen: that the bar is absent until something is ticked, that it names
 * the count and the verbs together, that each verb is drawn with its key beside
 * it rather than instead of it, and that the destructive one turns into a
 * question instead of acting.
 *
 * `useMach` is stubbed rather than mounted. The provider would need a data
 * source, a boot, a stream and a theme to render one strip, and none of those
 * decide anything this file is about — the bar reads six fields and draws them.
 */

import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import { KeymapProvider } from "@/hooks/useKeymap";
import type { Thread, ThreadId } from "@/types";

const state = {
  labelId: "INBOX" as string,
  ids: [] as ThreadId[],
  /**
   * What a command would act on, which is not always what is ticked:
   * `commandTargets` falls back to the open conversation. Defaults to `ids`.
   */
  targets: null as ThreadId[] | null,
  confirmDiscard: false,
  threads: [] as Thread[],
};

vi.mock("@/hooks/useMach", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/hooks/useMach")>();
  return {
    ...actual,
    useMach: () => ({
      ui: {
        labelId: state.labelId,
        selection: { ids: state.ids, anchor: null },
        confirmDiscard: state.confirmDiscard,
        overlays: 0,
      },
      dispatch: () => {},
      visibleThreads: state.threads,
      commandTargets: state.targets ?? state.ids,
      isUnread: (thread: Thread) => thread.unread,
      actions: {},
    }),
  };
});

const { SelectionBar } = await import("./SelectionBar");

function thread(over: Partial<Thread> = {}): Thread {
  return {
    id: 1,
    accountId: 1,
    subject: "Re: contract",
    snippet: "Sending the redline back",
    participants: [{ name: "Dana", email: "dana@example.test" }],
    timestamp: 1_000,
    unread: true,
    starred: false,
    hasAttachment: false,
    messageCount: 1,
    labelIds: ["INBOX"],
    ...over,
  };
}

function render(over: Partial<typeof state> = {}): HTMLElement {
  Object.assign(state, {
    labelId: "INBOX",
    ids: [],
    targets: null,
    confirmDiscard: false,
    threads: [],
    ...over,
  });
  const host = document.createElement("div");
  host.innerHTML = renderToStaticMarkup(
    <KeymapProvider>
      <SelectionBar />
    </KeymapProvider>,
  );
  return host;
}

/** The verbs on offer, in the order they are drawn. */
function verbs(host: HTMLElement): string[] {
  return [...host.querySelectorAll("button")].map((button) =>
    (button.textContent ?? "").replace(/\s+/g, " ").trim(),
  );
}

const rows = (n: number) =>
  Array.from({ length: n }, (_, i) => thread({ id: i + 1 }));

const ids = (n: number) => rows(n).map((t) => t.id);

describe("the selection bar", () => {
  it("is not there until something is selected", () => {
    expect(render().innerHTML).toBe("");
    expect(render({ ids: [1], threads: rows(1) }).innerHTML).not.toBe("");
  });

  it("goes away again when the selection is cleared", () => {
    expect(render({ ids: ids(6), threads: rows(6) }).innerHTML).not.toBe("");
    expect(render({ ids: [], threads: rows(6) }).innerHTML).toBe("");
  });

  it("counts what the next keystroke will act on", () => {
    const host = render({ ids: ids(6), threads: rows(6) });
    expect(host.textContent).toContain("6 selected");
  });

  it("offers the inbox its triage verbs, each with its key", () => {
    const host = render({ ids: ids(6), threads: rows(6) });
    expect(verbs(host)).toEqual([
      "E Archive",
      "B Snooze",
      "S Star",
      "⇧I Mark read",
      "# Trash",
    ]);
  });

  it("offers Drafts a discard and nothing else", () => {
    const host = render({
      labelId: "DRAFT",
      ids: ids(6),
      threads: rows(6).map((t) => ({ ...t, labelIds: ["DRAFT"] })),
    });
    expect(verbs(host)).toEqual(["# Discard"]);
    expect(host.textContent).toContain("6 selected");
  });

  it("offers Trash a restore and Spam a way to say it is not spam", () => {
    expect(verbs(render({ labelId: "TRASH", ids: ids(3), threads: rows(3) }))).toEqual([
      "⇧E Restore",
    ]);
    expect(verbs(render({ labelId: "SPAM", ids: ids(3), threads: rows(3) }))).toEqual([
      "⇧E Not spam",
      "# Trash",
    ]);
  });

  it("says Unstar and Mark unread when that is the honest direction", () => {
    const starredAndRead = rows(3).map((t) => ({ ...t, starred: true, unread: false }));
    expect(verbs(render({ ids: ids(3), threads: starredAndRead }))).toEqual([
      "E Archive",
      "B Snooze",
      "S Unstar",
      "⇧U Mark unread",
      "# Trash",
    ]);
  });

  /*
   * The one action that asks. Six drafts must not be easier to destroy than
   * one, and one has been asking since `ComposerDock` was written — a discard
   * ends at `drafts.delete` and leaves ⌘Z nothing to run.
   */
  it("turns into a question rather than discarding six drafts on one press", () => {
    const host = render({
      labelId: "DRAFT",
      ids: ids(6),
      confirmDiscard: true,
      threads: rows(6),
    });
    expect(host.textContent).toContain("Throw these 6 drafts away?");
    expect(host.textContent).toContain("Not kept anywhere else");
    expect(verbs(host)).toEqual(["# Discard", "Esc Keep"]);
    // The question states the number, so the count beside it stands down.
    expect(host.textContent).not.toContain("selected");
  });

  it("shows the question even with nothing ticked, because one draft can arm it too", () => {
    // `commandTargets` falls back to the open conversation, so `#` on a single
    // draft arms the same flag. A bar that drew only on a count would leave
    // that press invisible and the next one fatal.
    const host = render({
      labelId: "DRAFT",
      ids: [],
      targets: [7],
      confirmDiscard: true,
      threads: rows(1),
    });
    expect(host.textContent).toContain("Throw this draft away?");
    expect(host.textContent).not.toContain("selected");
    expect(verbs(host)).toEqual(["# Discard", "Esc Keep"]);
  });

  it("does not ask anything in a mailbox with nothing to discard", () => {
    // The flag is app state and outlives nothing, but the question belongs to
    // the action — no discard on offer, no question.
    const host = render({ ids: ids(6), confirmDiscard: true, threads: rows(6) });
    expect(host.textContent).not.toContain("Throw");
    expect(verbs(host)).toHaveLength(5);
  });
});
