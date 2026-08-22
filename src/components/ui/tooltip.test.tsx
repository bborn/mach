import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { ShortcutHint } from "./tooltip";

describe("a shortcut hint", () => {
  it("names the action and the key", () => {
    // `mod` is pinned rather than left to the platform. `detectModKey` reads
    // `navigator` and answers `ctrl` off a Mac, so an assertion about `⌘`
    // passes locally and fails on CI, which is Linux. Same reasoning, and the
    // same fix, as the note in `menu.test.ts`. What is under test is that the
    // hint names the action and renders the binding, not which key `mod` is.
    const html = renderToStaticMarkup(<ShortcutHint label="Mail" keys="mod+1" mod="meta" />);
    expect(html).toContain("Mail");
    expect(html).toContain("⌘1");
  });

  it("splits a sequence into chips with then between them", () => {
    const html = renderToStaticMarkup(<ShortcutHint label="Inbox" keys="g i" />);
    expect(html).toContain("Inbox");
    expect(html).toContain(">G<");
    expect(html).toContain(">I<");
    expect(html).toContain("then");
    // Two keys of a sequence, not one chip that already says "G then I".
    expect(html.match(/<kbd/g)).toHaveLength(2);
  });

  it("treats two spellings as alternatives, not a sequence", () => {
    // Pinned, as above.
    const html = renderToStaticMarkup(
      <ShortcutHint label="Search" keys={["/", "mod+f"]} mod="meta" />,
    );
    expect(html).toContain("Search");
    expect(html).toContain("/");
    expect(html).toContain("⌘F");
    expect(html).not.toContain("then");
  });
});
