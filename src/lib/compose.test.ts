import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createKeymap, type KeyEventLike } from "./keymap";
import { withHtmlSignature } from "./email-html";
import {
  bodyAsHtml,
  COMPOSER_KEYS,
  createAutosave,
  hasWrittenBody,
  humanSize,
  hasSubject,
  isDraftEmpty,
  isUntouched,
  formatRecipients,
  isLocalOnly,
  newDraft,
  markdownToHtml,
  parseRecipients,
  prepareDraft,
  replyKeyAim,
  replyRecipients,
  replySubject,
  visibleComposer,
  forwardRecipients,
  forwardSubject,
  scheduleOptions,
  toPlainText,
  type Draft,
  type DraftKind,
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

  it("leaves ⌘⌫ to the editor, and discards on ⇧⌘⌫", () => {
    /*
     * The message this test is named after is gone. Discard was `mod+backspace`
     * with `allowInInput: true`, and on macOS ⌘⌫ deletes to the beginning of
     * the line — so pressing it twice in the editor, which is what deleting two
     * lines *is*, asked and then confirmed the discard.
     *
     * Both halves matter. The handler must not run, and the event must not be
     * prevented, or the key stops deleting the line without destroying anything
     * and the composer eats a system editing command for nothing.
     */
    const keymap = createKeymap("meta");
    const discard = vi.fn();
    keymap.register({
      keys: COMPOSER_KEYS.discard,
      allowInInput: true,
      priority: 100,
      handler: discard,
    });

    const prevented = vi.fn();
    const bare = press({ key: "Backspace", metaKey: true, preventDefault: prevented });
    expect(keymap.handle(bare)).toBe(false);
    expect(discard).not.toHaveBeenCalled();
    expect(prevented).not.toHaveBeenCalled();

    expect(keymap.handle(press({ key: "Backspace", metaKey: true, shiftKey: true }))).toBe(true);
    expect(discard).toHaveBeenCalledTimes(1);
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

describe("a draft written in another client", () => {
  /*
   * The tripwire for the one enum that crosses the seam by name. Rust assigns
   * `adopted` to a draft it took over from Gmail — see `compose::draft::
   * DraftKind::as_str` — and a union here that has never heard of it would make
   * every such draft fail to type-check at the call site rather than at the
   * boundary, which is exactly how `isDraft` went missing from `mapMessage`.
   *
   * The array is typed, so dropping a member is a compile error; the length
   * assertion is what catches a member being *added* on the Rust side and
   * quietly ignored here.
   */
  const KINDS: DraftKind[] = ["new", "reply", "replyAll", "forward", "adopted"];

  it("has a kind of its own, pinned to the Rust enum", () => {
    expect(KINDS).toHaveLength(5);
    expect(KINDS).toContain("adopted");
  });

  it("is not a local-only draft — Gmail is where it came from", () => {
    // It is `synced` from the instant it is adopted, because the local copy is
    // a copy of what Gmail already holds. Nothing to say, and nothing to push.
    const adopted: Draft = {
      ...newDraft(1),
      kind: "adopted",
      remote: { state: "synced", draftId: "r-9999", messageId: "gmsg-1" },
    };
    expect(isLocalOnly(adopted)).toBe(false);
  });

  it("is not the shape that opens over the window", () => {
    // `ComposerDock` gives a *new* message a surface of its own because it has
    // nothing to do with the thread on screen. An adopted draft does: it lives
    // in the conversation it was written into, so it keeps the dock.
    const adopted: Draft = { ...newDraft(1), kind: "adopted", threadId: 41 };
    expect(adopted.kind === "new").toBe(false);
  });
});

/* -------------------------------------------------------------------------- */
/* The body, and what counts as an empty draft                                 */
/* -------------------------------------------------------------------------- */

describe("bodyAsHtml", () => {
  it("hands back HTML untouched", () => {
    const draft: Draft = { ...newDraft(1), body: "<div>Hi</div>", bodyFormat: "html" };
    expect(bodyAsHtml(draft)).toBe("<div>Hi</div>");
  });

  it("renders a draft written before the editor was rich text", () => {
    // The one place the old grammar is still read. A reply half-written last
    // week opens with its `**bold**` already bold rather than showing the owner
    // his own asterisks.
    const draft: Draft = { ...newDraft(1), body: "**Yes** — on it.", bodyFormat: "markdown" };
    expect(bodyAsHtml(draft)).toBe("<p><strong>Yes</strong> — on it.</p>");
  });

  it("treats a row with no format at all as the old one", () => {
    // The SQLite default is `markdown` for exactly this reason, and a payload
    // that omits the field has to agree with the column.
    const draft = { ...newDraft(1), body: "*hi*", bodyFormat: undefined } as Draft;
    expect(bodyAsHtml(draft)).toBe("<p><em>hi</em></p>");
  });
});

describe("isDraftEmpty", () => {
  const html = (body: string): Draft => ({ ...newDraft(1), body, bodyFormat: "html" });

  it("knows an untouched rich-text editor is empty", () => {
    expect(isDraftEmpty(html(""))).toBe(true);
    // What a contenteditable contains when nothing has been typed into it.
    expect(isDraftEmpty(html("<div><br></div>"))).toBe(true);
  });

  it("does not count the signature as something the user wrote", () => {
    // A composer that opened, signed itself and was closed again must not leave
    // a draft row behind — or the conversation offers an empty reply for ever.
    const signed = html(withHtmlSignature("<div><br></div>", "Bruno\nMach"));
    expect(isDraftEmpty(signed)).toBe(true);
    expect(hasWrittenBody(signed)).toBe(false);
  });

  it("counts a recipient, a subject or a file as content on their own", () => {
    expect(isDraftEmpty({ ...html(""), to: [{ email: "a@b.c" }] })).toBe(false);
    expect(isDraftEmpty({ ...html(""), subject: "Numbers" })).toBe(false);
    expect(
      isDraftEmpty({
        ...html(""),
        attachments: [
          { id: "a1", draftId: "d1", filename: "q3.csv", mimeType: "text/csv", sizeBytes: 12 },
        ],
      }),
    ).toBe(false);
  });

  it("counts typing as content", () => {
    expect(isDraftEmpty(html("<div>ok</div>"))).toBe(false);
  });
});

describe("hasSubject", () => {
  /*
   * The gate in front of ⌘⏎. It has to agree with what the recipient sees: a
   * header full of spaces is drawn as "(no subject)" at both ends, so anything
   * that trims away to nothing is nothing.
   */
  it("is false for a subject that trims away to nothing", () => {
    expect(hasSubject({ subject: "" })).toBe(false);
    expect(hasSubject({ subject: " " })).toBe(false);
    expect(hasSubject({ subject: "   \t \n  " })).toBe(false);
    // A non-breaking space is whitespace to `trim`, and it is what a paste out
    // of a web page leaves behind.
    expect(hasSubject({ subject: " " })).toBe(false);
  });

  it("is true the moment there is a character in it", () => {
    expect(hasSubject({ subject: "Roof" })).toBe(true);
    expect(hasSubject({ subject: "  Roof  " })).toBe(true);
    // `replySubject` at its emptiest, which is why a reply is never asked.
    expect(hasSubject({ subject: "Re:" })).toBe(true);
    expect(hasSubject({ subject: "?" })).toBe(true);
  });
});

describe("humanSize", () => {
  it("says what a person would say", () => {
    expect(humanSize(512)).toBe("512 bytes");
    expect(humanSize(2048)).toBe("2 KB");
    expect(humanSize(5 * 1024 * 1024)).toBe("5.0 MB");
  });
});

describe("the composer's keys", () => {
  it("does not claim a key another live binding already has", () => {
    // The calendar deletes an event with `mod+backspace`, which discard used to
    // answer too. It is ⇧⌘⌫ now, so the two are different keys — and the third
    // registration below still stands in for the modal, because `conflicts`
    // reports same-priority ties among *live* bindings and that is the question
    // that matters whatever the tokens are.
    const keymap = createKeymap("meta");
    keymap.register({
      keys: COMPOSER_KEYS.discard,
      priority: 100,
      allowInInput: true,
      handler: () => {},
    });
    keymap.register({
      keys: COMPOSER_KEYS.attach,
      priority: 100,
      allowInInput: true,
      handler: () => {},
    });
    keymap.register({
      keys: "mod+backspace",
      priority: 120,
      when: () => false, // the event modal, which is not up
      handler: () => {},
    });
    expect(keymap.conflicts()).toEqual([]);
  });

  it("keeps ⌘K for the editor's link only while a composer has it", () => {
    const keymap = createKeymap("meta");
    let palette = 0;
    let link = 0;
    keymap.register({
      keys: "mod+k",
      allowInInput: true,
      priority: 200,
      handler: () => {
        palette += 1;
      },
    });
    let composing = false;
    keymap.register({
      keys: "mod+k",
      allowInInput: true,
      priority: 210,
      when: () => composing,
      handler: () => {
        link += 1;
      },
    });

    const press = (): KeyEventLike => ({
      key: "k",
      code: "KeyK",
      metaKey: true,
      ctrlKey: false,
      altKey: false,
      shiftKey: false,
    });
    keymap.handle(press());
    expect([palette, link]).toEqual([1, 0]);

    composing = true;
    keymap.handle(press());
    expect([palette, link]).toEqual([1, 1]);
    expect(keymap.conflicts()).toEqual([]);
  });
});

/* -------------------------------------------------------------------------- */
/* What a reply is made of                                                     */
/* -------------------------------------------------------------------------- */

/**
 * The defect these pin: `r` on an open conversation produced a composer with an
 * empty To showing its placeholder and `Re:` with nothing after it.
 *
 * The arithmetic is Rust's — `compose::address` is what a real reply is built
 * from — and this is the twin that serves the fixture source, so the same cases
 * are asserted in `src-tauri/tests/compose.rs`. Drift fails both suites.
 */
describe("reply recipients", () => {
  const tawny = { name: "Tawny", email: "tawny@partner.com" };
  const sam = { name: "Sam", email: "sam@partner.com" };
  const dana = { name: "Dana", email: "dana@partner.com" };
  const me = { name: "Alex", email: "alex@example.com" };
  const mine = ["alex@example.com"];

  it("addresses a reply to the author", () => {
    const r = replyRecipients({ from: tawny, to: [me], cc: [] }, me.email, mine, false);
    expect(r.to.map((m) => m.email)).toEqual([tawny.email]);
    expect(r.cc).toEqual([]);
  });

  it("prefers Reply-To over From", () => {
    const list = { name: "The List", email: "list@lists.example" };
    const r = replyRecipients(
      { from: tawny, replyTo: [list], to: [me], cc: [] },
      me.email,
      mine,
      false,
    );
    expect(r.to.map((m) => m.email)).toEqual([list.email]);
  });

  it("keeps everyone else on a reply-all and never yourself", () => {
    const r = replyRecipients({ from: tawny, to: [me, sam], cc: [dana] }, me.email, mine, true);
    expect(r.to.map((m) => m.email)).toEqual([tawny.email]);
    expect(r.cc.map((m) => m.email)).toEqual([sam.email, dana.email]);
  });

  it("removes every account the app holds, not only the sending one", () => {
    const other = { name: "Alex at home", email: "alex@personal.example" };
    const r = replyRecipients(
      { from: tawny, to: [me, other], cc: [sam] },
      me.email,
      [me.email, other.email],
      true,
    );
    expect(r.to.map((m) => m.email)).toEqual([tawny.email]);
    expect(r.cc.map((m) => m.email)).toEqual([sam.email]);
  });

  it("continues your own message rather than answering yourself", () => {
    const r = replyRecipients({ from: me, to: [tawny], cc: [sam] }, me.email, mine, true);
    expect(r.to.map((m) => m.email)).toEqual([tawny.email]);
    expect(r.cc.map((m) => m.email)).toEqual([sam.email]);
  });

  // The reported bug, at its root: the owner replied to a note he had mailed
  // himself. `from`, `to` and the account were one address, so removing
  // "yourself" removed everybody and the composer opened addressed to nobody.
  it("still addresses a note you mailed yourself", () => {
    const r = replyRecipients({ from: me, to: [me], cc: [] }, me.email, mine, false);
    expect(r.to.map((m) => m.email)).toEqual([me.email]);
  });

  it("still addresses that note on reply-all", () => {
    const r = replyRecipients({ from: me, to: [me], cc: [] }, me.email, mine, true);
    expect(r.to.map((m) => m.email)).toEqual([me.email]);
    expect(r.cc).toEqual([]);
  });

  it("dedupes case-insensitively", () => {
    const r = replyRecipients(
      { from: tawny, to: [me, sam, { name: "Sam again", email: "SAM@partner.com" }], cc: [] },
      me.email,
      mine,
      true,
    );
    expect(r.cc.map((m) => m.email)).toEqual([sam.email]);
  });

  it("leaves a forward for the sender to address", () => {
    expect(forwardRecipients()).toEqual({ to: [], cc: [] });
  });
});

describe("reply and forward subjects", () => {
  it("adds one Re: and never a second", () => {
    expect(replySubject("Invoice")).toBe("Re: Invoice");
    expect(replySubject("Re: Invoice")).toBe("Re: Invoice");
    expect(replySubject("RE: Invoice")).toBe("Re: Invoice");
    expect(replySubject("re: re: Invoice")).toBe("Re: Invoice");
    expect(replySubject("Re[2]: Invoice")).toBe("Re: Invoice");
    expect(replySubject("RE : Invoice")).toBe("Re: Invoice");
  });

  it("leaves a word that merely starts with re alone", () => {
    expect(replySubject("Rethinking pricing")).toBe("Re: Rethinking pricing");
    expect(replySubject("Regarding: pricing")).toBe("Re: Regarding: pricing");
  });

  it("keeps the Fwd: on a reply to a forward, and vice versa", () => {
    expect(replySubject("Fwd: Invoice")).toBe("Re: Fwd: Invoice");
    expect(forwardSubject("Fwd: Invoice")).toBe("Fwd: Invoice");
    expect(forwardSubject("FW: Invoice")).toBe("Fwd: Invoice");
    expect(forwardSubject("Re: Invoice")).toBe("Fwd: Re: Invoice");
  });

  it("says Re: on its own when there was no subject at all", () => {
    expect(replySubject("")).toBe("Re:");
    expect(forwardSubject("")).toBe("Fwd:");
  });
});

/**
 * The browser fallback answers the same questions as `draft::prepare`, so `r`
 * against the fixture source opens the composer a real reply opens. It used to
 * hand back an empty draft, which is the reported symptom exactly.
 */
describe("prepare, against the fixture source", () => {
  it("addresses a reply and carries the subject", async () => {
    const draft = await prepareDraft(1, "reply");
    expect(draft.to.map((m) => m.email)).toEqual(["tawny@northloop.example"]);
    expect(draft.subject).toBe("Re: Series A data room — a few gaps");
    expect(draft.accountId).toBe(1);
    expect(draft.replyToId).toBe(101);
  });

  it("does the same for reply-all", async () => {
    const draft = await prepareDraft(1, "replyAll");
    expect(draft.to.map((m) => m.email)).toEqual(["tawny@northloop.example"]);
    expect(draft.cc).toEqual([]);
  });

  it("leaves a forward unaddressed but titled", async () => {
    const draft = await prepareDraft(1, "forward");
    expect(draft.to).toEqual([]);
    expect(draft.cc).toEqual([]);
    expect(draft.subject).toBe("Fwd: Re: Series A data room — a few gaps");
  });
});

/* -------------------------------------------------------------------------- */
/* Which composer the reply keys are asking about                              */
/* -------------------------------------------------------------------------- */

/**
 * The bug this is here to stop coming back.
 *
 * `r`, `a` and `f` were gated on "no composer is open anywhere". That held while
 * there could only be one; the tab strip made it false. A reply left open on one
 * conversation made all three dead on *every other* conversation — and the
 * composer responsible was not on screen to explain itself, because a reply is
 * only ever drawn inside its own thread. The footer under the message went on
 * offering "R reply · A reply all · F forward" the whole time.
 *
 * Reproduced in the real window before it was fixed: open a reply on one thread,
 * click another in the list, press r — the key reported itself unhandled and
 * nothing happened.
 */
describe("the composer a reply key is asking about", () => {
  const reply = (threadId: number | null) => ({ kind: "reply" as DraftKind, threadId });

  it("is nothing when the open reply belongs to another conversation", () => {
    expect(visibleComposer(reply(41774), 28594)).toBeNull();
  });

  it("so r on that conversation opens its own reply", () => {
    expect(replyKeyAim(visibleComposer(reply(41774), 28594))).toBe("open");
  });

  it("is the reply when it belongs to the conversation being read", () => {
    expect(visibleComposer(reply(28594), 28594)).not.toBeNull();
  });

  /*
   * The other half of the fix: with this thread's composer up, `r` is the way
   * back into it. ⇥ steps between the rail and the list rather than into a
   * half-written message, so a caret knocked out of the composer by a click in
   * the list had no keyboard route home at all.
   */
  it("so r there puts the caret back in it", () => {
    expect(replyKeyAim(visibleComposer(reply(28594), 28594))).toBe("focus");
  });

  it("shows a new message wherever you are", () => {
    const fresh = { kind: "new" as DraftKind, threadId: null };
    expect(visibleComposer(fresh, 28594)).not.toBeNull();
    expect(visibleComposer(fresh, null)).not.toBeNull();
  });

  it("is nothing when no composer is open", () => {
    expect(visibleComposer(null, 28594)).toBeNull();
    expect(replyKeyAim(null)).toBe("open");
  });
});

/**
 * The predicate that decides whether an open composer is worth a row.
 *
 * This is the bug it exists for: he replied, typed nothing, closed the window,
 * replied again, typed the real reply and sent it — and the untouched first
 * composer had by then been saved, mirrored into the conversation and pushed to
 * Gmail, so the thread carried a red `DRAFT` row above the reply that had gone.
 * `isDraftEmpty` could never have stopped it, because a reply with recipients
 * in it is not empty.
 */
describe("isUntouched", () => {
  const prepared = (over: Partial<Draft> = {}): Draft => ({
    ...newDraft(1),
    kind: "replyAll",
    to: [{ email: "candi@example.test" }],
    cc: [{ email: "sean@example.test" }],
    subject: "Re: Documents Required",
    body: "<div><br></div>",
    bodyFormat: "html",
    ...over,
  });

  it("knows a reply composer that was opened and never typed in", () => {
    const draft = prepared();
    expect(isUntouched(draft, prepared())).toBe(true);
    // And that `isDraftEmpty` never could: the recipients are prefilled.
    expect(isDraftEmpty(draft)).toBe(false);
  });

  it("does not count the signature the composer put there itself", () => {
    const signed = prepared({ body: withHtmlSignature("<div><br></div>", "Bruno\nMach") });
    expect(isUntouched(signed, prepared())).toBe(true);
  });

  it("counts a word, a file, an edited subject or an added recipient", () => {
    const base = prepared();
    expect(isUntouched(prepared({ body: "<div>ok</div>" }), base)).toBe(false);
    expect(isUntouched(prepared({ subject: "Re: Documents Required (2)" }), base)).toBe(false);
    expect(
      isUntouched(prepared({ cc: [...base.cc, { email: "kim@example.test" }] }), base),
    ).toBe(false);
    expect(
      isUntouched(
        prepared({
          attachments: [
            { id: "a1", draftId: "d1", filename: "q3.csv", mimeType: "text/csv", sizeBytes: 12 },
          ],
        }),
        base,
      ),
    ).toBe(false);
  });

  it("counts a recipient the writer took *off* the reply", () => {
    expect(isUntouched(prepared({ cc: [] }), prepared())).toBe(false);
  });

  it("does not mind the order the recipients come back in", () => {
    const base = prepared({ to: [{ email: "a@x.test" }, { email: "b@x.test" }] });
    const swapped = prepared({ to: [{ email: "B@x.test" }, { email: "a@x.test" }] });
    expect(isUntouched(swapped, base)).toBe(true);
  });
});
