import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createKeymap, type KeyEventLike } from "./keymap";
import {
  COMPOSER_KEYS,
  createAutosave,
  formatRecipients,
  isLocalOnly,
  newDraft,
  markdownToHtml,
  parseRecipients,
  scheduleOptions,
  toPlainText,
  type Draft,
} from "./compose";

/* -------------------------------------------------------------------------- */
/* The editor grammar                                                          */
/* -------------------------------------------------------------------------- */

/**
 * Duplicated verbatim from `MARKDOWN_CASES` in `src-tauri/tests/compose.rs`.
 *
 * Two implementations of one grammar exist because the thread shows the reply
 * the instant ⌘⏎ is pressed, before Rust has been asked anything. Rust's output
 * is what actually gets sent; this one only has to agree with it. Pinning both
 * to the same table is the only thing that keeps them from drifting — if they
 * do, both suites fail.
 */
const MARKDOWN_CASES: [string, string][] = [
  ["plain", "<p>plain</p>"],
  ["**bold**", "<p><strong>bold</strong></p>"],
  ["*italic*", "<p><em>italic</em></p>"],
  ["_italic_", "<p><em>italic</em></p>"],
  ["`code`", "<p><code>code</code></p>"],
  ["a\nb", "<p>a<br>b</p>"],
  ["a\n\nb", "<p>a</p><p>b</p>"],
  ["- one\n- two", "<ul><li>one</li><li>two</li></ul>"],
  ["1. one\n2. two", "<ol><li>one</li><li>two</li></ol>"],
  ["> quoted", "<blockquote><p>quoted</p></blockquote>"],
  ["# Title", "<h1>Title</h1>"],
  ["### Small", "<h3>Small</h3>"],
  ["<script>x</script>", "<p>&lt;script&gt;x&lt;/script&gt;</p>"],
  ["5 * 3 * 2", "<p>5 * 3 * 2</p>"],
  ["`**not bold**`", "<p><code>**not bold**</code></p>"],
  [
    "see https://example.com/a_b_c now",
    '<p>see <a href="https://example.com/a_b_c">https://example.com/a_b_c</a> now</p>',
  ],
  ["a & b", "<p>a &amp; b</p>"],
  ["¿Sí?", "<p>¿Sí?</p>"],
];

describe("the markdown-ish grammar", () => {
  it.each(MARKDOWN_CASES)("renders %j", (source, expected) => {
    expect(markdownToHtml(source)).toBe(expected);
  });

  it("never emits markup the user did not type", () => {
    const html = markdownToHtml('<img src=x onerror=alert(1)> <a href="javascript:x">hi</a>');
    expect(html).not.toContain("<img");
    expect(html).not.toContain("<a ");
    expect(html).toContain("&lt;img");
  });

  it("keeps a URL intact through the emphasis pass", () => {
    // `_` is legal in a path. Emphasis running over an href is a broken link,
    // not a typo, which is why URLs are lifted out before it.
    const html = markdownToHtml("https://ex.com/a_b_c_d");
    expect(html).toBe('<p><a href="https://ex.com/a_b_c_d">https://ex.com/a_b_c_d</a></p>');
  });

  it("leaves the trailing full stop out of a link", () => {
    expect(markdownToHtml("see https://ex.com.")).toBe(
      '<p>see <a href="https://ex.com">https://ex.com</a>.</p>',
    );
  });

  it("uses the source as the plain-text part", () => {
    const source = "**Yes** — see https://example.com\n\n- one\n- two";
    expect(toPlainText(source)).toBe(source);
  });

  it("normalises Windows line endings", () => {
    expect(toPlainText("a\r\nb")).toBe("a\nb");
    expect(markdownToHtml("a\r\nb")).toBe("<p>a<br>b</p>");
  });
});

/* -------------------------------------------------------------------------- */
/* Recipients                                                                  */
/* -------------------------------------------------------------------------- */

describe("recipient fields", () => {
  it("round-trips what the field shows", () => {
    const list = [{ name: "Tawny Rivers", email: "tawny@partner.com" }, { email: "sam@x.com" }];
    expect(formatRecipients(list)).toBe("Tawny Rivers <tawny@partner.com>, sam@x.com");
    expect(parseRecipients(formatRecipients(list))).toEqual(list);
  });

  it("dedupes case-insensitively, so a pasted list cannot mail anyone twice", () => {
    expect(parseRecipients("a@x.com, A@X.com, b@x.com").map((m) => m.email)).toEqual([
      "a@x.com",
      "b@x.com",
    ]);
  });

  it("does not split inside a quoted display name", () => {
    const parsed = parseRecipients('"Patel, Sam" <sam@x.com>, bob@y.com');
    expect(parsed).toEqual([
      { name: "Patel, Sam", email: "sam@x.com" },
      { email: "bob@y.com" },
    ]);
  });

  it("ignores empty fragments from a trailing comma", () => {
    expect(parseRecipients("a@x.com, ")).toEqual([{ email: "a@x.com" }]);
    expect(parseRecipients("")).toEqual([]);
  });
});

/* -------------------------------------------------------------------------- */
/* Keys                                                                        */
/* -------------------------------------------------------------------------- */

function press(over: Partial<KeyEventLike> & { key: string }): KeyEventLike {
  return {
    metaKey: false,
    ctrlKey: false,
    altKey: false,
    shiftKey: false,
    // The composer's own bindings have to fire while the cursor is in the
    // editor, which is the whole point of the assertions below.
    target: { tagName: "TEXTAREA" },
    preventDefault: () => {},
    stopPropagation: () => {},
    ...over,
  };
}

describe("the composer's bindings", () => {
  it("sends on ⌘⏎ while the cursor is in the editor", () => {
    const keymap = createKeymap("meta");
    const send = vi.fn();
    keymap.register({
      keys: COMPOSER_KEYS.send,
      allowInInput: true,
      priority: 100,
      handler: send,
    });

    expect(keymap.handle(press({ key: "Enter", metaKey: true }))).toBe(true);
    expect(send).toHaveBeenCalledTimes(1);
  });

  it("schedules on ⌃S while the cursor is in the editor", () => {
    const keymap = createKeymap("meta");
    const schedule = vi.fn();
    keymap.register({
      keys: COMPOSER_KEYS.schedule,
      allowInInput: true,
      priority: 100,
      handler: schedule,
    });

    expect(keymap.handle(press({ key: "s", ctrlKey: true }))).toBe(true);
    expect(schedule).toHaveBeenCalledTimes(1);
    // ⌘S is the browser's save dialog, not ours: only ⌃S schedules.
    expect(keymap.handle(press({ key: "s", metaKey: true }))).toBe(false);
    expect(schedule).toHaveBeenCalledTimes(1);
  });

  it("does not fire on a bare Enter — that is a newline", () => {
    const keymap = createKeymap("meta");
    const send = vi.fn();
    keymap.register({
      keys: COMPOSER_KEYS.send,
      allowInInput: true,
      priority: 100,
      handler: send,
    });
    expect(keymap.handle(press({ key: "Enter" }))).toBe(false);
    expect(send).not.toHaveBeenCalled();
  });

  it("would be dead in the editor without allowInInput — the trap this avoids", () => {
    const keymap = createKeymap("meta");
    const send = vi.fn();
    keymap.register({ keys: COMPOSER_KEYS.send, priority: 100, handler: send });
    expect(keymap.handle(press({ key: "Enter", metaKey: true }))).toBe(false);
    expect(send).not.toHaveBeenCalled();
  });

  it("opens the composer on r, a and f from the list", () => {
    const keymap = createKeymap("meta");
    const opened: string[] = [];
    for (const [kind, key] of [
      ["reply", COMPOSER_KEYS.reply],
      ["replyAll", COMPOSER_KEYS.replyAll],
      ["forward", COMPOSER_KEYS.forward],
    ] as const) {
      keymap.register({
        keys: key,
        priority: 5,
        handler: () => {
          opened.push(kind);
        },
      });
    }

    keymap.handle(press({ key: "r", target: { tagName: "DIV" } }));
    keymap.handle(press({ key: "a", target: { tagName: "DIV" } }));
    keymap.handle(press({ key: "f", target: { tagName: "DIV" } }));
    expect(opened).toEqual(["reply", "replyAll", "forward"]);

    // …and never while typing.
    keymap.handle(press({ key: "r" }));
    expect(opened).toHaveLength(3);
  });
});

/* -------------------------------------------------------------------------- */
/* Autosave                                                                    */
/* -------------------------------------------------------------------------- */

function draft(body: string): Draft {
  return {
    id: "d1",
    accountId: 1,
    threadId: 7,
    replyToId: 9,
    kind: "reply",
    to: [{ email: "tawny@partner.com" }],
    cc: [],
    bcc: [],
    subject: "Re: Series A data room",
    body,
    updatedAt: 0,
  };
}

describe("draft autosave", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("writes once after the typing stops, with the last thing typed", () => {
    const save = vi.fn();
    const autosave = createAutosave(save, 700);

    autosave.queue(draft("O"));
    vi.advanceTimersByTime(300);
    autosave.queue(draft("On "));
    vi.advanceTimersByTime(300);
    autosave.queue(draft("On it"));
    expect(save).not.toHaveBeenCalled();

    vi.advanceTimersByTime(700);
    expect(save).toHaveBeenCalledTimes(1);
    expect(save.mock.calls[0][0].body).toBe("On it");
  });

  it("flushes immediately when the editor is closed mid-debounce", () => {
    // This is the difference between "a crash loses the last 700ms" and "a
    // crash loses the reply".
    const save = vi.fn();
    const autosave = createAutosave(save, 700);

    autosave.queue(draft("half a thought"));
    autosave.flush();
    expect(save).toHaveBeenCalledTimes(1);
    expect(save.mock.calls[0][0].body).toBe("half a thought");

    // And the pending timer does not save a second time.
    vi.advanceTimersByTime(2000);
    expect(save).toHaveBeenCalledTimes(1);
  });

  it("cancel discards without saving", () => {
    const save = vi.fn();
    const autosave = createAutosave(save, 700);
    autosave.queue(draft("never mind"));
    autosave.cancel();
    vi.advanceTimersByTime(2000);
    expect(save).not.toHaveBeenCalled();
  });

  it("a flush with nothing pending is a no-op", () => {
    const save = vi.fn();
    createAutosave(save, 700).flush();
    expect(save).not.toHaveBeenCalled();
  });
});

/* -------------------------------------------------------------------------- */
/* Scheduling                                                                  */
/* -------------------------------------------------------------------------- */

describe("⌃S", () => {
  it("offers three answers, all in the future", () => {
    // Wednesday 2026-08-05, 10:00 local.
    const now = new Date(2026, 7, 5, 10, 0, 0).getTime();
    const options = scheduleOptions(now);
    expect(options).toHaveLength(3);
    expect(options.map((o) => o.label)).toEqual(["In 3 hours", "Tomorrow, 8am", "Monday, 8am"]);
    for (const option of options) expect(option.at).toBeGreaterThan(now);
    expect(new Date(options[2].at).getDay()).toBe(1);
  });

  it("means *next* Monday when it is already Monday", () => {
    const monday = new Date(2026, 7, 3, 10, 0, 0).getTime();
    const options = scheduleOptions(monday);
    const chosen = options.find((o) => o.label === "Monday, 8am")!;
    expect(new Date(chosen.at).getDate()).toBe(10);
  });
});

/* -------------------------------------------------------------------------- */
/* where a draft actually is                                                   */
/* -------------------------------------------------------------------------- */

describe("a draft's Gmail state", () => {
  // A draft is written locally and pushed to Gmail in the background, so it is
  // on his phone too. The only state worth saying out loud is the one where
  // that failed — silence would leave him believing a draft was somewhere it
  // is not.
  it("says nothing while a draft is on its way", () => {
    const draft: Draft = { ...newDraft(1), remote: { state: "pending" } };
    expect(isLocalOnly(draft)).toBe(false);
  });

  it("says nothing once Gmail has it", () => {
    const draft: Draft = {
      ...newDraft(1),
      remote: { state: "synced", draftId: "r-1", messageId: "m-1" },
    };
    expect(isLocalOnly(draft)).toBe(false);
  });

  it("speaks up when the push was refused", () => {
    const draft: Draft = {
      ...newDraft(1),
      remote: { state: "failed", error: "Google refused" },
    };
    expect(isLocalOnly(draft)).toBe(true);
  });

  it("says nothing about a draft that has never been through Rust", () => {
    // The browser fallback, and any draft built in the editor before its first
    // save. Absent is not a failure.
    expect(isLocalOnly(newDraft(1))).toBe(false);
  });
});
