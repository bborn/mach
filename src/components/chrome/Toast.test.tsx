/**
 * The undo toast, tested as three claims and some markup.
 *
 * The claims are the ones the surface exists to make. That it lives exactly as
 * long as the preference says — the toast is the undo *offer*, and an offer
 * that outlasts or undershoots the window the user chose is a different
 * feature. That a run of archives is one card and not a wall of them. And that
 * a failure is told apart from a confirmation by something other than its
 * wording, because the wording is the one thing a screen reader user cannot
 * see.
 *
 * Rendered with `react-dom/server`, no jsdom and nothing to click, for the same
 * reason `CalendarSidebar.test.tsx` is: the assertions worth having here are
 * about *elements* — a live region of the right politeness, a real button with
 * a real accessible name — and those survive being read as markup.
 */

import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { ERROR_HOLD, statusLifetime, type StatusMessage } from "@/hooks/useMach";
import { DEFAULT_PREFERENCES, undoWindowMs } from "@/lib/prefs";
import { ToastLayer, collapse, noToast, offerFor, type ToastAction } from "./Toast";

function status(over: Partial<StatusMessage> = {}): StatusMessage {
  return { message: "Archived 3 conversations", tone: "info", ...over };
}

const UNDO: ToastAction = {
  word: "Undo",
  title: "Undo archived 3 conversations",
  keys: "mod+z",
  run: () => {},
};

/**
 * The two live regions, separately.
 *
 * Which one a message landed in is the whole accessibility claim, and it is
 * invisible in a flat string search of the document.
 */
function regions(html: string): { polite: string; assertive: string } {
  const split = html.indexOf('role="alert"');
  expect(split).toBeGreaterThan(-1);
  return { polite: html.slice(0, split), assertive: html.slice(split) };
}

function render(props: Partial<Parameters<typeof ToastLayer>[0]> = {}): string {
  return renderToStaticMarkup(
    <ToastLayer
      status={status()}
      repeat={1}
      action={UNDO}
      onDismiss={() => {}}
      {...props}
    />,
  );
}

describe("how long a toast lives", () => {
  it("is the undo window, and nothing else", () => {
    const window = undoWindowMs(DEFAULT_PREFERENCES);
    expect(statusLifetime(status(), window)).toBe(window);
  });

  it("follows the preference when the preference moves", () => {
    // The point of reading it rather than hardcoding: a person who sets the
    // window to five seconds is saying how long they want to be offered the
    // button, and the button is on the toast.
    const short = undoWindowMs({ ...DEFAULT_PREFERENCES, undoWindowSeconds: 5 });
    const long = undoWindowMs({ ...DEFAULT_PREFERENCES, undoWindowSeconds: 45 });
    expect(statusLifetime(status(), short)).toBe(5_000);
    expect(statusLifetime(status(), long)).toBe(45_000);
  });

  it("holds a failure longer than a confirmation, off the same number", () => {
    const window = undoWindowMs(DEFAULT_PREFERENCES);
    const failure = statusLifetime(status({ tone: "error" }), window);
    expect(failure).toBe(window * ERROR_HOLD);
    expect(failure).toBeGreaterThan(statusLifetime(status(), window));
  });
});

describe("a run of actions", () => {
  it("stays one toast however fast they arrive", () => {
    let run = noToast;
    for (let i = 0; i < 20; i++) run = collapse(run, status());
    // One message on screen, and a count that says how many it stands for.
    expect(run.status?.message).toBe("Archived 3 conversations");
    expect(run.repeat).toBe(20);
  });

  it("starts counting again when the news changes", () => {
    const first = collapse(collapse(noToast, status()), status());
    expect(first.repeat).toBe(2);
    const next = collapse(first, status({ message: "Snoozed 1 conversation" }));
    expect(next.repeat).toBe(1);
    expect(next.status?.message).toBe("Snoozed 1 conversation");
  });

  it("forgets the count once the toast is gone", () => {
    const run = collapse(collapse(noToast, status()), status());
    expect(collapse(run, null)).toEqual(noToast);
  });

  it("says nothing about a count of one", () => {
    expect(render({ repeat: 1 })).not.toContain("×");
    expect(render({ repeat: 3 })).toContain("×3");
  });
});

describe("what the toast offers", () => {
  it("offers undo for anything that carried an inverse", () => {
    expect(offerFor(status({ undo: { kind: "unarchive", threadIds: [1] } }))).toBe("undo");
  });

  it("offers redo for a message an undo produced", () => {
    expect(offerFor(status({ message: "Undid archived 3 conversations", offer: "redo" }))).toBe(
      "redo",
    );
  });

  it("offers nothing for a message that is not about an action", () => {
    // The status bar used to put `Undo` beside these, offering to take back
    // something entirely unrelated to what it was saying.
    expect(offerFor(status({ message: "Open a conversation first" }))).toBeNull();
    expect(offerFor(null)).toBeNull();
  });

  it("puts the shortcut on the button, so the shortcut is learnable", () => {
    const html = render();
    expect(html).toContain("<kbd");
    expect(html).toContain("Undo");
    // The whole sentence is the accessible name; "Undo" alone is what is drawn.
    expect(html).toContain('aria-label="Undo archived 3 conversations"');
  });

  it("draws no button when the stack has nothing to offer", () => {
    const html = render({ action: null });
    expect(html).not.toContain("aria-label=\"Undo archived 3 conversations\"");
    // The dismiss button stays: a toast you cannot get rid of is worse.
    expect(html).toContain('aria-label="Dismiss"');
  });
});

describe("failures", () => {
  it("interrupts, where a confirmation waits its turn", () => {
    const confirmation = regions(render());
    expect(confirmation.polite).toContain("Archived 3 conversations");
    expect(confirmation.assertive).not.toContain("Archived 3 conversations");

    const failure = regions(
      render({ status: status({ message: "2 failed — Google is rate limiting", tone: "error" }) }),
    );
    expect(failure.assertive).toContain("2 failed");
    expect(failure.polite).not.toContain("2 failed");
  });

  it("keeps both regions on the page whether or not they have anything in them", () => {
    // A live region that arrives with its content is not reliably announced.
    const html = render({ status: null });
    expect(html).toContain('aria-live="polite"');
    expect(html).toContain('aria-live="assertive"');
    expect(html).not.toContain("Archived");
  });

  it("looks different, not just louder", () => {
    expect(render()).toContain("border-border-strong");
    const failure = render({ status: status({ tone: "error" }) });
    expect(failure).toContain("border-danger");
    expect(failure).toContain("text-danger");
  });
});

describe("the layer itself", () => {
  it("cannot take a click that was meant for the list", () => {
    // The layer is inert; the card inside it is not.
    const html = render();
    expect(html).toContain("pointer-events-none fixed");
    expect(html).toContain("pointer-events-auto");
  });

  it("does not animate for anyone who asked it not to", () => {
    expect(render()).toContain("motion-reduce:transition-none");
  });
});
