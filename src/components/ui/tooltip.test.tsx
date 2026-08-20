import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { ShortcutHint } from "./tooltip";

describe("a shortcut hint", () => {
  it("names the action and the key", () => {
    const html = renderToStaticMarkup(<ShortcutHint label="Mail" keys="mod+1" />);
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
    const html = renderToStaticMarkup(<ShortcutHint label="Search" keys={["/", "mod+f"]} />);
    expect(html).toContain("Search");
    expect(html).toContain("/");
    expect(html).toContain("⌘F");
    expect(html).not.toContain("then");
  });
});
