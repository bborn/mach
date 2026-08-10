/**
 * The card that says there is new code, and the one keystroke that takes it.
 *
 * `HeldUpdate` itself needs a keymap and a dev server's socket; `UpdateOffer`
 * is the part with the claims in it — a message, a button that can be reached
 * by name, and the binding printed on it — and those survive being read as
 * markup, the same way the undo toast's do.
 */

import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { ToastLayer } from "./Toast";
import { TAKE_KEYS, UpdateOffer } from "./HeldUpdate";

function offer(over: { onTake?: () => void; onDismiss?: () => void } = {}): string {
  return renderToStaticMarkup(
    <UpdateOffer onTake={over.onTake ?? (() => {})} onDismiss={over.onDismiss ?? (() => {})} />,
  );
}

describe("the offer", () => {
  it("names the news and nothing else", () => {
    expect(offer()).toContain("New version");
  });

  it("holds out a button a screen reader can ask for by name", () => {
    expect(offer()).toContain('aria-label="Update to the new version"');
  });

  it("prints the binding on the button, the way Undo prints ⌘Z", () => {
    // ⌘R rather than a new letter: reloading is a gesture every window on the
    // machine already has, and the toast is where it is learned.
    expect(TAKE_KEYS).toBe("mod+r");
    expect(offer()).toContain("⌘R");
  });

  it("can be put away, since not now is an answer", () => {
    expect(offer()).toContain('aria-label="Dismiss"');
  });
});

describe("where it sits", () => {
  it("goes in the toast layer, above the message, not in a surface of its own", () => {
    const html = renderToStaticMarkup(
      <ToastLayer
        status={{ message: "Archived 3 conversations", tone: "info" }}
        repeat={1}
        action={null}
        onDismiss={() => {}}
      >
        <div id="held">held</div>
      </ToastLayer>,
    );
    expect(html.indexOf('id="held"')).toBeGreaterThan(-1);
    expect(html.indexOf('id="held"')).toBeLessThan(html.indexOf("Archived 3 conversations"));
    // One fixed layer, not two stacked on each other.
    expect(html.match(/class="pointer-events-none fixed/g)).toHaveLength(1);
  });

  it("does not need the layer to have a message in it", () => {
    const html = renderToStaticMarkup(
      <ToastLayer status={null} repeat={0} action={null} onDismiss={() => {}}>
        <div id="held">held</div>
      </ToastLayer>,
    );
    expect(html).toContain('id="held"');
  });
});
