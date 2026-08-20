/**
 * The thread row, tested as markup.
 *
 * Three lines is a claim about *layout*, and layout is the thing a unit test is
 * worst at — nothing here can see that the subject is legible. What it can see
 * is the structure that makes it so: that the subject is in a box of its own
 * rather than sharing one with the snippet, that the sender line carries the
 * date and the marks, and that the pieces which used to fight for width no
 * longer share a parent. When somebody flattens this back into one line to save
 * a few pixels, these are the assertions that say so.
 *
 * Rendered with `react-dom/server`, like `ThreadMessage.test.tsx`: no jsdom,
 * nothing to click, and the output is the markup itself.
 */

import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type { Thread } from "@/types";
import { ThreadRow } from "./ThreadRow";

const LONG_SUBJECT =
  "[InfluenceKit] production - 10 occurrences in 5 minutes - ActiveRecord::StatementInvalid";

function thread(over: Partial<Thread> = {}): Thread {
  return {
    id: 1,
    accountId: 1,
    subject: LONG_SUBJECT,
    snippet: "Project: InfluenceKit Environment: production Code Version:",
    participants: [{ name: "Rollbar Notification", email: "no-reply@rollbar.com" }],
    timestamp: Date.UTC(2026, 7, 9, 12, 2),
    unread: false,
    starred: false,
    hasAttachment: false,
    messageCount: 1,
    labelIds: ["INBOX"],
    ...over,
  };
}

function row(over: Partial<Thread> = {}, props: Partial<Parameters<typeof ThreadRow>[0]> = {}) {
  return renderToStaticMarkup(
    <ThreadRow
      thread={thread(over)}
      unread={over.unread ?? false}
      cursor={false}
      checked={false}
      selecting={false}
      onSelect={() => {}}
      {...props}
    />,
  );
}

/** The text of every element that truncates, in document order. */
function truncatingText(html: string): string[] {
  return [...html.matchAll(/<(?:span|div)[^>]*truncate[^>]*>([^<]*)</g)].map((m) => m[1] ?? "");
}

describe("the thread row", () => {
  it("puts the sender, the subject and the preview in three separate boxes", () => {
    const html = row();
    const lines = truncatingText(html);
    // Each of the three is on its own, so each truncates against the width of
    // the list rather than against the other two.
    expect(lines).toContain("Rollbar Notification");
    expect(lines).toContain(LONG_SUBJECT);
    expect(lines).toContain("Project: InfluenceKit Environment: production Code Version:");
  });

  it("gives a long subject the whole line, with nothing beside it", () => {
    const html = row();
    // The subject's own element carries no width cap and no siblings: the only
    // thing that can cut it is the list itself. The old row gave it 66% of what
    // was left after the sender, the count and the date had taken theirs.
    expect(html).not.toContain("max-w-[66%]");
    const subjectBox = html.match(/<div[^>]*>([^<]*)<\/div>/g)?.find((box) =>
      box.includes(LONG_SUBJECT),
    );
    expect(subjectBox).toBeDefined();
    expect(subjectBox).toContain("truncate");
  });

  it("keeps the date last on the sender line, shrink-wrapped to its own text", () => {
    const html = row();
    // Not a fixed-width box. One of those aligns the dates' left edges and, in
    // doing so, opens a hole between the marks and the digits on every row with
    // a short time — see the note in the component.
    expect(html).not.toMatch(/w-\[[\d.]+rem\][^"]*tabular-nums/);
    expect(html).toContain("tabular-nums");
    expect(html.indexOf("Rollbar Notification")).toBeLessThan(html.indexOf(LONG_SUBJECT));
  });

  it("shows the message count for a thread, and nothing for a single message", () => {
    expect(row({ messageCount: 4 })).toContain(">4<");
    // The count sits on the sender line, so a four-message thread and a
    // one-message thread give their subjects exactly the same width.
    const one = row({ messageCount: 1 });
    expect(one).not.toContain(">1<");
    expect(one).toContain(LONG_SUBJECT);
  });

  it("draws the star and the attachment mark only when they are true", () => {
    expect(row()).not.toContain("<svg");
    expect([...row({ starred: true }).matchAll(/<svg/g)]).toHaveLength(1);
    expect([...row({ hasAttachment: true }).matchAll(/<svg/g)]).toHaveLength(1);
    expect([...row({ starred: true, hasAttachment: true }).matchAll(/<svg/g)]).toHaveLength(2);
  });

  it("puts the marks immediately beside the date, as one right-hand cluster", () => {
    const cluster = row({ starred: true, hasAttachment: true }).split("ml-auto")[1] ?? "";
    const date = cluster.indexOf("tabular-nums");
    // Both marks inside the cluster and ahead of the date, with no reserved
    // slot between them: an empty slot, like the fixed-width date box that used
    // to sit here, reads on screen as a hole.
    expect(cluster.lastIndexOf("<svg")).toBeGreaterThan(-1);
    expect(cluster.lastIndexOf("<svg")).toBeLessThan(date);
    expect(cluster.slice(0, date)).not.toContain("justify-center");
  });

  it("marks unread with the dot and the weight, on the sender and the subject", () => {
    // Counted over the whole row this used to be `>= 2` and `not.toContain`,
    // which held only for as long as nothing else in the row was ever medium.
    // The monogram's letters are, so the assertion now looks at the two
    // elements it is actually about: the line-one sender, and the subject.
    const weighted = (markup: string) =>
      [...markup.matchAll(/class="([^"]*)"[^>]*>(?:[^<]*)/g)]
        .filter(([, cls]) => /truncate/.test(cls) && !/tabular-nums/.test(cls))
        .filter(([, cls]) => /font-medium/.test(cls)).length;

    const unread = row({ unread: true });
    expect(unread).toContain("bg-accent");
    expect(weighted(unread)).toBeGreaterThanOrEqual(2);

    const read = row();
    expect(read).toContain("bg-transparent");
    expect(weighted(read)).toBe(0);
  });

  /**
   * The gutter is symmetrical, and it is symmetrical on a *read* row.
   *
   * The unread dot used to sit in the row's flex flow and reserve its 6px
   * whether or not it was drawn, so a read row spent 12px of padding, 6px of
   * nothing and an 8px gap before the monogram, and 8px after it. On screen the
   * tile looked shoved against the text. The dot is out of flow now; this pins
   * that it stays out of flow, because putting it back is a one-word change
   * that looks like nothing in a diff.
   */
  it("keeps the sender's tile off the text, on read rows and unread alike", () => {
    for (const unread of [true, false]) {
      const markup = row({}, { unread });
      // Absolutely placed, so it costs the row no horizontal space.
      expect(markup).toMatch(/class="[^"]*absolute left-2[^"]*rounded-full/);
      // And the tile owns the whole 16px on its right — `pl-4` on one side,
      // `mr-4` on the other. It used to be `mr-2` against a `gap-2` on the row;
      // the gap went when the tick column started animating its own 8px, and
      // the tile absorbed it rather than losing it.
      expect(markup).toMatch(/class="[^"]*mr-4[^"]*"[^>]*style="width:26px/);
    }
  });

  /**
   * The tick column costs nothing until a selection exists — and it opens
   * rather than appears.
   *
   * It used to be `{selecting && <button/>}`, which is free when shut and
   * un-animatable: entering selection shoved the sender, subject and preview
   * 22px right between one frame and the next, on every visible row at once.
   * The button is drawn always now and its column widened from zero, so the
   * bargain is the same and the change is something the eye can follow.
   *
   * Both halves are asserted, because either alone would let the other rot: a
   * width that never returns to zero is the permanent column the row refused,
   * and a column with no transition is the snap that was reported.
   */
  it("keeps the tick column shut and free until a selection is live", () => {
    const idle = row({}, { selecting: false });
    expect(idle).toContain('role="checkbox"');
    expect(idle).toContain("w-0");
    // Shut means shut: not reachable by tab, not announced.
    expect(idle).toContain('aria-hidden="true"');
    expect(idle).toContain('tabindex="-1"');
  });

  it("opens the tick column rather than snapping it open", () => {
    const live = row({}, { selecting: true });
    expect(live).toContain("w-[22px]");
    expect(live).toContain("transition-[width]");
    expect(live).toContain("motion-reduce:transition-none");
    // Scoped to the tick's own wrapper — the monogram is aria-hidden too.
    expect(live).toMatch(/aria-hidden="false"[^>]*transition-\[width\]/);
  });

  it("says which row the cursor and the selection are on", () => {
    expect(row({}, { cursor: true })).toContain('aria-selected="true"');
    expect(row({}, { checked: true })).toContain('aria-selected="true"');
    expect(row()).toContain('aria-selected="false"');
  });

  it("shows the mailbox a search result came from, on the sender line", () => {
    const html = row({}, { context: "Archive" });
    expect(html).toContain("Archive");
    expect(html.indexOf("Archive")).toBeLessThan(html.indexOf(LONG_SUBJECT));
  });

  it("is one fixed-height row, so the list scrolls in a rhythm", () => {
    expect(row()).toContain("h-row");
  });

  it("says the preview is your own unsent text, and still shows the text", () => {
    const html = row({ labelIds: ["INBOX", "DRAFT"] });
    // The word, not just a colour: this row is read in greyscale, by a
    // colour-blind reader, and by a screen reader that gets no CSS at all.
    expect(html).toContain(">Draft<");
    // And the draft's own words stay — they are the last thing on the
    // conversation, and replacing them would put stale text on the row.
    expect(html).toContain("Project: InfluenceKit Environment: production Code Version:");
    // On the preview line, ahead of the snippet, not up beside the sender.
    expect(html.indexOf(">Draft<")).toBeGreaterThan(html.indexOf(LONG_SUBJECT));
  });

  it("leaves an ordinary conversation unmarked", () => {
    expect(row()).not.toContain(">Draft<");
  });

  it("lets the Drafts mailbox turn the mark off, where every row would carry it", () => {
    const html = row({ labelIds: ["DRAFT"] }, { draft: false });
    expect(html).not.toContain(">Draft<");
  });
});
